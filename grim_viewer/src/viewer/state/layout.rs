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
    state
        .overlays
        .apply_layouts(&state.device, &state.ui_layout, state.size);
}

pub(super) fn minimap_layout(state: &ViewerState) -> Option<MinimapLayout> {
    let rect = state.ui_layout.panel_rect(PanelKind::Minimap)?;
    MinimapLayout::from_rect(rect, state.size)
}
