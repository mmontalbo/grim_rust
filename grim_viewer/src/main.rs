use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

mod audio_log;
mod timeline;
mod ui_layout;

use audio_log::{AudioAggregation, AudioLogTracker};

use anyhow::{Context, Result, anyhow, bail, ensure};
use bytemuck::{Pod, Zeroable, cast_slice};
use clap::Parser;
use font8x8::legacy::BASIC_LEGACY;
use glam::{Mat3, Mat4, Vec3, Vec4};
use grim_formats::set::Setup;
use grim_formats::{BmFile, DepthStats, SetFile, decode_bm, decode_bm_with_seed};
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use pollster::FutureExt;
#[cfg(feature = "audio")]
use rodio::OutputStream;
use serde::Deserialize;
use serde_json::Value;
use timeline::{
    HookLookup, HookReference, TimelineSummary, build_timeline_summary, parse_hook_reference,
};
use ui_layout::{MinimapConstraints, PanelKind, PanelSize, UiLayout, ViewportRect};
use wgpu::{
    Backends, COPY_BYTES_PER_ROW_ALIGNMENT, InstanceDescriptor, InstanceFlags, Maintain,
    SurfaceError, util::DeviceExt,
};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowBuilder},
};

#[derive(Parser, Debug)]
#[command(about = "Minimal viewer stub that boots wgpu and rodio", version)]
struct Args {
    /// Asset manifest JSON produced by grim_engine --asset-manifest
    #[arg(long, default_value = "artifacts/manny_office_assets.json")]
    manifest: PathBuf,

    /// Asset to load from the LAB archives for inspection
    #[arg(long, default_value = "mo_0_ddtws.bm")]
    asset: String,

    /// Optional boot timeline manifest produced by grim_engine --timeline-json
    #[arg(long)]
    timeline: Option<PathBuf>,

    /// When set, stream audio cue updates from the given log file
    #[arg(long)]
    audio_log: Option<PathBuf>,

    /// When set, overlay Manny's movement trace captured via --movement-log-json
    #[arg(long)]
    movement_log: Option<PathBuf>,

    /// When set, overlay hotspot event log captured via --event-log-json
    #[arg(long)]
    event_log: Option<PathBuf>,

    /// When set, write the decoded bitmap to disk (PNG) before launching the viewer
    #[arg(long)]
    dump_frame: Option<PathBuf>,

    /// When set, render the textured quad offscreen and dump the final output PNG
    #[arg(long)]
    dump_render: Option<PathBuf>,

    /// Run the offscreen renderer and validate the output against the decoded bitmap
    #[arg(long)]
    verify_render: bool,

    /// Maximum allowed fraction (0-1) of pixels that may diverge in the render diff
    #[arg(long, default_value_t = 0.01)]
    render_diff_threshold: f32,

    /// Skip creating a winit window/event loop; useful for headless automation
    #[arg(long)]
    headless: bool,

