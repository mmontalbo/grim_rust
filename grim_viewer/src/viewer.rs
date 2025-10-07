use std::{borrow::Cow, sync::Arc};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable, cast_slice};
use font8x8::legacy::BASIC_LEGACY;
use wgpu::util::DeviceExt;
use wgpu::{self, SurfaceError};
use winit::{dpi::PhysicalSize, window::Window};

use crate::audio::AudioStatus;
use crate::cli::{LayoutPreset, PanelPreset};
use crate::scene::{
    CameraProjector, HotspotEventKind, MovementScrubber, SceneEntityKind, ViewerScene,
    event_marker_style,
};
use crate::texture::{
    PreviewTexture, generate_placeholder_texture, prepare_rgba_upload, preview_color,
};
use crate::ui_layout::{MinimapConstraints, PanelKind, PanelSize, UiLayout, ViewportRect};

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

#[derive(Clone, Copy)]
struct MarkerPalette {
    color: [f32; 3],
    highlight: f32,
}

const MANNY_ANCHOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.2, 0.95, 0.85],
    highlight: 1.0,
};
const DESK_ANCHOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.28, 0.82, 0.52],
    highlight: 0.45,
};
const TUBE_ANCHOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.98, 0.74, 0.28],
    highlight: 0.65,
};
const ENTITY_SELECTED_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.95, 0.35, 0.25],
    highlight: 1.0,
};
const ENTITY_ACTOR_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.2, 0.85, 0.6],
    highlight: 0.0,
};
const ENTITY_OBJECT_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.25, 0.6, 0.95],
    highlight: 0.0,
};
const ENTITY_INTEREST_PALETTE: MarkerPalette = MarkerPalette {
    color: [0.85, 0.7, 0.25],
    highlight: 0.0,
};

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

fn entity_palette(kind: SceneEntityKind, is_selected: bool) -> MarkerPalette {
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
                Some([
                    clamp_h * (1.0 - MAP_MARGIN * 2.0) + MAP_MARGIN,
                    1.0 - (clamp_v * (1.0 - MAP_MARGIN * 2.0) + MAP_MARGIN),
                ])
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
        assert!(outside[0] <= 1.0 && outside[0] >= 0.0);
        assert!(outside[1] <= 1.0 && outside[1] >= 0.0);
    }
}

#[cfg(test)]
mod minimap_layout_tests {
    use super::*;

