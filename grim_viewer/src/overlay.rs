use std::borrow::Cow;
use std::collections::HashMap;
use std::mem;
use std::sync::{Arc, Mutex};

use anyhow::{Result, ensure};
use fontdue::{Font, FontSettings, Metrics};
use once_cell::sync::Lazy;
const FONT_SIZE_PX: f32 = 18.0;

static FONT_DATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dev-install/FontsHD/OCRA.ttf"
));
static FONT: Lazy<Font> = Lazy::new(|| {
    Font::from_bytes(FONT_DATA, FontSettings::default())
        .expect("failed to load OCRA font for overlays")
});
static GLYPH_LAYOUT: Lazy<GlyphLayout> = Lazy::new(|| GlyphLayout::from_font(&FONT, FONT_SIZE_PX));
static GLYPH_CACHE: Lazy<Mutex<HashMap<char, GlyphBitmap>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub struct TextOverlay {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    padding_x: u32,
    padding_y: u32,
    dirty: bool,
    visible: bool,
    label: &'static str,
}

pub struct TextOverlayConfig {
    pub width: u32,
    pub height: u32,
    pub padding_x: u32,
    pub padding_y: u32,
    pub label: &'static str,
}

impl TextOverlayConfig {
    pub const fn new(
        width: u32,
        height: u32,
        padding_x: u32,
        padding_y: u32,
        label: &'static str,
    ) -> Self {
        Self {
            width,
            height,
            padding_x,
            padding_y,
            label,
        }
    }
}

impl TextOverlay {
    const FG_COLOR: [u8; 4] = [255, 255, 255, 235];
    const BG_COLOR: [u8; 4] = [16, 20, 28, 200];

    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        config: TextOverlayConfig,
    ) -> Result<Self> {
        let (texture, view, sampler, bind_group) = Self::create_resources(
            device,
            bind_group_layout,
            config.width,
            config.height,
            config.label,
        )?;

        let mut overlay = Self {
            texture,
            view,
            sampler,
            bind_group,
            pixels: vec![0u8; (config.width * config.height * 4) as usize],
            width: config.width,
            height: config.height,
            padding_x: config.padding_x,
            padding_y: config.padding_y,
            dirty: true,
            visible: false,
            label: config.label,
        };
        overlay.fill_background();
        overlay.upload(queue);
        Ok(overlay)
    }

    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if self.width == width && self.height == height {
            return Ok(());
        }
        let (texture, view, sampler, bind_group) =
            Self::create_resources(device, bind_group_layout, width, height, self.label)?;
        self.texture = texture;
        self.view = view;
        self.sampler = sampler;
        self.bind_group = bind_group;
        self.width = width.max(1);
        self.height = height.max(1);
        self.pixels = vec![0u8; (self.width * self.height * 4) as usize];
        self.fill_background();
        self.visible = false;
        self.dirty = true;
        self.upload(queue);
        Ok(())
    }

    pub fn set_lines(&mut self, lines: &[String]) {
        self.fill_background();

        let usable_width = self.width.saturating_sub(self.padding_x * 2);
        let usable_height = self.height.saturating_sub(self.padding_y * 2);
        if usable_width == 0 || usable_height == 0 {
            self.visible = false;
            self.dirty = true;
            return;
        }

        let layout = &*GLYPH_LAYOUT;
        let glyph_width = layout.cell_advance.max(1);
        let glyph_height = layout.line_height.max(1);
        let max_cols = (usable_width / glyph_width) as usize;
        let max_rows = (usable_height / glyph_height) as usize;

        if max_cols == 0 || max_rows == 0 {
            self.visible = false;
            self.dirty = true;
            return;
        }

        let display_lines = wrap_lines(lines, max_cols, max_rows);
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

        self.visible = !display_lines.is_empty();
        self.dirty = true;
    }

    pub fn set_label(&mut self, text: &str) {
        self.fill_background();

        let layout = &*GLYPH_LAYOUT;
        let baseline_y = self.padding_y;
        let mut current_x = self.padding_x;

        for ch in text.chars() {
            if current_x >= self.width {
                break;
            }
            if ch == '\r' {
                continue;
            }
            let glyph = glyph_for_char(ch);
            self.blit_glyph(current_x, baseline_y, &glyph, layout);
            current_x += layout.cell_advance;
        }

        self.visible = true;
        self.dirty = true;
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
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.dirty = false;
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    fn create_resources(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
        label: &str,
    ) -> Result<(
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::Sampler,
        wgpu::BindGroup,
    )> {
        let extent = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let texture_label = format!("{label}-overlay-texture");
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
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler_label = format!("{label}-overlay-sampler");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(sampler_label.as_str()),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_label = format!("{label}-overlay-bind-group");
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(bind_label.as_str()),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Ok((texture, view, sampler, bind_group))
    }

    fn fill_background(&mut self) {
        for chunk in self.pixels.chunks_exact_mut(4) {
            chunk.copy_from_slice(&Self::BG_COLOR);
        }
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
}

struct TextureUpload<'a> {
    data: Cow<'a, [u8]>,
    bytes_per_row: u32,
}

impl<'a> TextureUpload<'a> {
    fn pixels(&self) -> &[u8] {
        &self.data
    }

    fn bytes_per_row(&self) -> u32 {
        self.bytes_per_row
    }
}

fn prepare_rgba_upload<'a>(width: u32, height: u32, data: &'a [u8]) -> Result<TextureUpload<'a>> {
    ensure!(width > 0 && height > 0, "texture has no dimensions");
    let row_bytes = 4usize * width as usize;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    ensure!(
        data.len() >= row_bytes * height as usize,
        "texture buffer ({}) smaller than {}x{} RGBA ({})",
        data.len(),
        width,
        height,
        row_bytes * height as usize
    );

    if row_bytes % alignment == 0 && data.len() == row_bytes * height as usize {
        return Ok(TextureUpload {
            data: Cow::Borrowed(data),
            bytes_per_row: row_bytes as u32,
        });
    }

    let padded_row_bytes = ((row_bytes + alignment - 1) / alignment) * alignment;
    let mut buffer = vec![0u8; padded_row_bytes * height as usize];
    for row in 0..height as usize {
        let src_offset = row * row_bytes;
        if src_offset >= data.len() {
            break;
        }
        let remaining = data.len() - src_offset;
        let to_copy = remaining.min(row_bytes);
        let dst_offset = row * padded_row_bytes;
        buffer[dst_offset..dst_offset + to_copy]
            .copy_from_slice(&data[src_offset..src_offset + to_copy]);
    }

    Ok(TextureUpload {
        data: Cow::Owned(buffer),
        bytes_per_row: padded_row_bytes as u32,
    })
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
            wrap_segment(&mut result, segment, max_cols, max_rows);
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
