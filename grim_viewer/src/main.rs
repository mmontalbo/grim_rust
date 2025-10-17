use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError},
    },
    time::Instant,
};

mod display;
mod layout;
mod live_scene;
mod live_stream;
mod movie;
mod overlay;
#[allow(dead_code)]
mod scene;
#[allow(dead_code)]
mod texture;
#[allow(dead_code)]
mod timeline;

use anyhow::Result;
use clap::Parser;
use display::ViewerState;
use env_logger;
use grim_stream::{Frame, Hello, StateUpdate, StreamConfig};
use live_scene::{LiveSceneConfig, LiveSceneState};
use live_stream::{EngineEvent, RetailEvent, spawn_engine_client, spawn_retail_client};
use wgpu::SurfaceError;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::WindowBuilder,
};

#[derive(Parser, Debug)]
#[command(about = "Live GrimStream viewer", version)]
struct Args {
    /// GrimStream endpoint that publishes retail frames (host:port)
    #[arg(long, default_value = "127.0.0.1:17400")]
    retail_stream: String,

    /// Optional GrimStream endpoint that publishes engine state updates
    #[arg(long)]
    engine_stream: Option<String>,

    /// Initial window width in pixels
    #[arg(long, default_value_t = 1280)]
    window_width: u32,

    /// Initial window height in pixels
    #[arg(long, default_value_t = 720)]
    window_height: u32,

    /// Optional asset manifest that enumerates Manny office resources
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    scene_assets_manifest: Option<PathBuf>,

    /// Optional timeline manifest exported by grim_engine
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    scene_timeline: Option<PathBuf>,

    /// Optional Lua geometry snapshot to validate entity placement
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    scene_geometry: Option<PathBuf>,

    /// Optional movement log to seed the Manny scrubber
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    scene_movement_log: Option<PathBuf>,

    /// Optional hotspot event log for Manny office fixtures
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    scene_hotspot_log: Option<PathBuf>,

    /// Preferred Manny office background asset (e.g. mo_0_ddtws.bm)
    #[arg(long)]
    scene_active_asset: Option<String>,
}

struct RetailStreamState {
    rx: Receiver<RetailEvent>,
    config: Option<StreamConfig>,
    hello: Option<Hello>,
    pending_frames: VecDeque<QueuedFrame>,
    last_frame: Option<FrameStats>,
}

struct EngineStreamState {
    rx: Receiver<EngineEvent>,
    hello: Option<Hello>,
    last_update: Option<StateUpdate>,
    last_update_received: Option<Instant>,
    active_movie: Option<ActiveMovieStatus>,
}

struct QueuedFrame {
    frame: Frame,
    received_at: Instant,
}

#[derive(Clone, Copy)]
struct FrameStats {
    frame_id: u64,
    host_time_ns: u64,
    received_at: Instant,
}

struct ActiveMovieStatus {
    name: String,
    started: Instant,
}

#[derive(Default)]
struct SyncControls {
    paused: bool,
    pending_steps: u32,
    diff_enabled: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    env_logger::init();

