use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use glam::{Mat3, Mat4, Vec3, Vec4};
use serde::Deserialize;
use serde_json::Value;

use crate::texture::load_asset_bytes;
use crate::timeline::{
    HookLookup, HookReference, TimelineSummary, build_timeline_summary, parse_hook_reference,
};
use grim_formats::SetFile;
use grim_formats::set::Setup;

#[derive(Debug, Clone, Deserialize)]
pub struct MovementSample {
    pub frame: u32,
    pub position: [f32; 3],
    #[serde(default)]
    pub yaw: Option<f32>,
    #[serde(default)]
    pub sector: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HotspotEventLog {
    events: Vec<HotspotEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HotspotEvent {
    #[allow(dead_code)]
    pub sequence: u32,
    #[serde(default)]
    pub frame: Option<u32>,
    pub label: String,
}

impl HotspotEvent {
    pub fn kind(&self) -> HotspotEventKind {
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
pub enum HotspotEventKind {
    Hotspot,
    HeadTarget,
    IgnoreBoxes,
    Chore,
    Dialog,
    Selection,
    Other,
}

pub fn event_marker_style(kind: HotspotEventKind) -> (f32, [f32; 3], f32) {
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
pub struct MovementTrace {
    pub samples: Vec<MovementSample>,
    pub first_frame: u32,
    pub last_frame: u32,
    pub total_distance: f32,
    pub yaw_min: Option<f32>,
    pub yaw_max: Option<f32>,
    pub sector_counts: BTreeMap<String, u32>,
    pub bounds: SceneBounds,
}

impl MovementTrace {
    pub fn from_samples(mut samples: Vec<MovementSample>) -> Result<Self> {
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

    pub fn sample_count(&self) -> usize {
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

    pub fn yaw_range(&self) -> Option<(f32, f32)> {
        match (self.yaw_min, self.yaw_max) {
            (Some(min), Some(max)) => Some((min, max)),
            _ => None,
        }
    }

    pub fn dominant_sectors(&self, limit: usize) -> Vec<(&str, u32)> {
        let mut sectors: Vec<(&str, u32)> = self
            .sector_counts
            .iter()
            .map(|(name, count)| (name.as_str(), *count))
            .collect();
        sectors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        sectors.truncate(limit);
        sectors
    }

    pub fn nearest_position(&self, frame: u32) -> Option<[f32; 3]> {
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
pub struct ScrubEvent {
    pub scene_index: usize,
    pub frame: u32,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct MovementScrubber {
    current_sample: usize,
    sample_frames: Vec<u32>,
    head_targets: Vec<ScrubEvent>,
    highlighted_event: Option<usize>,
}

impl MovementScrubber {
    pub fn new(scene: &ViewerScene) -> Option<Self> {
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

    pub fn current_frame(&self) -> u32 {
        self.sample_frames
            .get(self.current_sample)
            .copied()
            .unwrap_or_default()
    }

    pub fn current_position(&self, trace: &MovementTrace) -> Option<[f32; 3]> {
        trace
            .samples
            .get(self.current_sample)
            .map(|sample| sample.position)
    }

    pub fn current_yaw(&self, trace: &MovementTrace) -> Option<f32> {
        trace
            .samples
            .get(self.current_sample)
            .and_then(|sample| sample.yaw)
    }

    pub fn highlighted_event(&self) -> Option<&ScrubEvent> {
        self.highlighted_event
            .and_then(|idx| self.head_targets.get(idx))
    }

    pub fn next_event(&self) -> Option<&ScrubEvent> {
        let current_frame = self.current_frame();
        self.head_targets
            .iter()
            .find(|event| event.frame > current_frame)
    }

    pub fn step(&mut self, delta: i32) -> bool {
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

    pub fn seek_to_frame(&mut self, frame: u32) -> bool {
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

    pub fn jump_to_head_target(&mut self, direction: i32) -> bool {
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

    pub fn overlay_lines(&self, trace: &MovementTrace) -> Vec<String> {
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

    #[test]
    fn movement_trace_finds_nearest_positions() {
        let samples = vec![
            MovementSample {
                frame: 1,
                position: [0.0, 0.0, 0.0],
                yaw: None,
                sector: None,
            },
            MovementSample {
                frame: 4,
                position: [4.0, 0.0, 0.0],
                yaw: None,
                sector: None,
            },
            MovementSample {
                frame: 7,
                position: [7.0, 0.0, 0.0],
                yaw: None,
                sector: None,
            },
        ];

        let trace = MovementTrace::from_samples(samples).expect("trace");

        assert_eq!(trace.nearest_position(1), Some([0.0, 0.0, 0.0]));
        assert_eq!(trace.nearest_position(5), Some([4.0, 0.0, 0.0]));
        assert_eq!(trace.nearest_position(8), Some([7.0, 0.0, 0.0]));
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

pub struct ViewerScene {
    pub entities: Vec<SceneEntity>,
    pub position_bounds: Option<SceneBounds>,
    pub timeline: Option<TimelineSummary>,
    pub movement: Option<MovementTrace>,
    pub hotspot_events: Vec<HotspotEvent>,
    pub camera: Option<CameraParameters>,
    pub active_setup: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CameraParameters {
    pub name: String,
    pub position: [f32; 3],
    pub interest: [f32; 3],
    pub roll_degrees: f32,
    pub fov_degrees: f32,
    pub near_clip: f32,
    pub far_clip: f32,
}

impl CameraParameters {
    pub fn from_setup(name: &str, setup: &Setup) -> Option<Self> {
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
pub struct CameraProjector {
    view_projection: Mat4,
}

impl CameraProjector {
    pub fn project(&self, position: [f32; 3]) -> Option<[f32; 2]> {
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
    pub fn attach_movement_trace(&mut self, trace: MovementTrace) {
        if let Some(bounds) = self.position_bounds.as_mut() {
            bounds.include_bounds(&trace.bounds);
        } else {
            self.position_bounds = Some(trace.bounds.clone());
        }
        self.movement = Some(trace);
    }

    pub fn movement_trace(&self) -> Option<&MovementTrace> {
        self.movement.as_ref()
    }

    pub fn attach_hotspot_events(&mut self, events: Vec<HotspotEvent>) {
        self.hotspot_events = events;
    }

    pub fn hotspot_events(&self) -> &[HotspotEvent] {
        &self.hotspot_events
    }

    pub fn entity_position(&self, name: &str) -> Option<[f32; 3]> {
        self.entities
            .iter()
            .find(|entity| entity.name.eq_ignore_ascii_case(name))
            .and_then(|entity| entity.position)
    }

    pub fn camera_projector(&self, aspect_ratio: f32) -> Option<CameraProjector> {
        self.camera
            .as_ref()
            .and_then(|camera| camera.projector(aspect_ratio))
    }

    pub fn active_setup(&self) -> Option<&str> {
        self.active_setup.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SceneEntityKind {
    Actor,
    Object,
    InterestActor,
}

impl SceneEntityKind {
    pub fn label(self) -> &'static str {
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
pub struct SceneEntity {
    pub kind: SceneEntityKind,
    pub name: String,
    pub created_by: Option<String>,
    pub timeline_hook_index: Option<usize>,
    pub timeline_stage_index: Option<u32>,
    pub timeline_stage_label: Option<String>,
    pub timeline_hook_name: Option<String>,
    pub methods: Vec<String>,
    pub position: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub facing_target: Option<String>,
    pub head_control: Option<String>,
    pub head_look_rate: Option<f32>,
    pub last_played: Option<String>,
    pub last_looping: Option<String>,
    pub last_completed: Option<String>,
}

impl SceneEntity {
    pub fn describe(&self) -> String {
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

fn prune_entities_for_set(
    entities: Vec<SceneEntity>,
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) -> Vec<SceneEntity> {
    if is_manny_office(set_variable_name, set_display_name) {
        return prune_manny_office_entities(entities, set_variable_name);
    }
    entities
}

fn is_manny_office(set_variable_name: Option<&str>, set_display_name: Option<&str>) -> bool {
    set_variable_name
        .map(|value| value.eq_ignore_ascii_case("mo"))
        .unwrap_or(false)
        || set_display_name
            .map(|value| value.eq_ignore_ascii_case("Manny's Office"))
            .unwrap_or(false)
}

fn manny_office_entity_names(set_prefix: &str) -> Vec<String> {
    let prefix = if set_prefix.is_empty() {
        "mo"
    } else {
        set_prefix
    };
    let mut names = vec!["manny".to_string()];
    for suffix in [
        "cards",
        "cards.interest_actor",
        "computer",
        "tube",
        "tube.interest_actor",
    ] {
        names.push(format!("{prefix}.{suffix}"));
    }
    names
}

fn prune_manny_office_entities(
    entities: Vec<SceneEntity>,
    set_variable_name: Option<&str>,
) -> Vec<SceneEntity> {
    let set_prefix = set_variable_name.unwrap_or("mo");
    let allowed = manny_office_entity_names(set_prefix);

    entities
        .into_iter()
        .filter(|entity| {
            allowed
                .iter()
                .any(|allowed| entity.name.eq_ignore_ascii_case(allowed))
        })
        .collect()
}

#[cfg(test)]
mod entity_filter_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn make_entity(kind: SceneEntityKind, name: &str) -> SceneEntity {
        SceneEntityBuilder::new(kind, name.to_string()).build()
    }

    #[test]
    fn manny_office_allowlist_matches_trimmed_entities() {
        let expected = vec![
            "manny".to_string(),
            "mo.cards".to_string(),
            "mo.cards.interest_actor".to_string(),
            "mo.computer".to_string(),
            "mo.tube".to_string(),
            "mo.tube.interest_actor".to_string(),
        ];
        assert_eq!(manny_office_entity_names("mo"), expected);
        assert_eq!(manny_office_entity_names(""), expected);

        let mut with_custom_prefix = expected.clone();
        for name in with_custom_prefix.iter_mut().skip(1) {
            *name = name.replace("mo", "custom");
        }
        assert_eq!(
            manny_office_entity_names("custom"),
            with_custom_prefix,
            "allowlist should respect provided prefix"
        );
    }

    #[test]
    fn prune_entities_for_manny_office_keeps_core_entities() {
        let entities = vec![
            make_entity(SceneEntityKind::Actor, "Actor"),
            make_entity(SceneEntityKind::Actor, "meche"),
            make_entity(SceneEntityKind::Actor, "mo"),
            make_entity(SceneEntityKind::Object, "loading_menu"),
            make_entity(SceneEntityKind::Object, "manny"),
            make_entity(SceneEntityKind::Object, "mo.cards"),
            make_entity(SceneEntityKind::InterestActor, "mo.cards"),
            make_entity(SceneEntityKind::InterestActor, "mo.cards.interest_actor"),
            make_entity(SceneEntityKind::Object, "mo.computer"),
            make_entity(SceneEntityKind::Object, "mo.tube"),
            make_entity(SceneEntityKind::InterestActor, "mo.tube.interest_actor"),
            make_entity(SceneEntityKind::Object, "canister_actor"),
        ];

        let pruned = prune_entities_for_set(entities, Some("mo"), Some("Manny's Office"));
        let names: Vec<&str> = pruned.iter().map(|entity| entity.name.as_str()).collect();

        assert_eq!(
            names,
            vec![
                "manny",
                "mo.cards",
                "mo.cards",
                "mo.cards.interest_actor",
                "mo.computer",
                "mo.tube",
                "mo.tube.interest_actor",
            ]
        );

        let unique: BTreeSet<&str> = names.iter().copied().collect();
        let expected: BTreeSet<String> = manny_office_entity_names("mo").into_iter().collect();
        let expected_refs: BTreeSet<&str> = expected.iter().map(|s| s.as_str()).collect();
        assert_eq!(unique, expected_refs);
    }

    #[test]
    fn prune_entities_leaves_other_sets_untouched() {
        let entities = vec![
            make_entity(SceneEntityKind::Actor, "Actor"),
            make_entity(SceneEntityKind::Object, "gl.cards"),
            make_entity(SceneEntityKind::InterestActor, "gl.cards"),
        ];

        let pruned = prune_entities_for_set(entities, Some("gl"), Some("Glottis' Garage"));
        let names: Vec<&str> = pruned.iter().map(|entity| entity.name.as_str()).collect();

        assert_eq!(names, vec!["Actor", "gl.cards", "gl.cards"]);
    }
}

#[derive(Debug, Clone)]
pub struct SceneBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl SceneBounds {
    pub fn update(&mut self, position: [f32; 3]) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(position[axis]);
            self.max[axis] = self.max[axis].max(position[axis]);
        }
    }

    pub fn include_bounds(&mut self, other: &SceneBounds) {
        self.update(other.min);
        self.update(other.max);
    }

    pub fn top_down_axes(&self) -> (usize, usize) {
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

    pub fn projection_axes(&self) -> (usize, usize) {
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

pub fn load_scene_from_timeline(
    path: &Path,
    manifest_path: &Path,
    active_asset: Option<&str>,
    geometry: Option<&LuaGeometrySnapshot>,
) -> Result<ViewerScene> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading timeline manifest {}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&data)
        .with_context(|| format!("parsing timeline manifest {}", path.display()))?;

    let set_info = manifest
        .get("engine_state")
        .and_then(|state| state.get("set"));

    let set_file_name = set_info
        .and_then(|set| set.get("set_file"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let set_variable_name = set_info
        .and_then(|set| set.get("variable_name"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let set_display_name = set_info
        .and_then(|set| set.get("display_name"))
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

    entities = prune_entities_for_set(
        entities,
        set_variable_name.as_deref(),
        set_display_name.as_deref(),
    );

    if let Some(snapshot) = geometry {
        apply_geometry_overrides(
            &mut entities,
            snapshot,
            set_variable_name.as_deref(),
            set_display_name.as_deref(),
        );
    }

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

#[derive(Debug, Clone, Copy)]
struct GeometryPose {
    position: [f32; 3],
    rotation: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LuaGeometrySnapshot {
    actors: BTreeMap<String, LuaActorSnapshot>,
    objects: Vec<LuaObjectSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct LuaActorSnapshot {
    name: Option<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
}

impl LuaActorSnapshot {
    fn pose(&self) -> Option<GeometryPose> {
        self.position.map(|position| GeometryPose {
            position,
            rotation: self.rotation,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct LuaObjectSnapshot {
    name: Option<String>,
    string_name: Option<String>,
    position: Option<[f32; 3]>,
    interest_actor: Option<LuaObjectActorLink>,
}

impl LuaObjectSnapshot {
    fn pose(&self) -> Option<GeometryPose> {
        self.position.map(|position| GeometryPose {
            position,
            rotation: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct LuaObjectActorLink {
    actor_id: Option<String>,
    actor_label: Option<String>,
}

pub fn load_lua_geometry_snapshot(path: &Path) -> Result<LuaGeometrySnapshot> {
    let data = fs::read(path)
        .with_context(|| format!("reading Lua geometry snapshot {}", path.display()))?;
    let snapshot: LuaGeometrySnapshot = serde_json::from_slice(&data)
        .with_context(|| format!("parsing Lua geometry snapshot {}", path.display()))?;
    Ok(snapshot)
}

fn apply_geometry_overrides(
    entities: &mut [SceneEntity],
    geometry: &LuaGeometrySnapshot,
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) {
    if !is_manny_office(set_variable_name, set_display_name) {
        return;
    }
    apply_manny_office_geometry(entities, geometry, set_variable_name);
}

fn apply_manny_office_geometry(
    entities: &mut [SceneEntity],
    geometry: &LuaGeometrySnapshot,
    set_variable_name: Option<&str>,
) {
    let prefix = set_variable_name.unwrap_or("mo");
    let prefix_lower = prefix.to_ascii_lowercase();

    let mut overrides: Vec<(String, GeometryPose)> = Vec::new();

    if let Some(pose) = geometry.actor_pose("manny") {
        overrides.push(("manny".to_string(), pose));
    }

    if let Some(pose) = geometry.object_pose_by_string_name("computer") {
        overrides.push((format!("{prefix_lower}.computer"), pose));
    }

    if let Some(pose) = geometry.object_pose_by_string_name("tube") {
        overrides.push((format!("{prefix_lower}.tube"), pose));
    }

    if let Some(pose) = geometry.actor_pose("motx083tube") {
        overrides.push((format!("{prefix_lower}.tube.interest_actor"), pose));
    }

    if let Some(pose) = geometry.object_pose_by_string_name("deck of playing cards") {
        overrides.push((format!("{prefix_lower}.cards"), pose));
    }

    if let Some(pose) = geometry.actor_pose("motx094deck_of_playing_cards") {
        overrides.push((format!("{prefix_lower}.cards.interest_actor"), pose));
    }

    for (name, pose) in overrides {
        if let Some(entity) = entities
            .iter_mut()
            .find(|entity| entity.name.eq_ignore_ascii_case(&name))
        {
            entity.position = Some(pose.position);
            if let Some(rotation) = pose.rotation {
                entity.rotation = Some(rotation);
            }
        }
    }
}

impl LuaGeometrySnapshot {
    fn actor_pose(&self, key: &str) -> Option<GeometryPose> {
        self.actors
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .and_then(|(_, actor)| actor.pose())
    }

    fn object_pose_by_string_name(&self, name: &str) -> Option<GeometryPose> {
        self.objects
            .iter()
            .find(|object| {
                object
                    .string_name
                    .as_deref()
                    .map(|value| value.eq_ignore_ascii_case(name))
                    .unwrap_or(false)
            })
            .and_then(|object| object.pose())
    }
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

pub fn load_movement_trace(path: &Path) -> Result<MovementTrace> {
    let data =
        fs::read(path).with_context(|| format!("reading movement log {}", path.display()))?;
    let samples: Vec<MovementSample> = serde_json::from_slice(&data)
        .with_context(|| format!("parsing movement log {}", path.display()))?;
    MovementTrace::from_samples(samples)
        .with_context(|| format!("summarising movement trace from {}", path.display()))
}

#[cfg(test)]
mod movement_log_io_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn movement_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/tests/movement_log.json")
    }

    #[test]
    fn load_movement_trace_summarises_baseline_fixture() {
        let trace = load_movement_trace(&movement_fixture_path()).expect("movement trace");

        assert_eq!(trace.sample_count(), 114);
        assert_eq!(trace.first_frame, 1);
        assert_eq!(trace.last_frame, 114);
        assert!((trace.total_distance - 1.1599987).abs() < 1e-6);

        let yaw_range = trace.yaw_range().expect("yaw range");
        assert!(yaw_range.0.abs() < 1e-6);
        assert!((yaw_range.1 - 270.0).abs() < 1e-6);

        let sectors = trace.dominant_sectors(3);
        assert_eq!(sectors.len(), 3);
        assert_eq!(sectors[0], ("floor_17", 42));
        assert_eq!(sectors[1], ("floor_21", 25));
        assert_eq!(sectors[2], ("floor_1734", 18));

        assert!((trace.bounds.min[0] - 0.607).abs() < 1e-6);
        assert!((trace.bounds.max[0] - 1.086_999_5).abs() < 1e-6);
        assert!((trace.bounds.min[1] - 2.021).abs() < 1e-6);
        assert!((trace.bounds.max[1] - 2.140_999_8).abs() < 1e-6);
    }

    #[test]
    fn load_movement_trace_surfaces_parse_errors() {
        let mut temp = NamedTempFile::new().expect("temp file");
        writeln!(temp, "this is not valid JSON").expect("write invalid content");

        let error = load_movement_trace(temp.path()).expect_err("expected parse failure");
        let message = format!("{error}");
        assert!(message.contains("parsing movement log"));
    }
}

#[cfg(test)]
mod hotspot_log_tests {
    use super::*;

    fn hotspot_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/tests/hotspot_events.json")
    }

    #[test]
    fn load_hotspot_event_log_matches_fixture_snapshot() {
        let events =
            load_hotspot_event_log(&hotspot_fixture_path()).expect("load hotspot event fixture");

        assert_eq!(events.len(), 38);
        assert_eq!(events[0].label, "actor.select manny");
        assert_eq!(events[0].kind(), HotspotEventKind::Selection);
        assert!(
            events
                .iter()
                .any(|event| event.label == "hotspot.demo.start computer")
        );

        let mut selections = 0usize;
        let mut hotspots = 0usize;
        let mut head_targets = 0usize;
        let mut ignore_boxes = 0usize;
        let mut chores = 0usize;
        let mut dialogs = 0usize;
        let mut other = 0usize;

        for event in &events {
            match event.kind() {
                HotspotEventKind::Selection => selections += 1,
                HotspotEventKind::Hotspot => hotspots += 1,
                HotspotEventKind::HeadTarget => head_targets += 1,
                HotspotEventKind::IgnoreBoxes => ignore_boxes += 1,
                HotspotEventKind::Chore => chores += 1,
                HotspotEventKind::Dialog => dialogs += 1,
                HotspotEventKind::Other => other += 1,
            }
        }

        assert_eq!(selections, 8);
        assert_eq!(hotspots, 4);
        assert_eq!(head_targets, 4);
        assert_eq!(ignore_boxes, 2);
        assert_eq!(chores, 3);
        assert_eq!(dialogs, 8);
        assert_eq!(other, 9);
    }
}

pub fn load_hotspot_event_log(path: &Path) -> Result<Vec<HotspotEvent>> {
    let data =
        fs::read(path).with_context(|| format!("reading hotspot event log {}", path.display()))?;
    let log: HotspotEventLog = serde_json::from_slice(&data)
        .with_context(|| format!("parsing hotspot event log {}", path.display()))?;
    Ok(log.events)
}

pub fn print_scene_summary(scene: &ViewerScene) {
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
            "Markers overlay: color-coded discs track entities (red = selected) and mirror the minimap anchors."
        );
    }
    if let Some(trace) = scene.movement_trace() {
        print_movement_trace_summary(trace);
    }
    let event_preview = scene.hotspot_events();
    if !event_preview.is_empty() {
        print_hotspot_preview(event_preview);
    }
    println!();
}

pub fn print_movement_trace_summary(trace: &MovementTrace) {
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
        "  Overlay markers: jade = desk anchor, violet = path, amber = tube anchor, teal = Manny, gold = highlighted hotspot, red = entity selection."
    );
    println!("  Scrubber controls: '['/']' step Manny frames; '{{'/'}}' jump head-target markers.");
}

pub fn print_hotspot_preview(events: &[HotspotEvent]) {
    println!("Hotspot event log: {} entries", events.len());
    for event in events.iter().take(6) {
        let frame_label = event
            .frame
            .map(|frame| frame.to_string())
            .unwrap_or_else(|| String::from("--"));
        println!("  [{frame_label}] {}", event.label);
    }
    if events.len() > 6 {
        println!("  ... +{} more", events.len() - 6);
    }
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
