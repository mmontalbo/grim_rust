use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

mod display;
mod layout;
mod live_stream;
mod movie;
mod overlay;

use anyhow::{Result, anyhow};
use clap::Parser;
use crossbeam_channel::TryRecvError as CrossbeamTryRecvError;
use display::ViewerState;
use env_logger;
use grim_stream::{Frame, Hello, MovieAction, MovieControl, MovieStart, StateUpdate, StreamConfig};
use live_stream::{
    EngineCommand, EngineCommandSender, EngineEvent, RetailEvent, spawn_engine_client,
    spawn_retail_client,
};
use movie::{MovieFrame, MoviePlayback, MoviePlaybackEvent};
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
    #[arg(long, default_value = "127.0.0.1:17400", conflicts_with = "no_retail")]
    retail_stream: String,

    /// Optional GrimStream endpoint that publishes engine state updates
    #[arg(long)]
    engine_stream: Option<String>,

    /// Disable the retail capture stream and focus on the engine viewport only
    #[arg(long)]
    no_retail: bool,

    /// Initial window width in pixels
    #[arg(long, default_value_t = 1280)]
    window_width: u32,

    /// Initial window height in pixels
    #[arg(long, default_value_t = 720)]
    window_height: u32,
}

struct RetailStreamState {
    rx: Option<Receiver<RetailEvent>>,
    enabled: bool,
    config: Option<StreamConfig>,
    hello: Option<Hello>,
    pending_frames: VecDeque<QueuedFrame>,
    last_frame: Option<FrameStats>,
}

impl RetailStreamState {
    fn with_receiver(rx: Receiver<RetailEvent>) -> Self {
        Self {
            rx: Some(rx),
            enabled: true,
            config: None,
            hello: None,
            pending_frames: VecDeque::new(),
            last_frame: None,
        }
    }

    fn disabled() -> Self {
        Self {
            rx: None,
            enabled: false,
            config: None,
            hello: None,
            pending_frames: VecDeque::new(),
            last_frame: None,
        }
    }
}

struct EngineStreamState {
    rx: Receiver<EngineEvent>,
    command_tx: EngineCommandSender,
    hello: Option<Hello>,
    last_update: Option<StateUpdate>,
    active_movie: Option<ActiveMovieStatus>,
    install_root: PathBuf,
}

struct QueuedFrame {
    frame: Frame,
}

#[derive(Clone, Copy)]
struct FrameStats {
    frame_id: u64,
    host_time_ns: u64,
}

struct ActiveMovieStatus {
    name: String,
    playback: MoviePlayback,
    status: MovieDisplayStatus,
    frames_rendered: u64,
    frames_received: u64,
    last_pts: Option<Duration>,
    upload_time_ms_total: f64,
    last_log_report: Instant,
    presentation_origin: Option<Instant>,
    last_present_wall: Option<Instant>,
    pending_frame: Option<MovieFrame>,
    pending_deadline: Option<Instant>,
}

const MOVIE_PROGRESS_FRAME_INTERVAL: u64 = 60;
const MOVIE_PROGRESS_TIME_INTERVAL: Duration = Duration::from_secs(1);
const MAX_FRAME_LAG: Duration = Duration::from_millis(200);
const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(16);

enum MovieDisplayStatus {
    Playing,
    Skipping,
}

impl MovieDisplayStatus {
    fn as_label(&self) -> &'static str {
        match self {
            MovieDisplayStatus::Playing => "playing",
            MovieDisplayStatus::Skipping => "skipping",
        }
    }
}

impl ActiveMovieStatus {
    fn new(name: String, playback: MoviePlayback) -> Self {
        Self {
            name,
            playback,
            status: MovieDisplayStatus::Playing,
            frames_rendered: 0,
            frames_received: 0,
            last_pts: None,
            upload_time_ms_total: 0.0,
            last_log_report: Instant::now(),
            presentation_origin: None,
            last_present_wall: None,
            pending_frame: None,
            pending_deadline: None,
        }
    }

    fn log_frame_receipt(&mut self, frame: &MovieFrame) {
        self.frames_received = self.frames_received.saturating_add(1);
        self.last_pts = frame.timestamp;
    }

