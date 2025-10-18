use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result, anyhow};
use crossbeam_channel;
use crossbeam_channel::{Receiver, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gstreamer_video::VideoInfo;

#[derive(Debug, Clone)]
pub struct MovieFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug)]
pub enum MoviePlaybackEvent {
    Frame(MovieFrame),
    Finished,
    Skipped,
    Error(String),
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
        gst::init().map_err(|err| anyhow!("failed to initialise GStreamer: {err:?}"))?;

        let movie_path = PathBuf::from(path);
        if !movie_path.is_file() {
            return Err(anyhow!("movie file {:?} not found", movie_path));
        }

        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();

        let handle = thread::Builder::new()
            .name("grim_movie_player".to_string())
            .spawn(move || run_pipeline(movie_path, event_tx, command_rx))
            .context("failed to spawn movie playback thread")?;

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
            if let Err(err) = dispatch_frame(sample, &event_tx) {
                let _ = event_tx.send(MoviePlaybackEvent::Error(err.to_string()));
                break;
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

fn dispatch_frame(sample: gst::Sample, event_tx: &Sender<MoviePlaybackEvent>) -> Result<()> {
    let caps = sample
        .caps()
        .ok_or_else(|| anyhow!("sample missing caps"))?;
    let info =
        VideoInfo::from_caps(caps).map_err(|err| anyhow!("invalid caps for sample: {err:?}"))?;

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

    let frame = MovieFrame {
        width: info.width(),
        height: info.height(),
        stride: stride as u32,
        pixels,
    };
    event_tx
        .send(MoviePlaybackEvent::Frame(frame))
        .map_err(|err| anyhow!("failed to forward frame: {err:?}"))?;
    Ok(())
}