    /// Optional layout preset JSON describing overlay sizes and minimap constraints
    #[arg(long)]
    layout_preset: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LayoutPreset {
    #[serde(default)]
    audio: Option<PanelPreset>,
    #[serde(default)]
    timeline: Option<PanelPreset>,
    #[serde(default)]
    scrubber: Option<PanelPreset>,
    #[serde(default)]
    minimap: Option<MinimapPreset>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PanelPreset {
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    padding_x: Option<u32>,
    #[serde(default)]
    padding_y: Option<u32>,
    #[serde(default)]
    enabled: Option<bool>,
}

impl PanelPreset {
    fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MinimapPreset {
    #[serde(default)]
    min_side: Option<f32>,
    #[serde(default)]
    preferred_fraction: Option<f32>,
    #[serde(default)]
    max_fraction: Option<f32>,
}

#[derive(Debug, Clone)]
struct AudioStatus {
    state: AudioAggregation,
    seen_events: bool,
}

impl AudioStatus {
    fn new(state: AudioAggregation, seen_events: bool) -> Self {
        Self { state, seen_events }
    }
}

struct AudioLogWatcher {
    path: PathBuf,
    tracker: AudioLogTracker,
    last_len: Option<u64>,
    last_modified: Option<SystemTime>,
    seen_events: bool,
}

impl AudioLogWatcher {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            tracker: AudioLogTracker::default(),
            last_len: None,
            last_modified: None,
            seen_events: false,
        }
    }

    fn poll(&mut self) -> Result<Option<AudioStatus>> {
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

    fn current_status(&self) -> AudioStatus {
        AudioStatus::new(self.tracker.state.clone(), self.seen_events)
    }

    fn has_seen_events(&self) -> bool {
        self.seen_events
    }

    fn reset(&mut self) {
        self.tracker = AudioLogTracker::default();
        self.seen_events = false;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct MovementSample {
    frame: u32,
    position: [f32; 3],
    #[serde(default)]
    yaw: Option<f32>,
    #[serde(default)]
    sector: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HotspotEventLog {
    events: Vec<HotspotEvent>,
}

#[derive(Debug, Clone, Deserialize)]
struct HotspotEvent {
    #[allow(dead_code)]
    sequence: u32,
    #[serde(default)]
    frame: Option<u32>,
    label: String,
}

impl HotspotEvent {
    fn kind(&self) -> HotspotEventKind {
        if self.label.starts_with("set.setup.")
            || self.label.starts_with("set.switch")
            || self.label.starts_with("actor.select")
        {
            HotspotEventKind::Selection
        } else if self.label.starts_with("hotspot.") {
            HotspotEventKind::Hotspot
        } else if self.label.starts_with("actor.manny.head_target") {
            HotspotEventKind::HeadTarget
        } else if self.label.starts_with("actor.manny.ignore_boxes") {
            HotspotEventKind::IgnoreBoxes
        } else if self.label.starts_with("actor.manny.chore") {
            HotspotEventKind::Chore
        } else if self.label.starts_with("dialog.") {
            HotspotEventKind::Dialog
        } else {
            HotspotEventKind::Other
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotspotEventKind {
    Hotspot,
    HeadTarget,
    IgnoreBoxes,
    Chore,
    Dialog,
    Selection,
    Other,
}

fn event_marker_style(kind: HotspotEventKind) -> (f32, [f32; 3], f32) {
    match kind {
        HotspotEventKind::Hotspot => (0.05, [0.95, 0.85, 0.35], 0.4),
        HotspotEventKind::HeadTarget => (0.045, [0.35, 0.9, 0.95], 0.35),
        HotspotEventKind::IgnoreBoxes => (0.045, [0.95, 0.45, 0.35], 0.35),
        HotspotEventKind::Chore => (0.042, [0.6, 0.4, 0.95], 0.25),
        HotspotEventKind::Dialog => (0.042, [0.95, 0.65, 0.75], 0.3),
        HotspotEventKind::Selection => (0.044, [0.45, 0.95, 0.55], 0.32),
        HotspotEventKind::Other => (0.04, [0.78, 0.78, 0.78], 0.2),
    }
}

#[derive(Debug, Clone)]
struct MovementTrace {
    samples: Vec<MovementSample>,
    first_frame: u32,
    last_frame: u32,
    total_distance: f32,
    yaw_min: Option<f32>,
    yaw_max: Option<f32>,
    sector_counts: BTreeMap<String, u32>,
    bounds: SceneBounds,
}

impl MovementTrace {
    fn from_samples(mut samples: Vec<MovementSample>) -> Result<Self> {
        ensure!(!samples.is_empty(), "movement trace is empty");
        samples.sort_by(|a, b| a.frame.cmp(&b.frame));

        let first_frame = samples.first().map(|sample| sample.frame).unwrap_or(0);
        let last_frame = samples
            .last()
            .map(|sample| sample.frame)
            .unwrap_or(first_frame);

        let mut bounds = SceneBounds {
            min: samples[0].position,
            max: samples[0].position,
        };
        let mut total_distance = 0.0_f32;
        let mut previous = samples.first().map(|sample| sample.position);
        let mut yaw_min = None;
        let mut yaw_max = None;
        let mut sector_counts: BTreeMap<String, u32> = BTreeMap::new();

        for sample in &samples {
            bounds.update(sample.position);
            if let Some(prev) = previous {
                total_distance += distance(prev, sample.position);
            }
            previous = Some(sample.position);

            if let Some(yaw) = sample.yaw {
                yaw_min = Some(yaw_min.map_or(yaw, |current: f32| current.min(yaw)));
                yaw_max = Some(yaw_max.map_or(yaw, |current: f32| current.max(yaw)));
            }

            if let Some(sector) = sample.sector.as_ref() {
                *sector_counts.entry(sector.clone()).or_default() += 1;
            }
        }

        Ok(Self {
            samples,
            first_frame,
            last_frame,
            total_distance,
            yaw_min,
            yaw_max,
            sector_counts,
            bounds,
        })
    }

    fn sample_count(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    fn first_frame(&self) -> u32 {
        self.first_frame
    }

    #[cfg(test)]
    fn last_frame(&self) -> u32 {
        self.last_frame
    }

    fn yaw_range(&self) -> Option<(f32, f32)> {
        match (self.yaw_min, self.yaw_max) {
            (Some(min), Some(max)) => Some((min, max)),
            _ => None,
        }
    }

    fn dominant_sectors(&self, limit: usize) -> Vec<(&str, u32)> {
        let mut sectors: Vec<(&str, u32)> = self
            .sector_counts
            .iter()
            .map(|(name, count)| (name.as_str(), *count))
            .collect();
        sectors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        sectors.truncate(limit);
        sectors
    }

    fn nearest_position(&self, frame: u32) -> Option<[f32; 3]> {
        let mut best: Option<[f32; 3]> = None;
        let mut best_delta = u32::MAX;
        for sample in &self.samples {
            let delta = if sample.frame >= frame {
                sample.frame - frame
            } else {
                frame - sample.frame
            };
            if delta < best_delta {
                best_delta = delta;
                best = Some(sample.position);
                if delta == 0 {
                    break;
                }
            }
        }
        best
    }
}

#[derive(Debug, Clone)]
struct ScrubEvent {
    scene_index: usize,
    frame: u32,
    label: String,
}

#[derive(Debug, Clone)]
struct MovementScrubber {
    current_sample: usize,
    sample_frames: Vec<u32>,
    head_targets: Vec<ScrubEvent>,
    highlighted_event: Option<usize>,
}

impl MovementScrubber {
    fn new(scene: &ViewerScene) -> Option<Self> {
        let trace = scene.movement_trace()?;
        if trace.samples.is_empty() {
            return None;
        }

        let sample_frames: Vec<u32> = trace.samples.iter().map(|sample| sample.frame).collect();
        let mut head_targets: Vec<ScrubEvent> = scene
            .hotspot_events()
            .iter()
            .enumerate()
            .filter_map(|(idx, event)| match (event.kind(), event.frame) {
                (HotspotEventKind::HeadTarget, Some(frame)) => Some(ScrubEvent {
                    scene_index: idx,
                    frame,
                    label: event.label.clone(),
                }),
                _ => None,
            })
            .collect();
        head_targets.sort_by(|a, b| a.frame.cmp(&b.frame));

        let mut scrubber = Self {
            current_sample: 0,
            sample_frames,
            head_targets,
            highlighted_event: None,
        };
        scrubber.update_highlight();
        Some(scrubber)
    }

    fn current_frame(&self) -> u32 {
        self.sample_frames
            .get(self.current_sample)
            .copied()
            .unwrap_or_default()
    }

    fn current_position(&self, trace: &MovementTrace) -> Option<[f32; 3]> {
        trace
            .samples
            .get(self.current_sample)
            .map(|sample| sample.position)
    }

    fn current_yaw(&self, trace: &MovementTrace) -> Option<f32> {
        trace
            .samples
            .get(self.current_sample)
            .and_then(|sample| sample.yaw)
    }

    fn highlighted_event(&self) -> Option<&ScrubEvent> {
        self.highlighted_event
            .and_then(|idx| self.head_targets.get(idx))
    }

    fn next_event(&self) -> Option<&ScrubEvent> {
        let current_frame = self.current_frame();
        self.head_targets
            .iter()
            .find(|event| event.frame > current_frame)
    }

    fn step(&mut self, delta: i32) -> bool {
        if self.sample_frames.is_empty() {
            return false;
        }
        let len = self.sample_frames.len() as i32;
        let current = self.current_sample as i32;
        let next = (current + delta).clamp(0, len - 1);
        if next == current {
            return false;
        }
        self.current_sample = next as usize;
        self.update_highlight();
        true
    }

    fn seek_to_frame(&mut self, frame: u32) -> bool {
        if self.sample_frames.is_empty() {
            return false;
        }
        let mut best_idx = self.current_sample;
        let mut best_delta = u32::MAX;
        for (idx, sample_frame) in self.sample_frames.iter().enumerate() {
            let delta = sample_frame.abs_diff(frame);
            if delta < best_delta {
                best_delta = delta;
                best_idx = idx;
                if delta == 0 {
                    break;
                }
            }
        }
        if best_idx == self.current_sample {
            self.highlighted_event = self
                .head_targets
                .iter()
                .position(|event| event.frame == frame);
            return false;
        }
        self.current_sample = best_idx;
        self.update_highlight();
        true
    }

    fn jump_to_head_target(&mut self, direction: i32) -> bool {
        if self.head_targets.is_empty() {
            return false;
        }

        let current_frame = self.current_frame();
        let next_index = if direction >= 0 {
            self.head_targets
                .iter()
                .enumerate()
                .find(|(_, event)| event.frame > current_frame)
                .map(|(idx, _)| idx)
                .or(Some(0))
        } else {
            self.head_targets
                .iter()
                .enumerate()
                .rev()
                .find(|(_, event)| event.frame < current_frame)
                .map(|(idx, _)| idx)
                .or_else(|| self.head_targets.len().checked_sub(1))
        };

        if let Some(idx) = next_index {
            let frame = self.head_targets[idx].frame;
            let moved = self.seek_to_frame(frame);
            self.highlighted_event = Some(idx);
            if !moved {
                self.update_highlight();
            }
            return true;
        }
        false
    }

    fn overlay_lines(&self, trace: &MovementTrace) -> Vec<String> {
        const MAX_LINE: usize = 78;

        if self.sample_frames.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::new();
        lines.push("Scrubber".to_string());

        let frame = self.current_frame();
        let position = trace
            .samples
            .get(self.current_sample)
            .map(|sample| sample.position)
            .unwrap_or([0.0, 0.0, 0.0]);
        let idx_label = format!(
            "  frame: {frame} ({}/{})",
            self.current_sample + 1,
            self.sample_frames.len()
        );
        lines.push(truncate_line(&idx_label, MAX_LINE));

        lines.push(truncate_line(
            &format!(
                "  pos: ({:.3}, {:.3}, {:.3})",
                position[0], position[1], position[2]
            ),
            MAX_LINE,
        ));

        if let Some(yaw) = self.current_yaw(trace) {
            lines.push(truncate_line(&format!("  yaw: {:.3}", yaw), MAX_LINE));
        }

        if let Some(event) = self.highlighted_event() {
            let delta_label = if event.frame <= frame {
                format!("-{}", frame - event.frame)
            } else {
                format!("+{}", event.frame - frame)
            };
            lines.push(truncate_line(
                &format!("  head: [{}|Δ{}] {}", event.frame, delta_label, event.label),
                MAX_LINE,
            ));
        } else {
            lines.push("  head: (no target)".to_string());
        }

        if let Some(next) = self.next_event() {
            lines.push(truncate_line(
                &format!("  next head: [{}] {}", next.frame, next.label),
                MAX_LINE,
            ));
        }

        lines.push(truncate_line(
            "  Overlay: markers render in plate space using the active camera transform.",
            MAX_LINE,
        ));
        lines.push(truncate_line(
            "  Legend: teal spawn, violet path, orange finish, aqua current frame, gold head target highlight.",
            MAX_LINE,
        ));
        lines.push("  Controls: [ ] step, { } jump".to_string());

        lines
    }

    fn update_highlight(&mut self) {
        if self.head_targets.is_empty() || self.sample_frames.is_empty() {
            self.highlighted_event = None;
            return;
        }

        let current_frame = self.current_frame();
        let mut best_past: Option<(usize, u32)> = None;
        let mut best_future: Option<(usize, u32)> = None;

        for (idx, event) in self.head_targets.iter().enumerate() {
            if event.frame <= current_frame {
                let delta = current_frame - event.frame;
                if best_past.map_or(true, |(_, best)| delta < best) {
                    best_past = Some((idx, delta));
                }
            } else {
                let delta = event.frame - current_frame;
                if best_future.map_or(true, |(_, best)| delta < best) {
                    best_future = Some((idx, delta));
                }
            }
        }

        self.highlighted_event = best_past.or(best_future).map(|(idx, _)| idx);
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod movement_tests {
    use super::*;

    #[test]
    fn movement_trace_summarises_samples() {
        let samples = vec![
            MovementSample {
                frame: 3,
                position: [1.0, 0.0, 0.0],
                yaw: Some(0.3),
                sector: Some("b".to_string()),
            },
            MovementSample {
                frame: 2,
                position: [0.0, 0.0, 0.0],
                yaw: Some(0.1),
                sector: Some("a".to_string()),
            },
            MovementSample {
                frame: 4,
                position: [1.0, 1.0, 0.0],
                yaw: None,
                sector: Some("a".to_string()),
            },
        ];

        let trace = MovementTrace::from_samples(samples).expect("trace");

        assert_eq!(trace.sample_count(), 3);
        assert_eq!(trace.first_frame, 2);
        assert_eq!(trace.last_frame, 4);
        assert!((trace.total_distance - 2.0).abs() < 1e-6);
        assert_eq!(trace.yaw_range(), Some((0.1, 0.3)));

        let sectors = trace.dominant_sectors(3);
        assert_eq!(sectors.len(), 2);
        assert_eq!(sectors[0], ("a", 2));
        assert_eq!(sectors[1], ("b", 1));

        assert!((trace.bounds.min[0] - 0.0).abs() < 1e-6);
        assert!((trace.bounds.max[1] - 1.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod scrubber_tests {
    use super::*;

    fn sample_scene() -> ViewerScene {
        let samples = vec![
            MovementSample {
                frame: 1,
                position: [0.0, 0.0, 0.0],
                yaw: Some(0.0),
                sector: None,
            },
            MovementSample {
                frame: 2,
                position: [1.0, 0.0, 0.0],
                yaw: Some(0.2),
                sector: None,
            },
            MovementSample {
                frame: 4,
                position: [2.0, 0.0, 0.0],
                yaw: Some(0.4),
                sector: None,
            },
            MovementSample {
                frame: 5,
                position: [3.0, 0.0, 0.0],
                yaw: Some(0.6),
                sector: None,
            },
        ];

        let trace = MovementTrace::from_samples(samples).expect("trace");
        let mut scene = ViewerScene {
            entities: Vec::new(),
            position_bounds: None,
            timeline: None,
            movement: None,
            hotspot_events: vec![
                HotspotEvent {
                    sequence: 1,
                    frame: Some(2),
                    label: "actor.manny.head_target /desk".to_string(),
                },
                HotspotEvent {
                    sequence: 2,
                    frame: Some(5),
                    label: "actor.manny.head_target /tube".to_string(),
                },
            ],
            camera: None,
            active_setup: None,
        };
        scene.attach_movement_trace(trace);
        scene
    }

    #[test]
    fn scrubber_prefers_recent_head_target() {
        let scene = sample_scene();
        let mut scrubber = MovementScrubber::new(&scene).expect("scrubber");

        assert_eq!(scrubber.current_frame(), 1);
        assert_eq!(
            scrubber.highlighted_event().map(|event| event.frame),
            Some(2)
        );

        scrubber.step(1);
        assert_eq!(scrubber.current_frame(), 2);
        assert_eq!(
            scrubber.highlighted_event().map(|event| event.frame),
            Some(2)
        );

        scrubber.step(1);
        assert_eq!(scrubber.current_frame(), 4);
        assert_eq!(
            scrubber.highlighted_event().map(|event| event.frame),
            Some(2)
        );
    }

    #[test]
    fn scrubber_jumps_between_head_targets() {
        let scene = sample_scene();
        let mut scrubber = MovementScrubber::new(&scene).expect("scrubber");

        assert_eq!(scrubber.current_frame(), 1);
        scrubber.step(1);
        assert_eq!(scrubber.current_frame(), 2);

        scrubber.jump_to_head_target(1);
        assert_eq!(
            scrubber.highlighted_event().map(|event| event.frame),
            Some(5)
        );
        assert_eq!(scrubber.current_frame(), 5);

        scrubber.jump_to_head_target(-1);
        assert_eq!(
            scrubber.highlighted_event().map(|event| event.frame),
            Some(2)
        );
    }
}

fn audio_overlay_lines(status: &AudioStatus) -> Vec<String> {
    if !status.seen_events {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push("Audio Monitor".to_string());

    match status.state.current_music.as_ref() {
        Some(music) => {
            lines.push(truncate_line(&format!("Music: {}", music.cue), 62));
            if !music.params.is_empty() {
                lines.push(truncate_line(
                    &format!("  params: {}", music.params.join(", ")),
                    62,
                ));
            }
        }
        None => {
            let stop = status.state.last_music_stop_mode.as_deref().unwrap_or("-");
            lines.push(truncate_line(&format!("Music: <none> (stop: {stop})"), 62));
        }
    }

    lines.push("SFX:".to_string());
    if status.state.active_sfx.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        const MAX_SFX_LINES: usize = 6;
        for (idx, (handle, entry)) in status.state.active_sfx.iter().enumerate() {
            if idx >= MAX_SFX_LINES {
                let remaining = status.state.active_sfx.len() - MAX_SFX_LINES;
                lines.push(format!("  ... +{} more", remaining));
                break;
            }
            let mut line = format!("  {}: {}", handle, entry.cue);
            if !entry.params.is_empty() {
                line.push_str(&format!(" [{}]", entry.params.join(", ")));
            }
            lines.push(truncate_line(&line, 62));
        }
    }

    lines
}

fn timeline_overlay_lines(
    scene: Option<&ViewerScene>,
    selected_index: Option<usize>,
) -> Vec<String> {
    const MAX_LINE: usize = 84;

    let scene = match scene {
        Some(scene) => scene,
        None => return Vec::new(),
    };
    let summary = match scene.timeline.as_ref() {
        Some(summary) => summary,
        None => return Vec::new(),
    };

    let selected_entity = selected_index.and_then(|idx| scene.entities.get(idx));
    if selected_entity.is_none() && summary.hooks.is_empty() && scene.movement_trace().is_none() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push("Entity Focus".to_string());

    if let Some(entity) = selected_entity {
        lines.push(truncate_line(
            &format!("> [{}] {}", entity.kind.label(), entity.name),
            MAX_LINE,
        ));
        if let Some(stage_index) = entity.timeline_stage_index {
            let label = entity.timeline_stage_label.as_deref().unwrap_or("-");
            lines.push(truncate_line(
                &format!("  Stage {:02} {label}", stage_index),
                MAX_LINE,
            ));
        } else if let Some(label) = entity.timeline_stage_label.as_deref() {
            lines.push(truncate_line(&format!("  Stage -- {label}"), MAX_LINE));
        }

        let mut detail_lines: Vec<String> = Vec::new();
        if let Some(position) = entity.position {
            detail_lines.push(format!(
                "  pos: ({:.3}, {:.3}, {:.3})",
                position[0], position[1], position[2]
            ));
        }
        if let Some(rotation) = entity.rotation {
            detail_lines.push(format!(
                "  rot: ({:.3}, {:.3}, {:.3})",
                rotation[0], rotation[1], rotation[2]
            ));
        }
        if let Some(target) = entity.facing_target.as_deref() {
            if !target.is_empty() {
                detail_lines.push(format!("  facing: {target}"));
            }
        }
        if let Some(control) = entity.head_control.as_deref() {
            if !control.is_empty() {
                detail_lines.push(format!("  head control: {control}"));
            }
        }
        if let Some(rate) = entity.head_look_rate {
            detail_lines.push(format!("  head rate: {:.3}", rate));
        }

        if entity.last_played.is_some()
            || entity.last_looping.is_some()
            || entity.last_completed.is_some()
        {
            let played = entity.last_played.as_deref().unwrap_or("-");
            let looping = entity.last_looping.as_deref().unwrap_or("-");
            let completed = entity.last_completed.as_deref().unwrap_or("-");
            detail_lines.push(format!(
                "  chore: played={} looping={} completed={}",
                played, looping, completed
            ));
        }

        for line in detail_lines {
            lines.push(truncate_line(&line, MAX_LINE));
        }

        if let Some(created) = entity.created_by.as_ref() {
            lines.push(truncate_line(&format!("  Hook {created}"), MAX_LINE));
        }
    } else {
        lines.push("  (Use Left/Right arrows to select a marker)".to_string());
    }

    let selected_hook_idx = selected_entity.and_then(|entity| {
        entity
            .timeline_hook_index
            .or_else(|| {
                entity
                    .timeline_hook_name
                    .as_ref()
                    .and_then(|name| summary.hooks.iter().position(|hook| hook.key.name == *name))
            })
            .or_else(|| {
                entity.timeline_stage_index.and_then(|stage_idx| {
                    summary
                        .hooks
                        .iter()
                        .position(|hook| hook.stage_index == Some(stage_idx))
                })
            })
            .or_else(|| {
                summary
                    .hooks
                    .iter()
                    .position(|hook| hook.targets.iter().any(|target| target == &entity.name))
            })
    });

    if !summary.hooks.is_empty() {
        lines.push(String::new());
        lines.push("Timeline Hooks".to_string());

        for (idx, hook) in summary.hooks.iter().enumerate() {
            let stage = hook
                .stage_index
                .map(|value| format!("{:02}", value))
                .unwrap_or_else(|| String::from("--"));
            let label = hook.stage_label.as_deref().unwrap_or("(no stage)");
            let marker = if Some(idx) == selected_hook_idx {
                '>'
            } else {
                ' '
            };
            let summary_line = format!("{marker} {stage} {} — {label}", hook.key.name);
            lines.push(truncate_line(&summary_line, MAX_LINE));
            if Some(idx) == selected_hook_idx {
                if let Some(kind) = hook.kind.as_deref() {
                    lines.push(truncate_line(&format!("    kind: {kind}"), MAX_LINE));
                }
                if let Some(source) = hook.defined_in.as_deref() {
                    let location = match hook.defined_at_line {
                        Some(line) => format!("{source}:{line}"),
                        None => source.to_string(),
                    };
                    lines.push(truncate_line(&format!("    source: {location}"), MAX_LINE));
                }
                if !hook.prerequisites.is_empty() {
                    let preview: Vec<&str> = hook
                        .prerequisites
                        .iter()
                        .take(3)
                        .map(String::as_str)
                        .collect();
                    let mut prereq_line = format!("    prereqs: {}", preview.join(" -> "));
                    if hook.prerequisites.len() > preview.len() {
                        let remaining = hook.prerequisites.len() - preview.len();
                        prereq_line.push_str(&format!(" (+{remaining})"));
                    }
                    lines.push(truncate_line(&prereq_line, MAX_LINE));
                }
                if !hook.targets.is_empty() {
                    let preview: Vec<&str> =
                        hook.targets.iter().take(3).map(String::as_str).collect();
                    let mut target_line = format!("    targets: {}", preview.join(", "));
                    if hook.targets.len() > preview.len() {
                        let remaining = hook.targets.len() - preview.len();
                        target_line.push_str(&format!(" (+{remaining})"));
                    }
                    lines.push(truncate_line(&target_line, MAX_LINE));
                }
            }
        }
    }

    if !summary.stages.is_empty() {
        lines.push(String::new());
        lines.push("Boot Stages".to_string());
        let highlight_stage = selected_entity
            .and_then(|entity| entity.timeline_stage_index)
            .or_else(|| {
                selected_hook_idx
                    .and_then(|idx| summary.hooks.get(idx).and_then(|hook| hook.stage_index))
            });
        for stage in &summary.stages {
            let marker = if Some(stage.index) == highlight_stage {
                '>'
            } else {
                ' '
            };
            let stage_line = format!("{marker} {:02} {}", stage.index, stage.label);
            lines.push(truncate_line(&stage_line, MAX_LINE));
        }
    }

    if let Some(trace) = scene.movement_trace() {
        lines.push(String::new());
        lines.push("Movement Trace".to_string());

        lines.push(truncate_line(
            &format!(
                "  samples: {} (frames {}–{})",
                trace.sample_count(),
                trace.first_frame,
                trace.last_frame
            ),
            MAX_LINE,
        ));
        lines.push(truncate_line(
            &format!("  distance: {:.3}", trace.total_distance),
            MAX_LINE,
        ));
        if let Some((min_yaw, max_yaw)) = trace.yaw_range() {
            lines.push(truncate_line(
                &format!("  yaw: {:.3} → {:.3}", min_yaw, max_yaw),
                MAX_LINE,
            ));
        }
        if let Some(first) = trace.samples.first() {
            let sector = first.sector.as_deref().unwrap_or("-");
            lines.push(truncate_line(
                &format!(
                    "  start: ({:.3}, {:.3}, {:.3}) {}",
                    first.position[0], first.position[1], first.position[2], sector
                ),
                MAX_LINE,
            ));
        }
        if let Some(last) = trace.samples.last() {
            let sector = last.sector.as_deref().unwrap_or("-");
            let yaw_label = last
                .yaw
                .map(|value| format!(" yaw {:.3}", value))
                .unwrap_or_default();
            lines.push(truncate_line(
                &format!(
                    "  end: ({:.3}, {:.3}, {:.3}) {}{}",
                    last.position[0], last.position[1], last.position[2], sector, yaw_label
                ),
                MAX_LINE,
            ));
        }
        let sectors = trace.dominant_sectors(3);
        if !sectors.is_empty() {
            let summary = sectors
                .iter()
                .map(|(name, count)| format!("{}×{}", count, name))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(truncate_line(&format!("  sectors: {summary}"), MAX_LINE));
        }
    }

    let events = scene.hotspot_events();
    if !events.is_empty() {
        lines.push(String::new());
        lines.push("Hotspot Events".to_string());
        const MAX_EVENTS: usize = 6;
        for event in events.iter().take(MAX_EVENTS) {
            let frame = event
                .frame
                .map(|value| format!("{value:03}"))
                .unwrap_or_else(|| String::from("--"));
            let prefix = if matches!(event.kind(), HotspotEventKind::Selection) {
                "(sel) "
            } else {
                ""
            };
            let line = format!("  [{frame}] {prefix}{}", event.label);
            lines.push(truncate_line(&line, MAX_LINE));
        }
        if events.len() > MAX_EVENTS {
            let remaining = events.len() - MAX_EVENTS;
            lines.push(truncate_line(&format!("  ... +{remaining} more"), MAX_LINE));
        }
    }

    lines
}

fn truncate_line(line: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut count = 0;
    let mut result = String::new();
    for ch in line.chars() {
        if count + 1 >= limit {
            result.push('…');
            return result;
        }
        result.push(ch);
        count += 1;
    }
    result
}

fn log_audio_update(status: &AudioStatus) {
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

fn run_audio_log_headless(watcher: &mut AudioLogWatcher) -> Result<()> {
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

fn spawn_audio_log_thread(mut watcher: AudioLogWatcher) -> mpsc::Receiver<AudioStatus> {
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

fn main() -> Result<()> {
    let args = Args::parse();

    env_logger::init();

    ensure!(
        (0.0..=1.0).contains(&args.render_diff_threshold),
        "render_diff_threshold must be between 0 and 1 (got {})",
        args.render_diff_threshold
    );

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

    if args.verify_render || args.dump_render.is_some() {
        let preview = decode_result
            .as_ref()
            .map_err(|err| anyhow!("decoding bitmap for render verification: {err}"))?;
        let destination = args.dump_render.as_deref();
        let verification = render_texture_offscreen(preview, destination)
            .context("running offscreen render verification")?;
        let stats = &verification.stats;
        if let Some(path) = destination {
            println!(
                "Rendered quad exported to {} ({}x{} post-raster)",
                path.display(),
                preview.width,
                preview.height
            );
        } else {
            println!(
                "Rendered quad verification completed ({}x{} offscreen)",
                preview.width, preview.height
            );
        }
        if let Some(stats) = preview.depth_stats {
            let (min, max) = (stats.min, stats.max);
            if preview.depth_preview {
                println!(
                    "  source depth range (16-bit) 0x{min:04X} – 0x{max:04X}; render input is normalized depth"
                );
            } else {
                println!(
                    "  source depth range (16-bit) 0x{min:04X} – 0x{max:04X}; render input uses base bitmap colors"
                );
            }
            println!(
                "  depth pixels zero {zero} / {total}",
                zero = stats.zero_pixels,
                total = stats.total_pixels()
            );
        }
        println!(
            "  render luma avg {:.2}, min {}, max {}, opaque pixels {} / {}",
            stats.mean_luma,
            stats.min_luma,
            stats.max_luma,
            stats.opaque_pixels,
            stats.total_pixels
        );
        println!(
            "  render quadrant luma means (TL, TR, BL, BR): {:.2}, {:.2}, {:.2}, {:.2}",
            stats.quadrant_means[0],
            stats.quadrant_means[1],
            stats.quadrant_means[2],
            stats.quadrant_means[3]
        );
        let diff = &verification.diff;
        let mismatch_ratio = diff_mismatch_ratio(diff);
        let mismatch_pct = mismatch_ratio * 100.0;
        println!(
            "  render diff: mismatched_pixels={} ({:.4}%), max_abs_diff={}, mean_abs_diff={:.3}",
            diff.mismatched_pixels, mismatch_pct, diff.max_abs_diff, diff.mean_abs_diff
        );
        println!(
            "  render diff quadrant mismatch ratios (TL, TR, BL, BR): {:.4}, {:.4}, {:.4}, {:.4}",
            diff.quadrant_mismatch[0],
            diff.quadrant_mismatch[1],
            diff.quadrant_mismatch[2],
            diff.quadrant_mismatch[3]
        );
        validate_render_diff(diff, args.render_diff_threshold)?;
    }

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
            let mut scene =
                load_scene_from_timeline(path, &args.manifest, Some(asset_name.as_str()))
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
        println!();
        println!(
            "Scene bootstrap: {} entit{} from timeline manifest",
            scene.entities.len(),
            if scene.entities.len() == 1 {
                "y"
            } else {
                "ies"
            }
        );
        for entity in &scene.entities {
            println!("  - {}", entity.describe());
        }
        if let Some(setup) = scene.active_setup() {
            println!("Active camera setup: {}", setup);
            if scene.camera.is_some() {
                println!(
                    "Markers overlay renders Manny/head targets in plate space using this camera."
                );
            }
        }
        if !scene.entities.is_empty() {
            println!("\nUse ←/→ to cycle entity focus while the viewer is running.");
            println!(
                "Entity focus drives the highlighted marker, timeline overlay, and console dump for the active actor/object."
            );
            println!(
                "Markers overlay: green/blue squares mark entities; red highlights the current selection."
            );
        }
        if let Some(trace) = scene.movement_trace() {
            println!(
                "Movement trace: {} samples (frames {}–{}), distance {:.3}",
                trace.sample_count(),
                trace.first_frame,
                trace.last_frame,
                trace.total_distance
            );
            let sectors = trace.dominant_sectors(3);
            if !sectors.is_empty() {
                let preview: Vec<String> = sectors
                    .iter()
                    .map(|(name, count)| format!("{}×{}", count, name))
                    .collect();
                println!("  sectors: {}", preview.join(", "));
            }
            if let Some((min_yaw, max_yaw)) = trace.yaw_range() {
                println!("  yaw range: {:.3} – {:.3}", min_yaw, max_yaw);
            }
            println!(
                "  Overlay markers: teal = spawn, violet = path, orange = hotspot finish, lime = selection."
            );
            println!(
                "  Scrubber controls: '['/']' step Manny frames; '{{'/'}}' jump head-target markers."
            );
        }
        let event_preview = scene.hotspot_events();
        if !event_preview.is_empty() {
            println!("Hotspot event log: {} entries", event_preview.len());
            for event in event_preview.iter().take(6) {
                let frame_label = event
                    .frame
                    .map(|frame| format!("{frame}"))
                    .unwrap_or_else(|| String::from("--"));
                println!("  [{frame_label}] {}", event.label);
            }
            if event_preview.len() > 6 {
                println!("  ... +{} more", event_preview.len() - 6);
            }
        }
        println!();
    }

    if let Some(trace) = movement_trace.take() {
        println!(
            "Movement trace loaded without timeline overlay: {} samples (frames {}–{})",
            trace.sample_count(),
            trace.first_frame,
            trace.last_frame
        );
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

#[derive(Debug, Deserialize)]
struct AssetManifest {
    found: Vec<AssetManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct AssetManifestEntry {
    asset_name: String,
    archive_path: PathBuf,
    offset: u64,
    size: u32,
    #[serde(default)]
    metadata: Option<AssetMetadataSummary>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AssetMetadataSummary {
    Bitmap {
        codec: u32,
        bits_per_pixel: u32,
        frames: u32,
        width: u32,
        height: u32,
        supported: bool,
    },
}

#[derive(Debug)]
struct ViewerScene {
    entities: Vec<SceneEntity>,
    position_bounds: Option<SceneBounds>,
    timeline: Option<TimelineSummary>,
    movement: Option<MovementTrace>,
    hotspot_events: Vec<HotspotEvent>,
    camera: Option<CameraParameters>,
    active_setup: Option<String>,
}

#[derive(Debug, Clone)]
struct CameraParameters {
    name: String,
    position: [f32; 3],
    interest: [f32; 3],
    roll_degrees: f32,
    fov_degrees: f32,
    near_clip: f32,
    far_clip: f32,
}

impl CameraParameters {
    fn from_setup(name: &str, setup: &Setup) -> Option<Self> {
        let position = setup.position.as_ref()?;
        let interest = setup.interest.as_ref()?;
        let roll_degrees = setup.roll.unwrap_or(0.0);
        let fov_degrees = setup.fov?;
        let near_clip = setup.near_clip?;
        let far_clip = setup.far_clip?;

        Some(Self {
            name: name.to_string(),
            position: [position.x, position.y, position.z],
            interest: [interest.x, interest.y, interest.z],
            roll_degrees,
            fov_degrees,
            near_clip,
            far_clip,
        })
    }

    fn projector(&self, aspect_ratio: f32) -> Option<CameraProjector> {
        if !aspect_ratio.is_finite() || aspect_ratio <= 0.0 {
            return None;
        }

        let eye = Vec3::from_array(self.position);
        let target = Vec3::from_array(self.interest);
        let mut forward = target - eye;
        if forward.length_squared() <= f32::EPSILON {
            return None;
        }
        forward = forward.normalize();

        let mut up = Vec3::Z;
        let roll_radians = self.roll_degrees.to_radians();
        if roll_radians.abs() > f32::EPSILON {
            let rotation = Mat3::from_axis_angle(forward, roll_radians);
            up = rotation * up;
        }

        if up.length_squared() <= f32::EPSILON {
            up = Vec3::Y;
        }

        let view = Mat4::look_at_rh(eye, target, up.normalize());
        let projection = Mat4::perspective_rh(
            self.fov_degrees.to_radians(),
            aspect_ratio,
            self.near_clip.max(1e-4),
            self.far_clip.max(self.near_clip + 1.0),
        );

        Some(CameraProjector {
            view_projection: projection * view,
        })
    }
}

#[derive(Debug, Clone)]
struct CameraProjector {
    view_projection: Mat4,
}

impl CameraProjector {
    fn project(&self, position: [f32; 3]) -> Option<[f32; 2]> {
        let clip = self.view_projection * Vec4::new(position[0], position[1], position[2], 1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if !ndc.x.is_finite() || !ndc.y.is_finite() {
            return None;
        }
        Some([ndc.x, ndc.y])
    }
}

impl ViewerScene {
    fn attach_movement_trace(&mut self, trace: MovementTrace) {
        if let Some(bounds) = self.position_bounds.as_mut() {
            bounds.include_bounds(&trace.bounds);
        } else {
            self.position_bounds = Some(trace.bounds.clone());
        }
        self.movement = Some(trace);
    }

    fn movement_trace(&self) -> Option<&MovementTrace> {
        self.movement.as_ref()
    }

    fn attach_hotspot_events(&mut self, events: Vec<HotspotEvent>) {
        self.hotspot_events = events;
    }

    fn hotspot_events(&self) -> &[HotspotEvent] {
        &self.hotspot_events
    }

    fn camera_projector(&self, aspect_ratio: f32) -> Option<CameraProjector> {
        self.camera
            .as_ref()
            .and_then(|camera| camera.projector(aspect_ratio))
    }

    fn active_setup(&self) -> Option<&str> {
        self.active_setup.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SceneEntityKind {
    Actor,
    Object,
    InterestActor,
}

impl SceneEntityKind {
    fn label(self) -> &'static str {
        match self {
            SceneEntityKind::Actor => "Actor",
            SceneEntityKind::Object => "Object",
            SceneEntityKind::InterestActor => "Interest Actor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SceneEntityKey {
    kind: SceneEntityKind,
    name: String,
}

impl SceneEntityKey {
    fn new(kind: SceneEntityKind, name: String) -> Self {
        Self { kind, name }
    }
}

#[derive(Debug)]
struct SceneEntityBuilder {
    key: SceneEntityKey,
    created_by: Option<String>,
    timeline_hook_index: Option<usize>,
    timeline_stage_index: Option<u32>,
    timeline_stage_label: Option<String>,
    timeline_hook_name: Option<String>,
    methods: BTreeSet<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
    facing_target: Option<String>,
    head_control: Option<String>,
    head_look_rate: Option<f32>,
    last_played: Option<String>,
    last_looping: Option<String>,
    last_completed: Option<String>,
}

impl SceneEntityBuilder {
    fn new(kind: SceneEntityKind, name: String) -> Self {
        Self {
            key: SceneEntityKey::new(kind, name),
            created_by: None,
            timeline_hook_index: None,
            timeline_stage_index: None,
            timeline_stage_label: None,
            timeline_hook_name: None,
            methods: BTreeSet::new(),
            position: None,
            rotation: None,
            facing_target: None,
            head_control: None,
            head_look_rate: None,
            last_played: None,
            last_looping: None,
            last_completed: None,
        }
    }

    fn apply_actor_snapshot(&mut self, value: &Value, hooks: &HookLookup) {
        if let Some(reference_value) = value.get("created_by") {
            if let Some(reference) = parse_hook_reference(reference_value) {
                if self.created_by.is_none() {
                    self.created_by = Some(format_hook_reference(&reference));
                }
                self.register_hook_reference(&reference, hooks);
            }
        }

        if let Some(methods) = value
            .get("method_totals")
            .and_then(|totals| totals.as_object())
        {
            for key in methods.keys() {
                self.methods.insert(key.clone());
            }
        }

        if let Some(transform) = value.get("transform") {
            if let Some(position) = transform.get("position") {
                self.position = parse_vec3_object(position);
            }
            if let Some(rotation) = transform.get("rotation") {
                self.rotation = parse_vec3_object(rotation);
            }
            if let Some(facing) = transform
                .get("facing_target")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.facing_target = Some(facing);
            }
            if let Some(control) = transform
                .get("head_control")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.head_control = Some(control);
            }
            if let Some(rate) = transform
                .get("head_look_rate")
                .and_then(|v| v.as_f64())
                .map(|value| value as f32)
            {
                self.head_look_rate = Some(rate);
            }
        }

        if let Some(chore) = value.get("chore_state") {
            if let Some(name) = chore
                .get("last_played")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_played = Some(name);
            }
            if let Some(name) = chore
                .get("last_looping")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_looping = Some(name);
            }
            if let Some(name) = chore
                .get("last_completed")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_completed = Some(name);
            }
        }
    }

    fn apply_event(
        &mut self,
        method: &str,
        args: &[String],
        trigger: Option<HookReference>,
        hooks: &HookLookup,
    ) {
        if let Some(reference) = trigger {
            if self.created_by.is_none() {
                self.created_by = Some(format_hook_reference(&reference));
            }
            self.register_hook_reference(&reference, hooks);
        }

        self.methods.insert(method.to_string());

        let lower = method.to_ascii_lowercase();
        match lower.as_str() {
            "setpos" | "set_pos" | "set_position" => {
                if let Some(vec) = parse_vec3_args(args) {
                    self.position = Some(vec);
                }
            }
            "setrot" | "set_rot" | "set_rotation" => {
                if let Some(vec) = parse_vec3_args(args) {
                    self.rotation = Some(vec);
                }
            }
            "set_face_target" | "set_facing" | "look_at" => {
                if let Some(target) = args.first() {
                    let trimmed = target.trim();
                    if !trimmed.is_empty() && trimmed != "<expr>" {
                        self.facing_target = Some(trimmed.to_string());
                    }
                }
            }
            "head_look_at" | "head_look_at_named" => {
                if let Some(target) = args.first() {
                    let trimmed = target.trim();
                    if !trimmed.is_empty() {
                        self.head_control = Some(format!("look_at {trimmed}"));
                    }
                }
            }
            "head_look_at_point" => {
                if args.len() >= 3 {
                    self.head_control = Some(format!(
                        "look_at_point ({}, {}, {})",
                        args[0], args[1], args[2]
                    ));
                }
            }
            "set_head" => {
                if args.is_empty() {
                    self.head_control = Some("set_head".to_string());
                } else {
                    self.head_control = Some(format!("set_head {}", args.join(", ")));
                }
            }
            "set_look_rate" => {
                if let Some(value) = args.first().and_then(|arg| arg.parse::<f32>().ok()) {
                    self.head_look_rate = Some(value);
                }
            }
            "enable_head_control" => {
                let state_label = args
                    .first()
                    .map(|value| format!("enable {value}"))
                    .unwrap_or_else(|| "enable".to_string());
                self.head_control = Some(state_label);
            }
            "disable_head_control" => {
                self.head_control = Some("disable".to_string());
            }
            "play_chore" => {
                if let Some(name) = args.first() {
                    self.last_played = Some(name.clone());
                }
            }
            "play_chore_looping" => {
                if let Some(name) = args.first() {
                    self.last_looping = Some(name.clone());
                    self.last_played = Some(name.clone());
                }
            }
            "complete_chore" => {
                if let Some(name) = args.first() {
                    self.last_completed = Some(name.clone());
                }
            }
            _ => {}
        }
    }

    fn build(self) -> SceneEntity {
        SceneEntity {
            kind: self.key.kind,
            name: self.key.name,
            created_by: self.created_by,
            timeline_hook_index: self.timeline_hook_index,
            timeline_stage_index: self.timeline_stage_index,
            timeline_stage_label: self.timeline_stage_label,
            timeline_hook_name: self.timeline_hook_name,
            methods: self.methods.into_iter().collect(),
            position: self.position,
            rotation: self.rotation,
            facing_target: self.facing_target,
            head_control: self.head_control,
            head_look_rate: self.head_look_rate,
            last_played: self.last_played,
            last_looping: self.last_looping,
            last_completed: self.last_completed,
        }
    }

    fn register_hook_reference(&mut self, reference: &HookReference, hooks: &HookLookup) {
        if self.timeline_hook_index.is_none() {
            self.timeline_hook_index = hooks.find(reference);
        }
        if self.timeline_stage_index.is_none() {
            self.timeline_stage_index = reference.stage_index;
        }
        if self.timeline_stage_label.is_none() {
            self.timeline_stage_label = reference.stage_label.clone();
        }
        if self.timeline_hook_name.is_none() {
            self.timeline_hook_name = Some(reference.name().to_string());
        }
    }
}

#[derive(Debug)]
struct SceneEntity {
    kind: SceneEntityKind,
    name: String,
    created_by: Option<String>,
    timeline_hook_index: Option<usize>,
    timeline_stage_index: Option<u32>,
    timeline_stage_label: Option<String>,
    timeline_hook_name: Option<String>,
    methods: Vec<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
    facing_target: Option<String>,
    head_control: Option<String>,
    head_look_rate: Option<f32>,
    last_played: Option<String>,
    last_looping: Option<String>,
    last_completed: Option<String>,
}

impl SceneEntity {
    fn describe(&self) -> String {
        let mut method_list = self.methods.clone();
        method_list.sort();
        let methods_label = if method_list.is_empty() {
            Cow::Borrowed("no recorded methods")
        } else {
            let preview_len = method_list.len().min(5);
            let mut label = method_list[..preview_len].join(", ");
            if method_list.len() > preview_len {
                label.push_str(&format!(", +{} more", method_list.len() - preview_len));
            }
            Cow::Owned(label)
        };

        let header = format!("[{}] {}", self.kind.label(), self.name);
        match &self.created_by {
            Some(source) => format!("{header} ({methods}) <= {source}", methods = methods_label),
            None => format!("{header} ({methods})", methods = methods_label),
        }
    }
}

#[derive(Debug, Clone)]
struct SceneBounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl SceneBounds {
    fn update(&mut self, position: [f32; 3]) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(position[axis]);
            self.max[axis] = self.max[axis].max(position[axis]);
        }
    }

    fn include_bounds(&mut self, other: &SceneBounds) {
        self.update(other.min);
        self.update(other.max);
    }

    fn top_down_axes(&self) -> (usize, usize) {
        let spans = [
            (self.max[0] - self.min[0]).abs(),
            (self.max[1] - self.min[1]).abs(),
            (self.max[2] - self.min[2]).abs(),
        ];

        let span_x = spans[0];
        let span_y = spans[1];
        let span_z = spans[2];
        const EPSILON: f32 = 1e-3;

        let has_x = span_x > EPSILON;
        let has_z = span_z > EPSILON;

        if has_x && has_z {
            if span_x >= span_z {
                return (0, 2);
            }
            return (2, 0);
        }

        if has_x {
            if span_z > EPSILON {
                return (0, 2);
            }
            if span_y > EPSILON {
                return (0, 1);
            }
            return (0, 2);
        }

        if has_z {
            if span_x > EPSILON {
                return (2, 0);
            }
            if span_y > EPSILON {
                return (2, 1);
            }
            return (2, 0);
        }

        self.projection_axes()
    }

    fn projection_axes(&self) -> (usize, usize) {
        let spans = [
            (self.max[0] - self.min[0]).abs(),
            (self.max[1] - self.min[1]).abs(),
            (self.max[2] - self.min[2]).abs(),
        ];

        let mut horizontal = 0usize;
        for axis in 1..3 {
            if spans[axis] > spans[horizontal] {
                horizontal = axis;
            }
        }

        let mut vertical = (horizontal + 1) % 3;
        for axis in 0..3 {
            if axis == horizontal {
                continue;
            }
            if spans[axis] > spans[vertical] || vertical == horizontal {
                vertical = axis;
            }
        }

        (horizontal, vertical)
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::SceneBounds;

    #[test]
    fn projection_axes_prioritise_largest_spans() {
        let bounds = SceneBounds {
            min: [0.0, -2.0, 1.0],
            max: [3.0, 4.0, 1.5],
        };
        let (horizontal, vertical) = bounds.projection_axes();
        assert_eq!(horizontal, 1);
        assert_eq!(vertical, 0);
    }

    #[test]
    fn projection_axes_fall_back_when_axes_flat() {
        let bounds = SceneBounds {
            min: [1.0, 1.0, 1.0],
            max: [1.0, 2.5, 1.0],
        };
        let (horizontal, vertical) = bounds.projection_axes();
        assert_eq!(horizontal, 1);
        assert_ne!(vertical, horizontal);
    }

    #[test]
    fn top_down_axes_prefer_ground_plane() {
        let bounds = SceneBounds {
            min: [-12.0, -3.0, -1.5],
            max: [18.0, 7.0, 2.0],
        };
        let (horizontal, vertical) = bounds.top_down_axes();
        assert_eq!(horizontal, 0);
        assert_eq!(vertical, 2);
    }

    #[test]
    fn top_down_axes_fall_back_when_flat() {
        let bounds = SceneBounds {
            min: [0.0, -1.0, 0.0],
            max: [0.0, 3.0, 0.0],
        };
        let (horizontal, vertical) = bounds.top_down_axes();
        assert_ne!(horizontal, vertical);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MarkerVertex {
    position: [f32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MarkerInstance {
    translate: [f32; 2],
    size: f32,
    highlight: f32,
    color: [f32; 3],
    _padding: f32,
}

const MARKER_VERTICES: [MarkerVertex; 6] = [
    MarkerVertex {
        position: [-0.5, -0.5],
    },
    MarkerVertex {
        position: [0.5, -0.5],
    },
    MarkerVertex {
        position: [-0.5, 0.5],
    },
    MarkerVertex {
        position: [-0.5, 0.5],
    },
    MarkerVertex {
        position: [0.5, -0.5],
    },
    MarkerVertex {
        position: [0.5, 0.5],
    },
];

enum MarkerProjection<'a> {
    Perspective(&'a CameraProjector),
    TopDown {
        horizontal_axis: usize,
        vertical_axis: usize,
        horizontal_min: f32,
        vertical_min: f32,
        horizontal_span: f32,
        vertical_span: f32,
    },
    TopDownPanel {
        horizontal_axis: usize,
        vertical_axis: usize,
        horizontal_min: f32,
        vertical_min: f32,
        horizontal_span: f32,
        vertical_span: f32,
        layout: MinimapLayout,
    },
}

impl<'a> MarkerProjection<'a> {
    fn project(&self, position: [f32; 3]) -> Option<[f32; 2]> {
        match self {
            MarkerProjection::Perspective(projector) => projector.project(position),
            MarkerProjection::TopDown {
                horizontal_axis,
                vertical_axis,
                horizontal_min,
                vertical_min,
                horizontal_span,
                vertical_span,
            } => {
                let norm_h = (position[*horizontal_axis] - *horizontal_min) / *horizontal_span;
                let norm_v = (position[*vertical_axis] - *vertical_min) / *vertical_span;
                if !norm_h.is_finite() || !norm_v.is_finite() {
                    return None;
                }
                const MAP_MARGIN: f32 = 0.08;
                let clamp_h = norm_h.clamp(0.0, 1.0);
                let clamp_v = norm_v.clamp(0.0, 1.0);
                let scaled_h = MAP_MARGIN + clamp_h * (1.0 - 2.0 * MAP_MARGIN);
                let scaled_v = MAP_MARGIN + clamp_v * (1.0 - 2.0 * MAP_MARGIN);
                let ndc_x = scaled_h * 2.0 - 1.0;
                let ndc_y = 1.0 - scaled_v * 2.0;
                Some([ndc_x, ndc_y])
            }
            MarkerProjection::TopDownPanel {
                horizontal_axis,
                vertical_axis,
                horizontal_min,
                vertical_min,
                horizontal_span,
                vertical_span,
                layout,
            } => {
                let norm_h = (position[*horizontal_axis] - *horizontal_min) / *horizontal_span;
                let norm_v = (position[*vertical_axis] - *vertical_min) / *vertical_span;
                if !norm_h.is_finite() || !norm_v.is_finite() {
                    return None;
                }
                layout.project(norm_h, norm_v)
            }
        }
    }
}

const MINIMAP_CONTENT_MARGIN: f32 = 0.08;
const MINIMAP_MIN_MARKER_SIZE: f32 = 0.012;

#[derive(Clone, Copy)]
struct MinimapLayout {
    center: [f32; 2],
    half_extent_x: f32,
    half_extent_y: f32,
}

impl MinimapLayout {
    fn from_rect(rect: ViewportRect, window: PhysicalSize<u32>) -> Option<Self> {
        let width = window.width.max(1) as f32;
        let height = window.height.max(1) as f32;
        if width <= 0.0 || height <= 0.0 {
            return None;
        }

        let rect_width = rect.width.max(0.0);
        let rect_height = rect.height.max(0.0);
        if rect_width <= 0.0 || rect_height <= 0.0 {
            return None;
        }

        let center_x_px = rect.x + rect_width * 0.5;
        let center_y_px = rect.y + rect_height * 0.5;

        let center_x = (center_x_px / width) * 2.0 - 1.0;
        let center_y = 1.0 - (center_y_px / height) * 2.0;

        let half_extent_x = rect_width / width;
        let half_extent_y = rect_height / height;

        Some(Self {
            center: [center_x, center_y],
            half_extent_x,
            half_extent_y,
        })
    }

    fn panel_width(&self) -> f32 {
        self.half_extent_x * 2.0
    }

    fn panel_height(&self) -> f32 {
        self.half_extent_y * 2.0
    }

    fn scaled_size(&self, fraction: f32) -> f32 {
        (self.panel_width().min(self.panel_height()) * fraction).max(MINIMAP_MIN_MARKER_SIZE)
    }

    fn project(&self, norm_h: f32, norm_v: f32) -> Option<[f32; 2]> {
        if !norm_h.is_finite() || !norm_v.is_finite() {
            return None;
        }
        let clamp_h = norm_h.clamp(0.0, 1.0);
        let clamp_v = norm_v.clamp(0.0, 1.0);
        let usable = (1.0 - 2.0 * MINIMAP_CONTENT_MARGIN).max(0.0);
        let scaled_h = MINIMAP_CONTENT_MARGIN + clamp_h * usable;
        let scaled_v = MINIMAP_CONTENT_MARGIN + clamp_v * usable;

        let offset_x = (scaled_h - 0.5) * self.panel_width();
        let offset_y = (0.5 - scaled_v) * self.panel_height();

        Some([self.center[0] + offset_x, self.center[1] + offset_y])
    }
}

#[cfg(test)]
mod minimap_tests {
    use super::ui_layout::{MinimapConstraints, PanelKind, UiLayout};
    use super::{MarkerProjection, MinimapLayout};
    use winit::dpi::PhysicalSize;

    #[test]
    fn minimap_panel_matches_top_down_orientation() {
        let window_size = PhysicalSize::new(1280, 720);
        let ui_layout = UiLayout::new(window_size, None, None, None, MinimapConstraints::default())
            .expect("layout should resolve for standard window size");
        let minimap_rect = ui_layout
            .panel_rect(PanelKind::Minimap)
            .expect("minimap panel should exist");
        let layout = MinimapLayout::from_rect(minimap_rect, window_size)
            .expect("layout should resolve for minimap panel");

        let top_down = MarkerProjection::TopDown {
            horizontal_axis: 0,
            vertical_axis: 1,
            horizontal_min: 0.0,
            vertical_min: 0.0,
            horizontal_span: 1.0,
            vertical_span: 1.0,
        };

        let panel = MarkerProjection::TopDownPanel {
            horizontal_axis: 0,
            vertical_axis: 1,
            horizontal_min: 0.0,
            vertical_min: 0.0,
            horizontal_span: 1.0,
            vertical_span: 1.0,
            layout,
        };

        let bottom_flat = top_down
            .project([0.0, 0.0, 0.0])
            .expect("baseline projection should succeed")[1];
        let top_flat = top_down
            .project([0.0, 1.0, 0.0])
            .expect("baseline projection should succeed")[1];

        let bottom_panel = panel
            .project([0.0, 0.0, 0.0])
            .expect("panel projection should succeed")[1];
        let top_panel = panel
            .project([0.0, 1.0, 0.0])
            .expect("panel projection should succeed")[1];

        assert!(
            top_flat < bottom_flat,
            "top-down projection should place higher axes lower in clip space"
        );
        assert!(
            top_panel < bottom_panel,
            "minimap panel should preserve top-down vertical orientation"
        );
    }
}

fn load_scene_from_timeline(
    path: &Path,
    manifest_path: &Path,
    active_asset: Option<&str>,
) -> Result<ViewerScene> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading timeline manifest {}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&data)
        .with_context(|| format!("parsing timeline manifest {}", path.display()))?;

    let set_file_name = manifest
        .get("engine_state")
        .and_then(|state| state.get("set"))
        .and_then(|set| set.get("set_file"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    let timeline_summary = build_timeline_summary(&manifest)?;
    let hook_lookup = HookLookup::new(timeline_summary.as_ref());

    let mut builders: BTreeMap<SceneEntityKey, SceneEntityBuilder> = BTreeMap::new();
    let mut setup_hint: Option<String> = None;

    if let Some(engine_state) = manifest.get("engine_state") {
        if let Some(actor_map) = engine_state
            .get("replay_snapshot")
            .and_then(|replay| replay.get("actors"))
            .and_then(|actors| actors.as_object())
        {
            for (key, value) in actor_map {
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(key)
                    .to_string();
                let entry = builders
                    .entry(SceneEntityKey::new(SceneEntityKind::Actor, name.clone()))
                    .or_insert_with(|| SceneEntityBuilder::new(SceneEntityKind::Actor, name));
                entry.apply_actor_snapshot(value, &hook_lookup);
            }
        }

        if let Some(events) = engine_state
            .get("subsystem_delta_events")
            .and_then(|v| v.as_array())
        {
            for event in events {
                let subsystem = event
                    .get("subsystem")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = match event.get("target").and_then(|v| v.as_str()) {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => continue,
                };

                let kind = match subsystem {
                    "Objects" => SceneEntityKind::Object,
                    "InterestActors" => SceneEntityKind::InterestActor,
                    "Actors" => SceneEntityKind::Actor,
                    _ => continue,
                };

                let entry = builders
                    .entry(SceneEntityKey::new(kind, name.clone()))
                    .or_insert_with(|| SceneEntityBuilder::new(kind, name.clone()));

                let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let args: Vec<String> = event
                    .get("arguments")
                    .and_then(|v| v.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let trigger = event.get("triggered_by").and_then(parse_hook_reference);

                if subsystem.eq_ignore_ascii_case("Objects")
                    && name.eq_ignore_ascii_case("mo")
                    && method.eq_ignore_ascii_case("add_object_state")
                {
                    if let Some(first) = args.first() {
                        setup_hint = Some(first.clone());
                    }
                }

                entry.apply_event(method, &args, trigger, &hook_lookup);
            }
        }
    }

    let mut entities: Vec<SceneEntity> = builders
        .into_iter()
        .map(|(_, builder)| builder.build())
        .collect();
    entities.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));

    let mut bounds = None;
    for entity in &entities {
        if let Some(position) = entity.position {
            bounds
                .get_or_insert(SceneBounds {
                    min: position,
                    max: position,
                })
                .update(position);
        }
    }

    let mut scene = ViewerScene {
        entities,
        position_bounds: bounds,
        timeline: timeline_summary,
        movement: None,
        hotspot_events: Vec::new(),
        camera: None,
        active_setup: setup_hint.clone(),
    };

    if let Some(set_file) = set_file_name.as_deref() {
        match recover_camera_from_set(manifest_path, set_file, setup_hint.as_deref(), active_asset)
        {
            Ok(Some(camera)) => {
                scene.active_setup = Some(camera.name.clone());
                scene.camera = Some(camera);
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: unable to recover camera from {}: {err}",
                    set_file
                );
            }
        }
    }

    Ok(scene)
}

fn recover_camera_from_set(
    manifest_path: &Path,
    set_file_name: &str,
    setup_hint: Option<&str>,
    active_asset: Option<&str>,
) -> Result<Option<CameraParameters>> {
    let (_, set_bytes, _) = load_asset_bytes(manifest_path, set_file_name)
        .with_context(|| format!("loading set file {}", set_file_name))?;
    let set = SetFile::parse(&set_bytes)
        .with_context(|| format!("parsing set file {}", set_file_name))?;

    let mut selected_setup: Option<&Setup> = None;
    if let Some(hint) = setup_hint {
        selected_setup = set
            .setups
            .iter()
            .find(|setup| setup.name.eq_ignore_ascii_case(hint));
    }

    if selected_setup.is_none() {
        if let Some(asset) = active_asset {
            selected_setup = set.setups.iter().find(|setup| {
                setup
                    .background
                    .as_ref()
                    .map(|bg| bg.eq_ignore_ascii_case(asset))
                    .unwrap_or(false)
                    || setup
                        .zbuffer
                        .as_ref()
                        .map(|zb| zb.eq_ignore_ascii_case(asset))
                        .unwrap_or(false)
            });

            if selected_setup.is_none() {
                let lower = asset.to_ascii_lowercase();
                selected_setup = set.setups.iter().find(|setup| {
                    setup
                        .background
                        .as_ref()
                        .map(|bg| bg.to_ascii_lowercase() == lower)
                        .unwrap_or(false)
                        || setup
                            .zbuffer
                            .as_ref()
                            .map(|zb| zb.to_ascii_lowercase() == lower)
                            .unwrap_or(false)
                });
            }
        }
    }

    if selected_setup.is_none() {
        selected_setup = set.setups.first();
    }

    if let Some(setup) = selected_setup {
        if let Some(camera) = CameraParameters::from_setup(&setup.name, setup) {
            return Ok(Some(camera));
        }
    }

    Ok(None)
}

fn load_movement_trace(path: &Path) -> Result<MovementTrace> {
    let data =
        fs::read(path).with_context(|| format!("reading movement log {}", path.display()))?;
    let samples: Vec<MovementSample> = serde_json::from_slice(&data)
        .with_context(|| format!("parsing movement log {}", path.display()))?;
    MovementTrace::from_samples(samples)
        .with_context(|| format!("summarising movement trace from {}", path.display()))
}

fn load_hotspot_event_log(path: &Path) -> Result<Vec<HotspotEvent>> {
    let data =
        fs::read(path).with_context(|| format!("reading hotspot event log {}", path.display()))?;
    let log: HotspotEventLog = serde_json::from_slice(&data)
        .with_context(|| format!("parsing hotspot event log {}", path.display()))?;
    Ok(log.events)
}

fn format_hook_reference(reference: &HookReference) -> String {
    let defined_in = reference.defined_in().unwrap_or("unknown.lua");
    let line_suffix = reference
        .defined_at_line()
        .map(|line| format!(":{}", line))
        .unwrap_or_default();

    match reference.stage_label.as_deref() {
        Some(label) => format!(
            "{} @ {}{} [{}]",
            reference.name(),
            defined_in,
            line_suffix,
            label
        ),
        None => format!("{} @ {}{}", reference.name(), defined_in, line_suffix),
    }
}

fn parse_vec3_object(value: &Value) -> Option<[f32; 3]> {
    let x = value.get("x")?.as_f64()? as f32;
    let y = value.get("y")?.as_f64()? as f32;
    let z = value.get("z")?.as_f64()? as f32;
    Some([x, y, z])
}

fn parse_vec3_args(args: &[String]) -> Option<[f32; 3]> {
    if args.len() < 3 {
        return None;
    }
    let mut values = [0.0f32; 3];
    for (idx, slot) in values.iter_mut().enumerate() {
        let value = args[idx].trim();
        if value == "<expr>" {
            return None;
        }
        *slot = parse_f32(value)?;
    }
    Some(values)
}

fn parse_f32(value: &str) -> Option<f32> {
    let trimmed = value.trim().trim_matches('"');
    trimmed.parse::<f32>().ok()
}

fn load_layout_preset(path: &Path) -> Result<LayoutPreset> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("reading layout preset {}", path.display()))?;
    let preset: LayoutPreset = serde_json::from_str(&data)
        .with_context(|| format!("parsing layout preset {}", path.display()))?;
    Ok(preset)
}

fn load_asset_bytes(manifest_path: &Path, asset: &str) -> Result<(String, Vec<u8>, PathBuf)> {
    let data = std::fs::read(manifest_path)
        .with_context(|| format!("reading asset manifest {}", manifest_path.display()))?;
    let manifest: AssetManifest = serde_json::from_slice(&data)
        .with_context(|| format!("parsing asset manifest {}", manifest_path.display()))?;

    let entry = manifest
        .found
        .into_iter()
        .find(|entry| entry.asset_name.eq_ignore_ascii_case(asset))
        .ok_or_else(|| {
            anyhow!(
                "asset '{}' not listed in manifest {}",
                asset,
                manifest_path.display()
            )
        })?;

    if let Some(AssetMetadataSummary::Bitmap {
        codec, supported, ..
    }) = &entry.metadata
    {
        if !supported {
            bail!(
                "asset '{}' (codec {}) is not yet supported by the viewer; pick a classic-surface entry",
                entry.asset_name,
                codec
            );
        }
    }

    let archive_path = resolve_archive_path(manifest_path, &entry.archive_path);
    let bytes = read_asset_slice(&archive_path, entry.offset, entry.size).with_context(|| {
        format!(
            "reading {} from {}",
            entry.asset_name,
            archive_path.display()
        )
    })?;

    Ok((entry.asset_name, bytes, archive_path))
}

fn load_zbm_seed(manifest_path: &Path, asset: &str) -> Result<Option<BmFile>> {
    let lower = asset.to_ascii_lowercase();
    if !lower.ends_with(".zbm") || asset.len() <= 4 {
        return Ok(None);
    }

    let base_name = format!("{}{}", &asset[..asset.len() - 4], ".bm");
    match load_asset_bytes(manifest_path, &base_name) {
        Ok((base_asset, base_bytes, _)) => {
            let base_bm = decode_bm(&base_bytes)
                .with_context(|| format!("decoding base bitmap {} for {}", base_asset, asset))?;
            ensure!(
                !base_bm.frames.is_empty(),
                "base bitmap {} has no frames",
                base_asset
            );
            Ok(Some(base_bm))
        }
        Err(err) => {
            if err.to_string().contains("not listed in manifest") {
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

fn resolve_archive_path(manifest_path: &Path, archive_path: &Path) -> PathBuf {
    if archive_path.is_absolute() {
        return archive_path.to_path_buf();
    }

    let from_manifest = manifest_path
        .parent()
        .map(|parent| parent.join(archive_path))
        .unwrap_or_else(|| archive_path.to_path_buf());
    if from_manifest.exists() {
        return from_manifest;
    }

    if archive_path.exists() {
        return archive_path.to_path_buf();
    }

    from_manifest
}

fn read_asset_slice(path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("seeking to 0x{:X} in {}", offset, path.display()))?;

    let mut buffer = vec![0u8; size as usize];
    file.read_exact(&mut buffer)
        .with_context(|| format!("reading {} bytes from {}", size, path.display()))?;
    Ok(buffer)
}

#[derive(Clone, Copy)]
struct OverlayConfig {
    width: u32,
    height: u32,
    padding_x: u32,
    padding_y: u32,
    label: &'static str,
}

impl From<&OverlayConfig> for PanelSize {
    fn from(config: &OverlayConfig) -> Self {
        PanelSize {
            width: config.width as f32,
            height: config.height as f32,
        }
    }
}

impl OverlayConfig {
    fn with_preset(mut self, preset: Option<&PanelPreset>) -> Self {
        if let Some(preset) = preset {
            if let Some(width) = preset.width {
                self.width = width;
            }
            if let Some(height) = preset.height {
                self.height = height;
            }
            if let Some(padding_x) = preset.padding_x {
                self.padding_x = padding_x;
            }
            if let Some(padding_y) = preset.padding_y {
                self.padding_y = padding_y;
            }
        }
        self
    }
}

struct TextOverlay {
    texture: wgpu::Texture,
    _view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padding_x: u32,
    padding_y: u32,
    pixels: Vec<u8>,
    dirty: bool,
    visible: bool,
    label: &'static str,
    layout_rect: ViewportRect,
}

impl TextOverlay {
    const GLYPH_WIDTH: u32 = 8;
    const GLYPH_HEIGHT: u32 = 8;
    const FG_COLOR: [u8; 4] = [255, 255, 255, 240];
    const BG_COLOR: [u8; 4] = [0, 0, 0, 96];

    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        window_size: PhysicalSize<u32>,
        config: OverlayConfig,
    ) -> Result<Self> {
        let extent = wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        };
        let texture_label = format!("{}-texture", config.label);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(texture_label.as_str()),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler_label = format!("{}-sampler", config.label);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(sampler_label.as_str()),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_label = format!("{}-bind-group", config.label);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(bind_group_label.as_str()),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let mut pixels = vec![0u8; (config.width * config.height * 4) as usize];
        Self::fill_background(&mut pixels);

        let upload = prepare_rgba_upload(config.width, config.height, &pixels)?;
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload.pixels(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(upload.bytes_per_row()),
                rows_per_image: Some(config.height),
            },
            extent,
        );

        let initial_rect = ViewportRect {
            x: 0.0,
            y: 0.0,
            width: config.width as f32,
            height: config.height as f32,
        };

        let vertex_buffer = {
            let vertices = Self::vertex_positions(initial_rect, window_size);
            let vertex_label = format!("{}-vertices", config.label);
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(vertex_label.as_str()),
                contents: cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };

        Ok(Self {
            texture,
            _view: texture_view,
            _sampler: sampler,
            bind_group,
            vertex_buffer,
            width: config.width,
            height: config.height,
            padding_x: config.padding_x,
            padding_y: config.padding_y,
            pixels,
            dirty: false,
            visible: false,
            label: config.label,
            layout_rect: initial_rect,
        })
    }

    fn fill_background(pixels: &mut [u8]) {
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.copy_from_slice(&Self::BG_COLOR);
        }
    }

    fn create_vertex_buffer(
        device: &wgpu::Device,
        window_size: PhysicalSize<u32>,
        rect: ViewportRect,
        label: &str,
    ) -> wgpu::Buffer {
        let vertices = Self::vertex_positions(rect, window_size);
        let label = format!("{}-vertices", label);
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label.as_str()),
            contents: cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        })
    }

    fn vertex_positions(rect: ViewportRect, window_size: PhysicalSize<u32>) -> [QuadVertex; 4] {
        let win_width = window_size.width.max(1) as f32;
        let win_height = window_size.height.max(1) as f32;
        let left = (rect.x / win_width) * 2.0 - 1.0;
        let right = ((rect.x + rect.width) / win_width) * 2.0 - 1.0;
        let top = 1.0 - (rect.y / win_height) * 2.0;
        let bottom = 1.0 - ((rect.y + rect.height) / win_height) * 2.0;

        [
            QuadVertex {
                position: [left, top],
                uv: [0.0, 0.0],
            },
            QuadVertex {
                position: [right, top],
                uv: [1.0, 0.0],
            },
            QuadVertex {
                position: [left, bottom],
                uv: [0.0, 1.0],
            },
            QuadVertex {
                position: [right, bottom],
                uv: [1.0, 1.0],
            },
        ]
    }

    fn update_layout(
        &mut self,
        device: &wgpu::Device,
        window_size: PhysicalSize<u32>,
        rect: ViewportRect,
    ) {
        self.layout_rect = rect;
        self.vertex_buffer = Self::create_vertex_buffer(device, window_size, rect, self.label);
    }

    fn set_lines(&mut self, lines: &[String]) {
        Self::fill_background(&mut self.pixels);

        let usable_width = self.width.saturating_sub(self.padding_x * 2);
        let usable_height = self.height.saturating_sub(self.padding_y * 2);
        if usable_width == 0 || usable_height == 0 {
            self.dirty = true;
            self.visible = !lines.is_empty();
            return;
        }

        let max_cols = (usable_width / Self::GLYPH_WIDTH) as usize;
        let max_rows = (usable_height / Self::GLYPH_HEIGHT) as usize;

        if max_cols == 0 || max_rows == 0 {
            self.dirty = true;
            self.visible = !lines.is_empty();
            return;
        }

        for (row_idx, line) in lines.iter().take(max_rows).enumerate() {
            let glyph_row = self.padding_y + row_idx as u32 * Self::GLYPH_HEIGHT;
            for (col_idx, ch) in line.chars().take(max_cols).enumerate() {
                let glyph = glyph_for_char(ch);
                let glyph_col = self.padding_x + col_idx as u32 * Self::GLYPH_WIDTH;
                for (y_offset, bits) in glyph.iter().enumerate() {
                    let y = glyph_row + y_offset as u32;
                    if y >= self.height {
                        continue;
                    }
                    for x_bit in 0..Self::GLYPH_WIDTH {
                        if (bits >> x_bit) & 0x01 == 0 {
                            continue;
                        }
                        let x = glyph_col + x_bit;
                        if x >= self.width {
                            continue;
                        }
                        let idx = ((y * self.width + x) * 4) as usize;
                        self.pixels[idx..idx + 4].copy_from_slice(&Self::FG_COLOR);
                    }
                }
            }
        }

        self.dirty = true;
        self.visible = !lines.is_empty();
    }

    fn upload(&mut self, queue: &wgpu::Queue) {
        if !self.dirty {
            return;
        }
        let upload = match prepare_rgba_upload(self.width, self.height, &self.pixels) {
            Ok(upload) => upload,
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: overlay upload failed ({}x{}): {err}",
                    self.width, self.height
                );
                return;
            }
        };
        let extent = wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: 1,
        };
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload.pixels(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(upload.bytes_per_row()),
                rows_per_image: Some(self.height),
            },
            extent,
        );
        self.dirty = false;
    }

    fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    fn vertex_buffer(&self) -> &wgpu::Buffer {
        &self.vertex_buffer
    }

    fn is_visible(&self) -> bool {
        self.visible
    }
}

fn glyph_for_char(ch: char) -> [u8; 8] {
    let index = ch as usize;
    if index < BASIC_LEGACY.len() {
        BASIC_LEGACY[index]
    } else {
        BASIC_LEGACY[b'?' as usize]
    }
}

struct ViewerState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,
    quad_index_buffer: wgpu::Buffer,
    quad_index_count: u32,
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
    _texture_view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
    audio_overlay: Option<TextOverlay>,
    timeline_overlay: Option<TextOverlay>,
    scrubber_overlay: Option<TextOverlay>,
    background: wgpu::Color,
    scene: Option<Arc<ViewerScene>>,
    selected_entity: Option<usize>,
    scrubber: Option<MovementScrubber>,
    camera_projector: Option<CameraProjector>,
    marker_pipeline: wgpu::RenderPipeline,
    marker_vertex_buffer: wgpu::Buffer,
    marker_instance_buffer: wgpu::Buffer,
    marker_capacity: usize,
    ui_layout: UiLayout,
}

impl ViewerState {
    async fn new(
        window: Arc<Window>,
        asset_name: &str,
        asset_bytes: Vec<u8>,
        decode_result: Result<PreviewTexture>,
        scene: Option<Arc<ViewerScene>>,
        enable_audio_overlay: bool,
        layout_preset: Option<LayoutPreset>,
    ) -> Result<Self> {
        let size = window.inner_size();

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .context("creating wgpu surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .context("requesting wgpu adapter")?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("grim-viewer-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .context("requesting wgpu device")?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let present_mode = surface_caps
            .present_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::PresentMode::Mailbox)
            .or(Some(wgpu::PresentMode::Fifo))
            .unwrap_or(wgpu::PresentMode::Fifo);
        let alpha_mode = surface_caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Opaque);

        let (preview, background) = match decode_result {
            Ok(texture) => {
                println!(
                    "Decoded BM frame: {}x{} ({} frames, codec {}, format {})",
                    texture.width,
                    texture.height,
                    texture.frame_count,
                    texture.codec,
                    texture.format
                );
                if let Some(stats) = texture.depth_stats {
                    println!(
                        "  depth range (raw 16-bit): 0x{min:04X} – 0x{max:04X}",
                        min = stats.min,
                        max = stats.max
                    );
                    println!(
                        "  depth pixels zero {zero} / {total}",
                        zero = stats.zero_pixels,
                        total = stats.total_pixels()
                    );
                    if texture.depth_preview {
                        println!("  preview mapped to normalized depth values");
                    } else {
                        println!("  preview uses paired base bitmap for RGB");
                    }
                }
                (texture, wgpu::Color::BLACK)
            }
            Err(err) => {
                eprintln!("[grim_viewer] falling back to placeholder texture: {err:?}");
                let placeholder = generate_placeholder_texture(&asset_bytes, asset_name);
                let color = preview_color(&asset_bytes);
                (placeholder, color)
            }
        };
        let texture_width = preview.width;
        let texture_height = preview.height;
        let texture_aspect = (texture_width.max(1) as f32) / (texture_height.max(1) as f32);
        let camera_projector = scene
            .as_ref()
            .and_then(|scene| scene.camera_projector(texture_aspect));
        if let Some(scene_ref) = scene.as_ref() {
            if let Some(setup) = scene_ref.active_setup() {
                println!("[grim_viewer] active camera setup: {}", setup);
            }
            if let Some(camera) = scene_ref.camera.as_ref() {
                println!(
                    "  camera eye ({:.3}, {:.3}, {:.3}) interest ({:.3}, {:.3}, {:.3}) fov {:.2} roll {:.2}",
                    camera.position[0],
                    camera.position[1],
                    camera.position[2],
                    camera.interest[0],
                    camera.interest[1],
                    camera.interest[2],
                    camera.fov_degrees,
                    camera.roll_degrees
                );
            }
        }
        let texture_extent = wgpu::Extent3d {
            width: texture_width,
            height: texture_height,
            depth_or_array_layers: 1,
        };

        println!(
            "Preview texture sized {}x{} ({} bytes of source)",
            texture_width,
            texture_height,
            asset_bytes.len()
        );
        println!("Preview RGBA buffer len {}", preview.data.len());

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("asset-texture"),
            size: texture_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let upload = prepare_rgba_upload(texture_width, texture_height, &preview.data)?;
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload.pixels(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(upload.bytes_per_row()),
                rows_per_image: Some(texture_height),
            },
            texture_extent,
        );
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("asset-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("asset-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("asset-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("asset-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
        });

        let layout_preset = layout_preset.unwrap_or_default();
        let audio_preset = layout_preset.audio.as_ref();
        let scrubber_preset = layout_preset.scrubber.as_ref();
        let timeline_preset = layout_preset.timeline.as_ref();
        let minimap_preset = layout_preset.minimap.as_ref();

        let audio_enabled =
            enable_audio_overlay && audio_preset.map(PanelPreset::enabled).unwrap_or(true);
        let (audio_overlay, audio_panel) = if audio_enabled {
            let config = OverlayConfig {
                width: 520,
                height: 144,
                padding_x: 8,
                padding_y: 8,
                label: "audio-overlay",
            }
            .with_preset(audio_preset);
            let panel = PanelSize::from(&config);
            let overlay = TextOverlay::new(&device, &queue, &bind_group_layout, size, config)?;
            (Some(overlay), Some(panel))
        } else {
            (None, None)
        };

        let scrubber = scene
            .as_ref()
            .and_then(|scene| MovementScrubber::new(scene));

        let scrubber_available = scrubber.is_some();
        let scrubber_enabled =
            scrubber_available && scrubber_preset.map(PanelPreset::enabled).unwrap_or(true);
        let (scrubber_overlay, scrubber_panel) = if scrubber_enabled {
            let config = OverlayConfig {
                width: 520,
                height: 176,
                padding_x: 8,
                padding_y: 8,
                label: "scrubber-overlay",
            }
            .with_preset(scrubber_preset);
            let panel = PanelSize::from(&config);
            let overlay = TextOverlay::new(&device, &queue, &bind_group_layout, size, config)?;
            (Some(overlay), Some(panel))
        } else {
            (None, None)
        };

        let timeline_available = scene
            .as_ref()
            .and_then(|scene| scene.timeline.as_ref())
            .is_some();

        let timeline_enabled =
            timeline_available && timeline_preset.map(PanelPreset::enabled).unwrap_or(true);
        let (timeline_overlay, timeline_panel) = if timeline_enabled {
            let config = OverlayConfig {
                width: 640,
                height: 224,
                padding_x: 8,
                padding_y: 8,
                label: "timeline-overlay",
            }
            .with_preset(timeline_preset);
            let panel = PanelSize::from(&config);
            let overlay = TextOverlay::new(&device, &queue, &bind_group_layout, size, config)?;
            (Some(overlay), Some(panel))
        } else {
            (None, None)
        };

        let mut minimap_constraints = MinimapConstraints::default();
        if let Some(preset) = minimap_preset {
            if let Some(min_side) = preset.min_side {
                minimap_constraints.min_side = min_side;
            }
            if let Some(preferred_fraction) = preset.preferred_fraction {
                minimap_constraints.preferred_fraction = preferred_fraction;
            }
            if let Some(max_fraction) = preset.max_fraction {
                minimap_constraints.max_fraction = max_fraction;
            }
        }
        let ui_layout = UiLayout::new(
            size,
            audio_panel,
            timeline_panel,
            scrubber_panel,
            minimap_constraints,
        )?;

        let quad_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<QuadVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
        };

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("asset-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("asset-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[quad_vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let quad_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("asset-quad-vertex-buffer"),
            contents: cast_slice(&QUAD_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let quad_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("asset-quad-index-buffer"),
            contents: cast_slice(&QUAD_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let quad_index_count = QUAD_INDICES.len() as u32;

        let marker_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("marker-vertex-buffer"),
            contents: cast_slice(&MARKER_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let initial_marker_capacity = 4usize;
        let marker_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("marker-instance-buffer"),
            size: (initial_marker_capacity * std::mem::size_of::<MarkerInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let marker_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MarkerVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2],
        };

        let marker_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MarkerInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32,
                },
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x3,
                },
            ],
        };

        let marker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("marker-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MARKER_SHADER_SOURCE)),
        });

        let marker_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("marker-pipeline-layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        let marker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("marker-pipeline"),
            layout: Some(&marker_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &marker_shader,
                entry_point: "vs_main",
                buffers: &[marker_vertex_layout, marker_instance_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &marker_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let selected_entity = scene.as_ref().and_then(|scene| {
            if scene.entities.is_empty() {
                None
            } else {
                Some(
                    scene
                        .entities
                        .iter()
                        .enumerate()
                        .find(|(_, entity)| entity.position.is_some())
                        .map(|(idx, _)| idx)
                        .unwrap_or(0),
                )
            }
        });

        let mut state = Self {
            window,
            surface,
            device,
            queue,
            config: wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode,
                alpha_mode,
                view_formats: vec![],
                desired_maximum_frame_latency: 1,
            },
            size,
            pipeline,
            quad_vertex_buffer,
            quad_index_buffer,
            quad_index_count,
            bind_group,
            _texture: texture,
            _texture_view: texture_view,
            _sampler: sampler,
            audio_overlay,
            timeline_overlay,
            scrubber_overlay,
            background,
            scene: scene.clone(),
            selected_entity,
            scrubber,
            camera_projector,
            marker_pipeline,
            marker_vertex_buffer,
            marker_instance_buffer,
            marker_capacity: initial_marker_capacity,
            ui_layout,
        };

        state.surface.configure(&state.device, &state.config);
        state.print_selected_entity();
        state.refresh_timeline_overlay();
        state.refresh_scrubber_overlay();
        state.apply_panel_layouts();

        Ok(state)
    }

    fn window(&self) -> &Window {
        self.window.as_ref()
    }

    fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    fn apply_panel_layouts(&mut self) {
        let window_size = self.size;
        if let Some(overlay) = self.audio_overlay.as_mut() {
            if let Some(rect) = self.ui_layout.panel_rect(PanelKind::Audio) {
                overlay.update_layout(&self.device, window_size, rect);
            }
        }
        if let Some(overlay) = self.timeline_overlay.as_mut() {
            if let Some(rect) = self.ui_layout.panel_rect(PanelKind::Timeline) {
                overlay.update_layout(&self.device, window_size, rect);
            }
        }
        if let Some(overlay) = self.scrubber_overlay.as_mut() {
            if let Some(rect) = self.ui_layout.panel_rect(PanelKind::Scrubber) {
                overlay.update_layout(&self.device, window_size, rect);
            }
        }
    }

    fn minimap_layout(&self) -> Option<MinimapLayout> {
        let rect = self.ui_layout.panel_rect(PanelKind::Minimap)?;
        MinimapLayout::from_rect(rect, self.size)
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            match self.ui_layout.set_window_size(new_size) {
                Ok(()) => self.apply_panel_layouts(),
                Err(err) => eprintln!("[grim_viewer] layout resize failed: {err:?}"),
            }
        }
    }

    fn render(&mut self) -> Result<(), SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("grim-viewer-encoder"),
            });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim-viewer-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &self.bind_group, &[]);
            rpass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            rpass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            rpass.draw_indexed(0..self.quad_index_count, 0, 0..1);
        }

        let marker_instances = self.build_marker_instances();
        if !marker_instances.is_empty() {
            self.ensure_marker_capacity(marker_instances.len());
            self.queue.write_buffer(
                &self.marker_instance_buffer,
                0,
                cast_slice(&marker_instances),
            );

            let mut marker_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("marker-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            marker_pass.set_pipeline(&self.marker_pipeline);
            marker_pass.set_vertex_buffer(0, self.marker_vertex_buffer.slice(..));
            let instance_byte_len =
                (marker_instances.len() * std::mem::size_of::<MarkerInstance>()) as u64;
            marker_pass
                .set_vertex_buffer(1, self.marker_instance_buffer.slice(0..instance_byte_len));
            marker_pass.draw(
                0..MARKER_VERTICES.len() as u32,
                0..marker_instances.len() as u32,
            );
        }

        if let Some(minimap_instances) = self.build_minimap_instances() {
            if !minimap_instances.is_empty() {
                self.ensure_marker_capacity(minimap_instances.len());
                self.queue.write_buffer(
                    &self.marker_instance_buffer,
                    0,
                    cast_slice(&minimap_instances),
                );

                let mut minimap_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("minimap-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                minimap_pass.set_pipeline(&self.marker_pipeline);
                minimap_pass.set_vertex_buffer(0, self.marker_vertex_buffer.slice(..));
                let minimap_byte_len =
                    (minimap_instances.len() * std::mem::size_of::<MarkerInstance>()) as u64;
                minimap_pass
                    .set_vertex_buffer(1, self.marker_instance_buffer.slice(0..minimap_byte_len));
                minimap_pass.draw(
                    0..MARKER_VERTICES.len() as u32,
                    0..minimap_instances.len() as u32,
                );
            }
        }

        if let Some(overlay) = self.audio_overlay.as_mut() {
            overlay.upload(&self.queue);
        }
        if let Some(overlay) = self.audio_overlay.as_ref() {
            self.draw_overlay(&mut encoder, &view, overlay, "audio-overlay-pass");
        }

        if let Some(overlay) = self.timeline_overlay.as_mut() {
            overlay.upload(&self.queue);
        }
        if let Some(overlay) = self.timeline_overlay.as_ref() {
            self.draw_overlay(&mut encoder, &view, overlay, "timeline-overlay-pass");
        }

        if let Some(overlay) = self.scrubber_overlay.as_mut() {
            overlay.upload(&self.queue);
        }
        if let Some(overlay) = self.scrubber_overlay.as_ref() {
            self.draw_overlay(&mut encoder, &view, overlay, "scrubber-overlay-pass");
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    fn draw_overlay(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        overlay: &TextOverlay,
        label: &'static str,
    ) {
        if !overlay.is_visible() {
            return;
        }
        let mut overlay_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        overlay_pass.set_pipeline(&self.pipeline);
        overlay_pass.set_bind_group(0, overlay.bind_group(), &[]);
        overlay_pass.set_vertex_buffer(0, overlay.vertex_buffer().slice(..));
        overlay_pass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        overlay_pass.draw_indexed(0..self.quad_index_count, 0, 0..1);
    }

    fn update_audio_overlay(&mut self, status: &AudioStatus) {
        if let Some(overlay) = self.audio_overlay.as_mut() {
            let lines = audio_overlay_lines(status);
            overlay.set_lines(&lines);
        }
    }

    fn refresh_timeline_overlay(&mut self) {
        if let Some(overlay) = self.timeline_overlay.as_mut() {
            let scene = self.scene.as_deref();
            let lines = timeline_overlay_lines(scene, self.selected_entity);
            overlay.set_lines(&lines);
        }
    }

    fn refresh_scrubber_overlay(&mut self) {
        if let Some(overlay) = self.scrubber_overlay.as_mut() {
            if let (Some(scrubber), Some(scene)) = (self.scrubber.as_ref(), self.scene.as_deref()) {
                if let Some(trace) = scene.movement_trace() {
                    let lines = scrubber.overlay_lines(trace);
                    overlay.set_lines(&lines);
                    return;
                }
            }
            overlay.set_lines(&[]);
        }
    }

    fn next_entity(&mut self) {
        if let Some(scene) = self.scene.as_ref() {
            if scene.entities.is_empty() {
                return;
            }
            let next = match self.selected_entity {
                Some(idx) => (idx + 1) % scene.entities.len(),
                None => 0,
            };
            self.selected_entity = Some(next);
            self.print_selected_entity();
            self.refresh_timeline_overlay();
        }
    }

    fn previous_entity(&mut self) {
        if let Some(scene) = self.scene.as_ref() {
            if scene.entities.is_empty() {
                return;
            }
            let prev = match self.selected_entity {
                Some(0) | None => scene.entities.len().saturating_sub(1),
                Some(idx) => idx.saturating_sub(1),
            };
            self.selected_entity = Some(prev);
            self.print_selected_entity();
            self.refresh_timeline_overlay();
        }
    }

    fn scrub_step(&mut self, delta: i32) {
        if let Some(scrubber) = self.scrubber.as_mut() {
            let changed = scrubber.step(delta);
            if self.scrubber_overlay.is_some() {
                self.refresh_scrubber_overlay();
            }
            if changed {
                self.window().request_redraw();
            }
        }
    }

    fn scrub_jump_to_head_target(&mut self, direction: i32) {
        if let Some(scrubber) = self.scrubber.as_mut() {
            let changed = scrubber.jump_to_head_target(direction);
            if self.scrubber_overlay.is_some() {
                self.refresh_scrubber_overlay();
            }
            if changed {
                self.window().request_redraw();
            }
        }
    }

    fn handle_character_input(&mut self, key: &str) {
        match key {
            "]" => self.scrub_step(1),
            "[" => self.scrub_step(-1),
            "}" => self.scrub_jump_to_head_target(1),
            "{" => self.scrub_jump_to_head_target(-1),
            _ => {}
        }
    }

    fn print_selected_entity(&self) {
        if let (Some(scene), Some(idx)) = (self.scene.as_ref(), self.selected_entity) {
            if let Some(entity) = scene.entities.get(idx) {
                println!("[grim_viewer] selected entity: {}", entity.describe());
                if let Some(position) = entity.position {
                    println!(
                        "    position: ({:.3}, {:.3}, {:.3})",
                        position[0], position[1], position[2]
                    );
                }
                if let Some(rotation) = entity.rotation {
                    println!(
                        "    rotation: ({:.3}, {:.3}, {:.3})",
                        rotation[0], rotation[1], rotation[2]
                    );
                }
                if let Some(target) = &entity.facing_target {
                    println!("    facing target: {target}");
                }
                if let Some(control) = &entity.head_control {
                    println!("    head control: {control}");
                }
                if let Some(rate) = entity.head_look_rate {
                    println!("    head look rate: {rate:.3}");
                }
                if entity.last_played.is_some()
                    || entity.last_looping.is_some()
                    || entity.last_completed.is_some()
                {
                    let played = entity.last_played.as_deref().unwrap_or("-");
                    let looping = entity.last_looping.as_deref().unwrap_or("-");
                    let completed = entity.last_completed.as_deref().unwrap_or("-");
                    println!(
                        "    chore state: played={}, looping={}, completed={}",
                        played, looping, completed
                    );
                }
                if entity.name.eq_ignore_ascii_case("manny") {
                    if let Some(scene) = self.scene.as_ref() {
                        if let Some(trace) = scene.movement_trace() {
                            println!(
                                "    movement: {} samples (frames {}-{}) distance {:.3}",
                                trace.sample_count(),
                                trace.first_frame,
                                trace.last_frame,
                                trace.total_distance
                            );
                        }
                    }
                }
            }
        }
    }

    fn build_marker_instances(&self) -> Vec<MarkerInstance> {
        let mut instances = Vec::new();

        let scene = match self.scene.as_ref() {
            Some(scene) => scene,
            None => return instances,
        };

        let projection = if let Some(projector) = self.camera_projector.as_ref() {
            MarkerProjection::Perspective(projector)
        } else {
            let bounds = match scene.position_bounds.as_ref() {
                Some(bounds) => bounds,
                None => return instances,
            };
            let (horizontal_axis, vertical_axis) = bounds.top_down_axes();
            let horizontal_min = bounds.min[horizontal_axis];
            let vertical_min = bounds.min[vertical_axis];
            let horizontal_span = (bounds.max[horizontal_axis] - horizontal_min).max(0.001);
            let vertical_span = (bounds.max[vertical_axis] - vertical_min).max(0.001);
            MarkerProjection::TopDown {
                horizontal_axis,
                vertical_axis,
                horizontal_min,
                vertical_min,
                horizontal_span,
                vertical_span,
            }
        };

        let selected = self.selected_entity;

        let mut push_marker = |position: [f32; 3], size: f32, color: [f32; 3], highlight: f32| {
            if let Some([ndc_x, ndc_y]) = projection.project(position) {
                if !ndc_x.is_finite() || !ndc_y.is_finite() {
                    return;
                }
                instances.push(MarkerInstance {
                    translate: [ndc_x, ndc_y],
                    size,
                    highlight,
                    color,
                    _padding: 0.0,
                });
            }
        };

        if let Some(trace) = scene.movement_trace() {
            if !trace.samples.is_empty() {
                let limit = 96_usize;
                let step = (trace.samples.len().max(limit) / limit).max(1);
                let path_color = [0.78, 0.58, 0.95];
                let start_color = [0.32, 0.74, 0.86];
                let end_color = [0.95, 0.55, 0.42];
                let path_size = 0.032;
                let start_size = 0.044;
                let end_size = 0.045;

                let (scrub_position, highlight_event_scene_index) = match self.scrubber.as_ref() {
                    Some(scrubber) => (
                        scrubber.current_position(trace),
                        scrubber.highlighted_event().map(|event| event.scene_index),
                    ),
                    None => (None, None),
                };

                if let Some(first) = trace.samples.first() {
                    push_marker(first.position, start_size, start_color, 0.0);
                }

                let len = trace.samples.len();
                for (idx, sample) in trace.samples.iter().enumerate().step_by(step) {
                    if idx == 0 || idx + 1 == len {
                        continue;
                    }
                    push_marker(sample.position, path_size, path_color, 0.0);
                }

                if trace.samples.len() > 1 {
                    if let Some(last) = trace.samples.last() {
                        push_marker(last.position, end_size, end_color, 0.0);
                    }
                }

                for (idx, event) in scene.hotspot_events().iter().enumerate() {
                    let frame = match event.frame {
                        Some(frame) => frame,
                        None => continue,
                    };
                    let position = match trace.nearest_position(frame) {
                        Some(pos) => pos,
                        None => continue,
                    };
                    let (mut marker_size, mut marker_color, mut marker_highlight) =
                        event_marker_style(event.kind());
                    if Some(idx) == highlight_event_scene_index {
                        marker_highlight = marker_highlight.max(0.9);
                        marker_color = [0.98, 0.93, 0.32];
                        marker_size *= 1.08;
                    }
                    push_marker(position, marker_size, marker_color, marker_highlight);
                }

                if let Some(position) = scrub_position {
                    push_marker(position, 0.058, [0.2, 0.95, 0.85], 1.0);
                }
            }
        }

        for (idx, entity) in scene.entities.iter().enumerate() {
            let position = match entity.position {
                Some(pos) => pos,
                None => continue,
            };

            let is_selected = matches!(selected, Some(sel) if sel == idx);
            let base_size = match entity.kind {
                SceneEntityKind::Actor => 0.06,
                SceneEntityKind::Object => 0.05,
                SceneEntityKind::InterestActor => 0.045,
            };
            let size = if is_selected {
                base_size * 1.2
            } else {
                base_size
            };
            let color = if is_selected {
                [0.95, 0.35, 0.25]
            } else {
                match entity.kind {
                    SceneEntityKind::Actor => [0.2, 0.85, 0.6],
                    SceneEntityKind::Object => [0.25, 0.6, 0.95],
                    SceneEntityKind::InterestActor => [0.85, 0.7, 0.25],
                }
            };
            let highlight = if is_selected { 1.0 } else { 0.0 };
            push_marker(position, size, color, highlight);
        }

        instances
    }

    fn build_minimap_instances(&self) -> Option<Vec<MarkerInstance>> {
        let scene = self.scene.as_ref()?;
        let bounds = scene.position_bounds.as_ref()?;
        let layout = self.minimap_layout()?;

        let (horizontal_axis, vertical_axis) = bounds.top_down_axes();
        let horizontal_min = bounds.min[horizontal_axis];
        let vertical_min = bounds.min[vertical_axis];
        let horizontal_span = (bounds.max[horizontal_axis] - horizontal_min).max(0.001);
        let vertical_span = (bounds.max[vertical_axis] - vertical_min).max(0.001);

        let projection = MarkerProjection::TopDownPanel {
            horizontal_axis,
            vertical_axis,
            horizontal_min,
            vertical_min,
            horizontal_span,
            vertical_span,
            layout,
        };

        let mut instances = Vec::new();
        instances.push(MarkerInstance {
            translate: layout.center,
            size: layout.panel_width(),
            highlight: 0.0,
            color: [0.07, 0.08, 0.12],
            _padding: 0.0,
        });

        let mut push_marker = |position: [f32; 3], size: f32, color: [f32; 3], highlight: f32| {
            if let Some([ndc_x, ndc_y]) = projection.project(position) {
                if !ndc_x.is_finite() || !ndc_y.is_finite() {
                    return;
                }
                instances.push(MarkerInstance {
                    translate: [ndc_x, ndc_y],
                    size,
                    highlight,
                    color,
                    _padding: 0.0,
                });
            }
        };

        let scale_size = |base: f32| layout.scaled_size(base * 0.5);

        if let Some(trace) = scene.movement_trace() {
            if !trace.samples.is_empty() {
                let limit = 96_usize;
                let step = (trace.samples.len().max(limit) / limit).max(1);
                let path_color = [0.75, 0.65, 0.95];
                let start_color = [0.35, 0.78, 0.88];
                let end_color = [0.98, 0.58, 0.42];
                let path_size = scale_size(0.032);
                let start_size = scale_size(0.044);
                let end_size = scale_size(0.045);

                let (scrub_position, highlight_event_scene_index) = match self.scrubber.as_ref() {
                    Some(scrubber) => (
                        scrubber.current_position(trace),
                        scrubber.highlighted_event().map(|event| event.scene_index),
                    ),
                    None => (None, None),
                };

                if let Some(first) = trace.samples.first() {
                    push_marker(first.position, start_size, start_color, 0.0);
                }

                let len = trace.samples.len();
                for (idx, sample) in trace.samples.iter().enumerate().step_by(step) {
                    if idx == 0 || idx + 1 == len {
                        continue;
                    }
                    push_marker(sample.position, path_size, path_color, 0.0);
                }

                if trace.samples.len() > 1 {
                    if let Some(last) = trace.samples.last() {
                        push_marker(last.position, end_size, end_color, 0.0);
                    }
                }

                for (idx, event) in scene.hotspot_events().iter().enumerate() {
                    let frame = match event.frame {
                        Some(frame) => frame,
                        None => continue,
                    };
                    let position = match trace.nearest_position(frame) {
                        Some(pos) => pos,
                        None => continue,
                    };
                    let (mut marker_size, mut marker_color, mut marker_highlight) =
                        event_marker_style(event.kind());
                    if Some(idx) == highlight_event_scene_index {
                        marker_highlight = marker_highlight.max(0.9);
                        marker_color = [0.98, 0.93, 0.32];
                        marker_size *= 1.08;
                    }
                    push_marker(
                        position,
                        scale_size(marker_size),
                        marker_color,
                        marker_highlight,
                    );
                }

                if let Some(position) = scrub_position {
                    push_marker(position, scale_size(0.058), [0.2, 0.95, 0.85], 1.0);
                }
            }
        }

        let selected = self.selected_entity;
        for (idx, entity) in scene.entities.iter().enumerate() {
            let position = match entity.position {
                Some(pos) => pos,
                None => continue,
            };

            let is_selected = matches!(selected, Some(sel) if sel == idx);
            let base_size = match entity.kind {
                SceneEntityKind::Actor => 0.06,
                SceneEntityKind::Object => 0.05,
                SceneEntityKind::InterestActor => 0.045,
            };
            let size = if is_selected {
                scale_size(base_size * 1.2)
            } else {
                scale_size(base_size)
            };
            let color = if is_selected {
                [0.95, 0.35, 0.25]
            } else {
                match entity.kind {
                    SceneEntityKind::Actor => [0.25, 0.85, 0.62],
                    SceneEntityKind::Object => [0.3, 0.65, 0.98],
                    SceneEntityKind::InterestActor => [0.88, 0.74, 0.32],
                }
            };
            let highlight = if is_selected { 1.0 } else { 0.0 };
            push_marker(position, size, color, highlight);
        }

        Some(instances)
    }

    fn ensure_marker_capacity(&mut self, required: usize) {
        if required <= self.marker_capacity {
            return;
        }

        let new_capacity = required.next_power_of_two().max(4);
        let new_size = (new_capacity * std::mem::size_of::<MarkerInstance>()) as u64;
        self.marker_instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("marker-instance-buffer"),
            size: new_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.marker_capacity = new_capacity;
    }
}
struct PreviewTexture {
    data: Vec<u8>,
    width: u32,
    height: u32,
    frame_count: u32,
    codec: u32,
    format: u32,
    depth_stats: Option<DepthStats>,
    depth_preview: bool,
}

fn decode_asset_texture(
    asset_name: &str,
    bytes: &[u8],
    seed_bitmap: Option<&BmFile>,
) -> Result<PreviewTexture> {
    let lower = asset_name.to_ascii_lowercase();
    if !(lower.ends_with(".bm") || lower.ends_with(".zbm")) {
        bail!("asset {asset_name} is not a BM surface");
    }

    let mut seed_slice: Option<&[u8]> = None;
    if let Some(seed) = seed_bitmap {
        if let Some(frame) = seed.frames.first() {
            seed_slice = Some(frame.data.as_slice());
        }
    }

    let bm = decode_bm_with_seed(bytes, seed_slice)?;
    let metadata = bm.metadata();
    let frame = bm
        .frames
        .first()
        .ok_or_else(|| anyhow!("BM surface has no frames"))?;
    let mut depth_stats: Option<DepthStats> = None;
    let mut used_color_seed = false;
    let mut seed_dimensions: Option<(u32, u32)> = None;
    let mut rgba = if metadata.format == 5 {
        let stats = frame.depth_stats(&metadata)?;
        depth_stats = Some(stats);
        if let Some(seed) = seed_bitmap {
            if let Some(base_frame) = seed.frames.first() {
                let base_metadata = seed.metadata();
                used_color_seed = true;
                seed_dimensions = Some((base_metadata.width, base_metadata.height));
                base_frame.as_rgba8888(&base_metadata)?
            } else {
                frame.as_rgba8888(&metadata)?
            }
        } else {
            frame.as_rgba8888(&metadata)?
        }
    } else {
        frame.as_rgba8888(&metadata)?
    };

    let depth_preview = metadata.format == 5 && !used_color_seed;

    let expected_len = (frame.width * frame.height * 4) as usize;
    if rgba.len() != expected_len {
        let (src_w, src_h) = seed_dimensions.unwrap_or((frame.width, frame.height));
        rgba = resample_rgba_nearest(&rgba, src_w, src_h, frame.width, frame.height);
    }

    if metadata.format == 5 {
        match (used_color_seed, seed_bitmap.is_some()) {
            (true, _) => {
                println!("  paired base bitmap detected; RGB preview sourced from color plate");
            }
            (false, true) => {
                println!("  base bitmap missing frame data; preview shows normalized depth");
            }
            (false, false) => {
                println!(
                    "  no base bitmap available; preview shows normalized depth buffer values"
                );
            }
        }
    }
    Ok(PreviewTexture {
        data: rgba,
        width: frame.width,
        height: frame.height,
        frame_count: bm.image_count,
        codec: bm.codec,
        format: metadata.format,
        depth_stats,
        depth_preview,
    })
}

struct TextureStats {
    min_luma: u8,
    max_luma: u8,
    mean_luma: f32,
    opaque_pixels: u32,
    total_pixels: u32,
    quadrant_means: [f32; 4],
}

struct RenderVerification {
    stats: TextureStats,
    diff: TextureDiffSummary,
}

struct TextureDiffSummary {
    total_pixels: u32,
    mismatched_pixels: u32,
    max_abs_diff: u8,
    mean_abs_diff: f32,
    quadrant_mismatch: [f32; 4],
}

fn diff_mismatch_ratio(diff: &TextureDiffSummary) -> f32 {
    if diff.total_pixels == 0 {
        0.0
    } else {
        diff.mismatched_pixels as f32 / diff.total_pixels as f32
    }
}

fn resample_rgba_nearest(
    src: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<u8> {
    if src_width == 0 || src_height == 0 || dst_width == 0 || dst_height == 0 {
        return vec![0u8; (dst_width * dst_height * 4) as usize];
    }
    let mut dst = vec![0u8; (dst_width * dst_height * 4) as usize];
    for dy in 0..dst_height as usize {
        let sy = ((dy as u64 * src_height as u64) / dst_height as u64) as u32;
        let sy = sy.min(src_height.saturating_sub(1));
        for dx in 0..dst_width as usize {
            let sx = ((dx as u64 * src_width as u64) / dst_width as u64) as u32;
            let sx = sx.min(src_width.saturating_sub(1));
            let src_idx = ((sy * src_width + sx) * 4) as usize;
            let dst_idx = ((dy as u32 * dst_width + dx as u32) * 4) as usize;
            dst[dst_idx..dst_idx + 4].copy_from_slice(
                src.get(src_idx..src_idx + 4)
                    .unwrap_or(&[0u8, 0u8, 0u8, 0xFF]),
            );
        }
    }
    dst
}

struct TextureUpload<'a> {
    data: Cow<'a, [u8]>,
    bytes_per_row: u32,
}

impl<'a> TextureUpload<'a> {
    fn pixels(&self) -> &[u8] {
        &self.data
    }

    fn bytes_per_row(&self) -> u32 {
        self.bytes_per_row
    }
}

fn prepare_rgba_upload<'a>(width: u32, height: u32, data: &'a [u8]) -> Result<TextureUpload<'a>> {
    ensure!(width > 0 && height > 0, "texture has no dimensions");
    let row_bytes = 4usize * width as usize;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    ensure!(
        data.len() >= row_bytes * height as usize,
        "texture buffer ({}) smaller than {}x{} RGBA ({})",
        data.len(),
        width,
        height,
        row_bytes * height as usize
    );

    if row_bytes % alignment == 0 && data.len() == row_bytes * height as usize {
        return Ok(TextureUpload {
            data: Cow::Borrowed(data),
            bytes_per_row: row_bytes as u32,
        });
    }

    let padded_row_bytes = ((row_bytes + alignment - 1) / alignment) * alignment;
    let mut buffer = vec![0u8; padded_row_bytes * height as usize];
    for row in 0..height as usize {
        let src_offset = row * row_bytes;
        if src_offset >= data.len() {
            break;
        }
        let remaining = data.len() - src_offset;
        let to_copy = remaining.min(row_bytes);
        let dst_offset = row * padded_row_bytes;
        buffer[dst_offset..dst_offset + to_copy]
            .copy_from_slice(&data[src_offset..src_offset + to_copy]);
    }

    Ok(TextureUpload {
        data: Cow::Owned(buffer),
        bytes_per_row: padded_row_bytes as u32,
    })
}

fn validate_render_diff(diff: &TextureDiffSummary, threshold: f32) -> Result<()> {
    ensure!(
        (0.0..=1.0).contains(&threshold),
        "render diff threshold must be between 0 and 1 (got {})",
        threshold
    );
    let ratio = diff_mismatch_ratio(diff);
    if ratio > threshold {
        bail!(
            "post-render image diverges from decoded bitmap beyond allowed threshold ({:.4} > {:.4})",
            ratio,
            threshold
        );
    }
    Ok(())
}

fn dump_texture_to_png(preview: &PreviewTexture, destination: &Path) -> Result<TextureStats> {
    export_rgba_to_png(preview.width, preview.height, &preview.data, destination)
}

fn export_rgba_to_png(
    width: u32,
    height: u32,
    data: &[u8],
    destination: &Path,
) -> Result<TextureStats> {
    let expected_len = width as usize * height as usize * 4;
    ensure!(
        data.len() == expected_len,
        "RGBA buffer size {} does not match dimensions {}x{}",
        data.len(),
        width,
        height
    );

    let file = File::create(destination)?;
    let encoder = PngEncoder::new(file);
    encoder.write_image(data, width, height, ColorType::Rgba8.into())?;

    Ok(compute_texture_stats(width, height, data))
}

fn compute_texture_stats(width: u32, height: u32, data: &[u8]) -> TextureStats {
    let mut min_luma = u8::MAX;
    let mut max_luma = u8::MIN;
    let mut sum_luma: u64 = 0;
    let mut total_pixels: u32 = 0;
    let mut opaque_pixels: u32 = 0;
    let mut quadrant_sums = [0u64; 4];
    let mut quadrant_counts = [0u32; 4];

    let half_h = height / 2;
    let half_w = width / 2;

    for (idx, chunk) in data.chunks(4).enumerate() {
        let r = chunk[0] as u16;
        let g = chunk[1] as u16;
        let b = chunk[2] as u16;
        let a = chunk[3];
        let luma = ((r + g + b) / 3) as u8;
        min_luma = min_luma.min(luma);
        max_luma = max_luma.max(luma);
        sum_luma += luma as u64;
        total_pixels += 1;
        if a > 0 {
            opaque_pixels += 1;
        }

        let px = idx as u32 % width;
        let py = idx as u32 / width;
        let quadrant = match (px < half_w, py < half_h) {
            (true, true) => 0,   // top-left
            (false, true) => 1,  // top-right
            (true, false) => 2,  // bottom-left
            (false, false) => 3, // bottom-right
        };
        quadrant_sums[quadrant] += luma as u64;
        quadrant_counts[quadrant] += 1;
    }

    let mean_luma = if total_pixels > 0 {
        (sum_luma as f64 / total_pixels as f64) as f32
    } else {
        0.0
    };

    let mut quadrant_means = [0.0; 4];
    for idx in 0..4 {
        quadrant_means[idx] = if quadrant_counts[idx] > 0 {
            (quadrant_sums[idx] as f64 / quadrant_counts[idx] as f64) as f32
        } else {
            0.0
        };
    }

    TextureStats {
        min_luma,
        max_luma,
        mean_luma,
        opaque_pixels,
        total_pixels,
        quadrant_means,
    }
}

fn render_texture_offscreen(
    preview: &PreviewTexture,
    destination: Option<&Path>,
) -> Result<RenderVerification> {
    let instance = wgpu::Instance::new(InstanceDescriptor {
        backends: Backends::all(),
        flags: InstanceFlags::default(),
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .block_on()
        .or_else(|| {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                })
                .block_on()
        })
        .or_else(|| {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: true,
                    compatible_surface: None,
                })
                .block_on()
        })
        .context("requesting adapter for offscreen render")?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("grim-viewer-offscreen-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        )
        .block_on()
        .context("requesting device for offscreen render")?;

    let texture_extent = wgpu::Extent3d {
        width: preview.width,
        height: preview.height,
        depth_or_array_layers: 1,
    };

    let asset_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen-asset-texture"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let upload = prepare_rgba_upload(preview.width, preview.height, &preview.data)?;
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &asset_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        upload.pixels(),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(upload.bytes_per_row()),
            rows_per_image: Some(preview.height),
        },
        texture_extent,
    );

    let asset_view = asset_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("offscreen-asset-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("offscreen-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("offscreen-bind-group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&asset_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("offscreen-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
    });

    let quad_vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<QuadVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
    };

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("offscreen-pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("offscreen-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[quad_vertex_layout],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview: None,
    });

    let quad_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("offscreen-quad-vertex-buffer"),
        contents: cast_slice(&QUAD_VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let quad_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("offscreen-quad-index-buffer"),
        contents: cast_slice(&QUAD_INDICES),
        usage: wgpu::BufferUsages::INDEX,
    });
    let quad_index_count = QUAD_INDICES.len() as u32;

    let render_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen-target"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let render_view = render_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("offscreen-encoder"),
    });

    {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("offscreen-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &render_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rpass.set_pipeline(&pipeline);
        rpass.set_bind_group(0, &bind_group, &[]);
        rpass.set_vertex_buffer(0, quad_vertex_buffer.slice(..));
        rpass.set_index_buffer(quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        rpass.draw_indexed(0..quad_index_count, 0, 0..1);
    }

    let bytes_per_row = 4 * preview.width;
    let padded_bytes_per_row = ((bytes_per_row + COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        / COPY_BYTES_PER_ROW_ALIGNMENT)
        * COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer_size = padded_bytes_per_row as u64 * preview.height as u64;
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &render_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback_buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(preview.height),
            },
        },
        texture_extent,
    );

    queue.submit(std::iter::once(encoder.finish()));
    device.poll(Maintain::Wait);

    let buffer_slice = readback_buffer.slice(..);
    let (tx, rx) = mpsc::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(Maintain::Wait);
    match rx
        .recv()
        .context("waiting for offscreen readback completion")?
    {
        Ok(()) => {}
        Err(err) => bail!("mapping offscreen readback buffer: {err}"),
    }
    let padded = buffer_slice.get_mapped_range();
    let mut rgba = vec![0u8; (preview.width * preview.height * 4) as usize];

    for row in 0..preview.height as usize {
        let src_offset = row * padded_bytes_per_row as usize;
        let dst_offset = row * bytes_per_row as usize;
        rgba[dst_offset..dst_offset + bytes_per_row as usize]
            .copy_from_slice(&padded[src_offset..src_offset + bytes_per_row as usize]);
    }
    drop(padded);
    readback_buffer.unmap();

    let diff = summarize_texture_diff(preview, &rgba)?;
    let stats = match destination {
        Some(path) => export_rgba_to_png(preview.width, preview.height, &rgba, path)?,
        None => compute_texture_stats(preview.width, preview.height, &rgba),
    };
    Ok(RenderVerification { stats, diff })
}

