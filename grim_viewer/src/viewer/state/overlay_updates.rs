use super::super::overlays::{audio_overlay_lines, timeline_overlay_lines};
use super::ViewerState;
use crate::audio::AudioStatus;

pub(super) fn update_audio_overlay(state: &mut ViewerState, status: &AudioStatus) {
    if let Some(overlay) = state.audio_overlay.as_mut() {
        let lines = audio_overlay_lines(status);
        overlay.set_lines(&lines);
    }
}

pub(super) fn refresh_scene_overlays(state: &mut ViewerState) {
    let snapshot = SceneOverlaySnapshot::gather(
        state.scene.as_deref(),
        state.scrubber.as_ref(),
        state.selected_entity,
    );

    if let Some(overlay) = state.timeline_overlay.as_mut() {
        overlay.set_lines(snapshot.timeline_lines());
    }

    if let Some(overlay) = state.scrubber_overlay.as_mut() {
        overlay.set_lines(snapshot.scrubber_lines());
    }
}

/// Cached overlay text derived from the current scene/scrubber state. Keeps the
/// formatting logic in one place so overlays stay in sync.
struct SceneOverlaySnapshot {
    timeline: Vec<String>,
    scrubber: Vec<String>,
}

impl SceneOverlaySnapshot {
    /// Pull lines for the timeline and movement scrubber overlays, returning
    /// empty lists when the required data is missing (e.g., headless timeline).
    fn gather(
        scene: Option<&crate::scene::ViewerScene>,
        scrubber: Option<&crate::scene::MovementScrubber>,
        selected_entity: Option<usize>,
    ) -> Self {
        let timeline = timeline_overlay_lines(scene, selected_entity);
        let scrubber_lines = match (scrubber, scene.and_then(|s| s.movement_trace())) {
            (Some(scrubber), Some(trace)) => scrubber.overlay_lines(trace),
            _ => Vec::new(),
        };

        Self {
            timeline,
            scrubber: scrubber_lines,
        }
    }

    fn timeline_lines(&self) -> &[String] {
        &self.timeline
    }

    fn scrubber_lines(&self) -> &[String] {
        // Empty when either the scrubber is disabled or the scene lacks
        // movement traces, keeping overlay rendering straightforward.
        &self.scrubber
    }
}

#[cfg(test)]
mod tests {
    use super::SceneOverlaySnapshot;
    use crate::scene::{MovementSample, MovementScrubber, MovementTrace, ViewerScene};

    fn sample_scene() -> ViewerScene {
        let mut scene = ViewerScene {
            entities: Vec::new(),
            position_bounds: None,
            timeline: None,
            movement: None,
            hotspot_events: Vec::new(),
            camera: None,
            active_setup: None,
        };
        let trace = MovementTrace::from_samples(vec![MovementSample {
            frame: 1,
            position: [0.0, 0.0, 0.0],
            yaw: None,
            sector: None,
        }])
        .expect("trace");
        scene.attach_movement_trace(trace);
        scene
    }

    #[test]
    fn snapshot_includes_scrubber_lines_when_scene_present() {
        let scene = sample_scene();
        let scrubber = MovementScrubber::new(&scene);
        let snapshot = SceneOverlaySnapshot::gather(Some(&scene), scrubber.as_ref(), None);
        assert!(!snapshot.scrubber_lines().is_empty());
    }

    #[test]
    fn snapshot_returns_empty_scrubber_when_trace_missing() {
        let snapshot = SceneOverlaySnapshot::gather(None, None, None);
        assert!(snapshot.scrubber_lines().is_empty());
    }
}
