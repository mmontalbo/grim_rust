use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        mpsc::{Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

mod display;
mod layout;
mod live_scene;
mod live_stream;
mod movie;
mod overlay;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use crossbeam_channel::TryRecvError as CrossbeamTryRecvError;
use display::ViewerState;
use env_logger;
use grim_stream::{Frame, Hello, MovieAction, MovieControl, MovieStart, StateUpdate, StreamConfig};
use live_scene::LiveSceneState;
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

    /// Dump incoming engine StateUpdates (including events) to a JSONL file
    #[arg(long)]
    dump_engine_events: Option<PathBuf>,

    /// Automatically request a skip when a movie with the given name starts
    #[arg(long, value_name = "NAME")]
    auto_skip_movie: Vec<String>,

    /// Show the engine event log overlay on startup
    #[arg(long)]
    show_events: bool,

    /// Dump the next rendered frame after an engine event arrives to this path
    #[arg(long)]
    dump_debug_frame: Option<PathBuf>,
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
    scene: LiveSceneState,
    event_dump: Option<EventDumpWriter>,
    auto_skip_movies: HashSet<String>,
    event_log: EngineEventLog,
    debug_frame_pending: bool,
}

struct EngineEventLog {
    last_seq: Option<u64>,
    total_events: usize,
    truncated: bool,
    preview: Vec<String>,
}

