use std::{
    collections::VecDeque,
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError},
    },
    time::Instant,
};

mod display;
mod live_stream;

use anyhow::Result;
use clap::Parser;
use display::ViewerState;
use env_logger;
use grim_stream::{Frame, Hello, StateUpdate, StreamConfig};
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
                drain_engine_events(engine_stream.as_mut());
                update_window_title(
                    viewer.window(),
                    &controls,
                    &retail_stream,
                    engine_stream.as_ref(),
                );
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
                    stream.config = Some(config);
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

    stream.last_frame = Some(FrameStats {
        frame_id: queued.frame.frame_id,
        host_time_ns: queued.frame.host_time_ns,
        received_at: queued.received_at,
    });
}

fn drain_engine_events(stream: Option<&mut EngineStreamState>) {
    let Some(stream) = stream else {
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
                    stream.last_update = Some(update);
                }
                EngineEvent::ProtocolError(message) => {
                    eprintln!("[grim_viewer] engine protocol: {message}");
                }
                EngineEvent::Disconnected { reason } => {
                    eprintln!("[grim_viewer] engine disconnected: {reason}");
                    stream.hello = None;
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

fn update_window_title(
    window: &winit::window::Window,
    controls: &SyncControls,
    retail: &RetailStreamState,
    engine: Option<&EngineStreamState>,
) {
    let mode = if controls.paused {
        if controls.pending_steps > 0 {
            format!("paused ({} steps queued)", controls.pending_steps)
        } else {
            "paused".to_string()
        }
    } else {
        "live".to_string()
    };

    let mut parts = vec![format!("Grim Viewer - {}", mode)];

    if let Some(frame) = retail.last_frame.as_ref() {
        let age = Instant::now().saturating_duration_since(frame.received_at);
        parts.push(format!(
            "frame {} ({} ms old)",
            frame.frame_id,
            age.as_millis()
        ));
    } else if retail.hello.is_some() {
        parts.push("waiting for frames".to_string());
    } else {
        parts.push("retail offline".to_string());
    }

    if let Some(engine) = engine {
        if let Some(update) = engine.last_update.as_ref() {
            let age = engine
                .last_update_received
                .map(|ts| Instant::now().saturating_duration_since(ts).as_millis())
                .unwrap_or(0);
            let mut engine_part = format!("engine seq {}", update.seq);
            if let Some(frame) = retail.last_frame.as_ref() {
                let delta = update.host_time_ns as i128 - frame.host_time_ns as i128;
                if controls.diff_enabled {
                    engine_part.push_str(&format!(" Δt {} ms", (delta as f64) / 1_000_000.0));
                }
            }
            engine_part.push_str(&format!(" ({} ms old)", age));
            parts.push(engine_part);
        } else if engine.hello.is_some() {
            parts.push("engine awaiting state".to_string());
        } else {
            parts.push("engine offline".to_string());
        }
    }

    window.set_title(&parts.join(" | "));
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
