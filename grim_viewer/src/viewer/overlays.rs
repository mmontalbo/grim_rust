use anyhow::Result;
use bytemuck::{Pod, Zeroable, cast_slice};
use fontdue::{Font, FontSettings, Metrics};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::mem;
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

use crate::audio::AudioStatus;
use crate::cli::PanelPreset;
use crate::scene::{HotspotEventKind, ViewerScene};
use crate::texture::prepare_rgba_upload;
use crate::ui_layout::{PanelSize, ViewportRect};

const FONT_SIZE_PX: f32 = 16.0;

static FONT_DATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dev-install/FontsHD/OCRA.ttf"
));
static FONT: Lazy<Font> = Lazy::new(|| {
    Font::from_bytes(FONT_DATA, FontSettings::default())
        .expect("failed to load OCRA font for overlays")
});
static GLYPH_LAYOUT: Lazy<GlyphLayout> = Lazy::new(|| GlyphLayout::from_font(&*FONT, FONT_SIZE_PX));
static GLYPH_CACHE: Lazy<Mutex<HashMap<char, GlyphBitmap>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub(super) struct OverlayConfig {
    pub width: u32,
    pub height: u32,
    pub padding_x: u32,
    pub padding_y: u32,
    pub label: &'static str,
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
    pub fn with_preset(mut self, preset: Option<&PanelPreset>) -> Self {
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

pub(super) struct TextOverlay {
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
    const FG_COLOR: [u8; 4] = [255, 255, 255, 240];
    const BG_COLOR: [u8; 4] = [0, 0, 0, 96];

    pub fn new(
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

    pub fn update_layout(
        &mut self,
        device: &wgpu::Device,
        window_size: PhysicalSize<u32>,
        rect: ViewportRect,
    ) {
        self.layout_rect = rect;
        self.vertex_buffer = Self::create_vertex_buffer(device, window_size, rect, self.label);
    }

    pub fn set_lines(&mut self, lines: &[String]) {
        Self::fill_background(&mut self.pixels);

        let usable_width = self.width.saturating_sub(self.padding_x * 2);
        let usable_height = self.height.saturating_sub(self.padding_y * 2);
        if usable_width == 0 || usable_height == 0 {
            self.dirty = true;
            self.visible = !lines.is_empty();
            return;
        }

        let layout = &*GLYPH_LAYOUT;
        let glyph_width = layout.cell_advance.max(1);
        let glyph_height = layout.line_height.max(1);
        let max_cols = (usable_width / glyph_width) as usize;
        let max_rows = (usable_height / glyph_height) as usize;

        if max_cols == 0 || max_rows == 0 {
            self.dirty = true;
            self.visible = !lines.is_empty();
            return;
        }

        let display_lines = Self::wrap_lines(lines, max_cols, max_rows);

        for (row_idx, line) in display_lines.iter().enumerate() {
            let line_top = self.padding_y + row_idx as u32 * glyph_height;
            for (col_idx, ch) in line.chars().take(max_cols).enumerate() {
                if ch == '\r' {
                    continue;
                }
                let glyph = glyph_for_char(ch);
                let glyph_col = self.padding_x + col_idx as u32 * glyph_width;
                self.blit_glyph(glyph_col, line_top, &glyph, layout);
            }
        }

        self.dirty = true;
        self.visible = !display_lines.is_empty();
    }

    fn blit_glyph(
        &mut self,
        cell_x: u32,
        line_top: u32,
        glyph: &GlyphBitmap,
        layout: &GlyphLayout,
    ) {
        if glyph.width == 0 || glyph.height == 0 {
            return;
        }

        let start_x = cell_x as i32 + layout.left_bearing + glyph.xmin;
        let baseline = line_top as i32 + layout.ascent;
        let glyph_ymax = glyph.ymin + glyph.height as i32;
        let start_y = baseline - glyph_ymax;

        for gy in 0..glyph.height {
            let dest_y = start_y + gy as i32;
            if dest_y < 0 || dest_y >= self.height as i32 {
                continue;
            }
            let dest_y = dest_y as u32;
            let source_row_offset = gy as usize * glyph.width as usize;
            for gx in 0..glyph.width {
                let coverage = glyph.alpha[source_row_offset + gx as usize];
                if coverage == 0 {
                    continue;
                }
                let dest_x = start_x + gx as i32;
                if dest_x < 0 || dest_x >= self.width as i32 {
                    continue;
                }
                let dest_x = dest_x as u32;
                let idx = ((dest_y * self.width + dest_x) * 4) as usize;
                let alpha = ((coverage as u16 * Self::FG_COLOR[3] as u16) / u8::MAX as u16) as u8;
                self.pixels[idx..idx + 4].copy_from_slice(&[
                    Self::FG_COLOR[0],
                    Self::FG_COLOR[1],
                    Self::FG_COLOR[2],
                    alpha,
                ]);
            }
        }
    }

    pub fn upload(&mut self, queue: &wgpu::Queue) {
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

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn vertex_buffer(&self) -> &wgpu::Buffer {
        &self.vertex_buffer
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    fn vertex_positions(rect: ViewportRect, window: PhysicalSize<u32>) -> [OverlayVertex; 4] {
        let width = window.width.max(1) as f32;
        let height = window.height.max(1) as f32;

        let left = (rect.x / width) * 2.0 - 1.0;
        let right = ((rect.x + rect.width) / width) * 2.0 - 1.0;
        let top = 1.0 - (rect.y / height) * 2.0;
        let bottom = 1.0 - ((rect.y + rect.height) / height) * 2.0;

        [
            OverlayVertex {
                position: [left, top],
                uv: [0.0, 0.0],
            },
            OverlayVertex {
                position: [right, top],
                uv: [1.0, 0.0],
            },
            OverlayVertex {
                position: [left, bottom],
                uv: [0.0, 1.0],
            },
            OverlayVertex {
                position: [right, bottom],
                uv: [1.0, 1.0],
            },
        ]
    }

    fn create_vertex_buffer(
        device: &wgpu::Device,
        window_size: PhysicalSize<u32>,
        rect: ViewportRect,
        label: &str,
    ) -> wgpu::Buffer {
        let vertices = Self::vertex_positions(rect, window_size);
        let vertex_label = format!("{label}-vertices");
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(vertex_label.as_str()),
            contents: cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        })
    }

    fn fill_background(buffer: &mut [u8]) {
        for chunk in buffer.chunks_exact_mut(4) {
            chunk.copy_from_slice(&Self::BG_COLOR);
        }
    }

    fn wrap_lines(lines: &[String], max_cols: usize, max_rows: usize) -> Vec<String> {
        if max_cols == 0 || max_rows == 0 {
            return Vec::new();
        }
        let mut result = Vec::new();
        for line in lines {
            if result.len() >= max_rows {
                break;
            }
            for segment in line.split('\n') {
                if result.len() >= max_rows {
                    break;
                }
                Self::wrap_segment(&mut result, segment, max_cols, max_rows);
            }
        }
        result
    }

    fn wrap_segment(out: &mut Vec<String>, segment: &str, max_cols: usize, max_rows: usize) {
        if out.len() >= max_rows {
            return;
        }
        if segment.is_empty() {
            out.push(String::new());
            return;
        }

        let mut buffer = String::new();
        let mut count = 0;
        for ch in segment.chars() {
            buffer.push(ch);
            count += 1;
            if count == max_cols {
                if out.len() >= max_rows {
                    return;
                }
                out.push(mem::take(&mut buffer));
                count = 0;
            }
        }

        if count > 0 && out.len() < max_rows {
            out.push(buffer);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OverlayVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

#[derive(Clone)]
struct GlyphBitmap {
    width: u32,
    height: u32,
    xmin: i32,
    ymin: i32,
    alpha: Arc<[u8]>,
}

struct GlyphLayout {
    line_height: u32,
    cell_advance: u32,
    ascent: i32,
    left_bearing: i32,
}

impl GlyphBitmap {
    fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            xmin: 0,
            ymin: 0,
            alpha: Arc::<[u8]>::from([]),
        }
    }
}

impl GlyphLayout {
    fn from_font(font: &Font, size: f32) -> Self {
        let mut min_xmin = 0;
        let mut max_xmax = 0;
        let mut min_ymin = 0;
        let mut max_ymax = 0;
        let mut max_advance = 0.0f32;
        let mut initialized = false;

        let mut sample_chars: Vec<char> = (32u8..=126).map(|b| b as char).collect();
        sample_chars.push('?');
        sample_chars.push('…');

        for ch in sample_chars {
            Self::accumulate_metrics(
                font,
                size,
                ch,
                &mut min_xmin,
                &mut max_xmax,
                &mut min_ymin,
                &mut max_ymax,
                &mut max_advance,
                &mut initialized,
            );
        }

        if !initialized {
            return Self {
                line_height: 1,
                cell_advance: 1,
                ascent: 0,
                left_bearing: 0,
            };
        }

        let left_bearing = -min_xmin;
        let descent = -min_ymin;
        let ascent = max_ymax;
        let cell_width = (left_bearing + max_xmax).max(1) as u32;
        let advance = max_advance.max(cell_width as f32).ceil() as u32;
        let line_height = (ascent + descent).max(1) as u32;

        Self {
            line_height,
            cell_advance: advance.max(1),
            ascent,
            left_bearing,
        }
    }

    fn accumulate_metrics(
        font: &Font,
        size: f32,
        ch: char,
        min_xmin: &mut i32,
        max_xmax: &mut i32,
        min_ymin: &mut i32,
        max_ymax: &mut i32,
        max_advance: &mut f32,
        initialized: &mut bool,
    ) {
        let glyph_index = font.lookup_glyph_index(ch);
        let metrics: Metrics = font.metrics_indexed(glyph_index, size);
        *max_advance = (*max_advance).max(metrics.advance_width);

        if metrics.width == 0 && metrics.height == 0 {
            if !*initialized {
                *min_xmin = 0;
                *max_xmax = 0;
                *min_ymin = 0;
                *max_ymax = 0;
                *initialized = true;
            }
            return;
        }

        let xmax = metrics.xmin + metrics.width as i32;
        let ymax = metrics.ymin + metrics.height as i32;

        if !*initialized {
            *min_xmin = metrics.xmin;
            *max_xmax = xmax;
            *min_ymin = metrics.ymin;
            *max_ymax = ymax;
            *initialized = true;
        } else {
            *min_xmin = (*min_xmin).min(metrics.xmin);
            *max_xmax = (*max_xmax).max(xmax);
            *min_ymin = (*min_ymin).min(metrics.ymin);
            *max_ymax = (*max_ymax).max(ymax);
        }
    }
}

fn glyph_for_char(ch: char) -> GlyphBitmap {
    load_or_cache_glyph(ch)
        .or_else(|| load_or_cache_glyph('?'))
        .unwrap_or_else(GlyphBitmap::empty)
}

fn load_or_cache_glyph(ch: char) -> Option<GlyphBitmap> {
    if let Some(glyph) = GLYPH_CACHE.lock().unwrap().get(&ch).cloned() {
        return Some(glyph);
    }

    let font = &*FONT;
    let glyph_index = font.lookup_glyph_index(ch);
    if glyph_index == 0 && ch != '?' && ch != ' ' {
        return None;
    }

    let (metrics, bitmap) = font.rasterize_indexed(glyph_index, FONT_SIZE_PX);
    let glyph = GlyphBitmap {
        width: metrics.width as u32,
        height: metrics.height as u32,
        xmin: metrics.xmin,
        ymin: metrics.ymin,
        alpha: Arc::from(bitmap.into_boxed_slice()),
    };

    let mut cache = GLYPH_CACHE.lock().unwrap();
    cache.insert(ch, glyph.clone());
    Some(glyph)
}

pub(super) fn audio_overlay_lines(status: &AudioStatus) -> Vec<String> {
    if !status.seen_events {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push("Audio Monitor".to_string());

    match status.state.current_music.as_ref() {
        Some(music) => {
            let params = if music.params.is_empty() {
                String::new()
            } else {
                format!(" [{}]", music.params.join(", "))
            };
            lines.push(truncate_line(
                &format!("Music: {}{}", music.cue, params),
                62,
            ));
        }
        None => {
            let stop = status.state.last_music_stop_mode.as_deref().unwrap_or("-");
            lines.push(truncate_line(&format!("Music: <none> (stop {stop})"), 62));
        }
    }

    let sfx_count = status.state.active_sfx.len();
    if sfx_count == 0 {
        lines.push("SFX: none".to_string());
    } else {
        let top_cues: Vec<String> = status
            .state
            .active_sfx
            .iter()
            .take(3)
            .map(|(_, entry)| entry.cue.clone())
            .collect();
        let remainder = sfx_count.saturating_sub(top_cues.len());
        let summary = if top_cues.is_empty() {
            String::new()
        } else {
            top_cues.join(", ")
        };
        let line = if summary.is_empty() {
            format!("SFX ({sfx_count})")
        } else if remainder > 0 {
            format!("SFX ({sfx_count}): {summary} (+{remainder})")
        } else {
            format!("SFX ({sfx_count}): {summary}")
        };
        lines.push(truncate_line(&line, 62));
    }

    lines
}

pub(super) fn timeline_overlay_lines(
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

    let stage_count = summary.stages.len();
    let hook_count = summary.hooks.len();
    let selected_entity = selected_index.and_then(|idx| scene.entities.get(idx));
    let has_trace = scene.movement_trace().is_some();
    let has_events = !scene.hotspot_events().is_empty();
    if selected_entity.is_none() && !has_trace && !has_events {
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
            let label = summary
                .stages
                .iter()
                .find(|stage| stage.index == stage_index)
                .map(|stage| stage.label.as_str())
                .or_else(|| entity.timeline_stage_label.as_deref())
                .unwrap_or("-");
            lines.push(truncate_line(
                &format!("Stage {stage_index:02}: {label}"),
                MAX_LINE,
            ));
        } else if let Some(label) = entity.timeline_stage_label.as_deref() {
            lines.push(truncate_line(&format!("Stage --: {label}"), MAX_LINE));
        }

        if let Some(hook_index) = entity.timeline_hook_index {
            if let Some(hook) = summary.hooks.get(hook_index) {
                let hook_name = entity
                    .timeline_hook_name
                    .as_deref()
                    .unwrap_or_else(|| hook.key.name.as_str());
                let stage_display = hook
                    .stage_index
                    .map(|value| format!("{value:02}"))
                    .unwrap_or_else(|| String::from("--"));
                let mut extras = Vec::new();
                if let Some(stage_label) = hook.stage_label.as_deref() {
                    extras.push(stage_label.to_string());
                }
                if let Some(kind) = hook.kind.as_deref() {
                    extras.push(kind.to_string());
                }
                if !hook.targets.is_empty() {
                    extras.push(format!("targets {}", hook.targets.len()));
                }
                if !hook.prerequisites.is_empty() {
                    extras.push(format!("prereqs {}", hook.prerequisites.len()));
                }
                if let Some(file) = hook.defined_in.as_deref() {
                    let location = hook
                        .defined_at_line
                        .map(|line| format!("{file}:{line}"))
                        .unwrap_or_else(|| file.to_string());
                    extras.push(location);
                }
                let extras = if extras.is_empty() {
                    String::new()
                } else {
                    format!(" | {}", extras.join(" | "))
                };
                lines.push(truncate_line(
                    &format!("Hook {hook_index:03} [{stage_display}]: {hook_name}{extras}"),
                    MAX_LINE,
                ));
            }
        } else if let Some(name) = entity.timeline_hook_name.as_deref() {
            lines.push(truncate_line(&format!("Hook --: {name}"), MAX_LINE));
        }

        if let Some(position) = entity.position {
            lines.push(truncate_line(
                &format!(
                    "Pos ({:.2}, {:.2}, {:.2})",
                    position[0], position[1], position[2]
                ),
                MAX_LINE,
            ));
        }
        if let Some(target) = entity.facing_target.as_deref() {
            if !target.is_empty() {
                lines.push(truncate_line(&format!("Facing: {target}"), MAX_LINE));
            }
        }
        if let Some(control) = entity.head_control.as_deref() {
            if !control.is_empty() {
                lines.push(truncate_line(&format!("Head: {control}"), MAX_LINE));
            }
        }
        if let Some(played) = entity.last_played.as_deref() {
            lines.push(truncate_line(&format!("Chore: {played}"), MAX_LINE));
        }
    } else {
        lines.push("  (Use Left/Right arrows to select a marker)".to_string());
        lines.push(truncate_line(
            &format!("Timeline: {stage_count} stages / {hook_count} hooks"),
            MAX_LINE,
        ));
    }

    if let Some(trace) = scene.movement_trace() {
        lines.push(String::new());
        lines.push("Movement".to_string());
        lines.push(truncate_line(
            &format!(
                "Frames {}–{} | samples {}",
                trace.first_frame,
                trace.last_frame,
                trace.sample_count()
            ),
            MAX_LINE,
        ));
        lines.push(truncate_line(
            &format!("Distance {:.2}", trace.total_distance),
            MAX_LINE,
        ));
        if let Some((min_yaw, max_yaw)) = trace.yaw_range() {
            lines.push(truncate_line(
                &format!("Yaw {:.1}→{:.1}", min_yaw, max_yaw),
                MAX_LINE,
            ));
        }
    }

    let events = scene.hotspot_events();
    if !events.is_empty() {
        lines.push(String::new());
        lines.push("Recent Events".to_string());
        let items: Vec<String> = events
            .iter()
            .take(3)
            .map(|event| {
                let frame = event
                    .frame
                    .map(|value| format!("{value:03}"))
                    .unwrap_or_else(|| String::from("--"));
                let prefix = if matches!(event.kind(), HotspotEventKind::Selection) {
                    "(sel) "
                } else {
                    ""
                };
                format!("[{frame}] {prefix}{}", event.label)
            })
            .collect();
        if !items.is_empty() {
            lines.push(truncate_line(&items.join(" | "), MAX_LINE));
        }
        if events.len() > 3 {
            lines.push(truncate_line(
                &format!("(+{} more)", events.len() - 3),
                MAX_LINE,
            ));
        }
    }

    lines
}

fn truncate_line(line: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    line.to_string()
}