    fn record_upload(&mut self, viewer: &mut ViewerState, upload_ms: f64, now: Instant) {
        if self.frames_rendered == 0 {
            println!(
                "[grim_viewer] first movie frame presented for {}",
                self.name
            );
            viewer.enable_next_frame_dump();
        } else if matches!(self.frames_rendered, 5 | 30 | 120) {
            println!(
                "[grim_viewer] re-arming frame dump after {} frames",
                self.frames_rendered
            );
            viewer.enable_next_frame_dump();
        }

        self.frames_rendered = self.frames_rendered.saturating_add(1);
        self.upload_time_ms_total += upload_ms;
        if !matches!(self.status, MovieDisplayStatus::Skipping) {
            self.status = MovieDisplayStatus::Playing;
        }
        self.last_present_wall = Some(now);

        if self.should_log_progress(now) {
            let avg_upload = self.upload_time_ms_total / self.frames_rendered.max(1) as f64;
            let pts_ms = self
                .last_pts
                .map(|pts| format!("{:.2}", pts.as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "unknown".to_string());
            println!(
                "[grim_viewer] movie {} progress: received={} presented={} avg_upload_ms={:.3} last_pts_ms={}",
                self.name, self.frames_received, self.frames_rendered, avg_upload, pts_ms
            );
            self.last_log_report = now;
        }
    }

    fn should_log_progress(&self, now: Instant) -> bool {
        self.frames_rendered == 1
            || self.frames_rendered % MOVIE_PROGRESS_FRAME_INTERVAL == 0
            || now.duration_since(self.last_log_report) >= MOVIE_PROGRESS_TIME_INTERVAL
    }

    fn clear_pending(&mut self) {
        self.pending_frame = None;
        self.pending_deadline = None;
    }

    fn poll_pending_ready(&mut self, now: Instant) -> Option<MovieFrame> {
        match (self.pending_deadline, self.pending_frame.as_ref()) {
            (Some(deadline), Some(_)) if now >= deadline => {
                self.pending_deadline = None;
                self.pending_frame.take()
            }
            _ => None,
        }
    }

    fn pending_deadline(&self) -> Option<Instant> {
        self.pending_deadline
    }

    fn schedule_frame(&mut self, frame: MovieFrame, now: Instant) -> FrameSchedule {
        if matches!(self.status, MovieDisplayStatus::Skipping) {
            self.clear_pending();
            return FrameSchedule::Present(frame);
        }

        if let Some(pts) = frame.timestamp {
            let origin = self
                .presentation_origin
                .get_or_insert_with(|| now.checked_sub(pts).unwrap_or(now));
            if let Some(target) = origin.checked_add(pts) {
                if target > now {
                    self.pending_deadline = Some(target);
                    self.pending_frame = Some(frame);
                    return FrameSchedule::Deferred(target);
                }
                if now
                    .checked_duration_since(target)
                    .map_or(false, |lag| lag > MAX_FRAME_LAG)
                {
                    let realigned = now.checked_sub(pts).unwrap_or(now);
                    self.presentation_origin = Some(realigned);
                }
                self.clear_pending();
                return FrameSchedule::Present(frame);
            }
        } else if let Some(last_wall) = self.last_present_wall {
            let target = last_wall + DEFAULT_FRAME_INTERVAL;
            if target > now {
                self.pending_deadline = Some(target);
                self.pending_frame = Some(frame);
                return FrameSchedule::Deferred(target);
            }
        }

        self.clear_pending();
        FrameSchedule::Present(frame)
    }
}

#[derive(Default)]
struct SyncControls {
    paused: bool,
    pending_steps: u32,
    diff_enabled: bool,
}

enum FrameSchedule {
    Present(MovieFrame),
    Deferred(Instant),
}

#[derive(Default)]
struct MoviePumpOutcome {
    needs_redraw: bool,
    next_deadline: Option<Instant>,
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

    let mut retail_stream = if args.no_retail {
        println!(
            "[grim_viewer] retail stream disabled (window {}x{})",
            args.window_width, args.window_height
        );
        RetailStreamState::disabled()
    } else {
        println!(
            "[grim_viewer] retail stream -> {} (window {}x{})",
            args.retail_stream, args.window_width, args.window_height
        );
        RetailStreamState::with_receiver(spawn_retail_client(args.retail_stream.clone()))
    };

    let mut engine_stream = args.engine_stream.as_ref().map(|addr| {
        println!("[grim_viewer] engine stream -> {addr}");
        let client = spawn_engine_client(addr.clone());
        EngineStreamState {
            rx: client.events,
            command_tx: client.commands,
            hello: None,
            last_update: None,
            active_movie: None,
            install_root: movie_install_root(),
        }
    });

