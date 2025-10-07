use super::super::markers::MinimapLayout;
use super::ViewerState;
use winit::dpi::PhysicalSize;

use crate::ui_layout::PanelKind;

pub(super) fn resize(state: &mut ViewerState, new_size: PhysicalSize<u32>) {
    if new_size.width == 0 || new_size.height == 0 {
        return;
    }

    state.size = new_size;
    state.config.width = new_size.width;
    state.config.height = new_size.height;
    state.surface.configure(&state.device, &state.config);
    if let Err(err) = state.ui_layout.set_window_size(new_size) {
        eprintln!("[grim_viewer] layout resize failed: {err:?}");
    } else {
        apply_panel_layouts(state);
    }
}

pub(super) fn apply_panel_layouts(state: &mut ViewerState) {
    let window_size = state.size;
    if let Some(overlay) = state.audio_overlay.as_mut() {
        if let Some(rect) = state.ui_layout.panel_rect(PanelKind::Audio) {
            overlay.update_layout(&state.device, window_size, rect);
        }
    }
    if let Some(overlay) = state.timeline_overlay.as_mut() {
        if let Some(rect) = state.ui_layout.panel_rect(PanelKind::Timeline) {
            overlay.update_layout(&state.device, window_size, rect);
        }
    }
    if let Some(overlay) = state.scrubber_overlay.as_mut() {
        if let Some(rect) = state.ui_layout.panel_rect(PanelKind::Scrubber) {
            overlay.update_layout(&state.device, window_size, rect);
        }
    }
}

pub(super) fn minimap_layout(state: &ViewerState) -> Option<MinimapLayout> {
    let rect = state.ui_layout.panel_rect(PanelKind::Minimap)?;
    MinimapLayout::from_rect(rect, state.size)
}
