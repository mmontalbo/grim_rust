use std::borrow::Cow;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Once, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use crossbeam_channel;
use crossbeam_channel::{Receiver, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gstreamer_video::VideoInfo;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};

#[derive(Debug, Clone)]
pub struct MovieFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixels: Vec<u8>,
    pub timestamp: Option<Duration>,
}

#[derive(Debug)]
pub enum MoviePlaybackEvent {
    Frame(MovieFrame),
    Finished,
    Skipped,
    Error(String),
}

static FRAME_DUMP_CONFIG: OnceLock<FrameDumpConfig> = OnceLock::new();
const MOVIE_EVENT_QUEUE_DEPTH: usize = 8;

#[derive(Debug)]
struct FrameDumpConfig {
    mode: FrameDumpMode,
    directory: PathBuf,
}

#[derive(Debug)]
enum FrameDumpMode {
    Disabled,
    Indices(Vec<u64>),
    First(u64),
}

impl FrameDumpConfig {
    fn from_env() -> Self {
        let directory = std::env::var_os("GRIM_MOVIE_DUMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let mode = FrameDumpMode::from_env();
        Self { mode, directory }
    }

    fn should_dump(&self, frame_index: u64) -> bool {
        self.mode.should_dump(frame_index)
    }

    fn target_path(&self, movie_path: &Path, source: &str, frame_index: u64) -> Option<PathBuf> {
        if !self.should_dump(frame_index) {
            return None;
        }
        let stem = sanitize_movie_stem(movie_path);
        let filename = format!("movie_frame_{stem}_{source}_{frame_index:05}.png");
        Some(self.directory.join(filename))
    }
}

impl FrameDumpMode {
    fn from_env() -> Self {
        let raw = std::env::var("GRIM_MOVIE_DUMP_FRAMES").unwrap_or_default();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return FrameDumpMode::Disabled;
        }
        if trimmed == "*" {
            // Capture a small burst of frames by default when wildcard requested.
            return FrameDumpMode::First(5);
        }
        if !trimmed.contains(',') && !trimmed.contains('-') {
            if let Ok(count) = trimmed.parse::<u64>() {
                if count == 0 {
                    return FrameDumpMode::Disabled;
                }
                return FrameDumpMode::First(count);
            }
        }

        let mut frames = Vec::new();
        for token in trimmed.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some((start, end)) = token.split_once('-') {
                if let (Ok(start_idx), Ok(end_idx)) =
                    (start.trim().parse::<u64>(), end.trim().parse::<u64>())
                {
                    let (lo, hi) = if start_idx <= end_idx {
                        (start_idx, end_idx)
                    } else {
                        (end_idx, start_idx)
                    };
                    for idx in lo..=hi {
                        frames.push(idx);
                    }
                    continue;
                }
            }

            if let Ok(idx) = token.parse::<u64>() {
                frames.push(idx);
            } else {
                eprintln!("[grim_viewer] ignoring invalid GRIM_MOVIE_DUMP_FRAMES token: {token}");
            }
        }

        if frames.is_empty() {
            FrameDumpMode::Disabled
        } else {
            frames.sort_unstable();
            frames.dedup();
            FrameDumpMode::Indices(frames)
        }
    }

    fn should_dump(&self, frame_index: u64) -> bool {
        match self {
            FrameDumpMode::Disabled => false,
            FrameDumpMode::Indices(indices) => indices.binary_search(&frame_index).is_ok(),
            FrameDumpMode::First(count) => frame_index < *count,
        }
    }
}

fn frame_dump_config() -> &'static FrameDumpConfig {
    FRAME_DUMP_CONFIG.get_or_init(FrameDumpConfig::from_env)
}