impl EngineEventLog {
    fn new() -> Self {
        Self {
            last_seq: None,
            total_events: 0,
            truncated: false,
            preview: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.last_seq = None;
        self.total_events = 0;
        self.truncated = false;
        self.preview.clear();
    }

    fn ingest(&mut self, update: &StateUpdate) -> bool {
        if update.events.is_empty() {
            return false;
        }

        self.last_seq = Some(update.seq);
        self.total_events = update.events.len();
        let take = update.events.len().min(MAX_DISPLAY_EVENTS);
        self.preview.clear();
        self.preview
            .extend(update.events.iter().take(take).cloned());
        self.truncated = self.total_events > self.preview.len();
        if let Some(first) = self.preview.get(0) {
            println!(
                "[grim_viewer] ingesting {} engine events for seq {} (first='{}')",
                self.total_events, update.seq, first
            );
        }
        true
    }

    fn render_into(&self, lines: &mut Vec<String>) {
        if let Some(seq) = self.last_seq {
            let mut summary = format!("Last seq: {} ({} events", seq, self.total_events);
            if self.truncated {
                summary.push_str(&format!(", showing first {}", self.preview.len()));
            }
            summary.push(')');
            lines.push(summary);
            if self.preview.is_empty() {
                lines.push("(none)".to_string());
            } else {
                for event in &self.preview {
                    lines.push(format!("- {event}"));
                }
                if self.truncated {
                    let remaining = self.total_events.saturating_sub(self.preview.len());
                    if remaining > 0 {
                        lines.push(format!("… (+{remaining} more)"));
                    }
                }
            }
        } else {
            lines.push("(no events received yet)".to_string());
        }
    }

    fn debug_summary(&self) -> Option<(u64, usize)> {
        self.last_seq.map(|seq| (seq, self.preview.len()))
    }
}

struct EventDumpWriter {
    writer: BufWriter<File>,
}

impl EventDumpWriter {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create engine event dump directory {}",
                        parent.display()
                    )
                })?;
            }
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create engine event dump at {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn write(&mut self, update: &StateUpdate) -> Result<()> {
        serde_json::to_writer(&mut self.writer, update)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

struct QueuedFrame {
    frame: Frame,
}

#[derive(Clone, Copy)]
struct FrameStats {
    frame_id: u64,
    host_time_ns: u64,
}

struct MovieTimeline {
    pts_origin: Option<Duration>,
    presentation_origin: Option<Instant>,
    last_normalized_pts: Option<Duration>,
    last_raw_pts: Option<Duration>,
    origin_reset_pending: bool,
}

struct PendingMovieFrame {
    frame: MovieFrame,
    pts: Option<Duration>,
    deadline: Instant,
    since: Instant,
}

enum TimelineRecord {
    Absent,
    Timestamp { origin_reset: bool },
}

struct ActiveMovieStatus {
    name: String,
    playback: MoviePlayback,
    status: MovieDisplayStatus,
    frames_rendered: u64,
    frames_received: u64,
    upload_time_ms_total: f64,
    last_log_report: Instant,
    timeline: MovieTimeline,
    last_present_wall: Option<Instant>,
    pending: VecDeque<PendingMovieFrame>,
    last_pending_log: Option<Instant>,
}

const MOVIE_PROGRESS_FRAME_INTERVAL: u64 = 60;
const MOVIE_PROGRESS_TIME_INTERVAL: Duration = Duration::from_secs(1);
const MAX_FRAME_LAG: Duration = Duration::from_millis(200);
const MAX_FRAME_LEAD: Duration = Duration::from_secs(8);
const PTS_RESET_THRESHOLD: Duration = Duration::from_millis(500);
const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(16);
// Allow a small jitter buffer so bursts from the decoder do not get dropped.
const MAX_PENDING_FRAMES: usize = 12;
// Clamp to at most ~250 ms of lead so we never drift far ahead of realtime.
const MAX_PRESENTATION_LEAD: Duration = Duration::from_millis(250);
// Try to hover around 1–2 frames of lead; give ourselves 80–140 ms of slack.
const TARGET_PRESENTATION_LEAD: Duration = Duration::from_millis(140);
const MIN_PRESENTATION_LEAD: Duration = Duration::from_millis(80);
const MAX_DISPLAY_EVENTS: usize = 48;

#[derive(Clone, Copy)]
enum MovieLogLevel {
    Info,
    Verbose,
}

fn movie_pacing_verbose_enabled() -> bool {
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| {
        std::env::var("GRIM_MOVIE_PACING_VERBOSE")
            .map(|value| {
                let lower = value.trim().to_ascii_lowercase();
                matches!(lower.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

fn movie_pacing_should_emit(level: MovieLogLevel) -> bool {
    matches!(level, MovieLogLevel::Info) || movie_pacing_verbose_enabled()
}

fn movie_pacing_log(
    level: MovieLogLevel,
    movie: &str,
    event: &str,
    fields: impl IntoIterator<Item = (&'static str, String)>,
) {
    if !movie_pacing_should_emit(level) {
        return;
    }
    let mut message = format!("[grim_viewer] movie {} {}", movie, event);
    for (key, value) in fields {
        message.push(' ');
        message.push_str(key);
        message.push('=');
        message.push_str(&value);
    }
    println!("{message}");
}

enum MovieDisplayStatus {
    Playing,
    Skipping,
}

impl MovieTimeline {
    fn new() -> Self {
        Self {
            pts_origin: None,
            presentation_origin: None,
            last_normalized_pts: None,
            last_raw_pts: None,
            origin_reset_pending: false,
        }
    }

    fn record(&mut self, movie: &str, raw_pts: Option<Duration>) -> TimelineRecord {
        match raw_pts {
            Some(raw) => {
                let (normalized, origin_reset) = self.normalise_pts(movie, raw);
                self.last_normalized_pts = Some(normalized);
                self.last_raw_pts = Some(raw);
                if origin_reset {
                    self.presentation_origin = None;
                    self.origin_reset_pending = true;
                }
                TimelineRecord::Timestamp { origin_reset }
            }
            None => {
                self.last_normalized_pts = None;
                self.last_raw_pts = None;
                TimelineRecord::Absent
            }
        }
    }

    fn normalise_pts(&mut self, movie: &str, raw_pts: Duration) -> (Duration, bool) {
        match self.pts_origin {
            Some(origin) => {
                if let Some(delta) = raw_pts.checked_sub(origin) {
                    (delta, false)
                } else if origin
                    .checked_sub(raw_pts)
                    .map(|delta| delta > PTS_RESET_THRESHOLD)
                    .unwrap_or(false)
                {
                    movie_pacing_log(
                        MovieLogLevel::Info,
                        movie,
                        "pts_origin_reset",
                        [
                            (
                                "raw_pts_ms",
                                format!("{:.2}", raw_pts.as_secs_f64() * 1000.0),
                            ),
                            (
                                "previous_origin_ms",
                                format!("{:.2}", origin.as_secs_f64() * 1000.0),
                            ),
                        ],
                    );
                    self.pts_origin = Some(raw_pts);
                    (Duration::ZERO, true)
                } else {
                    (Duration::ZERO, false)
                }
            }
            None => {
                movie_pacing_log(
                    MovieLogLevel::Info,
                    movie,
                    "pts_origin_establish",
                    [(
                        "raw_pts_ms",
                        format!("{:.2}", raw_pts.as_secs_f64() * 1000.0),
                    )],
                );
                self.pts_origin = Some(raw_pts);
                (Duration::ZERO, true)
            }
        }
    }

    fn take_origin_reset(&mut self) -> bool {
        let reset = self.origin_reset_pending;
        self.origin_reset_pending = false;
        reset
    }

    fn last_normalized(&self) -> Option<Duration> {
        self.last_normalized_pts
    }

    fn last_raw(&self) -> Option<Duration> {
        self.last_raw_pts
    }

    fn ensure_presentation_origin(&mut self, pts: Duration, now: Instant) -> Option<Instant> {
        let origin = self
            .presentation_origin
            .get_or_insert_with(|| now.checked_sub(pts).unwrap_or(now));
        origin.checked_add(pts)
    }

    fn realign_presentation_origin(&mut self, origin: Instant) {
        self.presentation_origin = Some(origin);
    }
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
            upload_time_ms_total: 0.0,
            last_log_report: Instant::now(),
            timeline: MovieTimeline::new(),
            last_present_wall: None,
            pending: VecDeque::new(),
            last_pending_log: None,
        }
    }

    fn log_frame_receipt(&mut self, frame: &MovieFrame) {
        self.frames_received = self.frames_received.saturating_add(1);
        match self.timeline.record(&self.name, frame.timestamp) {
            TimelineRecord::Timestamp { origin_reset, .. } if origin_reset => {
                self.clear_pending();
            }
            _ => {}
        }
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
                .timeline
                .last_normalized()
                .map(|pts| format!("{:.2}", pts.as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "unknown".to_string());
            movie_pacing_log(
                MovieLogLevel::Verbose,
                &self.name,
                "progress",
                [
                    ("frames_received", self.frames_received.to_string()),
                    ("frames_presented", self.frames_rendered.to_string()),
                    ("avg_upload_ms", format!("{:.3}", avg_upload)),
                    ("last_pts_ms", pts_ms),
                ],
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
        self.pending.clear();
    }

    fn reschedule_pending_deadlines(&mut self, now: Instant) {
        let Some(origin) = self.timeline.presentation_origin else {
            return;
        };
        for pending in &mut self.pending {
            if let Some(pts) = pending.pts {
                if let Some(deadline) = origin.checked_add(pts) {
                    pending.deadline = deadline;
                } else {
                    pending.deadline = now;
                }
            }
            if pending.deadline < now {
                pending.deadline = now;
            }
        }
    }

    fn poll_pending_ready(&mut self, now: Instant) -> Option<MovieFrame> {
        if let Some(front) = self.pending.front() {
            if now >= front.deadline {
                let PendingMovieFrame {
                    frame,
                    pts,
                    deadline,
                    since,
                } = self.pending.pop_front().unwrap();
                self.log_pending_release(now, deadline, since, pts, self.pending.len());
                return Some(frame);
            }
        }
        None
    }

    fn pending_deadline(&self) -> Option<Instant> {
        self.pending.front().map(|pending| pending.deadline)
    }

    fn schedule_frame(&mut self, frame: MovieFrame, now: Instant) -> FrameSchedule {
        if matches!(self.status, MovieDisplayStatus::Skipping) {
            self.clear_pending();
            return FrameSchedule::Present(frame);
        }

        if self.timeline.take_origin_reset() {
            self.clear_pending();
        }

        if let Some(pts) = self.timeline.last_normalized() {
            if let Some(mut target) = self.timeline.ensure_presentation_origin(pts, now) {
                if target > now {
                    if target
                        .checked_duration_since(now)
                        .map(|lead| lead > MAX_FRAME_LEAD)
                        .unwrap_or(false)
                    {
                        let realigned = now.checked_sub(pts).unwrap_or(now);
                        let mut fields = vec![
                            (
                                "lead_ms",
                                format!("{:.2}", target.duration_since(now).as_secs_f64() * 1000.0),
                            ),
                            ("pts_ms", format!("{:.2}", pts.as_secs_f64() * 1000.0)),
                        ];
                        if let Some(raw) = self.timeline.last_raw() {
                            fields
                                .push(("raw_pts_ms", format!("{:.2}", raw.as_secs_f64() * 1000.0)));
                        }
                        movie_pacing_log(MovieLogLevel::Info, &self.name, "clamp_lead", fields);
                        self.timeline.realign_presentation_origin(realigned);
                        self.clear_pending();
                        return FrameSchedule::Present(frame);
                    }

                    if let Some(current_lead) = target.checked_duration_since(now) {
                        if current_lead > MAX_PRESENTATION_LEAD {
                            let desired_lead = TARGET_PRESENTATION_LEAD
                                .min(current_lead)
                                .max(MIN_PRESENTATION_LEAD);
                            let desired_target = now
                                .checked_add(desired_lead)
                                .unwrap_or_else(|| now + TARGET_PRESENTATION_LEAD);
                            let realigned = desired_target.checked_sub(pts).unwrap_or(now);
                            movie_pacing_log(
                                MovieLogLevel::Info,
                                &self.name,
                                "limit_lead",
                                [
                                    (
                                        "lead_ms",
                                        format!("{:.2}", current_lead.as_secs_f64() * 1000.0),
                                    ),
                                    ("pts_ms", format!("{:.2}", pts.as_secs_f64() * 1000.0)),
                                    (
                                        "desired_lead_ms",
                                        format!("{:.2}", desired_lead.as_secs_f64() * 1000.0),
                                    ),
                                ],
                            );
                            self.timeline.realign_presentation_origin(realigned);
                            self.reschedule_pending_deadlines(now);
                            if let Some(adjusted) =
                                self.timeline.ensure_presentation_origin(pts, now)
                            {
                                target = adjusted;
                            } else {
                                target = desired_target;
                            }
                        }
                    }

                    let entry = PendingMovieFrame {
                        frame,
                        pts: Some(pts),
                        deadline: target,
                        since: now,
                    };

                    if self.pending.len() >= MAX_PENDING_FRAMES {
                        let forced = self.pending.pop_front().map(|pending| {
                            let PendingMovieFrame {
                                frame,
                                pts,
                                deadline,
                                since,
                            } = pending;
                            self.log_pending_release(now, deadline, since, pts, self.pending.len());
                            frame
                        });
                        self.pending.push_back(entry);
                        let (deadline, since, pts) = {
                            let pending = self.pending.back().unwrap();
                            (pending.deadline, pending.since, pending.pts)
                        };
                        self.log_pending_deferral(now, deadline, since, pts, self.pending.len());
                        if let Some(frame) = forced {
                            return FrameSchedule::Present(frame);
                        }
                        return FrameSchedule::Deferred(target);
                    }

                    self.pending.push_back(entry);
                    let (deadline, since, pts) = {
                        let pending = self.pending.back().unwrap();
                        (pending.deadline, pending.since, pending.pts)
                    };
                    self.log_pending_deferral(now, deadline, since, pts, self.pending.len());
                    return FrameSchedule::Deferred(target);
                }
                if now
                    .checked_duration_since(target)
                    .map_or(false, |lag| lag > MAX_FRAME_LAG)
                {
                    let realigned = now.checked_sub(pts).unwrap_or(now);
                    let mut fields = vec![
                        (
                            "lag_ms",
                            format!("{:.2}", now.duration_since(target).as_secs_f64() * 1000.0),
                        ),
                        ("pts_ms", format!("{:.2}", pts.as_secs_f64() * 1000.0)),
                    ];
                    if let Some(raw) = self.timeline.last_raw() {
                        fields.push(("raw_pts_ms", format!("{:.2}", raw.as_secs_f64() * 1000.0)));
                    }
                    movie_pacing_log(MovieLogLevel::Info, &self.name, "realign_origin", fields);
                    self.timeline.realign_presentation_origin(realigned);
                }
                self.clear_pending();
                return FrameSchedule::Present(frame);
            }
        } else if let Some(last_wall) = self.last_present_wall {
            let target = last_wall + DEFAULT_FRAME_INTERVAL;
            if target > now {
                let entry = PendingMovieFrame {
                    frame,
                    pts: None,
                    deadline: target,
                    since: now,
                };
                if self.pending.len() >= MAX_PENDING_FRAMES {
                    let forced = self.pending.pop_front().map(|pending| {
                        let PendingMovieFrame {
                            frame,
                            pts,
                            deadline,
                            since,
                        } = pending;
                        self.log_pending_release(now, deadline, since, pts, self.pending.len());
                        frame
                    });
                    self.pending.push_back(entry);
                    let (deadline, since, pts) = {
                        let pending = self.pending.back().unwrap();
                        (pending.deadline, pending.since, pending.pts)
                    };
                    self.log_pending_deferral(now, deadline, since, pts, self.pending.len());
                    if let Some(frame) = forced {
                        return FrameSchedule::Present(frame);
                    }
                    return FrameSchedule::Deferred(target);
                }
                self.pending.push_back(entry);
                let (deadline, since, pts) = {
                    let pending = self.pending.back().unwrap();
                    (pending.deadline, pending.since, pending.pts)
                };
                self.log_pending_deferral(now, deadline, since, pts, self.pending.len());
                return FrameSchedule::Deferred(target);
            }
        }

        self.clear_pending();
        FrameSchedule::Present(frame)
    }

    fn log_pending_deferral(
        &mut self,
        now: Instant,
        deadline: Instant,
        since: Instant,
        pts: Option<Duration>,
        queued: usize,
    ) {
        if !self.should_log_pending(now) {
            return;
        }
        let wait_ms = deadline
            .checked_duration_since(now)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let pending_ms = now
            .checked_duration_since(since)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let pts_ms = pts
            .map(|d| format!("{:.2}", d.as_secs_f64() * 1000.0))
            .unwrap_or_else(|| "unknown".to_string());
        let mut fields = vec![
            ("wait_ms", format!("{:.2}", wait_ms)),
            ("pending_ms", format!("{:.2}", pending_ms)),
            ("pts_ms", pts_ms),
            ("frames_received", self.frames_received.to_string()),
            ("frames_presented", self.frames_rendered.to_string()),
            ("status", self.status.as_label().to_string()),
        ];
        fields.push(("queued_frames", queued.to_string()));
        movie_pacing_log(
            MovieLogLevel::Verbose,
            &self.name,
            "pending_deferral",
            fields,
        );
    }

    fn log_pending_release(
        &mut self,
        now: Instant,
        deadline: Instant,
        since: Instant,
        pts: Option<Duration>,
        remaining: usize,
    ) {
        if !self.should_log_pending(now) {
            return;
        }
        let overshoot_ms = now
            .checked_duration_since(deadline)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let pending_ms = now
            .checked_duration_since(since)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let pts_ms = pts
            .map(|d| format!("{:.2}", d.as_secs_f64() * 1000.0))
            .unwrap_or_else(|| "unknown".to_string());
        movie_pacing_log(
            MovieLogLevel::Verbose,
            &self.name,
            "pending_release",
            [
                ("overshoot_ms", format!("{:.2}", overshoot_ms)),
                ("pending_ms", format!("{:.2}", pending_ms)),
                ("pts_ms", pts_ms),
                ("frames_received", self.frames_received.to_string()),
                ("frames_presented", self.frames_rendered.to_string()),
                ("queued_frames", remaining.to_string()),
            ],
        );
    }

    fn should_log_pending(&mut self, now: Instant) -> bool {
        const MIN_PENDING_LOG_INTERVAL: Duration = Duration::from_millis(200);
        match self.last_pending_log {
            Some(prev) if now.duration_since(prev) < MIN_PENDING_LOG_INTERVAL => false,
            _ => {
                self.last_pending_log = Some(now);
                true
            }
        }
    }
}

#[derive(Default)]
struct SyncControls {
    paused: bool,
    pending_steps: u32,
    diff_enabled: bool,
    show_events: bool,
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

    if let Some(path) = args.dump_debug_frame.as_ref() {
        // This is a CLI-provided path; setting env is required for the frame dump helper.
        unsafe {
            std::env::set_var("GRIM_DUMP_FRAME", path);
        }
    }

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

    let mut engine_stream = if let Some(addr) = args.engine_stream.as_ref() {
        println!("[grim_viewer] engine stream -> {addr}");
        let client = spawn_engine_client(addr.clone());
        let install_root = movie_install_root();
        let event_dump = match args.dump_engine_events.as_ref() {
            Some(path) => match EventDumpWriter::open(path) {
                Ok(writer) => {
                    println!("[grim_viewer] dumping engine events to {}", path.display());
                    Some(writer)
                }
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] failed to enable engine event dump {}: {err:?}",
                        path.display()
                    );
                    None
                }
            },
            None => None,
        };
        let auto_skip_movies: HashSet<String> = args.auto_skip_movie.iter().cloned().collect();
        let dump_frame_pending = args.dump_debug_frame.is_some();
        Some(EngineStreamState {
            rx: client.events,
            command_tx: client.commands,
            hello: None,
            last_update: None,
            active_movie: None,
            install_root: install_root.clone(),
            scene: LiveSceneState::new(install_root),
            event_dump,
            auto_skip_movies,
            event_log: EngineEventLog::new(),
            debug_frame_pending: dump_frame_pending,
        })
    } else {
        None
    };

    let mut controls = SyncControls::default();
    controls.show_events = args.show_events || args.dump_debug_frame.is_some();

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
                        if handle_sync_key(&key_event, &mut controls) {
                            viewer.window().request_redraw();
                        } else if handle_movie_key(&key_event, engine_stream.as_mut()) {
                            viewer.window().request_redraw();
                        } else {
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
                    stream.scene = LiveSceneState::new(stream.install_root.clone());
                    stream.event_log.reset();
                    if let Some(frame) = stream.scene.compose_frame() {
                        if let Err(err) =
                            viewer.upload_engine_frame(frame.width, frame.height, frame.pixels)
                        {
                            eprintln!(
                                "[grim_viewer] engine overlay upload failed after connect: {err:?}"
                            );
                        } else {
                            viewer.window().request_redraw();
                        }
                    }
                }
                EngineEvent::ViewerReady => {
                    println!("[grim_viewer] engine viewer-ready acknowledged");
                }
                EngineEvent::State(update) => {
                    let ingested_events = if !update.events.is_empty() {
                        println!(
                            "[grim_viewer] received engine update events={} seq={}",
                            update.events.len(),
                            update.seq
                        );
                        stream.event_log.ingest(&update)
                    } else {
                        false
                    };
                    if let Some(dump) = stream.event_dump.as_mut() {
                        if let Err(err) = dump.write(&update) {
                            eprintln!(
                                "[grim_viewer] engine event dump failed: {err:?}; disabling writer"
                            );
                            stream.event_dump = None;
                        }
                    }
                    if let Some(frame) = stream.scene.ingest_state_update(&update) {
                        if let Err(err) =
                            viewer.upload_engine_frame(frame.width, frame.height, frame.pixels)
                        {
                            eprintln!("[grim_viewer] engine overlay upload failed: {err:?}");
                        } else {
                            viewer.window().request_redraw();
                        }
                    }
                    if ingested_events {
                        viewer.window().request_redraw();
                    }
                    if stream.debug_frame_pending && ingested_events {
                        println!("[grim_viewer] frame dump armed after engine events");
                        viewer.enable_next_frame_dump();
                        stream.debug_frame_pending = false;
                    }
                    stream.last_update = Some(update);
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
                            if stream.auto_skip_movies.contains(&start.name) {
                                if request_movie_skip(stream) {
                                    println!(
                                        "[grim_viewer] auto-skip requested for movie {}",
                                        start.name
                                    );
                                }
                            }
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
                    stream.scene = LiveSceneState::new(stream.install_root.clone());
                    if let Some(frame) = stream.scene.compose_frame() {
                        if let Err(err) =
                            viewer.upload_engine_frame(frame.width, frame.height, frame.pixels)
                        {
                            eprintln!(
                                "[grim_viewer] engine overlay upload failed after disconnect: {err:?}"
                            );
                        } else {
                            viewer.window().request_redraw();
                        }
                    }
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
    let events_label = if controls.show_events {
        "visible"
    } else {
        "hidden"
    };
    lines.push(format!("Engine events: {events_label}"));

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
    if let Some(stream) = engine {
        if stream.hello.is_some() {
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
                lines.push(String::new());
                lines.push("Scene State".to_string());
                let hotspot_label = update
                    .active_hotspot
                    .as_deref()
                    .map(|name| format!("[{name}]"))
                    .unwrap_or_else(|| "(none)".to_string());
                lines.push(format!("Hotspot: {hotspot_label}"));
                if let Some(commentary) = update.commentary.as_ref() {
                    let label = commentary.label.as_deref().unwrap_or_else(|| {
                        if commentary.active {
                            "(active)"
                        } else {
                            "(idle)"
                        }
                    });
                    let status = if commentary.active { "ACTIVE" } else { "idle" };
                    let mut line = format!("Commentary: {status} {label}");
                    if let Some(reason) = commentary.suppressed_reason.as_ref() {
                        line.push_str(&format!(" (suppressed: {reason})"));
                    }
                    lines.push(line);
                } else {
                    lines.push("Commentary: (none)".to_string());
                }
                if let Some(tube) = update.tube.as_ref() {
                    let pose = tube.pose.as_deref().unwrap_or("(unknown pose)");
                    let contents = tube.contains.as_deref().unwrap_or("(empty)");
                    lines.push(format!("Tube: pose={pose} contains={contents}"));
                } else {
                    lines.push("Tube: (offline)".to_string());
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
        } else {
            lines.push("Status: offline".to_string());
        }
        if controls.show_events {
            append_engine_events(&mut lines, Some(stream));
        }
    } else {
        lines.push("Status: disabled".to_string());
        if controls.show_events {
            append_engine_events(&mut lines, None);
        }
    }

    viewer.set_debug_lines(&lines);
}

fn append_engine_events(lines: &mut Vec<String>, stream: Option<&EngineStreamState>) {
    lines.push(String::new());
    lines.push("Engine Events".to_string());
    match stream {
        Some(stream) => {
            stream.event_log.render_into(lines);
            if let Some((seq, count)) = stream.event_log.debug_summary() {
                println!(
                    "[grim_viewer] debug overlay events cached for seq {} ({} shown)",
                    seq, count
                );
            }
        }
        None => {
            lines.push("(disabled)".to_string());
        }
    }
}

fn update_deadline(slot: &mut Option<Instant>, candidate: Instant) {
    if slot.map_or(true, |current| candidate < current) {
        *slot = Some(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOVIE: &str = "test_movie";

    #[test]
    fn timeline_establishes_origin_and_normalises() {
        let mut timeline = MovieTimeline::new();
        match timeline.record(MOVIE, Some(Duration::from_millis(1_000))) {
            TimelineRecord::Timestamp { origin_reset } => assert!(origin_reset),
            _ => panic!("expected timestamp record"),
        }
        assert_eq!(timeline.last_normalized(), Some(Duration::ZERO));
        assert!(timeline.take_origin_reset());
    }

    #[test]
    fn timeline_preserves_forward_progress() {
        let mut timeline = MovieTimeline::new();
        let _ = timeline.record(MOVIE, Some(Duration::from_millis(1_000)));
        timeline.take_origin_reset();

        match timeline.record(MOVIE, Some(Duration::from_millis(1_033))) {
            TimelineRecord::Timestamp { origin_reset } => assert!(!origin_reset),
            _ => panic!("expected timestamp record"),
        }
        assert_eq!(timeline.last_normalized(), Some(Duration::from_millis(33)));
        assert!(!timeline.take_origin_reset());
    }

    #[test]
    fn timeline_resets_after_large_backward_jump() {
        let mut timeline = MovieTimeline::new();
        let _ = timeline.record(MOVIE, Some(Duration::from_millis(1_500)));
        timeline.take_origin_reset();
        let _ = timeline.record(MOVIE, Some(Duration::from_millis(3_000)));
        timeline.take_origin_reset();

        match timeline.record(MOVIE, Some(Duration::from_millis(900))) {
            TimelineRecord::Timestamp { origin_reset } => assert!(origin_reset),
            _ => panic!("expected timestamp record"),
        }
        assert_eq!(timeline.last_normalized(), Some(Duration::ZERO));
        assert!(timeline.take_origin_reset());
    }

    #[test]
    fn timeline_ignores_small_backward_noise() {
        let mut timeline = MovieTimeline::new();
        let _ = timeline.record(MOVIE, Some(Duration::from_millis(1_000)));
        timeline.take_origin_reset();
        let _ = timeline.record(MOVIE, Some(Duration::from_millis(2_000)));
        timeline.take_origin_reset();

        match timeline.record(MOVIE, Some(Duration::from_millis(1_950))) {
            TimelineRecord::Timestamp { origin_reset } => assert!(!origin_reset),
            _ => panic!("expected timestamp record"),
        }
        assert_eq!(timeline.last_normalized(), Some(Duration::from_millis(950)));
        assert!(!timeline.take_origin_reset());
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
            "e" | "E" => {
                controls.show_events = !controls.show_events;
                true
            }
            _ => false,
        },
        _ => false,
    }
}
