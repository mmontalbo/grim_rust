//! CLI entrypoint for `grim_viewer`. Wires the manifest loaders from `scene`,
//! prepares decoded bitmaps via `texture`, and drives `ViewerState` so the
//! runtime overlays stay in sync with the optional timeline, movement, and
//! audio fixtures. Also exposes headless and PNG-dump paths used by automation
//! to validate decoded assets without opening a window.

use std::sync::Arc;

mod audio;
mod audio_log;
mod cli;
mod scene;
mod texture;
mod timeline;
mod ui_layout;
mod viewer;

use anyhow::{Context, Result, anyhow};
use audio::{
    AudioLogWatcher, AudioStatus, init_audio, log_audio_update, run_audio_log_headless,
    spawn_audio_log_thread,
};
use audio_log::AudioAggregation;
use clap::Parser;
use cli::{Args, PanelPreset, load_layout_preset};
use env_logger;
use pollster::FutureExt;
use scene::{
    HotspotEvent, LuaGeometrySnapshot, MovementTrace, ViewerScene, load_hotspot_event_log,
    load_lua_geometry_snapshot, load_movement_trace, load_scene_from_timeline,
    print_movement_trace_summary, print_scene_summary,
};
use texture::{decode_asset_texture, dump_texture_to_png, load_asset_bytes, load_zbm_seed};
use ui_layout::{DEFAULT_MINIMAP_MIN_SIDE, PANEL_MARGIN};
use viewer::ViewerState;
use wgpu::SurfaceError;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::WindowBuilder,
};

/// Optional scene attachments loaded from CLI arguments. Keeps manifest parsing
/// in one place so `main` can focus on wiring `ViewerState`.
struct SceneFixtures {
    geometry: Option<LuaGeometrySnapshot>,
    movement: Option<MovementTrace>,
    hotspots: Option<Vec<HotspotEvent>>,
}

impl SceneFixtures {
    /// Load optional geometry, movement, and hotspot fixtures from CLI flags,
    /// preserving the per-file `with_context` error messages expected by the
    /// user-facing CLI.
    fn load(args: &Args) -> Result<Self> {
        let geometry = args
            .lua_geometry_json
            .as_ref()
            .map(|path| {
                load_lua_geometry_snapshot(path)
                    .with_context(|| format!("loading Lua geometry snapshot {}", path.display()))
            })
            .transpose()?;

        let movement = args
            .movement_log
            .as_ref()
            .map(|path| {
                load_movement_trace(path)
                    .with_context(|| format!("loading movement log {}", path.display()))
            })
            .transpose()?;

        let hotspots = args
            .event_log
            .as_ref()
            .map(|path| {
                load_hotspot_event_log(path)
                    .with_context(|| format!("loading hotspot event log {}", path.display()))
            })
            .transpose()?;

        Ok(Self {
            geometry,
            movement,
            hotspots,
        })
    }

    /// Geometry snapshot used to align Manny/desk/tube markers can be provided
    /// independent of a timeline; we only inspect it when a timeline is loaded.
    fn geometry(&self) -> Option<&LuaGeometrySnapshot> {
        self.geometry.as_ref()
    }

    /// Attach movement and hotspot fixtures to the constructed scene. Consumes
    /// the stored data so follow-up summaries know whether attachments occurred.
    fn attach_to_scene(&mut self, scene: &mut ViewerScene) {
        if let Some(trace) = self.movement.take() {
            scene.attach_movement_trace(trace);
        }
        if let Some(events) = self.hotspots.take() {
            scene.attach_hotspot_events(events);
        }
    }

    /// Indicates whether hotspot events were provided without a timeline. The
    /// CLI surfaces a hint in that scenario so users rerun with `--timeline`.
    fn has_unattached_hotspots(&self) -> bool {
        self.hotspots.is_some()
    }

    /// Give back movement traces when no timeline was requested so we can still
    /// print command-line summaries for the operator.
    fn take_movement(&mut self) -> Option<MovementTrace> {
        self.movement.take()
    }
}

/// Describes whether the viewer should render interactively or exit after
/// headless processing. Derived from CLI arguments so call sites can pattern
/// match instead of juggling booleans.
enum RunMode {
    Interactive,
    Headless,
}