fn maybe_dump_decoded_frame(
    source: &str,
    movie_path: &Path,
    frame_index: u64,
    width: u32,
    height: u32,
    stride_bytes: usize,
    data: &[u8],
) {
    let config = frame_dump_config();
    let Some(path) = config.target_path(movie_path, source, frame_index) else {
        return;
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            eprintln!(
                "[grim_viewer] failed to create movie dump directory {}: {err:?}",
                parent.display()
            );
            return;
        }
    }

    match write_rgba_png(&path, width, height, stride_bytes, data) {
        Ok(()) => println!(
            "[grim_viewer] movie frame dump {} frame={} size={}x{} stride={}",
            path.display(),
            frame_index,
            width,
            height,
            stride_bytes
        ),
        Err(err) => eprintln!(
            "[grim_viewer] failed to dump movie frame {} frame={} error={:?}",
            path.display(),
            frame_index,
            err
        ),
    }
}

fn write_rgba_png(
    path: &Path,
    width: u32,
    height: u32,
    stride_bytes: usize,
    data: &[u8],
) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(anyhow!("invalid frame dimensions {}x{}", width, height));
    }
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| anyhow!("frame width overflow for {}", width))? as usize;
    if stride_bytes < row_bytes {
        return Err(anyhow!(
            "stride {} smaller than row bytes {}",
            stride_bytes,
            row_bytes
        ));
    }
    let required = stride_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| anyhow!("frame height overflow for {}", height))?;
    if data.len() < required {
        return Err(anyhow!(
            "frame data {} smaller than required {}",
            data.len(),
            required
        ));
    }

    let buffer = if stride_bytes == row_bytes {
        Cow::Borrowed(&data[..row_bytes * height as usize])
    } else {
        Cow::Owned(repack_rgba_rows(
            row_bytes,
            stride_bytes,
            height as usize,
            data,
        ))
    };

    let file = File::create(path)
        .with_context(|| anyhow!("failed to create movie frame dump at {}", path.display()))?;
    let encoder = PngEncoder::new(file);
    encoder
        .write_image(buffer.as_ref(), width, height, ColorType::Rgba8.into())
        .with_context(|| anyhow!("failed to encode movie frame png {}", path.display()))?;
    Ok(())
}

fn repack_rgba_rows(row_bytes: usize, stride: usize, rows: usize, data: &[u8]) -> Vec<u8> {
    let mut output = vec![0u8; row_bytes * rows];
    for row in 0..rows {
        let src_offset = row * stride;
        let dst_offset = row * row_bytes;
        output[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&data[src_offset..src_offset + row_bytes]);
    }
    output
}

fn sanitize_movie_stem(path: &Path) -> String {
    let raw = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("movie");
    let mut result = String::new();
    for ch in raw.chars() {
        if result.len() >= 32 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            result.push(ch);
        } else {
            result.push('_');
        }
    }
    if result.is_empty() {
        "movie".to_string()
    } else {
        result
    }
}

pub enum MovieStopReason {
    Skipped,
    Shutdown,
}

enum MoviePlayerCommand {
    Stop(MovieStopReason),
}

pub struct MoviePlayback {
    events: Receiver<MoviePlaybackEvent>,
    commands: Sender<MoviePlayerCommand>,
    join: Option<thread::JoinHandle<()>>,
}

impl MoviePlayback {
    pub fn new(path: &Path) -> Result<Self> {
        let movie_path = PathBuf::from(path);
        if !movie_path.is_file() {
            return Err(anyhow!("movie file {:?} not found", movie_path));
        }

        let (event_tx, event_rx) = crossbeam_channel::bounded(MOVIE_EVENT_QUEUE_DEPTH);
        let (command_tx, command_rx) = crossbeam_channel::unbounded();

        let handle = if use_ffmpeg_decoder() {
            thread::Builder::new()
                .name("grim_movie_ffmpeg".to_string())
                .spawn(move || run_ffmpeg_pipeline(movie_path, event_tx.clone(), command_rx))
                .context("failed to spawn ffmpeg movie playback thread")?
        } else {
            gst::init().map_err(|err| anyhow!("failed to initialise GStreamer: {err:?}"))?;
            thread::Builder::new()
                .name("grim_movie_player".to_string())
                .spawn(move || run_pipeline(movie_path, event_tx.clone(), command_rx))
                .context("failed to spawn movie playback thread")?
        };

        Ok(Self {
            events: event_rx,
            commands: command_tx,
            join: Some(handle),
        })
    }

