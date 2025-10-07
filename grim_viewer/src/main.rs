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
use cli::{Args, load_layout_preset};
use env_logger;
use pollster::FutureExt;
use scene::{
    load_hotspot_event_log, load_lua_geometry_snapshot, load_movement_trace,
    load_scene_from_timeline, print_movement_trace_summary, print_scene_summary,
};
use texture::{decode_asset_texture, dump_texture_to_png, load_asset_bytes, load_zbm_seed};
use viewer::ViewerState;
use wgpu::SurfaceError;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::WindowBuilder,
};

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

    let geometry_snapshot = match args.lua_geometry_json.as_ref() {
        Some(path) => Some(
            load_lua_geometry_snapshot(path)
                .with_context(|| format!("loading Lua geometry snapshot {}", path.display()))?,
        ),
        None => None,
    };

    let mut movement_trace = match args.movement_log.as_ref() {
        Some(path) => Some(
            load_movement_trace(path)
                .with_context(|| format!("loading movement log {}", path.display()))?,
        ),
        None => None,
    };

    let mut hotspot_events = match args.event_log.as_ref() {
        Some(path) => Some(
            load_hotspot_event_log(path)
                .with_context(|| format!("loading hotspot event log {}", path.display()))?,
        ),
        None => None,
    };

    let scene_data = match args.timeline.as_ref() {
        Some(path) => {
            let mut scene = load_scene_from_timeline(
                path,
                &args.manifest,
                Some(asset_name.as_str()),
                geometry_snapshot.as_ref(),
            )
            .with_context(|| format!("loading timeline manifest {}", path.display()))?;
            if let Some(trace) = movement_trace.take() {
                scene.attach_movement_trace(trace);
            }
            if let Some(events) = hotspot_events.take() {
                scene.attach_hotspot_events(events);
            }
            Some(scene)
        }
        None => None,
    };

    if hotspot_events.is_some() {
        println!(
            "Hotspot event log loaded without timeline overlay; run with --timeline to visualise markers."
        );
    }

    if let Some(scene) = scene_data.as_ref() {
        print_scene_summary(scene);
    }

    if let Some(trace) = movement_trace.take() {
        println!(
            "Movement trace loaded without timeline overlay; pass --timeline alongside --movement-log to sync markers."
        );
        print_movement_trace_summary(&trace);
    }

    let audio_log_path = args.audio_log.clone();

    if args.headless {
        // Propagate any decoding failure before exiting early.
        decode_result?;
        if let Some(path) = audio_log_path.as_ref() {
            let mut watcher = AudioLogWatcher::new(path.clone());
            run_audio_log_headless(&mut watcher)?;
        }
        println!("Headless mode requested; viewer window bootstrap skipped.");
        return Ok(());
    }

    let scene = scene_data.map(Arc::new);

    let audio_status_rx = audio_log_path
        .as_ref()
        .map(|path| spawn_audio_log_thread(AudioLogWatcher::new(path.clone())));

    // Bring up the audio stack so the renderer can acquire an output stream later.
    init_audio()?;

    let event_loop = EventLoop::new().context("creating winit event loop")?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title(format!("Grim Viewer - {}", asset_name))
            .with_inner_size(PhysicalSize::new(1280, 720))
            .build(&event_loop)
            .context("creating viewer window")?,
    );

    let audio_overlay_requested = audio_status_rx.is_some();

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
                                KeyEvent {
                                    logical_key: Key::Character(key),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => state.handle_character_input(key.as_ref()),
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