    let mut controls = SyncControls::default();

    event_loop.run(move |event, target| {
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
                            if !handle_movie_key(&key_event, engine_stream.as_mut()) {
                                // no-op for now
                            }
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
                drain_engine_events(engine_stream.as_mut(), &mut viewer);
                let outcome = pump_movie_playback(engine_stream.as_mut(), &mut viewer);
                if outcome.needs_redraw {
                    viewer.window().request_redraw();
                }
                if let Some(deadline) = outcome.next_deadline {
                    target.set_control_flow(ControlFlow::WaitUntil(deadline));
                } else {
                    target.set_control_flow(ControlFlow::Poll);
                }
                update_view_labels(&mut viewer, &retail_stream, engine_stream.as_ref());
                update_debug_panel(
                    &mut viewer,
                    &controls,
                    &retail_stream,
                    engine_stream.as_ref(),
                );
                update_window_title(viewer.window(), &controls);
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
    if !stream.enabled {
        controls.diff_enabled = false;
        controls.pending_steps = 0;
        return;
    }

    let Some(rx) = stream.rx.as_ref() else {
        return;
    };

    loop {
        match rx.try_recv() {
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
                    stream.pending_frames.push_back(QueuedFrame { frame });
                    while stream.pending_frames.len() > 8 {
                        stream.pending_frames.pop_front();
                    }
                }
                RetailEvent::Timeline(mark) => {
                    println!(
                        "[grim_viewer] retail timeline: {} seq={} host_time_ns={}",
                        mark.label, mark.seq, mark.host_time_ns
                    );
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
    });
}

fn drain_engine_events(stream: Option<&mut EngineStreamState>, viewer: &mut ViewerState) {
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
                EngineEvent::ViewerReady => {
                    println!("[grim_viewer] engine viewer-ready acknowledged");
                }
                EngineEvent::State(update) => {
                    println!(
                        "[grim_viewer] received engine update events={} seq={}",
                        update.events.len(),
                        update.seq
                    );
                    stream.last_update = Some(update.clone());
                }
                EngineEvent::MovieStart(start) => {
                    let path_result = begin_movie_playback(stream, viewer, &start);
                    match path_result {
                        Ok(path) => {
                            println!(
                                "[grim_viewer] engine movie start: {} (path={})",
                                start.name,
                                path.display()
                            );
                        }
                        Err(err) => {
                            eprintln!(
                                "[grim_viewer] movie playback setup failed for {}: {err:?}",
                                start.name
                            );
                            notify_movie_control(
                                &stream.command_tx,
                                &start.name,
                                MovieAction::Error,
                                Some(err.to_string()),
                            );
                            viewer.hide_movie();
                            stream.active_movie = None;
                        }
                    }
                }
                EngineEvent::MovieControl(control) => {
                    println!(
                        "[grim_viewer] engine movie control: {} -> {:?}",
                        control.name, control.action
                    );
                    if matches!(
                        control.action,
                        MovieAction::Finished | MovieAction::Skipped | MovieAction::Error
                    ) {
                        viewer.hide_movie();
                        stream.active_movie = None;
                    }
                }
                EngineEvent::Timeline(mark) => {
                    println!(
                        "[grim_viewer] engine timeline: {} seq={} host_time_ns={}",
                        mark.label, mark.seq, mark.host_time_ns
                    );
                }
                EngineEvent::ProtocolError(message) => {
                    eprintln!("[grim_viewer] engine protocol: {message}");
                }
                EngineEvent::Disconnected { reason } => {
                    eprintln!("[grim_viewer] engine disconnected: {reason}");
                    stream.hello = None;
                    viewer.hide_movie();
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

fn begin_movie_playback(
    stream: &mut EngineStreamState,
    viewer: &mut ViewerState,
    start: &MovieStart,
) -> Result<PathBuf> {
    viewer.hide_movie();
    if stream.active_movie.is_some() {
        stream.active_movie = None;
    }

    let path = resolve_movie_path(&stream.install_root, start)?;
    let playback = MoviePlayback::new(&path)?;

    println!(
        "[grim_viewer] starting movie playback {} -> {}",
        start.name,
        path.display()
    );

    stream.active_movie = Some(ActiveMovieStatus::new(start.name.clone(), playback));

    Ok(path)
}

fn resolve_movie_path(install_root: &Path, start: &MovieStart) -> Result<PathBuf> {
    if let Some(relative) = start.relative_path.as_ref() {
        let candidate = install_root.join(relative);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(anyhow!("movie file not found at {}", candidate.display()));
    }

    let fallback_name = start.name.to_lowercase();
    let fallback = install_root
        .join("MoviesHD")
        .join(format!("{fallback_name}.ogv"));
    if fallback.is_file() {
        return Ok(fallback);
    }

    Err(anyhow!(
        "movie {} missing relative path and fallback {} not found",
        start.name,
        fallback.display()
    ))
}

fn notify_movie_control(
    tx: &EngineCommandSender,
    name: &str,
    action: MovieAction,
    message: Option<String>,
) {
    let control = MovieControl {
        name: name.to_string(),
        action: action.clone(),
        message,
    };
    if let Err(err) = tx.send(EngineCommand::MovieControl(control)) {
        eprintln!(
            "[grim_viewer] failed to send movie control {:?} for {}: {err:?}",
            action, name
        );
    }
}

fn pump_movie_playback(
    stream: Option<&mut EngineStreamState>,
    viewer: &mut ViewerState,
) -> MoviePumpOutcome {
    let Some(stream) = stream else {
        return MoviePumpOutcome::default();
    };
    let mut outcome = MoviePumpOutcome::default();
    let mut completion: Option<(String, MovieAction, Option<String>, u64)> = None;
    if let Some(active) = stream.active_movie.as_mut() {
        loop {
            let now = Instant::now();
            if let Some(frame) = active.poll_pending_ready(now) {
                let upload_start = Instant::now();
                if let Err(err) = viewer.upload_movie_frame(
                    frame.width,
                    frame.height,
                    frame.stride,
                    &frame.pixels,
                ) {
                    eprintln!(
                        "[grim_viewer] failed to upload movie frame for {}: {err:?}",
                        active.name
                    );
                    completion = Some((
                        active.name.clone(),
                        MovieAction::Error,
                        Some(format!("viewer upload failed: {err}")),
                        active.frames_rendered,
                    ));
                    break;
                }

                let upload_ms = upload_start.elapsed().as_secs_f64() * 1000.0;
                active.record_upload(viewer, upload_ms, Instant::now());
                outcome.needs_redraw = true;
                continue;
            } else if let Some(deadline) = active.pending_deadline() {
                update_deadline(&mut outcome.next_deadline, deadline);
                break;
            }

            match active.playback.try_recv() {
                Ok(MoviePlaybackEvent::Frame(frame)) => {
                    active.log_frame_receipt(&frame);
                    let upload_start = Instant::now();
                    match active.schedule_frame(frame, Instant::now()) {
                        FrameSchedule::Present(frame) => {
                            if let Err(err) = viewer.upload_movie_frame(
                                frame.width,
                                frame.height,
                                frame.stride,
                                &frame.pixels,
                            ) {
                                eprintln!(
                                    "[grim_viewer] failed to upload movie frame for {}: {err:?}",
                                    active.name
                                );
                                completion = Some((
                                    active.name.clone(),
                                    MovieAction::Error,
                                    Some(format!("viewer upload failed: {err}")),
                                    active.frames_rendered,
                                ));
                                break;
                            }

                            let upload_ms = upload_start.elapsed().as_secs_f64() * 1000.0;
                            active.record_upload(viewer, upload_ms, Instant::now());
                            outcome.needs_redraw = true;
                        }
                        FrameSchedule::Deferred(deadline) => {
                            update_deadline(&mut outcome.next_deadline, deadline);
                            break;
                        }
                    }
                }
                Ok(MoviePlaybackEvent::Finished) => {
                    println!(
                        "[grim_viewer] decoder finished for {} (frames_received={}, frames_presented={})",
                        active.name, active.frames_received, active.frames_rendered
                    );
                    completion = Some((
                        active.name.clone(),
                        MovieAction::Finished,
                        None,
                        active.frames_rendered,
                    ));
                    break;
                }
                Ok(MoviePlaybackEvent::Skipped) => {
                    println!(
                        "[grim_viewer] decoder reported skip for {} (frames_received={}, frames_presented={})",
                        active.name, active.frames_received, active.frames_rendered
                    );
                    completion = Some((
                        active.name.clone(),
                        MovieAction::Skipped,
                        None,
                        active.frames_rendered,
                    ));
                    break;
                }
                Ok(MoviePlaybackEvent::Error(message)) => {
                    eprintln!(
                        "[grim_viewer] decoder error for {}: {} (frames_received={}, frames_presented={})",
                        active.name, message, active.frames_received, active.frames_rendered
                    );
                    completion = Some((
                        active.name.clone(),
                        MovieAction::Error,
                        Some(message),
                        active.frames_rendered,
                    ));
                    break;
                }
                Err(CrossbeamTryRecvError::Empty) => break,
                Err(CrossbeamTryRecvError::Disconnected) => {
                    eprintln!(
                        "[grim_viewer] movie pipeline disconnected unexpectedly for {} (frames_received={}, frames_presented={})",
                        active.name, active.frames_received, active.frames_rendered
                    );
                    completion = Some((
                        active.name.clone(),
                        MovieAction::Error,
                        Some("movie pipeline disconnected".to_string()),
                        active.frames_rendered,
                    ));
                    break;
                }
            }
        }
    } else {
        return outcome;
    }

    if let Some((name, action, message, frames)) = completion {
        println!(
            "[grim_viewer] movie {} completed with {:?}{} (frames={})",
            name,
            action,
            message
                .as_ref()
                .map(|msg| format!(" ({msg})"))
                .unwrap_or_default(),
            frames
        );
        if frames == 0 {
            println!(
                "[grim_viewer] warning: movie {} ended without delivering any frames",
                name
            );
        }
        viewer.hide_movie();
        outcome.needs_redraw = true;
        notify_movie_control(&stream.command_tx, &name, action, message);
        stream.active_movie = None;
    }

    outcome
}

fn request_movie_skip(stream: &mut EngineStreamState) -> bool {
    let Some(active) = stream.active_movie.as_mut() else {
        return false;
    };
    println!("[grim_viewer] skip requested for movie {}", active.name);
    active.status = MovieDisplayStatus::Skipping;
    active.playback.skip();
    true
}

fn handle_movie_key(event: &KeyEvent, engine_stream: Option<&mut EngineStreamState>) -> bool {
    let Some(stream) = engine_stream else {
        return false;
    };
    match event.logical_key.as_ref() {
        Key::Character(symbol) if matches!(symbol.as_ref(), "s" | "S") => {
            request_movie_skip(stream)
        }
        _ => false,
    }
}

fn movie_install_root() -> PathBuf {
    if let Some(path) = std::env::var_os("GRIM_INSTALL_PATH") {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os("DEV_INSTALL_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from("dev-install")
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
    let retail_label = if !retail.enabled {
        "Retail Capture (disabled)".to_string()
    } else if retail.hello.is_some() {
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
                label.push_str(&format!(" ⋅ {} [{}]", movie.name, movie.status.as_label()));
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
    let diff_label = if !retail.enabled {
        "off (retail disabled)".to_string()
    } else if controls.diff_enabled {
        "on".to_string()
    } else {
        "off".to_string()
    };
    lines.push(format!("Diff overlay: {diff_label}"));

    lines.push(String::new());
    lines.push("Retail Stream".to_string());
    if !retail.enabled {
        lines.push("Status: disabled".to_string());
    } else {
        if let Some(frame) = retail.last_frame.as_ref() {
            lines.push(format!("Frame: {}", frame.frame_id));
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
    }

    lines.push(String::new());
    lines.push("Engine Stream".to_string());
    match engine {
        Some(stream) if stream.hello.is_some() => {
            if let Some(update) = stream.last_update.as_ref() {
                lines.push(format!("Seq: {}", update.seq));
                if controls.diff_enabled && retail.enabled {
                    if let Some(frame) = retail.last_frame.as_ref() {
                        let delta_ms = (update.host_time_ns as i128 - frame.host_time_ns as i128)
                            as f64
                            / 1_000_000.0;
                        lines.push(format!("Frame Δt: {delta_ms:.2} ms"));
                    }
                }
                if let Some(movie) = stream.active_movie.as_ref() {
                    lines.push(format!(
                        "Cutscene: {} [{}]",
                        movie.name,
                        movie.status.as_label()
                    ));
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

fn update_deadline(slot: &mut Option<Instant>, candidate: Instant) {
    if slot.map_or(true, |current| candidate < current) {
        *slot = Some(candidate);
    }
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