    pub fn try_recv(&self) -> Result<MoviePlaybackEvent, crossbeam_channel::TryRecvError> {
        self.events.try_recv()
    }

    pub fn skip(&self) {
        let _ = self
            .commands
            .send(MoviePlayerCommand::Stop(MovieStopReason::Skipped));
    }
}

impl Drop for MoviePlayback {
    fn drop(&mut self) {
        let _ = self
            .commands
            .send(MoviePlayerCommand::Stop(MovieStopReason::Shutdown));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_pipeline(
    path: PathBuf,
    event_tx: Sender<MoviePlaybackEvent>,
    command_rx: Receiver<MoviePlayerCommand>,
) {
    if let Err(err) = run_pipeline_inner(path, event_tx.clone(), command_rx) {
        let _ = event_tx.send(MoviePlaybackEvent::Error(err.to_string()));
    }
}

fn use_ffmpeg_decoder() -> bool {
    matches!(
        std::env::var("GRIM_MOVIE_DECODER")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "ffmpeg" | "1" | "true"
    )
}

fn run_ffmpeg_pipeline(
    path: PathBuf,
    event_tx: Sender<MoviePlaybackEvent>,
    command_rx: Receiver<MoviePlayerCommand>,
) {
    if let Err(err) = run_ffmpeg_pipeline_inner(&path, event_tx.clone(), command_rx) {
        let _ = event_tx.send(MoviePlaybackEvent::Error(err.to_string()));
    }
}

fn run_ffmpeg_pipeline_inner(
    path: &Path,
    event_tx: Sender<MoviePlaybackEvent>,
    command_rx: Receiver<MoviePlayerCommand>,
) -> Result<()> {
    let (width, height, frame_interval) = probe_video_properties(path)?;
    let mut child = Command::new("ffmpeg")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| anyhow!("failed to spawn ffmpeg for {:?}", path))?;

    let frame_size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| anyhow!("frame size overflow for {}x{}", width, height))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ffmpeg stdout unavailable"))?;
    let mut buffer = vec![0u8; frame_size];

    println!(
        "[grim_viewer] ffmpeg pipeline decoding {} ({}x{})",
        path.display(),
        width,
        height
    );

    let mut finished = false;
    let mut frames_sent: u64 = 0;