    let event_loop = EventLoop::new()?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Grim Viewer")
            .with_inner_size(PhysicalSize::new(args.window_width, args.window_height))
            .build(&event_loop)?,
    );

    let mut viewer = pollster::block_on(ViewerState::new(
        window.clone(),
        args.window_width,
        args.window_height,
    ))?;

    let mut live_scene = match LiveSceneConfig::from_args(&args)? {
        Some(config) => match LiveSceneState::load(config) {
            Ok(state) => Some(state),
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: failed to bootstrap Manny scene overlays: {err:?}"
                );
                None
            }
        },
        None => None,
    };

    if let Some(scene) = live_scene.as_mut() {
        if let Some(frame) = scene.compose_engine_frame() {
            if let Err(err) = viewer.upload_engine_frame(frame.width, frame.height, frame.pixels) {
                eprintln!(
                    "[grim_viewer] warning: failed to upload initial engine overlay: {err:?}"
                );
            }
        }
    }

    let mut retail_stream = RetailStreamState {
        rx: spawn_retail_client(args.retail_stream.clone()),
        config: None,
        hello: None,
        pending_frames: VecDeque::new(),
        last_frame: None,
    };

    println!(
        "[grim_viewer] retail stream -> {} (window {}x{})",
        args.retail_stream, args.window_width, args.window_height
    );

    let mut engine_stream = args.engine_stream.as_ref().map(|addr| {
        println!("[grim_viewer] engine stream -> {addr}");
        EngineStreamState {
            rx: spawn_engine_client(addr.clone()),
            hello: None,
            last_update: None,
            last_update_received: None,
            active_movie: None,
        }
    });

    let mut controls = SyncControls::default();

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);

        match event {
            Event::WindowEvent { window_id, event } if window_id == viewer.window().id() => {
                match event {
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::Resized(size) => viewer.resize(size),
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
                            key_event @ KeyEvent {
                                state: ElementState::Pressed,
                                ..
                            },
                        ..
                    } => {
                        if !handle_sync_key(&key_event, &mut controls) {
                            // no-op for now
                        }
                    }
                    WindowEvent::RedrawRequested => match viewer.render() {
                        Ok(_) => {}
                        Err(SurfaceError::Lost) => viewer.resize(viewer.size()),
                        Err(SurfaceError::OutOfMemory) => target.exit(),
                        Err(err) => eprintln!("[grim_viewer] render error: {err:?}"),
                    },
                    _ => {}
                }
            }
            Event::AboutToWait => {
                drain_retail_events(&mut retail_stream, &mut viewer, &mut controls);
                drain_engine_events(engine_stream.as_mut(), &mut live_scene, &mut viewer);
                update_view_labels(&mut viewer, &retail_stream, engine_stream.as_ref());
                update_debug_panel(
                    &mut viewer,
                    &controls,
                    &retail_stream,
                    engine_stream.as_ref(),
                );
                update_window_title(viewer.window(), &controls);
                viewer.window().request_redraw();
            }
            _ => {}
        }
    })?;
    Ok(())
}

fn drain_retail_events(
    stream: &mut RetailStreamState,
    viewer: &mut ViewerState,
    controls: &mut SyncControls,
) {
    loop {
        match stream.rx.try_recv() {
            Ok(event) => match event {
                RetailEvent::Connecting { addr, attempt } => {
                    if attempt > 1 {
                        println!(
                            "[grim_viewer] reconnecting to retail stream {addr} (attempt {attempt})"
                        );
                    }
                }
                RetailEvent::Connected(hello) => {
                    println!(
                        "[grim_viewer] retail connected: producer={} build={}",
                        hello.producer,
                        hello.build.as_deref().unwrap_or("-")
                    );
                    stream.hello = Some(hello);
                }
                RetailEvent::StreamConfig(config) => {
                    println!(
                        "[grim_viewer] retail stream config {}x{} stride {} pixel {:?} fps {:?}",
                        config.width,
                        config.height,
                        config.stride_bytes,
                        config.pixel_format,
                        config.nominal_fps
                    );
                    let width = config.width;
                    let height = config.height;
                    stream.config = Some(config);
                    viewer.set_frame_dimensions(width, height);
                }
                RetailEvent::Frame(frame) => {
                    stream.pending_frames.push_back(QueuedFrame {
                        frame,
                        received_at: Instant::now(),
                    });
                    while stream.pending_frames.len() > 8 {
                        stream.pending_frames.pop_front();
                    }
                }
                RetailEvent::ProtocolError(message) => {
                    eprintln!("[grim_viewer] retail protocol: {message}");
                }
                RetailEvent::Disconnected { reason } => {
                    eprintln!("[grim_viewer] retail disconnected: {reason}");
                    stream.hello = None;
                }
            },
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                eprintln!("[grim_viewer] retail stream channel closed");
                break;
            }
        }
    }

    if controls.paused {
        if controls.pending_steps == 0 {
            return;
        }
        if let Some(queued) = stream.pending_frames.pop_front() {
            present_frame(stream, viewer, queued);
        }
        controls.pending_steps = controls.pending_steps.saturating_sub(1);
        return;
    }

    controls.pending_steps = 0;
    while let Some(queued) = stream.pending_frames.pop_front() {
        present_frame(stream, viewer, queued);
    }
}

