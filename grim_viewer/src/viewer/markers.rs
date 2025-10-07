use bytemuck::{Pod, Zeroable};
use winit::dpi::PhysicalSize;

use crate::scene::{CameraProjector, SceneEntityKind};
use crate::ui_layout::ViewportRect;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct MarkerVertex {
    pub position: [f32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct MarkerInstance {
    pub translate: [f32; 2],
    pub size: f32,
    pub highlight: f32,
    pub color: [f32; 3],
    pub _padding: f32,
}

#[derive(Clone, Copy)]
pub(super) struct MarkerPalette {
    pub color: [f32; 3],
    pub highlight: f32,
}

pub(super) const MANNY_ANCHOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.2, 0.95, 0.85],
    highlight: 1.0,
};
pub(super) const DESK_ANCHOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.28, 0.82, 0.52],
    highlight: 0.45,
};
pub(super) const TUBE_ANCHOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.98, 0.74, 0.28],
    highlight: 0.65,
};
pub(super) const ENTITY_SELECTED_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.95, 0.35, 0.25],
    highlight: 1.0,
};
pub(super) const ENTITY_ACTOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.2, 0.85, 0.6],
    highlight: 0.0,
};
pub(super) const ENTITY_OBJECT_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.25, 0.6, 0.95],
    highlight: 0.0,
};
pub(super) const ENTITY_INTEREST_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.85, 0.7, 0.25],
    highlight: 0.0,
};

pub(super) const MARKER_VERTICES: [MarkerVertex; 6] = [
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

pub(super) fn entity_palette(kind: SceneEntityKind, is_selected: bool) -> MarkerPalette {
    if is_selected {
        ENTITY_SELECTED_PALETTE
    } else {
        match kind {
            SceneEntityKind::Actor => ENTITY_ACTOR_PALETTE,
            SceneEntityKind::Object => ENTITY_OBJECT_PALETTE,
            SceneEntityKind::InterestActor => ENTITY_INTEREST_PALETTE,
        }
    }
}

pub(super) enum MarkerProjection<'a> {
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
    pub fn project(&self, position: [f32; 3]) -> Option<[f32; 2]> {
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
                let scaled_h = clamp_h * (1.0 - MAP_MARGIN * 2.0) + MAP_MARGIN;
                let scaled_v = clamp_v * (1.0 - MAP_MARGIN * 2.0) + MAP_MARGIN;
                let clip_x = scaled_h * 2.0 - 1.0;
                let clip_y = (1.0 - scaled_v) * 2.0 - 1.0;
                Some([clip_x, clip_y])
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
pub(super) struct MinimapLayout {
    pub center: [f32; 2],
    pub half_extent_x: f32,
    pub half_extent_y: f32,
}

impl MinimapLayout {
    pub fn from_rect(rect: ViewportRect, window: PhysicalSize<u32>) -> Option<Self> {
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

    pub fn panel_width(&self) -> f32 {
        self.half_extent_x * 2.0
    }

    pub fn panel_height(&self) -> f32 {
        self.half_extent_y * 2.0
    }

    pub fn scaled_size(&self, fraction: f32) -> f32 {
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
mod marker_projection_tests {
    use super::*;

    #[test]
    fn top_down_projection_clamps_coordinates() {
        let projection = MarkerProjection::TopDown {
            horizontal_axis: 0,
            vertical_axis: 2,
            horizontal_min: -10.0,
            vertical_min: -5.0,
            horizontal_span: 20.0,
            vertical_span: 10.0,
        };

        let outside = projection.project([25.0, 0.0, -17.0]).expect("projection");
        assert!(outside[0].abs() <= 1.0);
        assert!(outside[1].abs() <= 1.0);
    }
}

#[cfg(test)]
mod minimap_layout_tests {
    use super::*;

    #[test]
    fn minimap_layout_preserves_top_down_orientation() {
        let layout = MinimapLayout::from_rect(
            ViewportRect {
                x: 0.1,
                y: 0.1,
                width: 0.4,
                height: 0.4,
            },
            PhysicalSize::new(800, 600),
        )
        .expect("minimap layout");

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

#[cfg(test)]
mod marker_palette_tests {
    use super::*;

    #[test]
    fn entity_palettes_match_expected_colours() {
        let actor = entity_palette(SceneEntityKind::Actor, false);
        assert_eq!(actor.color, ENTITY_ACTOR_PALETTE.color);
        assert_eq!(actor.highlight, 0.0);

        let object = entity_palette(SceneEntityKind::Object, false);
        assert_eq!(object.color, ENTITY_OBJECT_PALETTE.color);
        assert_eq!(object.highlight, 0.0);

        let interest = entity_palette(SceneEntityKind::InterestActor, false);
        assert_eq!(interest.color, ENTITY_INTEREST_PALETTE.color);
        assert_eq!(interest.highlight, 0.0);
    }

    #[test]
    fn selected_palette_overrides_colour_and_highlight() {
        let selected = entity_palette(SceneEntityKind::Actor, true);
        assert_eq!(selected.color, ENTITY_SELECTED_PALETTE.color);
        assert_eq!(selected.highlight, ENTITY_SELECTED_PALETTE.highlight);
    }

    #[test]
    fn anchor_palettes_remain_in_sync() {
        assert_eq!(DESK_ANCHOR_PALETTE.highlight, 0.45);
        assert_eq!(TUBE_ANCHOR_PALETTE.highlight, 0.65);
        assert_eq!(MANNY_ANCHOR_PALETTE.highlight, 1.0);

        assert_eq!(DESK_ANCHOR_PALETTE.color, [0.28, 0.82, 0.52]);
        assert_eq!(TUBE_ANCHOR_PALETTE.color, [0.98, 0.74, 0.28]);
        assert_eq!(MANNY_ANCHOR_PALETTE.color, [0.2, 0.95, 0.85]);
    }
}
