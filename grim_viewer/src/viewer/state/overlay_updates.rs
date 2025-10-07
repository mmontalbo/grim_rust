use super::super::overlays::{audio_overlay_lines, timeline_overlay_lines};
use super::ViewerState;
use crate::audio::AudioStatus;

pub(super) fn update_audio_overlay(state: &mut ViewerState, status: &AudioStatus) {
    if let Some(overlay) = state.audio_overlay.as_mut() {
        let lines = audio_overlay_lines(status);
        overlay.set_lines(&lines);
    }
}

pub(super) fn refresh_timeline_overlay(state: &mut ViewerState) {
    if let Some(overlay) = state.timeline_overlay.as_mut() {
        let scene = state.scene.as_deref();
        let lines = timeline_overlay_lines(scene, state.selected_entity);
        overlay.set_lines(&lines);
    }
}

pub(super) fn refresh_scrubber_overlay(state: &mut ViewerState) {
    if let Some(overlay) = state.scrubber_overlay.as_mut() {
        if let (Some(scrubber), Some(scene)) = (state.scrubber.as_ref(), state.scene.as_deref()) {
            if let Some(trace) = scene.movement_trace() {
                let lines = scrubber.overlay_lines(trace);
                overlay.set_lines(&lines);
                return;
            }
        }
        overlay.set_lines(&[]);
    }
}