fn present_frame(stream: &mut RetailStreamState, viewer: &mut ViewerState, queued: QueuedFrame) {
    let Some(config) = stream.config.as_ref() else {
        eprintln!(
            "[grim_viewer] dropping frame {} (missing stream config)",
            queued.frame.frame_id
        );
        return;
    };

    if let Err(err) = viewer.upload_frame(
        config.width,
        config.height,
        config.stride_bytes,
        &queued.frame.data,
    ) {
        eprintln!(
            "[grim_viewer] frame {} upload failed: {err:?}",
            queued.frame.frame_id
        );
        return;
    }

    viewer.set_frame_dimensions(config.width, config.height);

    stream.last_frame = Some(FrameStats {
        frame_id: queued.frame.frame_id,
        host_time_ns: queued.frame.host_time_ns,
        received_at: queued.received_at,
    });
}

fn drain_engine_events(
    stream: Option<&mut EngineStreamState>,
    live_scene: &mut Option<LiveSceneState>,
    viewer: &mut ViewerState,
) {
    let Some(stream) = stream else {
        let _ = live_scene;
        let _ = viewer;
        return;
    };

    loop {
        match stream.rx.try_recv() {
            Ok(event) => match event {
                EngineEvent::Connecting { addr, attempt } => {
                    if attempt > 1 {
                        println!(
                            "[grim_viewer] reconnecting to engine stream {addr} (attempt {attempt})"
                        );
                    }
                }
                EngineEvent::Connected(hello) => {
                    println!(
                        "[grim_viewer] engine connected: producer={} build={}",
                        hello.producer,
                        hello.build.as_deref().unwrap_or("-")
                    );
                    stream.hello = Some(hello);
                }
                EngineEvent::State(update) => {
                    stream.last_update_received = Some(Instant::now());
                    stream.last_update = Some(update.clone());
                    apply_engine_events(stream, &update.events);
                    if let Some(scene) = live_scene.as_mut() {
                        if let Some(frame) = scene.ingest_state_update(&update) {
                            if let Err(err) =
                                viewer.upload_engine_frame(frame.width, frame.height, frame.pixels)
                            {
                                eprintln!("[grim_viewer] engine frame upload failed: {err:?}");
                            }
                        }
                    }
                }
                EngineEvent::ProtocolError(message) => {
                    eprintln!("[grim_viewer] engine protocol: {message}");
                }
                EngineEvent::Disconnected { reason } => {
                    eprintln!("[grim_viewer] engine disconnected: {reason}");
                    stream.hello = None;
                    stream.active_movie = None;
                }
            },
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                eprintln!("[grim_viewer] engine stream channel closed");
                break;
            }
        }
    }
}

fn apply_engine_events(stream: &mut EngineStreamState, events: &[String]) {
    for event in events {
        if let Some(movie) = event.strip_prefix("cut_scene.fullscreen.start ") {
            stream.active_movie = Some(ActiveMovieStatus {
                name: movie.to_string(),
                started: Instant::now(),
            });
        } else if event.starts_with("cut_scene.fullscreen.end ") {
            stream.active_movie = None;
        }
    }
}

fn update_window_title(window: &winit::window::Window, controls: &SyncControls) {
    let label = if controls.paused {
        if controls.pending_steps > 0 {
            format!(
                "Grim Viewer - paused ({} steps queued)",
                controls.pending_steps
            )
        } else {
            "Grim Viewer - paused".to_string()
        }
    } else {
        "Grim Viewer - live".to_string()
    };
    window.set_title(&label);
}

