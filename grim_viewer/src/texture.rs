use std::{
    borrow::Cow,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use grim_formats::{BmFile, DepthStats, decode_bm, decode_bm_with_seed};
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use serde::Deserialize;
use serde_json;
use wgpu;

#[derive(Debug, Deserialize)]
struct AssetManifest {
    found: Vec<AssetManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct AssetManifestEntry {
    asset_name: String,
    archive_path: PathBuf,
    offset: u64,
    size: u32,
    #[serde(default)]
    metadata: Option<AssetMetadataSummary>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AssetMetadataSummary {
    Bitmap {
        codec: u32,
        bits_per_pixel: u32,
        frames: u32,
        width: u32,
        height: u32,
        supported: bool,
    },
}

pub fn load_asset_bytes(manifest_path: &Path, asset: &str) -> Result<(String, Vec<u8>, PathBuf)> {
    let data = fs::read(manifest_path)
        .with_context(|| format!("reading asset manifest {}", manifest_path.display()))?;
    let manifest: AssetManifest = serde_json::from_slice(&data)
        .with_context(|| format!("parsing asset manifest {}", manifest_path.display()))?;

    let entry = manifest
        .found
        .into_iter()
        .find(|entry| entry.asset_name.eq_ignore_ascii_case(asset))
        .ok_or_else(|| {
            anyhow!(
                "asset '{}' not listed in manifest {}",
                asset,
                manifest_path.display()
            )
        })?;

    if let Some(AssetMetadataSummary::Bitmap {
        codec, supported, ..
    }) = &entry.metadata
    {
        if !supported {
            bail!(
                "asset '{}' (codec {}) is not yet supported by the viewer; pick a classic-surface entry",
                entry.asset_name,
                codec
            );
        }
    }

    let archive_path = resolve_archive_path(manifest_path, &entry.archive_path);
    let bytes = read_asset_slice(&archive_path, entry.offset, entry.size).with_context(|| {
        format!(
            "reading {} from {}",
            entry.asset_name,
            archive_path.display()
        )
    })?;

    Ok((entry.asset_name, bytes, archive_path))
}

pub fn load_zbm_seed(manifest_path: &Path, asset: &str) -> Result<Option<BmFile>> {
    let lower = asset.to_ascii_lowercase();
    if !lower.ends_with(".zbm") || asset.len() <= 4 {
        return Ok(None);
    }

    let base_name = format!("{}{}", &asset[..asset.len() - 4], ".bm");
    match load_asset_bytes(manifest_path, &base_name) {
        Ok((base_asset, base_bytes, _)) => {
            let base_bm = decode_bm(&base_bytes)
                .with_context(|| format!("decoding base bitmap {} for {}", base_asset, asset))?;
            ensure!(
                !base_bm.frames.is_empty(),
                "base bitmap {} has no frames",
                base_asset
            );
            Ok(Some(base_bm))
        }
        Err(err) => {
            if err.to_string().contains("not listed in manifest") {
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

fn resolve_archive_path(manifest_path: &Path, archive_path: &Path) -> PathBuf {
    if archive_path.is_absolute() {
        return archive_path.to_path_buf();
    }

    let from_manifest = manifest_path
        .parent()
        .map(|parent| parent.join(archive_path))
        .unwrap_or_else(|| archive_path.to_path_buf());
    if from_manifest.exists() {
        return from_manifest;
    }

    if archive_path.exists() {
        return archive_path.to_path_buf();
    }

    from_manifest
}

fn read_asset_slice(path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("seeking to 0x{:X} in {}", offset, path.display()))?;

    let mut buffer = vec![0u8; size as usize];
    file.read_exact(&mut buffer)
        .with_context(|| format!("reading {} bytes from {}", size, path.display()))?;
    Ok(buffer)
}

#[derive(Debug, Clone)]
pub struct PreviewTexture {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    pub codec: u32,
    pub format: u32,
    pub depth_stats: Option<DepthStats>,
    pub depth_preview: bool,
}

pub fn decode_asset_texture(
    asset_name: &str,
    bytes: &[u8],
    seed_bitmap: Option<&BmFile>,
) -> Result<PreviewTexture> {
    let lower = asset_name.to_ascii_lowercase();
    if !(lower.ends_with(".bm") || lower.ends_with(".zbm")) {
        bail!("asset {asset_name} is not a BM surface");
    }

    let mut seed_slice: Option<&[u8]> = None;
    if let Some(seed) = seed_bitmap {
        if let Some(frame) = seed.frames.first() {
            seed_slice = Some(frame.data.as_slice());
        }
    }

    let bm = decode_bm_with_seed(bytes, seed_slice)?;
    let metadata = bm.metadata();
    let frame = bm
        .frames
        .first()
        .ok_or_else(|| anyhow!("BM surface has no frames"))?;
    let mut depth_stats: Option<DepthStats> = None;
    let mut used_color_seed = false;
    let mut seed_dimensions: Option<(u32, u32)> = None;
    let mut rgba = if metadata.format == 5 {
        let stats = frame.depth_stats(&metadata)?;
        depth_stats = Some(stats);
        if let Some(seed) = seed_bitmap {
            if let Some(base_frame) = seed.frames.first() {
                let base_metadata = seed.metadata();
                used_color_seed = true;
                seed_dimensions = Some((base_metadata.width, base_metadata.height));
                base_frame.as_rgba8888(&base_metadata)?
            } else {
                frame.as_rgba8888(&metadata)?
            }
        } else {
            frame.as_rgba8888(&metadata)?
        }
    } else {
        frame.as_rgba8888(&metadata)?
    };

    let depth_preview = metadata.format == 5 && !used_color_seed;

    let expected_len = (frame.width * frame.height * 4) as usize;
    if rgba.len() != expected_len {
        let (src_w, src_h) = seed_dimensions.unwrap_or((frame.width, frame.height));
        rgba = resample_rgba_nearest(&rgba, src_w, src_h, frame.width, frame.height);
    }

    if metadata.format == 5 {
        match (used_color_seed, seed_bitmap.is_some()) {
            (true, _) => {
                println!("  paired base bitmap detected; RGB preview sourced from color plate");
            }
            (false, true) => {
                println!("  base bitmap missing frame data; preview shows normalized depth");
            }
            (false, false) => {
                println!(
                    "  no base bitmap available; preview shows normalized depth buffer values"
                );
            }
        }
    }
    Ok(PreviewTexture {
        data: rgba,
        width: frame.width,
        height: frame.height,
        frame_count: bm.image_count,
        codec: bm.codec,
        format: metadata.format,
        depth_stats,
        depth_preview,
    })
}

fn resample_rgba_nearest(
    src: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<u8> {
    if src_width == 0 || src_height == 0 || dst_width == 0 || dst_height == 0 {
        return vec![0u8; (dst_width * dst_height * 4) as usize];
    }
    let mut dst = vec![0u8; (dst_width * dst_height * 4) as usize];
    for dy in 0..dst_height as usize {
        let sy = ((dy as u64 * src_height as u64) / dst_height as u64) as u32;
        let sy = sy.min(src_height.saturating_sub(1));
        for dx in 0..dst_width as usize {
            let sx = ((dx as u64 * src_width as u64) / dst_width as u64) as u32;
            let sx = sx.min(src_width.saturating_sub(1));
            let src_idx = ((sy * src_width + sx) * 4) as usize;
            let dst_idx = ((dy as u32 * dst_width + dx as u32) * 4) as usize;
            dst[dst_idx..dst_idx + 4].copy_from_slice(
                src.get(src_idx..src_idx + 4)
                    .unwrap_or(&[0u8, 0u8, 0u8, 0xFF]),
            );
        }
    }
    dst
}

#[derive(Debug, Clone)]
pub struct TextureStats {
    pub min_luma: u8,
    pub max_luma: u8,
    pub mean_luma: f32,
    pub opaque_pixels: u32,
    pub total_pixels: u32,
    pub quadrant_means: [f32; 4],
}

pub struct TextureUpload<'a> {
    data: Cow<'a, [u8]>,
    bytes_per_row: u32,
}

impl<'a> TextureUpload<'a> {
    pub fn pixels(&self) -> &[u8] {
        &self.data
    }

    pub fn bytes_per_row(&self) -> u32 {
        self.bytes_per_row
    }
}

pub fn prepare_rgba_upload<'a>(
    width: u32,
    height: u32,
    data: &'a [u8],
) -> Result<TextureUpload<'a>> {
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

pub fn dump_texture_to_png(preview: &PreviewTexture, destination: &Path) -> Result<TextureStats> {
    fs::create_dir_all(
        destination
            .parent()
            .ok_or_else(|| anyhow!("destination has no parent"))?,
    )
    .with_context(|| format!("creating {}", destination.display()))?;

    export_rgba_to_png(destination, preview.width, preview.height, &preview.data)?;
    Ok(compute_texture_stats(
        preview.width,
        preview.height,
        &preview.data,
    ))
}

fn export_rgba_to_png(path: &Path, width: u32, height: u32, data: &[u8]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let encoder = PngEncoder::new(file);
    encoder
        .write_image(data, width, height, ColorType::Rgba8.into())
        .with_context(|| format!("writing PNG to {}", path.display()))?;
    Ok(())
}

fn compute_texture_stats(width: u32, height: u32, data: &[u8]) -> TextureStats {
    let mut min_luma = u8::MAX;
    let mut max_luma = u8::MIN;
    let mut sum_luma = 0u64;
    let mut opaque_pixels = 0u32;
    let mut quadrant_sums = [0u64; 4];
    let mut quadrant_counts = [0u32; 4];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let idx = (y * width as usize + x) * 4;
            let pixel = data.get(idx..idx + 4).unwrap_or(&[0, 0, 0, 0]);
            let (r, g, b, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
            let luma = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32).round() as u8;
            min_luma = min_luma.min(luma);
            max_luma = max_luma.max(luma);
            sum_luma += luma as u64;
            if a > 0 {
                opaque_pixels += 1;
            }
            let quadrant =
                (y >= height as usize / 2) as usize * 2 + (x >= width as usize / 2) as usize;
            quadrant_sums[quadrant] += luma as u64;
            quadrant_counts[quadrant] += 1;
        }
    }

    let total_pixels = width * height;
    let mean_luma = if total_pixels == 0 {
        0.0
    } else {
        sum_luma as f32 / total_pixels as f32
    };
    let mut quadrant_means = [0.0f32; 4];
    for idx in 0..4 {
        quadrant_means[idx] = if quadrant_counts[idx] == 0 {
            0.0
        } else {
            quadrant_sums[idx] as f32 / quadrant_counts[idx] as f32
        };
    }

    TextureStats {
        min_luma,
        max_luma,
        mean_luma,
        opaque_pixels,
        total_pixels,
        quadrant_means,
    }
}

pub fn generate_placeholder_texture(bytes: &[u8], asset_name: &str) -> PreviewTexture {
    const WIDTH: u32 = 256;
    const HEIGHT: u32 = 256;
    let mut data = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    let len = bytes.len().max(1);
    let seed = asset_name
        .as_bytes()
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));

    for (idx, pixel) in data.chunks_mut(4).enumerate() {
        let base = (idx + seed as usize) % len;
        let r = bytes.get(base).copied().unwrap_or(seed);
        let g = bytes.get((base + 17) % len).copied().unwrap_or(r);
        let b = bytes.get((base + 43) % len).copied().unwrap_or(g);
        pixel[0] = r;
        pixel[1] = g;
        pixel[2] = b;
        pixel[3] = 0xFF;
    }

    PreviewTexture {
        data,
        width: WIDTH,
        height: HEIGHT,
        frame_count: 0,
        codec: 0,
        format: 0,
        depth_stats: None,
        depth_preview: false,
    }
}

pub fn preview_color(bytes: &[u8]) -> wgpu::Color {
    if bytes.is_empty() {
        return wgpu::Color::BLACK;
    }

    let mut hash = 0u64;
    for chunk in bytes.chunks(8) {
        let mut padded = [0u8; 8];
        for (idx, value) in chunk.iter().enumerate() {
            padded[idx] = *value;
        }
        hash ^= u64::from_le_bytes(padded).rotate_left(7);
    }

    let r = ((hash >> 0) & 0xFF) as f64 / 255.0;
    let g = ((hash >> 8) & 0xFF) as f64 / 255.0;
    let b = ((hash >> 16) & 0xFF) as f64 / 255.0;

    wgpu::Color { r, g, b, a: 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grim_formats::decode_bm;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    fn asset_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../artifacts/manny_assets")
            .join(name)
    }

    #[test]
    fn load_asset_bytes_accepts_relative_archive_paths() {
        let temp = tempdir().expect("temp dir");
        let manifest_path = temp.path().join("manifest.json");
        let asset_path = asset_path("mo_tube_balloon.bm");
        let local_asset = temp.path().join("mo_tube_balloon.bm");
        fs::copy(&asset_path, &local_asset).expect("copy asset");
        let size = fs::metadata(&local_asset).expect("metadata").len();

        let manifest = json!({
            "found": [
                {
                    "asset_name": "mo_tube_balloon.bm",
                    "archive_path": "mo_tube_balloon.bm",
                    "offset": 0,
                    "size": size,
                    "metadata": {
                        "type": "bitmap",
                        "codec": 3,
                        "bits_per_pixel": 16,
                        "frames": 1,
                        "width": 497,
                        "height": 132,
                        "supported": true
                    }
                }
            ]
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
        )
        .expect("write manifest");

        let (name, bytes, archive_path) =
            load_asset_bytes(&manifest_path, "mo_tube_balloon.bm").expect("load asset bytes");

        assert_eq!(name, "mo_tube_balloon.bm");
        assert_eq!(bytes.len() as u64, size);
        assert_eq!(archive_path, local_asset);
    }

    #[test]
    fn load_zbm_seed_returns_base_bitmap() {
        let temp = tempdir().expect("temp dir");
        let manifest_path = temp.path().join("manifest.json");
        let base_source = asset_path("mo_tube_balloon.bm");
        let depth_source = asset_path("mo_tube_balloon.zbm");
        let base_copy = temp.path().join("mo_tube_balloon.bm");
        let depth_copy = temp.path().join("mo_tube_balloon.zbm");
        fs::copy(&base_source, &base_copy).expect("copy base asset");
        fs::copy(&depth_source, &depth_copy).expect("copy depth asset");
        let base_size = fs::metadata(&base_copy).expect("base metadata").len();
        let depth_size = fs::metadata(&depth_copy).expect("depth metadata").len();

        let manifest = json!({
            "found": [
                {
                    "asset_name": "mo_tube_balloon.bm",
                    "archive_path": "mo_tube_balloon.bm",
                    "offset": 0,
                    "size": base_size,
                    "metadata": {
                        "type": "bitmap",
                        "codec": 3,
                        "bits_per_pixel": 16,
                        "frames": 1,
                        "width": 497,
                        "height": 132,
                        "supported": true
                    }
                },
                {
                    "asset_name": "mo_tube_balloon.zbm",
                    "archive_path": "mo_tube_balloon.zbm",
                    "offset": 0,
                    "size": depth_size,
                    "metadata": {
                        "type": "bitmap",
                        "codec": 0,
                        "bits_per_pixel": 16,
                        "frames": 1,
                        "width": 507,
                        "height": 148,
                        "supported": true
                    }
                }
            ]
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
        )
        .expect("write manifest");

        let seed = load_zbm_seed(&manifest_path, "mo_tube_balloon.zbm")
            .expect("seed lookup")
            .expect("seed available");

        let metadata = seed.metadata();
        assert_eq!(metadata.width, 497);
        assert_eq!(metadata.height, 132);
        assert_eq!(metadata.image_count, 1);
    }

    #[test]
    fn decode_asset_texture_decodes_color_and_depth_assets() {
        let base_bytes = fs::read(asset_path("mo_tube_balloon.bm")).expect("read base asset");
        let depth_bytes = fs::read(asset_path("mo_tube_balloon.zbm")).expect("read depth asset");

        let color_preview =
            decode_asset_texture("mo_tube_balloon.bm", &base_bytes, None).expect("color preview");

        assert_eq!(color_preview.width, 497);
        assert_eq!(color_preview.height, 132);
        assert_eq!(color_preview.frame_count, 1);
        assert_eq!(color_preview.codec, 3);
        assert_eq!(color_preview.format, 1);
        assert!(color_preview.depth_stats.is_none());
        assert!(!color_preview.depth_preview);
        assert_eq!(color_preview.data.len(), (497 * 132 * 4) as usize);

        let seed_bitmap = decode_bm(&base_bytes).expect("decode base bm");
        let depth_preview =
            decode_asset_texture("mo_tube_balloon.zbm", &depth_bytes, Some(&seed_bitmap))
                .expect("depth preview");

        assert_eq!(depth_preview.width, 507);
        assert_eq!(depth_preview.height, 148);
        assert_eq!(depth_preview.frame_count, 1);
        assert_eq!(depth_preview.codec, 0);
        assert_eq!(depth_preview.format, 5);
        assert_eq!(
            depth_preview.data.len(),
            (depth_preview.width * depth_preview.height * 4) as usize
        );
        let stats = depth_preview.depth_stats.expect("depth stats present");
        assert_eq!(
            stats.total_pixels() as u32,
            depth_preview.width * depth_preview.height
        );
        assert!(!depth_preview.depth_preview);
    }
}