    #[test]
    fn minimap_layout_preserves_top_down_orientation() {
        let layout = MinimapConstraints {
            horizontal_axis: 0,
            vertical_axis: 1,
            horizontal_min: 0.0,
            vertical_min: 0.0,
            horizontal_span: 1.0,
            vertical_span: 1.0,
            minimap_rect: ViewportRect {
                x: 0.1,
                y: 0.1,
                width: 0.4,
                height: 0.4,
            },
        };

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

        if let Some(hook_index) = entity.timeline_hook_index {
            if let Some(hook) = summary.hooks.get(hook_index) {
                let hook_name = entity
                    .timeline_hook_name
                    .as_deref()
                    .unwrap_or_else(|| hook.key.name.as_str());
                let stage_label = hook
                    .stage_label
                    .as_deref()
                    .unwrap_or_else(|| entity.timeline_stage_label.as_deref().unwrap_or("-"));
                lines.push(truncate_line(
                    &format!("  Hook {hook_index:03} [{stage_label}] {hook_name}"),
                    MAX_LINE,
                ));
                if let Some(kind) = hook.kind.as_deref() {
                    lines.push(truncate_line(&format!("    kind: {kind}"), MAX_LINE));
                }
                if !hook.targets.is_empty() {
                    let targets = hook.targets.join(", ");
                    lines.push(truncate_line(&format!("    targets: {targets}"), MAX_LINE));
                }
                if !hook.prerequisites.is_empty() {
                    let prereqs = hook.prerequisites.join(", ");
                    lines.push(truncate_line(&format!("    prereqs: {prereqs}"), MAX_LINE));
                }
                if let Some(file) = hook.defined_in.as_deref() {
                    let location = match hook.defined_at_line {
                        Some(line) => format!("{file}:{line}"),
                        None => file.to_string(),
                    };
                    lines.push(truncate_line(
                        &format!("    defined in {location}"),
                        MAX_LINE,
                    ));
                }
            }
        } else if let Some(name) = entity.timeline_hook_name.as_deref() {
            lines.push(truncate_line(&format!("  Hook -- {name}"), MAX_LINE));
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

    if !summary.stages.is_empty() {
        lines.push(String::new());
        lines.push("Timeline Stages".to_string());
        const MAX_STAGES: usize = 6;
        for stage in summary.stages.iter().take(MAX_STAGES) {
            lines.push(truncate_line(
                &format!("  {:02}: {}", stage.index, stage.label),
                MAX_LINE,
            ));
        }
        if summary.stages.len() > MAX_STAGES {
            let remaining = summary.stages.len() - MAX_STAGES;
            lines.push(truncate_line(&format!("  ... +{remaining} more"), MAX_LINE));
        }
    }

    if !summary.hooks.is_empty() {
        lines.push(String::new());
        lines.push("Timeline Hooks".to_string());
        const MAX_HOOKS: usize = 5;
        for (idx, hook) in summary.hooks.iter().enumerate().take(MAX_HOOKS) {
            let stage_index = hook
                .stage_index
                .map(|value| format!("{:02}", value))
                .unwrap_or_else(|| String::from("--"));
            lines.push(truncate_line(
                &format!("  {:03} [{stage_index}] {}", idx, hook.key.name),
                MAX_LINE,
            ));
            if let Some(kind) = hook.kind.as_deref() {
                lines.push(truncate_line(&format!("    kind: {kind}"), MAX_LINE));
            }
            if !hook.targets.is_empty() {
                lines.push(truncate_line(
                    &format!("    targets: {}", hook.targets.join(", ")),
                    MAX_LINE,
                ));
            }
        }
        if summary.hooks.len() > MAX_HOOKS {
            let remaining = summary.hooks.len() - MAX_HOOKS;
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

pub struct ViewerState {
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
    pub async fn new(
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

    pub fn window(&self) -> &Window {
        self.window.as_ref()
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
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

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
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

    pub fn render(&mut self) -> Result<(), SurfaceError> {
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

    pub fn update_audio_overlay(&mut self, status: &AudioStatus) {
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

    pub fn next_entity(&mut self) {
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

    pub fn previous_entity(&mut self) {
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

    pub fn handle_character_input(&mut self, key: &str) {
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

        let mut scrub_position: Option<[f32; 3]> = None;
        let mut highlight_event_scene_index: Option<usize> = None;
        let mut desk_position = scene.entity_position("mo.computer");
        let mut tube_hint_position = scene.entity_position("mo.tube");

        if let Some(trace) = scene.movement_trace() {
            if !trace.samples.is_empty() {
                desk_position = trace.samples.first().map(|sample| sample.position);
                tube_hint_position = trace.samples.last().map(|sample| sample.position);

                if let Some(scrubber) = self.scrubber.as_ref() {
                    scrub_position = scrubber.current_position(trace);
                    highlight_event_scene_index =
                        scrubber.highlighted_event().map(|event| event.scene_index);
                }

                let limit = 96_usize;
                let step = (trace.samples.len().max(limit) / limit).max(1);
                let path_color = [0.78, 0.58, 0.95];
                let path_size = 0.032;

                let len = trace.samples.len();
                for (idx, sample) in trace.samples.iter().enumerate().step_by(step) {
                    if idx == 0 || idx + 1 == len {
                        continue;
                    }
                    push_marker(sample.position, path_size, path_color, 0.0);
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
            }
        }

        let manny_position = scene.entity_position("manny");
        let manny_anchor = scrub_position.or(manny_position).or(desk_position);

        if let Some(position) = desk_position {
            let palette = DESK_ANCHOR_PALETTE;
            push_marker(position, 0.075, palette.color, palette.highlight);
        }

        let tube_anchor = scene
            .entity_position("mo.tube")
            .or_else(|| scene.entity_position("mo.tube.interest_actor"))
            .or(tube_hint_position);

        if let Some(position) = tube_anchor {
            let palette = TUBE_ANCHOR_PALETTE;
            push_marker(position, 0.085, palette.color, palette.highlight);
        }

        if let Some(position) = manny_anchor {
            let palette = MANNY_ANCHOR_PALETTE;
            push_marker(position, 0.1, palette.color, palette.highlight);
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
            let palette = entity_palette(entity.kind, is_selected);
            let color = palette.color;
            let highlight = palette.highlight;
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

        let mut scrub_position: Option<[f32; 3]> = None;
        let mut highlight_event_scene_index: Option<usize> = None;
        let mut desk_position = scene.entity_position("mo.computer");
        let mut tube_hint_position = scene.entity_position("mo.tube");

        if let Some(trace) = scene.movement_trace() {
            if !trace.samples.is_empty() {
                desk_position = trace.samples.first().map(|sample| sample.position);
                tube_hint_position = trace.samples.last().map(|sample| sample.position);

                if let Some(scrubber) = self.scrubber.as_ref() {
                    scrub_position = scrubber.current_position(trace);
                    highlight_event_scene_index =
                        scrubber.highlighted_event().map(|event| event.scene_index);
                }

                let limit = 96_usize;
                let step = (trace.samples.len().max(limit) / limit).max(1);
                let path_color = [0.75, 0.65, 0.95];
                let path_size = scale_size(0.032);

                let len = trace.samples.len();
                for (idx, sample) in trace.samples.iter().enumerate().step_by(step) {
                    if idx == 0 || idx + 1 == len {
                        continue;
                    }
                    push_marker(sample.position, path_size, path_color, 0.0);
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
            }
        }

        let manny_position = scene.entity_position("manny");
        let manny_anchor = scrub_position.or(manny_position).or(desk_position);

        if let Some(position) = desk_position {
            let palette = DESK_ANCHOR_PALETTE;
            push_marker(
                position,
                scale_size(0.058),
                palette.color,
                palette.highlight,
            );
        }

        let tube_anchor = scene
            .entity_position("mo.tube")
            .or_else(|| scene.entity_position("mo.tube.interest_actor"))
            .or(tube_hint_position);

        if let Some(position) = tube_anchor {
            let palette = TUBE_ANCHOR_PALETTE;
            push_marker(
                position,
                scale_size(0.064),
                palette.color,
                palette.highlight,
            );
        }

        if let Some(position) = manny_anchor {
            let palette = MANNY_ANCHOR_PALETTE;
            push_marker(position, scale_size(0.07), palette.color, palette.highlight);
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
            let palette = entity_palette(entity.kind, is_selected);
            let color = palette.color;
            let highlight = palette.highlight;
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
    @location(1) local_pos: vec2<f32>,
    @location(2) highlight: f32,
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
    out.local_pos = input.base_pos;
    out.highlight = input.highlight;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let radius = length(input.local_pos) * 1.41421356;
    let inner = 1.0 - smoothstep(0.68, 1.0, radius);
    let border_band = smoothstep(0.62, 0.94, radius) * (1.0 - smoothstep(0.94, 1.08, radius));
    let glow = input.highlight;
    let base_color = mix(input.color, vec3<f32>(1.0, 1.0, 1.0), glow * 0.35);
    let rim_color = mix(vec3<f32>(0.18, 0.2, 0.23), vec3<f32>(1.0, 1.0, 0.85), glow);
    let color = base_color * inner + rim_color * border_band;
    let alpha = max(inner, border_band * 0.9);
    if alpha < 0.03 {
        discard;
    }
    return vec4<f32>(color, alpha);
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
