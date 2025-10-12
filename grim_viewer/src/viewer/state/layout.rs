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
    state.rebuild_mesh_depth();
    if let Err(err) = state.ui_layout.set_window_size(new_size) {
        eprintln!("[grim_viewer] layout resize failed: {err:?}");
    } else {
        apply_panel_layouts(state);
    }
}

pub(super) fn apply_panel_layouts(state: &mut ViewerState) {
    let (plate_x, _plate_y, plate_w, _plate_h) = plate_viewport(state);
    let window_width = state.size.width as f32;
    let left_bar_width = plate_x;
    let right_bar_width = (window_width - (plate_x + plate_w)).max(0.0);

    state.overlays.apply_layouts(
        &state.device,
        &state.ui_layout,
        state.size,
        left_bar_width,
        right_bar_width,
    );
}

pub(super) fn minimap_layout(state: &ViewerState) -> Option<MinimapLayout> {
    let rect = state.ui_layout.panel_rect(PanelKind::Minimap)?;
    MinimapLayout::from_rect(rect, state.size)
}

pub(super) fn plate_viewport(state: &ViewerState) -> (f32, f32, f32, f32) {
    let window_w = state.size.width as f32;
    let window_h = state.size.height as f32;
    let tex_w = state.texture_size.width as f32;
    let tex_h = state.texture_size.height as f32;

    let viewport_w = tex_w.min(window_w).max(1.0);
    let viewport_h = tex_h.min(window_h).max(1.0);
    let origin_x = ((window_w - viewport_w) * 0.5).max(0.0);
    let origin_y = ((window_h - viewport_h) * 0.5).max(0.0);

    (origin_x, origin_y, viewport_w, viewport_h)
}