    while !finished {
        match command_rx.try_recv() {
            Ok(MoviePlayerCommand::Stop(MovieStopReason::Skipped)) => {
                println!(
                    "[grim_viewer] ffmpeg pipeline skip requested {}",
                    path.display()
                );
                let _ = child.kill();
                let _ = child.wait();
                let _ = event_tx.send(MoviePlaybackEvent::Skipped);
                return Ok(());
            }
            Ok(MoviePlayerCommand::Stop(MovieStopReason::Shutdown)) => {
                println!("[grim_viewer] ffmpeg pipeline shutdown {}", path.display());
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                finished = true;
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }

        let mut read_offset = 0;
        while read_offset < frame_size {
            match stdout.read(&mut buffer[read_offset..frame_size]) {
                Ok(0) => {
                    finished = true;
                    break;
                }
                Ok(n) => read_offset += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(anyhow!("ffmpeg read failed: {err:?}")),
            }
        }

        if read_offset < frame_size {
            // EOF reached mid-frame.
            break;
        }

        let stride_bytes = (width * 4) as usize;
        let frame_pixels = buffer[..frame_size].to_vec();
        maybe_dump_decoded_frame(
            "ffmpeg",
            path,
            frames_sent,
            width,
            height,
            stride_bytes,
            &frame_pixels,
        );

        let timestamp = frame_interval.map(|interval| interval.mul_f64(frames_sent as f64));
        let frame = MovieFrame {
            width,
            height,
            stride: stride_bytes as u32,
            pixels: frame_pixels,
            timestamp,
        };

        if let Err(err) = event_tx.send(MoviePlaybackEvent::Frame(frame)) {
            return Err(anyhow!("failed to forward ffmpeg frame: {err:?}"));
        }
        frames_sent = frames_sent.saturating_add(1);
    }

    let status = child
        .wait()
        .context("failed to await ffmpeg process termination")?;
    if status.success() {
        println!(
            "[grim_viewer] ffmpeg pipeline reached end {} (frames={})",
            path.display(),
            frames_sent
        );
        let _ = event_tx.send(MoviePlaybackEvent::Finished);
        Ok(())
    } else {
        Err(anyhow!(
            "ffmpeg exited with status {} for {:?}",
            status,
            path
        ))
    }
}

fn probe_video_properties(path: &Path) -> Result<(u32, u32, Option<Duration>)> {
    let output = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height,avg_frame_rate")
        .arg("-of")
        .arg("csv=p=0:s=x")
        .arg(path)
        .output()
        .with_context(|| anyhow!("failed to invoke ffprobe for {:?}", path))?;

    if !output.status.success() {
        return Err(anyhow!(
            "ffprobe failed for {:?}: {}",
            path,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8(output.stdout).context("ffprobe stdout was not valid UTF-8")?;
    let first_line = stdout.lines().next().unwrap_or_default().trim();
    let mut parts = first_line.split('x');
    let width = parts
        .next()
        .ok_or_else(|| anyhow!("ffprobe missing width output: {first_line}"))?
        .parse::<u32>()
        .context("failed to parse ffprobe width")?;
    let height = parts
        .next()
        .ok_or_else(|| anyhow!("ffprobe missing height output: {first_line}"))?
        .parse::<u32>()
        .context("failed to parse ffprobe height")?;
    let frame_interval = parts
        .next()
        .and_then(|raw| parse_avg_frame_rate(raw.trim()));

    Ok((width, height, frame_interval))
}

fn run_pipeline_inner(
    path: PathBuf,
    event_tx: Sender<MoviePlaybackEvent>,
    command_rx: Receiver<MoviePlayerCommand>,
) -> Result<()> {
    let uri = url::Url::from_file_path(&path)
        .map_err(|_| anyhow!("failed to convert path {:?} into file URI", path))?;

    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .build();
    let appsink = AppSink::builder()
        .caps(&caps)
        .drop(true)
        .max_buffers(1)
        .sync(false)
        .build();

    let playbin = gst::ElementFactory::make("playbin")
        .name("movie_playbin")
        .property("uri", uri.as_str())
        .property("video-sink", &appsink)
        .build()
        .context("failed to construct playbin")?;

    println!("[grim_viewer] movie pipeline booting {}", path.display());

    let bus = playbin
        .bus()
        .ok_or_else(|| anyhow!("playbin missing message bus"))?;

    playbin
        .set_state(gst::State::Playing)
        .context("failed to start playback")?;

    let mut finished = false;
    let sink_pull_timeout = gst::ClockTime::from_mseconds(15);
    let mut frame_index: u64 = 0;

    while !finished {
        match command_rx.try_recv() {
            Ok(MoviePlayerCommand::Stop(MovieStopReason::Skipped)) => {
                println!(
                    "[grim_viewer] movie pipeline skip requested {}",
                    path.display()
                );
                let _ = playbin.set_state(gst::State::Null);
                let _ = event_tx.send(MoviePlaybackEvent::Skipped);
                break;
            }
            Ok(MoviePlayerCommand::Stop(MovieStopReason::Shutdown)) => {
                println!("[grim_viewer] movie pipeline shutdown {}", path.display());
                let _ = playbin.set_state(gst::State::Null);
                break;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                finished = true;
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }

        if let Some(sample) = appsink.try_pull_sample(sink_pull_timeout) {
            match extract_movie_frame(sample, frame_index) {
                Ok(frame) => {
                    maybe_dump_decoded_frame(
                        "gstreamer",
                        &path,
                        frame_index,
                        frame.width,
                        frame.height,
                        frame.stride as usize,
                        &frame.pixels,
                    );
                    if let Err(err) = event_tx.send(MoviePlaybackEvent::Frame(frame)) {
                        let _ = event_tx.send(MoviePlaybackEvent::Error(err.to_string()));
                        break;
                    }
                    frame_index = frame_index.saturating_add(1);
                }
                Err(err) => {
                    let _ = event_tx.send(MoviePlaybackEvent::Error(err.to_string()));
                    break;
                }
            }
        }

        while let Some(message) = bus.pop() {
            use gstreamer::MessageView;
            match message.view() {
                MessageView::Eos(..) => {
                    println!(
                        "[grim_viewer] movie pipeline reached end {}",
                        path.display()
                    );
                    let _ = event_tx.send(MoviePlaybackEvent::Finished);
                    finished = true;
                    break;
                }
                MessageView::Error(err) => {
                    let debug = err.debug().map(|s| s.to_string());
                    eprintln!(
                        "[grim_viewer] movie pipeline error {}: {}",
                        path.display(),
                        err.error()
                    );
                    if let Some(ref dbg) = debug {
                        eprintln!(
                            "[grim_viewer] movie pipeline debug details {}: {}",
                            path.display(),
                            dbg
                        );
                    }
                    let mut details = err.error().message().to_string();
                    if let Some(extra) = debug {
                        details.push_str(&format!(" (debug: {extra})"));
                    }
                    let _ = event_tx.send(MoviePlaybackEvent::Error(details));
                    finished = true;
                    break;
                }
                _ => {}
            }
        }
    }

    playbin
        .set_state(gst::State::Null)
        .context("failed to stop playback cleanly")?;
    Ok(())
}

fn extract_movie_frame(sample: gst::Sample, frame_index: u64) -> Result<MovieFrame> {
    let caps = sample
        .caps()
        .ok_or_else(|| anyhow!("sample missing caps"))?;
    let info =
        VideoInfo::from_caps(caps).map_err(|err| anyhow!("invalid caps for sample: {err:?}"))?;
    static LOG_VIDEO_INFO: Once = Once::new();
    LOG_VIDEO_INFO.call_once(|| {
        println!(
            "[grim_viewer] movie sample caps format={:?} size={}x{} stride={:?}",
            info.format(),
            info.width(),
            info.height(),
            info.stride()
        );
    });

    let buffer = sample
        .buffer()
        .ok_or_else(|| anyhow!("sample missing buffer"))?;
    let map = buffer
        .map_readable()
        .map_err(|err| anyhow!("failed to map buffer: {err:?}"))?;
    let slice = map.as_slice();

    let stride = info.stride()[0];
    if stride <= 0 {
        return Err(anyhow!("unexpected stride {} for video frame", stride));
    }
    let height = info.height() as usize;
    let stride_usize = stride as usize;
    let expected = stride_usize
        .checked_mul(height)
        .ok_or_else(|| anyhow!("frame dimensions overflow"))?;
    if slice.len() < expected {
        return Err(anyhow!(
            "mapped buffer {} smaller than expected {expected}",
            slice.len()
        ));
    }

    let mut pixels = vec![0u8; expected];
    pixels.copy_from_slice(&slice[..expected]);

    let pts = buffer
        .pts()
        .map(|clock| Duration::from_nanos(clock.nseconds()));
    let fps_timestamp = frame_duration_from_fps(info.fps());
    let timestamp =
        pts.or_else(|| fps_timestamp.map(|interval| interval.mul_f64(frame_index as f64)));

    let frame = MovieFrame {
        width: info.width(),
        height: info.height(),
        stride: stride as u32,
        pixels,
        timestamp,
    };
    Ok(frame)
}

fn frame_duration_from_fps(fps: gst::Fraction) -> Option<Duration> {
    let numer = fps.numer();
    let denom = fps.denom();
    if numer <= 0 || denom <= 0 {
        return None;
    }
    let seconds = (denom as f64) / (numer as f64);
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(seconds))
}

fn parse_avg_frame_rate(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0/0" {
        return None;
    }
    let (num_str, den_str) = trimmed.split_once('/')?;
    let num: f64 = num_str.parse().ok()?;
    let den: f64 = den_str.parse().ok()?;
    if num <= 0.0 || den <= 0.0 {
        return None;
    }
    let seconds = den / num;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(seconds))
}