fn summarize_texture_diff(preview: &PreviewTexture, rendered: &[u8]) -> Result<TextureDiffSummary> {
    let expected_len = (preview.width * preview.height * 4) as usize;
    ensure!(
        rendered.len() == expected_len,
        "rendered RGBA buffer size {} does not match expected {}x{}",
        rendered.len(),
        preview.width,
        preview.height
    );

    let mut mismatched_pixels: u32 = 0;
    let mut max_abs_diff: u8 = 0;
    let mut sum_abs_diff: u64 = 0;
    let mut quadrant_counts = [0u32; 4];
    let mut quadrant_mismatches = [0u32; 4];

    let half_w = preview.width / 2;
    let half_h = preview.height / 2;

    for (idx, (expected, actual)) in preview.data.chunks(4).zip(rendered.chunks(4)).enumerate() {
        let mut pixel_abs_diff: u8 = 0;
        for channel in 0..4 {
            let diff = u8::abs_diff(expected[channel], actual[channel]);
            if diff > pixel_abs_diff {
                pixel_abs_diff = diff;
            }
        }

        let px = idx as u32 % preview.width;
        let py = idx as u32 / preview.width;
        let quadrant = match (px < half_w, py < half_h) {
            (true, true) => 0,
            (false, true) => 1,
            (true, false) => 2,
            (false, false) => 3,
        };
        quadrant_counts[quadrant] += 1;

        if pixel_abs_diff > 0 {
            mismatched_pixels += 1;
            quadrant_mismatches[quadrant] += 1;
            max_abs_diff = max_abs_diff.max(pixel_abs_diff);
        }
        sum_abs_diff += pixel_abs_diff as u64;
    }

    let total_pixels = preview.width * preview.height;
    let mean_abs_diff = if total_pixels > 0 {
        (sum_abs_diff as f64 / total_pixels as f64) as f32
    } else {
        0.0
    };

    let mut quadrant_mismatch = [0.0_f32; 4];
    for idx in 0..4 {
        quadrant_mismatch[idx] = if quadrant_counts[idx] > 0 {
            quadrant_mismatches[idx] as f32 / quadrant_counts[idx] as f32
        } else {
            0.0
        };
    }

    Ok(TextureDiffSummary {
        total_pixels,
        mismatched_pixels,
        max_abs_diff,
        mean_abs_diff,
        quadrant_mismatch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::path::PathBuf;

    fn make_diff(total: u32, mismatched: u32) -> TextureDiffSummary {
        TextureDiffSummary {
            total_pixels: total,
            mismatched_pixels: mismatched,
            max_abs_diff: 5,
            mean_abs_diff: 0.1,
            quadrant_mismatch: [0.0; 4],
        }
    }

    #[test]
    fn validate_render_diff_allows_within_threshold() {
        let diff = make_diff(10_000, 50); // 0.5%
        assert!(validate_render_diff(&diff, 0.01).is_ok());
    }

    #[test]
    fn validate_render_diff_rejects_exceeding_threshold() {
        let diff = make_diff(10_000, 5_000); // 50%
        let err = validate_render_diff(&diff, 0.25)
            .expect_err("expected failure when ratio exceeds threshold");
        assert!(
            err.to_string()
                .contains("post-render image diverges from decoded bitmap")
        );
    }

    #[test]
    fn validate_render_diff_rejects_invalid_threshold() {
        let diff = make_diff(10_000, 0);
        let err =
            validate_render_diff(&diff, 1.1).expect_err("threshold outside [0,1] should fail");
        assert!(
            err.to_string()
                .contains("render diff threshold must be between 0 and 1")
        );
    }

    #[test]
    fn manny_movement_projects_with_recovered_camera() -> Result<()> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().expect("workspace root should exist");

        let timeline_path = workspace_root.join("tools/tests/manny_office_timeline.json");
        let manifest_path = workspace_root.join("artifacts/manny_office_assets.json");
        let movement_path = workspace_root.join("tools/tests/movement_log.json");
        let event_log_path = workspace_root.join("tools/tests/hotspot_events.json");
        let dev_install = workspace_root.join("dev-install");

        if !(timeline_path.is_file()
            && manifest_path.is_file()
            && movement_path.is_file()
            && dev_install.is_dir())
        {
            eprintln!("skipping camera projection test; Manny baseline artefacts not available");
            return Ok(());
        }

        let scene = match load_scene_from_timeline(
            &timeline_path,
            &manifest_path,
            Some("mo_tube_balloon.zbm"),
        ) {
            Ok(scene) => scene,
            Err(err) => {
                eprintln!("skipping camera projection test; unable to load scene: {err}");
                return Ok(());
            }
        };
        let camera = match scene.camera.as_ref() {
            Some(camera) => camera,
            None => {
                eprintln!("skipping camera projection test; timeline did not recover camera");
                return Ok(());
            }
        };
        assert_eq!(camera.name, "mo_ddtws");

        let asset_bytes = match load_asset_bytes(&manifest_path, "mo_tube_balloon.zbm") {
            Ok((_, bytes, _)) => bytes,
            Err(err) => {
                eprintln!("skipping camera projection test; unable to load asset: {err}");
                return Ok(());
            }
        };
        let preview = match decode_asset_texture("mo_tube_balloon.zbm", &asset_bytes, None) {
            Ok(preview) => preview,
            Err(err) => {
                eprintln!("skipping camera projection test; unable to decode asset: {err}");
                return Ok(());
            }
        };
        let aspect = (preview.width.max(1) as f32) / (preview.height.max(1) as f32);

        let projector = scene
            .camera_projector(aspect)
            .expect("camera should provide a projector");

        let trace = load_movement_trace(&movement_path)?;
        let mut projected = 0usize;

        for frame in [trace.first_frame(), trace.last_frame()] {
            if let Some(position) = trace.nearest_position(frame) {
                let ndc = projector
                    .project(position)
                    .expect("movement sample should project");
                assert!(ndc[0].abs() <= 1.05 && ndc[1].abs() <= 1.05);
                projected += 1;
            }
        }

        if event_log_path.is_file() {
            let events = load_hotspot_event_log(&event_log_path)?;
            for event in events.iter().filter_map(|entry| entry.frame) {
                if let Some(position) = trace.nearest_position(event) {
                    let ndc = projector
                        .project(position)
                        .expect("hotspot marker should project");
                    assert!(ndc[0].abs() <= 1.05 && ndc[1].abs() <= 1.05);
                    projected += 1;
                    break;
                }
            }
        }

        assert!(projected >= 2, "expected to project baseline samples");

        Ok(())
    }
}

fn generate_placeholder_texture(bytes: &[u8], asset_name: &str) -> PreviewTexture {
    const WIDTH: u32 = 256;
    const HEIGHT: u32 = 256;
    let mut data = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    let len = bytes.len().max(1);
    let seed = asset_name
        .as_bytes()
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));

    for (idx, pixel) in data.chunks_mut(4).enumerate() {
        let base = (idx + seed as usize) % len;
        let r = bytes.get(base).copied().unwrap_or(seed);
        let g = bytes.get((base + 17) % len).copied().unwrap_or(r);
        let b = bytes.get((base + 43) % len).copied().unwrap_or(g);
        pixel[0] = r;
        pixel[1] = g;
        pixel[2] = b;
        pixel[3] = 0xFF;
    }

    PreviewTexture {
        data,
        width: WIDTH,
        height: HEIGHT,
        frame_count: 0,
        codec: 0,
        format: 0,
        depth_stats: None,
        depth_preview: false,
    }
}

