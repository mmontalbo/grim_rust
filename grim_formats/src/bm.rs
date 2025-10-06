use anyhow::{Context, Result, bail, ensure};
use std::convert::TryInto;

const MAGIC_PRIMARY: &[u8; 4] = b"BM  ";
const MAGIC_SECONDARY: &[u8; 4] = b"F\0\0\0";
const HEADER_SIZE: usize = 0x80;

#[derive(Debug, Clone)]
pub struct BmFile {
    pub codec: u32,
    pub bits_per_pixel: u32,
    pub image_count: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub frames: Vec<BmFrame>,
}

impl BmFile {
    pub fn metadata(&self) -> BmMetadata {
        BmMetadata {
            codec: self.codec,
            bits_per_pixel: self.bits_per_pixel,
            image_count: self.image_count,
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BmFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl BmFrame {
    pub fn as_rgba8888(&self, metadata: &BmMetadata) -> Result<Vec<u8>> {
        match metadata.format {
            1 => match metadata.bits_per_pixel {
                16 => convert_rgb565_to_rgba8888(&self.data),
                32 => convert_rgba8888_le(&self.data),
                other => bail!("unsupported bits-per-pixel value {other} for BM preview"),
            },
            5 => match metadata.bits_per_pixel {
                16 => convert_zbuffer16_to_rgba8888(&self.data),
                other => bail!("unsupported bits-per-pixel value {other} for BM zbuffer preview"),
            },
            other => bail!("unsupported BM format {other} for preview"),
        }
    }

    pub fn depth_stats(&self, metadata: &BmMetadata) -> Result<DepthStats> {
        ensure!(
            metadata.format == 5,
            "depth stats requested for non-depth format {}",
            metadata.format
        );
        ensure!(
            metadata.bits_per_pixel == 16,
            "depth stats only supported for 16bpp surfaces (got {}bpp)",
            metadata.bits_per_pixel
        );
        ensure!(
            self.data.len() % 2 == 0,
            "depth buffer payload must be a multiple of 2 bytes"
        );

        const DEPTH_SENTINEL: u16 = 0xF81F;

        let mut min_value = u16::MAX;
        let mut max_value = u16::MIN;
        let mut zero_pixels = 0usize;
        let mut nonzero_pixels = 0usize;

        for chunk in self.data.chunks_exact(2) {
            let mut value = u16::from_le_bytes([chunk[0], chunk[1]]);
            if value == DEPTH_SENTINEL {
                value = 0;
            }
            if value == 0 {
                zero_pixels += 1;
            } else {
                nonzero_pixels += 1;
            }
            if value < min_value {
                min_value = value;
            }
            if value > max_value {
                max_value = value;
            }
        }

        if zero_pixels + nonzero_pixels == 0 {
            min_value = 0;
            max_value = 0;
        } else if min_value == u16::MAX {
            min_value = 0;
        }

        Ok(DepthStats {
            min: min_value,
            max: max_value,
            zero_pixels,
            nonzero_pixels,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmMetadata {
    pub codec: u32,
    pub bits_per_pixel: u32,
    pub image_count: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthStats {
    pub min: u16,
    pub max: u16,
    pub zero_pixels: usize,
    pub nonzero_pixels: usize,
}

impl DepthStats {
    pub fn total_pixels(&self) -> usize {
        self.zero_pixels + self.nonzero_pixels
    }
}

fn parse_bm_header(bytes: &[u8]) -> Result<(BmMetadata, usize)> {
    ensure!(
        bytes.len() >= HEADER_SIZE + 8,
        "BM payload shorter than header"
    );
    ensure!(
        &bytes[0..4] == MAGIC_PRIMARY,
        "BM image missing primary magic header"
    );
    ensure!(
        &bytes[4..8] == MAGIC_SECONDARY,
        "BM image missing secondary magic header"
    );

    let codec = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let _palette_included = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let image_count = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let _x = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let _y = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let _transparent_color = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
    let format = u32::from_le_bytes(bytes[32..36].try_into().unwrap());
    let bits_per_pixel = u32::from_le_bytes(bytes[36..40].try_into().unwrap());

    ensure!(image_count >= 1, "BM image reports zero frames");
    ensure!(
        bits_per_pixel % 8 == 0,
        "invalid bits-per-pixel value {bits_per_pixel}"
    );

    ensure!(
        bytes.len() >= HEADER_SIZE + 8,
        "BM header truncated at dimension table"
    );
    let width = u32::from_le_bytes(bytes[HEADER_SIZE..HEADER_SIZE + 4].try_into().unwrap());
    let height = u32::from_le_bytes(bytes[HEADER_SIZE + 4..HEADER_SIZE + 8].try_into().unwrap());
    ensure!(
        width > 0 && height > 0,
        "BM image reports zero width or height"
    );

    let metadata = BmMetadata {
        codec,
        bits_per_pixel,
        image_count,
        width,
        height,
        format,
    };
    let bytes_per_pixel = (bits_per_pixel / 8) as usize;

    Ok((metadata, bytes_per_pixel))
}

pub fn peek_bm_metadata(bytes: &[u8]) -> Result<BmMetadata> {
    let (metadata, _) = parse_bm_header(bytes)?;
    Ok(metadata)
}

pub fn decode_bm(bytes: &[u8]) -> Result<BmFile> {
    decode_bm_with_seed(bytes, None)
}

pub fn decode_bm_with_seed(bytes: &[u8], initial_seed: Option<&[u8]>) -> Result<BmFile> {
    let (metadata, bytes_per_pixel) = parse_bm_header(bytes)?;
    let mut frames: Vec<BmFrame> = Vec::with_capacity(metadata.image_count as usize);
    let mut offset = HEADER_SIZE;

    for frame_index in 0..metadata.image_count {
        ensure!(
            offset + 8 <= bytes.len(),
            "frame {frame_index} missing dimension block"
        );
        let frame_width = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let frame_height = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        offset += 8;

        if frame_width != metadata.width || frame_height != metadata.height {
            eprintln!(
                "[grim_formats] warning: frame {frame_index} has mismatched dimensions {}x{} (expected {}x{})",
                frame_width, frame_height, metadata.width, metadata.height
            );
        }

        let pixel_count = frame_width as usize * frame_height as usize;
        let raw_size = pixel_count
            .checked_mul(bytes_per_pixel)
            .context("BM pixel buffer size overflow")?;

        let data = match metadata.codec {
            0 => {
                ensure!(
                    offset + raw_size <= bytes.len(),
                    "frame {frame_index} raw pixel data truncated"
                );
                let slice = &bytes[offset..offset + raw_size];
                offset += raw_size;
                slice.to_vec()
            }
            3 => {
                ensure!(
                    offset + 4 <= bytes.len(),
                    "frame {frame_index} missing compression header"
                );
                let compressed_len =
                    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                ensure!(
                    offset + compressed_len <= bytes.len(),
                    "frame {frame_index} compressed data truncated"
                );
                let compressed = &bytes[offset..offset + compressed_len];
                offset += compressed_len;
                let mut buffer = vec![0u8; raw_size];
                let seed = if frame_index == 0 {
                    initial_seed
                } else {
                    Some(&frames[(frame_index - 1) as usize].data[..])
                };
                decompress_codec3(compressed, &mut buffer, seed)
                    .with_context(|| format!("decompressing frame {frame_index}"))?;
                buffer
            }
            other => bail!("unsupported BM codec {other}"),
        };

        frames.push(BmFrame {
            width: frame_width,
            height: frame_height,
            data,
        });
    }

    Ok(BmFile {
        codec: metadata.codec,
        bits_per_pixel: metadata.bits_per_pixel,
        image_count: metadata.image_count,
        width: metadata.width,
        height: metadata.height,
        format: metadata.format,
        frames,
    })
}

fn read_u16_from(stream: &mut &[u8]) -> Result<u16> {
    ensure!(
        stream.len() >= 2,
        "codec3 stream exhausted while reading u16"
    );
    let value = u16::from_le_bytes([stream[0], stream[1]]);
    *stream = &stream[2..];
    Ok(value)
}

fn read_u8_from(stream: &mut &[u8]) -> Result<u8> {
    ensure!(
        stream.len() >= 1,
        "codec3 stream exhausted while reading byte"
    );
    let value = stream[0];
    *stream = &stream[1..];
    Ok(value)
}

fn decompress_codec3(compressed: &[u8], result: &mut [u8], seed: Option<&[u8]>) -> Result<()> {
    ensure!(
        compressed.len() >= 2,
        "BM codec3 payload too small for bitstream initialiser"
    );

    const WINDOW_SIZE: usize = 0x1000;
    let mut window = vec![0u8; WINDOW_SIZE + result.len()];
    if let Some(seed) = seed {
        ensure!(
            seed.len() == result.len(),
            "codec3 seed length {} does not match target length {}",
            seed.len(),
            result.len()
        );
        window[WINDOW_SIZE..WINDOW_SIZE + result.len()].copy_from_slice(seed);
        let seed_tail = seed.len().min(WINDOW_SIZE);
        let start = WINDOW_SIZE - seed_tail;
        window[start..WINDOW_SIZE].copy_from_slice(&seed[seed.len() - seed_tail..]);
    }

    let mut stream = compressed;
    let mut bitstr_value = read_u16_from(&mut stream)? as u32;
    let mut bitstr_len = 16u32;

    let mut get_bit = |data: &mut &[u8]| -> Result<u32> {
        let bit = bitstr_value & 1;
        bitstr_len -= 1;
        bitstr_value >>= 1;
        if bitstr_len == 0 {
            bitstr_value = read_u16_from(data)? as u32;
            bitstr_len = 16;
        }
        Ok(bit)
    };

    let mut write_pos = WINDOW_SIZE;

    while write_pos - WINDOW_SIZE < result.len() {
        let bit = get_bit(&mut stream)?;
        if bit == 1 {
            ensure!(
                !stream.is_empty(),
                "codec3 stream exhausted while reading literal"
            );
            let value = read_u8_from(&mut stream)?;
            window[write_pos] = value;
            write_pos += 1;
            continue;
        }

        let bit = get_bit(&mut stream)?;
        let (mut copy_len, copy_offset): (usize, isize) = if bit == 0 {
            let first = get_bit(&mut stream)? as usize;
            let mut copy_len = first * 2;
            let second = get_bit(&mut stream)? as usize;
            copy_len += second + 3;
            let offset_byte = read_u8_from(&mut stream)? as i32;
            (copy_len, offset_byte as isize - 0x100)
        } else {
            let lower = read_u8_from(&mut stream)? as i32;
            let upper = read_u8_from(&mut stream)? as i32;
            let copy_offset = (lower | ((upper & 0xF0) << 4)) - 0x1000;
            let mut copy_len = (upper & 0x0F) + 3;
            if copy_len == 3 {
                let extended = read_u8_from(&mut stream)? as i32 + 1;
                if extended == 1 {
                    break;
                }
                copy_len = extended;
            }
            (copy_len as usize, copy_offset as isize)
        };

        while copy_len > 0 && write_pos - WINDOW_SIZE < result.len() {
            let src_index = write_pos as isize + copy_offset;
            ensure!(
                (0..write_pos as isize).contains(&src_index),
                "codec3 copy source out of bounds: {src_index} (write_pos={write_pos}, copy_offset={copy_offset})"
            );
            let value = window[src_index as usize];
            window[write_pos] = value;
            write_pos += 1;
            copy_len -= 1;
        }
    }

    result.copy_from_slice(&window[WINDOW_SIZE..WINDOW_SIZE + result.len()]);
    Ok(())
}

fn convert_rgb565_to_rgba8888(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(data.len() % 2 == 0, "RGB565 buffer must be even length");
    let pixel_count = data.len() / 2;
    let mut rgba = Vec::with_capacity(pixel_count * 4);

    for chunk in data.chunks_exact(2) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        let r = ((value >> 11) & 0x1F) as u8;
        let g = ((value >> 5) & 0x3F) as u8;
        let b = (value & 0x1F) as u8;
        rgba.push((r << 3) | (r >> 2));
        rgba.push((g << 2) | (g >> 4));
        rgba.push((b << 3) | (b >> 2));
        rgba.push(0xFF);
    }

    Ok(rgba)
}

fn convert_rgba8888_le(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        data.len() % 4 == 0,
        "RGBA8888 buffer must be a multiple of 4 bytes"
    );
    let mut rgba = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(4) {
        // Source ordering appears as little-endian BGRA in the original engine.
        let b = chunk[0];
        let g = chunk[1];
        let r = chunk[2];
        let a = chunk[3];
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Ok(rgba)
}

fn convert_zbuffer16_to_rgba8888(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        data.len() % 2 == 0,
        "Z-buffer payload must be a multiple of 2 bytes"
    );

    let mut min_value = u16::MAX;
    let mut max_value = u16::MIN;

    for chunk in data.chunks_exact(2) {
        let mut value = u16::from_le_bytes([chunk[0], chunk[1]]);
        if value == 0xF81F {
            value = 0;
        }
        min_value = min_value.min(value);
        max_value = max_value.max(value);
    }

    if min_value == u16::MAX && max_value == u16::MIN {
        return Ok(vec![0u8; data.len() / 2 * 4]);
    }

    let range = max_value.saturating_sub(min_value);
    let mut rgba = Vec::with_capacity(data.len() * 2);

    for chunk in data.chunks_exact(2) {
        let mut value = u16::from_le_bytes([chunk[0], chunk[1]]);
        if value == 0xF81F {
            value = 0;
        }
        let normalized = if range == 0 {
            0.0
        } else {
            (value.saturating_sub(min_value)) as f32 / range as f32
        };
        let gray = (normalized * 255.0).round().clamp(0.0, 255.0) as u8;
        rgba.extend_from_slice(&[gray, gray, gray, 0xFF]);
    }

    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    #[test]
    fn decodes_minimal_raw_bitmap() {
        // Build a synthetic 1x1 bitmap with codec 0 and a single magenta pixel.
        let mut data = vec![0u8; HEADER_SIZE + 8 + 2];
        data[0..4].copy_from_slice(MAGIC_PRIMARY);
        data[4..8].copy_from_slice(MAGIC_SECONDARY);
        data[8..12].copy_from_slice(&0u32.to_le_bytes()); // codec 0
        data[32..36].copy_from_slice(&1u32.to_le_bytes()); // format (color)
        data[16..20].copy_from_slice(&1u32.to_le_bytes()); // image count
        data[36..40].copy_from_slice(&16u32.to_le_bytes()); // bpp
        data[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&1u32.to_le_bytes()); // width
        data[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&1u32.to_le_bytes()); // height
        let pixel_offset = HEADER_SIZE + 8;
        data[pixel_offset..pixel_offset + 2].copy_from_slice(&0xF81Fu16.to_le_bytes());

        let metadata = peek_bm_metadata(&data).expect("peek succeeds");
        assert_eq!(metadata.codec, 0);
        assert_eq!(metadata.bits_per_pixel, 16);
        assert_eq!(metadata.image_count, 1);
        assert_eq!(metadata.width, 1);
        assert_eq!(metadata.height, 1);

        let bm = decode_bm(&data).expect("decode succeeds");
        assert_eq!(bm.width, 1);
        assert_eq!(bm.height, 1);
        assert_eq!(bm.bits_per_pixel, 16);
        assert_eq!(bm.image_count, 1);
        assert_eq!(bm.frames.len(), 1);
        let rgba = bm.frames[0].as_rgba8888(&bm.metadata()).unwrap();
        assert_eq!(rgba, vec![0xFF, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn decodes_zbm_with_external_seed() {
        let base_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../artifacts/manny_assets/mo_6_mnycu.bm");
        let delta_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../artifacts/manny_assets/mo_6_mnycu.zbm");

        let base_bytes = fs::read(&base_path).expect("read base bm");
        let delta_bytes = fs::read(&delta_path).expect("read delta zbm");

        let base_bm = decode_bm(&base_bytes).expect("decode base bm");
        assert_eq!(base_bm.frames.len(), 1, "expected single frame base");
        let base_frame = &base_bm.frames[0];

        let delta_bm = decode_bm_with_seed(&delta_bytes, Some(&base_frame.data))
            .expect("decode delta with seed");
        assert_eq!(delta_bm.frames.len(), 1, "expected single frame delta");
        let delta_frame = &delta_bm.frames[0];

        assert_eq!(delta_bm.codec, 3, "expected codec3 delta");
        assert_eq!(delta_bm.format, 5, "expected zbuffer format for delta");
        assert_eq!(delta_bm.bits_per_pixel, base_bm.bits_per_pixel);
        assert_eq!(delta_bm.width, base_bm.width);
        assert_eq!(delta_bm.height, base_bm.height);
        assert_eq!(delta_frame.data.len(), base_frame.data.len());

        let checksum = seeded_frame_checksum(&delta_frame.data);
        assert_eq!(checksum, 11_233_156_562_487_960_357);
    }

    #[test]
    fn codec3_seed_window_reuses_trailing_seed_bytes() {
        let seed: Vec<u8> = vec![0x11, 0x22, 0x33, 0x44];
        let mut output = vec![0u8; seed.len()];
        let compressed = [0x08, 0x00, 0xFC];

        decompress_codec3(&compressed, &mut output, Some(&seed))
            .expect("decode with seed succeeds");

        assert_eq!(
            output, seed,
            "codec3 copy should source bytes from the seed trail"
        );
    }

    #[test]
    fn decodes_desk_delta_without_corruption() {
        let base_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../artifacts/manny_assets/mo_0_ddtws.bm");
        let delta_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../artifacts/manny_assets/mo_0_ddtws.zbm");

        let base_bytes = fs::read(&base_path).expect("read base bm");
        let delta_bytes = fs::read(&delta_path).expect("read delta zbm");

        let base_bm = decode_bm(&base_bytes).expect("decode base bm");
        let base_frame = base_bm.frames.first().expect("base frame present");

        let candidates = [("none", None), ("base", Some(base_frame.data.as_slice()))];

        for (label, seed) in candidates {
            let delta_bm = decode_bm_with_seed(&delta_bytes, seed)
                .unwrap_or_else(|err| panic!("decode delta with seed {label}: {err}"));
            let delta_frame = delta_bm.frames.first().expect("delta frame present");

            assert_eq!(delta_frame.data.len(), base_frame.data.len());
            assert_eq!(delta_bm.format, 5, "expected Z-buffer format");
            let checksum = seeded_frame_checksum(&delta_frame.data);
            let diff_bytes = delta_frame
                .data
                .iter()
                .zip(&base_frame.data)
                .filter(|(lhs, rhs)| lhs != rhs)
                .count();
            println!(
                "seed={label:>4} checksum={checksum} diff_bytes={diff_bytes}",
                label = label,
                checksum = checksum,
                diff_bytes = diff_bytes
            );
            assert_eq!(checksum, 233_610_493_010_832_586_3u64);

            if label == "base" {
                let stats = delta_frame
                    .depth_stats(&delta_bm.metadata())
                    .expect("depth stats available");
                assert_eq!(stats.min, 0x0007);
                assert_eq!(stats.max, 0xAFF5);
                assert_eq!(stats.zero_pixels, 0);
                assert_eq!(
                    stats.nonzero_pixels,
                    (delta_bm.width * delta_bm.height) as usize
                );
                assert_eq!(stats.total_pixels(), stats.nonzero_pixels);
            }
        }
    }

    fn seeded_frame_checksum(data: &[u8]) -> u64 {
        let mut acc = 0xcbf29ce484222325u64; // FNV-1a offset basis
        for byte in data {
            acc ^= *byte as u64;
            acc = acc.wrapping_mul(0x100000001b3);
        }
        acc
    }
}
