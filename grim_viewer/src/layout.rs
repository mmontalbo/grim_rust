use winit::dpi::PhysicalSize;

pub const VIEWPORT_PADDING: f32 = 24.0;
pub const LABEL_HEIGHT: f32 = 32.0;
pub const LABEL_GAP: f32 = 8.0;
pub const DEBUG_PANEL_HEIGHT: f32 = 300.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct ViewerLayout {
    pub retail_view: Rect,
    pub engine_view: Rect,
    pub debug_panel: Rect,
}

impl ViewerLayout {
    pub fn compute(window: PhysicalSize<u32>, frame_aspect: f32) -> Self {
        let width = window.width.max(1) as f32;
        let height = window.height.max(1) as f32;

        let padding = VIEWPORT_PADDING;
        let label_height = LABEL_HEIGHT;
        let label_gap = LABEL_GAP;
        let debug_height = DEBUG_PANEL_HEIGHT.min(height * 0.4);

        let top_offset = padding + label_height + label_gap;
        let bottom_offset = padding + debug_height;

        let available_height = (height - top_offset - bottom_offset).max(1.0);
        let available_width = (width - padding * 3.0).max(1.0); // left margin + between + right margin

        let max_view_width = ((available_width - padding).max(1.0)) / 2.0;
        let max_view_height = available_height;

        let width_from_height = (max_view_height * frame_aspect).min(max_view_width);
        let view_width = width_from_height.max(1.0).min(max_view_width);
        let view_height = (view_width / frame_aspect).min(max_view_height).max(1.0);

        let retail_x = padding;
        let retail_y = top_offset;
        let engine_x = (retail_x + view_width + padding).min(width - padding - view_width);
        let engine_y = retail_y;

        let debug_panel_width = (width - padding * 2.0).max(1.0);
        let debug_panel_height = debug_height.max(1.0);
        let debug_panel_y = height - padding - debug_panel_height;

        Self {
            retail_view: Rect {
                x: retail_x,
                y: retail_y,
                width: view_width,
                height: view_height,
            },
            engine_view: Rect {
                x: engine_x,
                y: engine_y,
                width: view_width,
                height: view_height,
            },
            debug_panel: Rect {
                x: padding,
                y: debug_panel_y,
                width: debug_panel_width,
                height: debug_panel_height,
            },
        }
    }
}