fn update_view_labels(
    viewer: &mut ViewerState,
    retail: &RetailStreamState,
    engine: Option<&EngineStreamState>,
) {
    let retail_label = if retail.hello.is_some() {
        if let Some(frame) = retail.last_frame.as_ref() {
            format!("Retail Capture (frame {})", frame.frame_id)
        } else {
            "Retail Capture (connected)".to_string()
        }
    } else {
        "Retail Capture (offline)".to_string()
    };
    viewer.set_retail_label(&retail_label);

    let engine_label = match engine {
        Some(stream) if stream.hello.is_some() => {
            let mut label = if let Some(update) = stream.last_update.as_ref() {
                format!("Rust Engine (seq {})", update.seq)
            } else {
                "Rust Engine (connected)".to_string()
            };
            if let Some(movie) = stream.active_movie.as_ref() {
                label.push_str(&format!(" ⋅ {}", movie.name));
            }
            label
        }
        Some(_) => "Rust Engine (offline)".to_string(),
        None => "Rust Engine".to_string(),
    };
    viewer.set_engine_label(&engine_label);
}

fn update_debug_panel(
    viewer: &mut ViewerState,
    controls: &SyncControls,
    retail: &RetailStreamState,
    engine: Option<&EngineStreamState>,
) {
    let mut lines = Vec::new();
    let mode_label = if controls.paused {
        if controls.pending_steps > 0 {
            format!("paused ({} steps queued)", controls.pending_steps)
        } else {
            "paused".to_string()
        }
    } else {
        "live".to_string()
    };
    lines.push("Session Status".to_string());
    lines.push(format!("Mode: {mode_label}"));
    lines.push(format!(
        "Diff overlay: {}",
        if controls.diff_enabled { "on" } else { "off" }
    ));

    lines.push(String::new());
    lines.push("Retail Stream".to_string());
    if let Some(frame) = retail.last_frame.as_ref() {
        let age_ms = Instant::now()
            .saturating_duration_since(frame.received_at)
            .as_millis();
        lines.push(format!("Frame: {} (age {} ms)", frame.frame_id, age_ms));
    } else if retail.hello.is_some() {
        lines.push("Frame: pending".to_string());
    } else {
        lines.push("Status: offline".to_string());
    }
    if let Some(config) = retail.config.as_ref() {
        let fps_label = config
            .nominal_fps
            .map(|fps| format!(" @ {:.1} fps", fps))
            .unwrap_or_default();
        lines.push(format!(
            "Config: {}x{}{}",
            config.width, config.height, fps_label
        ));
    }

    lines.push(String::new());
    lines.push("Engine Stream".to_string());
    match engine {
        Some(stream) if stream.hello.is_some() => {
            if let Some(update) = stream.last_update.as_ref() {
                let age_ms = stream
                    .last_update_received
                    .map(|ts| Instant::now().saturating_duration_since(ts).as_millis())
                    .unwrap_or(0);
                lines.push(format!("Seq: {} (age {} ms)", update.seq, age_ms));
                if controls.diff_enabled {
                    if let Some(frame) = retail.last_frame.as_ref() {
                        let delta_ms = (update.host_time_ns as i128 - frame.host_time_ns as i128)
                            as f64
                            / 1_000_000.0;
                        lines.push(format!("Frame Δt: {delta_ms:.2} ms"));
                    }
                }
                if let Some(movie) = stream.active_movie.as_ref() {
                    let movie_age = movie.started.elapsed().as_millis();
                    lines.push(format!("Cutscene: {} ({} ms)", movie.name, movie_age));
                }
            } else {
                lines.push("Seq: awaiting updates".to_string());
            }
        }
        Some(_) => {
            lines.push("Status: offline".to_string());
        }
        None => {
            lines.push("Status: disabled".to_string());
        }
    }

    viewer.set_debug_lines(&lines);
}

fn handle_sync_key(event: &KeyEvent, controls: &mut SyncControls) -> bool {
    match event.logical_key.as_ref() {
        Key::Named(NamedKey::Space) => {
            controls.paused = !controls.paused;
            if !controls.paused {
                controls.pending_steps = 0;
            }
            true
        }
        Key::Character(symbol) => match symbol.as_ref() {
            " " => {
                controls.paused = !controls.paused;
                if !controls.paused {
                    controls.pending_steps = 0;
                }
                true
            }
            "." | ">" => {
                if !controls.paused {
                    controls.paused = true;
                }
                controls.pending_steps = controls.pending_steps.saturating_add(1);
                true
            }
            "d" | "D" => {
                controls.diff_enabled = !controls.diff_enabled;
                true
            }
            _ => false,
        },
        _ => false,
    }
}
