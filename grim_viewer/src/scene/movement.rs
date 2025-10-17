/// Ordered samples describing Manny's captured positions; drives both minimap
/// markers and the scrubber overlay. Derived stats (distance, yaw range,
/// dominant sectors) spare the renderer from recomputing analytics every frame.
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
/// Tracks which movement sample is in focus and which head-target marker to
/// highlight; used by the viewer's scrubber overlay and `[`, `]`, `{`, `}`
/// shortcuts. Maintains lightweight indices so overlay text and marker
/// highlighting stay in lockstep with Manny's movement trace.
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
