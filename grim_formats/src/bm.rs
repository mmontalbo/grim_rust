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
    pub fn as_rgba8888(&self, bits_per_pixel: u32) -> Result<Vec<u8>> {
        match bits_per_pixel {
            16 => convert_rgb565_to_rgba8888(&self.data),
            32 => convert_rgba8888_le(&self.data),
            other => bail!("unsupported bits-per-pixel value {other} for BM preview"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmMetadata {
    pub codec: u32,
    pub bits_per_pixel: u32,
    pub image_count: u32,
    pub width: u32,
    pub height: u32,
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
    let _format = u32::from_le_bytes(bytes[32..36].try_into().unwrap());
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
    };
    let bytes_per_pixel = (bits_per_pixel / 8) as usize;

    Ok((metadata, bytes_per_pixel))
}

pub fn peek_bm_metadata(bytes: &[u8]) -> Result<BmMetadata> {
    let (metadata, _) = parse_bm_header(bytes)?;
    Ok(metadata)
}

pub fn decode_bm(bytes: &[u8]) -> Result<BmFile> {
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
                let seed = if frame_index > 0 {
                    Some(&frames[(frame_index - 1) as usize].data[..])
                } else {
                    None
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

fn decompress_codec3(mut compressed: &[u8], result: &mut [u8], seed: Option<&[u8]>) -> Result<()> {
    ensure!(
        compressed.len() >= 2,
        "BM codec3 payload too small for bitstream initialiser"
    );

    let mut bitstr_value = read_u16_from(&mut compressed)? as u32;
    let mut bitstr_len = 16u32;
    let mut byte_index: usize = 0;

    const WINDOW_SIZE: usize = 0x1000;
    let mut window = vec![0u8; WINDOW_SIZE + result.len()];
    if let Some(seed) = seed {
        let seed_tail = seed.len().min(WINDOW_SIZE);
        let start = WINDOW_SIZE - seed_tail;
        window[start..WINDOW_SIZE].copy_from_slice(&seed[seed.len() - seed_tail..]);
    }
    let mut write_pos = WINDOW_SIZE;

    let read_bit =
        |stream: &mut &[u8], bitstr_value: &mut u32, bitstr_len: &mut u32| -> Result<u32> {
            if *bitstr_len == 0 {
                *bitstr_value = read_u16_from(stream)? as u32;
                *bitstr_len = 16;
            }
            let bit = *bitstr_value & 1;
            *bitstr_value >>= 1;
            *bitstr_len -= 1;
            Ok(bit)
        };

    while byte_index < result.len() {
        let bit = read_bit(&mut compressed, &mut bitstr_value, &mut bitstr_len)?;
        if bit == 1 {
            if byte_index >= result.len() {
                break;
            }
            let value = read_u8_from(&mut compressed)?;
            window[write_pos] = value;
            write_pos += 1;
            byte_index += 1;
            continue;
        }

        let bit = read_bit(&mut compressed, &mut bitstr_value, &mut bitstr_len)?;
        let (mut copy_len, copy_offset) = if bit == 0 {
            let first = read_bit(&mut compressed, &mut bitstr_value, &mut bitstr_len)? as usize;
            let mut copy_len = first * 2;
            let second = read_bit(&mut compressed, &mut bitstr_value, &mut bitstr_len)? as usize;
            copy_len += second + 3;
            let offset_byte = read_u8_from(&mut compressed)? as i32;
            (copy_len, offset_byte - 0x100)
        } else {
            let lower = read_u8_from(&mut compressed)? as i32;
            let upper = read_u8_from(&mut compressed)? as i32;
            let copy_offset = (lower | ((upper & 0xF0) << 4)) - 0x1000;
            let mut copy_len = (upper & 0x0F) + 3;
            if copy_len == 3 {
                copy_len = read_u8_from(&mut compressed)? as i32 + 1;
                if copy_len == 1 {
                    return Ok(());
                }
            }
            (copy_len as usize, copy_offset)
        };

        while copy_len > 0 && byte_index < result.len() {
            let src_index = write_pos as isize + copy_offset as isize;
            ensure!(
                (0..write_pos as isize).contains(&src_index),
                "codec3 copy source out of bounds: {src_index} (write_pos={write_pos}, copy_offset={copy_offset})"
            );

            let value = window[src_index as usize];
            window[write_pos] = value;
            write_pos += 1;
            byte_index += 1;
            copy_len -= 1;
        }

        if byte_index >= result.len() {
            break;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_minimal_raw_bitmap() {
        // Build a synthetic 1x1 bitmap with codec 0 and a single magenta pixel.
        let mut data = vec![0u8; HEADER_SIZE + 8 + 2];
        data[0..4].copy_from_slice(MAGIC_PRIMARY);
        data[4..8].copy_from_slice(MAGIC_SECONDARY);
        data[8..12].copy_from_slice(&0u32.to_le_bytes()); // codec 0
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
        let rgba = bm.frames[0].as_rgba8888(bm.bits_per_pixel).unwrap();
        assert_eq!(rgba, vec![0xFF, 0x00, 0xFF, 0xFF]);
    }
}