const SHADER_SOURCE: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(input.position, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

@group(0) @binding(0)
var asset_texture: texture_2d<f32>;
@group(0) @binding(1)
var asset_sampler: sampler;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv = clamp(input.uv, vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    return textureSample(asset_texture, asset_sampler, uv);
}
"#;

const MARKER_SHADER_SOURCE: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

struct VertexIn {
    @location(0) base_pos: vec2<f32>,
    @location(1) translate: vec2<f32>,
    @location(2) size: f32,
    @location(3) highlight: f32,
    @location(4) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOutput {
    let scale = input.size * (1.0 + input.highlight * 0.6);
    let position = input.base_pos * scale + input.translate;
    var out: VertexOutput;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(input.color, 0.9);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const QUAD_VERTICES: [QuadVertex; 4] = [
    QuadVertex {
        position: [-1.0, 1.0],
        uv: [0.0, 0.0],
    },
    QuadVertex {
        position: [1.0, 1.0],
        uv: [1.0, 0.0],
    },
    QuadVertex {
        position: [-1.0, -1.0],
        uv: [0.0, 1.0],
    },
    QuadVertex {
        position: [1.0, -1.0],
        uv: [1.0, 1.0],
    },
];

const QUAD_INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];

fn preview_color(bytes: &[u8]) -> wgpu::Color {
    if bytes.is_empty() {
        return wgpu::Color::BLACK;
    }

    let mut hash = 0u64;
    for chunk in bytes.chunks(8) {
        let mut padded = [0u8; 8];
        for (idx, value) in chunk.iter().enumerate() {
            padded[idx] = *value;
        }
        hash ^= u64::from_le_bytes(padded).rotate_left(7);
    }

    let r = ((hash >> 0) & 0xFF) as f64 / 255.0;
    let g = ((hash >> 8) & 0xFF) as f64 / 255.0;
    let b = ((hash >> 16) & 0xFF) as f64 / 255.0;

    wgpu::Color { r, g, b, a: 1.0 }
}

fn init_audio() -> Result<()> {
    #[cfg(feature = "audio")]
    {
        let (_stream, _stream_handle) = OutputStream::try_default()
            .context("initializing default audio output device via rodio")?;
        let _ = (_stream, _stream_handle);
    }

    Ok(())
}
