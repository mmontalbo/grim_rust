use anyhow::Result;
use bytemuck::{Pod, Zeroable, cast_slice};
use font8x8::legacy::BASIC_MODERN;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

use crate::audio::AudioStatus;
use crate::cli::PanelPreset;
use crate::scene::{HotspotEventKind, ViewerScene};
use crate::texture::prepare_rgba_upload;
use crate::ui_layout::{PanelSize, ViewportRect};

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
    const GLYPH_WIDTH: u32 = 8;
    const GLYPH_HEIGHT: u32 = 8;
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
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OverlayVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

pub(super) fn audio_overlay_lines(status: &AudioStatus) -> Vec<String> {
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

        if let Some(created) = entity.created_by.as_ref() {
            detail_lines.push(format!("  Hook {created}"));
        }

        for line in detail_lines {
            lines.push(truncate_line(&line, MAX_LINE));
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

fn glyph_for_char(ch: char) -> [u8; 8] {
    let index = ch as usize;
    if index < BASIC_MODERN.len() {
        BASIC_MODERN[index]
    } else {
        BASIC_MODERN[b'?' as usize]
    }
}