fn main() -> Result<()> {
    let args = Args::parse();

    env_logger::init();

    let layout_preset = args
        .layout_preset
        .as_ref()
        .map(|path| load_layout_preset(path.as_path()))
        .transpose()?;

    if let Some(path) = args.layout_preset.as_ref() {
        println!("Using layout preset {}", path.display());
    }

    let (asset_name, asset_bytes, source_archive) =
        load_asset_bytes(&args.manifest, &args.asset).context("loading requested asset")?;
    println!(
        "Loaded {} ({} bytes) from {} (manifest: {})",
        asset_name,
        asset_bytes.len(),
        source_archive.display(),
        args.manifest.display()
    );

    let seed_bitmap = if asset_name.to_ascii_lowercase().ends_with(".zbm") {
        match load_zbm_seed(&args.manifest, &asset_name) {
            Ok(Some(seed)) => Some(seed),
            Ok(None) => None,
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: seed lookup failed for {}: {}",
                    asset_name, err
                );
                None
            }
        }
    } else {
        None
    };

    let decode_result = decode_asset_texture(&asset_name, &asset_bytes, seed_bitmap.as_ref());

    if let Some(output_path) = args.dump_frame.as_ref() {
        let preview = decode_result
            .as_ref()
            .map_err(|err| anyhow!("decoding bitmap for --dump-frame: {err}"))?;
        let stats = dump_texture_to_png(preview, output_path)
            .with_context(|| format!("writing PNG to {}", output_path.display()))?;
        println!(
            "Bitmap frame exported to {} ({}x{} codec {} format {} frame count {})",
            output_path.display(),
            preview.width,
            preview.height,
            preview.codec,
            preview.format,
            preview.frame_count
        );
        if let Some(stats) = preview.depth_stats {
            let (min, max) = (stats.min, stats.max);
            if preview.depth_preview {
                println!(
                    "  raw depth range (16-bit) 0x{min:04X} – 0x{max:04X}; export visualises normalized depth"
                );
            } else {
                println!(
                    "  raw depth range (16-bit) 0x{min:04X} – 0x{max:04X}; color sourced from base bitmap"
                );
            }
            println!(
                "  depth pixels zero {zero} / {total}",
                zero = stats.zero_pixels,
                total = stats.total_pixels()
            );
        }
        println!(
            "  luminance avg {:.2}, min {}, max {}, opaque pixels {} / {}",
            stats.mean_luma,
            stats.min_luma,
            stats.max_luma,
            stats.opaque_pixels,
            stats.total_pixels
        );
        println!(
            "  quadrant luma means (TL, TR, BL, BR): {:.2}, {:.2}, {:.2}, {:.2}",
            stats.quadrant_means[0],
            stats.quadrant_means[1],
            stats.quadrant_means[2],
            stats.quadrant_means[3]
        );
    }

    let mut fixtures = SceneFixtures::load(&args)?;
    let run_mode = if args.headless {
        RunMode::Headless
    } else {
        RunMode::Interactive
    };

    let scene_data = match args.timeline.as_ref() {
        Some(path) => {
            let mut scene = load_scene_from_timeline(
                path,
                &args.manifest,
                Some(asset_name.as_str()),
                fixtures.geometry(),
            )
            .with_context(|| format!("loading timeline manifest {}", path.display()))?;
            fixtures.attach_to_scene(&mut scene);
            Some(scene)
        }
        None => None,
    };

    if fixtures.has_unattached_hotspots() {
        println!(
            "Hotspot event log loaded without timeline overlay; run with --timeline to visualise markers."
        );
    }

    if let Some(scene) = scene_data.as_ref() {
        print_scene_summary(scene);
    }

    if let Some(trace) = fixtures.take_movement() {
        println!(
            "Movement trace loaded without timeline overlay; pass --timeline alongside --movement-log to sync markers."
        );
        print_movement_trace_summary(&trace);
    }

    let audio_log_path = args.audio_log.clone();

    if matches!(run_mode, RunMode::Headless) {
        // Propagate any decoding failure before exiting early.
        decode_result?;
        if let Some(path) = audio_log_path.as_ref() {
            let mut watcher = AudioLogWatcher::new(path.clone());
            run_audio_log_headless(&mut watcher)?;
        }
        println!("Headless mode requested; viewer window bootstrap skipped.");
        return Ok(());
    }

    let timeline_present = scene_data
        .as_ref()
        .and_then(|scene| scene.timeline.as_ref())
        .is_some();

    let scene = scene_data.map(Arc::new);

    let audio_status_rx = audio_log_path
        .as_ref()
        .map(|path| spawn_audio_log_thread(AudioLogWatcher::new(path.clone())));

    let audio_overlay_requested = audio_status_rx.is_some();

    let layout_preset_ref = layout_preset.as_ref();
    let panel_width = |preset: Option<&PanelPreset>, default: u32| -> u32 {
        preset.and_then(|p| p.width).unwrap_or(default)
    };

    let audio_preset = layout_preset_ref.and_then(|preset| preset.audio.as_ref());
    let scrubber_preset = layout_preset_ref.and_then(|preset| preset.scrubber.as_ref());
    let timeline_preset = layout_preset_ref.and_then(|preset| preset.timeline.as_ref());
    let minimap_preset = layout_preset_ref.and_then(|preset| preset.minimap.as_ref());

    let audio_enabled =
        audio_overlay_requested && audio_preset.map(PanelPreset::enabled).unwrap_or(true);
    let audio_width = if audio_enabled {
        panel_width(audio_preset, 520)
    } else {
        0
    };

    let scrubber_enabled = scrubber_preset.map(PanelPreset::enabled).unwrap_or(true);
    let scrubber_width = if scrubber_enabled {
        panel_width(scrubber_preset, 520)
    } else {
        0
    };

    let left_panel_width = audio_width.max(scrubber_width);

    let timeline_enabled =
        timeline_present && timeline_preset.map(PanelPreset::enabled).unwrap_or(true);
    let timeline_width = if timeline_enabled {
        panel_width(timeline_preset, 640)
    } else {
        0
    };

    let minimap_side = minimap_preset
        .and_then(|preset| preset.min_side)
        .unwrap_or(DEFAULT_MINIMAP_MIN_SIDE)
        .ceil() as u32;
    let right_panel_width = timeline_width.max(minimap_side);

    let margin = PANEL_MARGIN.ceil() as u32;
    let max_panel_width = left_panel_width.max(right_panel_width);
    let required_bar = if max_panel_width > 0 {
        margin * 2 + max_panel_width
    } else {
        margin * 2
    };

    // Bring up the audio stack so the renderer can acquire an output stream later.
    init_audio()?;

    let (initial_window_width, initial_window_height) = decode_result
        .as_ref()
        .map(|preview| {
            let base_width = preview.width.max(1);
            let base_height = preview.height.max(1);
            let expanded_width = base_width.saturating_add(required_bar.saturating_mul(2));
            (expanded_width.max(base_width), base_height)
        })
        .unwrap_or((1280, 720));

    let event_loop = EventLoop::new().context("creating winit event loop")?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title(format!("Grim Viewer - {}", asset_name))
            .with_inner_size(PhysicalSize::new(
                initial_window_width,
                initial_window_height,
            ))
            .build(&event_loop)
            .context("creating viewer window")?,
    );

    let mut state = ViewerState::new(
        window,
        &asset_name,
        asset_bytes,
        decode_result,
        scene.clone(),
        audio_overlay_requested,
        layout_preset,
    )
    .block_on()?;

    if audio_overlay_requested {
        let default_status = AudioStatus::new(AudioAggregation::default(), false);
        let initial_status = audio_status_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
            .unwrap_or_else(|| default_status.clone());
        state.update_audio_overlay(&initial_status);
        log_audio_update(&initial_status);
    }

    event_loop
        .run(move |event, target| {
            target.set_control_flow(ControlFlow::Poll);

            match event {
                Event::WindowEvent { window_id, event } if window_id == state.window().id() => {
                    match event {
                        WindowEvent::CloseRequested => target.exit(),
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Named(NamedKey::Escape),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => target.exit(),
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Named(NamedKey::ArrowRight),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => state.next_entity(),
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Named(NamedKey::ArrowLeft),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => state.previous_entity(),
                        WindowEvent::KeyboardInput {
                            event:
                                key_event @ KeyEvent {
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => state.handle_key_event(&key_event),
                        WindowEvent::Resized(new_size) => state.resize(new_size),
                        WindowEvent::RedrawRequested => match state.render() {
                            Ok(_) => {}
                            Err(SurfaceError::Lost) => state.resize(state.size()),
                            Err(SurfaceError::OutOfMemory) => target.exit(),
                            Err(err) => eprintln!("[grim_viewer] render error: {err:?}"),
                        },
                        _ => {}
                    }
                }
                Event::AboutToWait => {
                    if let Some(rx) = audio_status_rx.as_ref() {
                        while let Ok(status) = rx.try_recv() {
                            state.update_audio_overlay(&status);
                            log_audio_update(&status);
                        }
                    }
                    state.window().request_redraw();
                }
                _ => {}
            }
        })
        .context("running viewer application")?;
    Ok(())
}
