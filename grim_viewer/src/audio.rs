use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, anyhow};
#[cfg(feature = "audio")]
use rodio::OutputStream;

use crate::audio_log::{AudioAggregation, AudioLogTracker};

#[derive(Debug, Clone)]
pub struct AudioStatus {
    pub state: AudioAggregation,
    pub seen_events: bool,
}

impl AudioStatus {
    pub fn new(state: AudioAggregation, seen_events: bool) -> Self {
        Self { state, seen_events }
    }
}

pub struct AudioLogWatcher {
    pub path: PathBuf,
    tracker: AudioLogTracker,
    last_len: Option<u64>,
    last_modified: Option<SystemTime>,
    seen_events: bool,
}

impl AudioLogWatcher {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            tracker: AudioLogTracker::default(),
            last_len: None,
            last_modified: None,
            seen_events: false,
        }
    }

    pub fn poll(&mut self) -> Result<Option<AudioStatus>> {
        let metadata = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(anyhow!(err))
                    .with_context(|| format!("reading metadata for {}", self.path.display()));
            }
        };

        let len = metadata.len();
        let modified = metadata.modified().ok();
        let should_read = self.last_len.map(|prev| prev != len).unwrap_or(true)
            || match (self.last_modified, modified) {
                (Some(prev), Some(current)) => prev != current,
                (None, Some(_)) => true,
                _ => false,
            };

        if !should_read {
            return Ok(None);
        }

        let mut reset_triggered = false;
        if self.last_len.map_or(false, |prev| len < prev) {
            self.reset();
            reset_triggered = true;
        }

        let data = match fs::read_to_string(&self.path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(anyhow!(err))
                    .with_context(|| format!("reading audio log from {}", self.path.display()));
            }
        };

        let changed = self.tracker.ingest(&data).map_err(|err| anyhow!(err))?;

        self.last_len = Some(len);
        self.last_modified = modified;

        if changed {
            self.seen_events = true;
        }

        if changed || reset_triggered {
            return Ok(Some(self.current_status()));
        }
        Ok(None)
    }

    pub fn current_status(&self) -> AudioStatus {
        AudioStatus::new(self.tracker.state.clone(), self.seen_events)
    }

    pub fn has_seen_events(&self) -> bool {
        self.seen_events
    }

    fn reset(&mut self) {
        self.tracker = AudioLogTracker::default();
        self.seen_events = false;
    }
}

pub fn log_audio_update(status: &AudioStatus) {
    if !status.seen_events {
        return;
    }
    let music = status
        .state
        .current_music
        .as_ref()
        .map(|m| m.cue.as_str())
        .unwrap_or("<none>");
    let sfx: Vec<&str> = status
        .state
        .active_sfx
        .keys()
        .map(|key| key.as_str())
        .collect();
    println!(
        "[audio] music={} sfx_handles=[{}]",
        music,
        if sfx.is_empty() {
            String::from("<none>")
        } else {
            sfx.join(", ")
        }
    );
}

pub fn run_audio_log_headless(watcher: &mut AudioLogWatcher) -> Result<()> {
    let mut last_event = Instant::now();
    let start = Instant::now();

    println!(
        "[audio] monitoring {} (Ctrl+C to exit)",
        watcher.path.display()
    );

    loop {
        if let Some(status) = watcher.poll()? {
            log_audio_update(&status);
            last_event = Instant::now();
        }

        if watcher.has_seen_events() {
            if last_event.elapsed() > Duration::from_secs(1) {
                break;
            }
        } else if start.elapsed() > Duration::from_secs(5) {
            break;
        }

        thread::sleep(Duration::from_millis(120));
    }

    Ok(())
}

pub fn spawn_audio_log_thread(mut watcher: AudioLogWatcher) -> mpsc::Receiver<AudioStatus> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if tx.send(watcher.current_status()).is_err() {
            return;
        }

        loop {
            match watcher.poll() {
                Ok(Some(status)) => {
                    if tx.send(status).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(err) => eprintln!("[grim_viewer] audio log polling error: {err:?}"),
            }
            thread::sleep(Duration::from_millis(120));
        }
    });
    rx
}

pub fn init_audio() -> Result<()> {
    #[cfg(feature = "audio")]
    {
        let (_stream, _stream_handle) = OutputStream::try_default()
            .context("initializing default audio output device via rodio")?;
        let _ = (_stream, _stream_handle);
    }

    Ok(())
}
