// SPDX-License-Identifier: GPL-2.0-or-later
//
// Blocky16 video decoder translated from ScummVM
// (engines/grim/movie/codecs/blocky16.{h,cpp}).
//
// The codec stores Blocky16-compressed 16-bit (1555) frames inside Retail SNM
// containers. The logic below mirrors the original implementation so that
// decoded frames match the retail engine byte-for-byte.

use anyhow::{Context, Result, bail, ensure};
use byteorder::{ByteOrder, LittleEndian};

const BLOCKY16_TABLE_SMALL1: [i8; 16] = [0, 1, 2, 3, 3, 3, 3, 2, 1, 0, 0, 0, 1, 2, 2, 1];
const BLOCKY16_TABLE_SMALL2: [i8; 16] = [0, 0, 0, 0, 1, 2, 3, 3, 3, 3, 2, 1, 1, 1, 2, 2];
const BLOCKY16_TABLE_BIG1: [i8; 16] = [0, 2, 5, 7, 7, 7, 7, 7, 7, 5, 2, 0, 0, 0, 0, 0];
const BLOCKY16_TABLE_BIG2: [i8; 16] = [0, 0, 0, 0, 1, 3, 4, 6, 7, 7, 7, 7, 6, 4, 3, 1];

// The original table is 512 signed 16-bit values (pairs of x/y offsets).
const BLOCKY16_TABLE: [i16; 513] = [
    0, 0, -1, -43, 6, -43, -9, -42, 13, -41, -16, -40, 19, -39, -23, -36, 26, -34, -2, -33, 4, -33,
    -29, -32, -9, -32, 11, -31, -16, -29, 32, -29, 18, -28, -34, -26, -22, -25, -1, -25, 3, -25,
    -7, -24, 8, -24, 24, -23, 36, -23, -12, -22, 13, -21, -38, -20, 0, -20, -27, -19, -4, -19, 4,
    -19, -17, -18, -8, -17, 8, -17, 18, -17, 28, -17, 39, -17, -12, -15, 12, -15, -21, -14, -1,
    -14, 1, -14, -41, -13, -5, -13, 5, -13, 21, -13, -31, -12, -15, -11, -8, -11, 8, -11, 15, -11,
    -2, -10, 1, -10, 31, -10, -23, -9, -11, -9, -5, -9, 4, -9, 11, -9, 42, -9, 6, -8, 24, -8, -18,
    -7, -7, -7, -3, -7, -1, -7, 2, -7, 18, -7, -43, -6, -13, -6, -4, -6, 4, -6, 8, -6, -33, -5, -9,
    -5, -2, -5, 0, -5, 2, -5, 5, -5, 13, -5, -25, -4, -6, -4, -3, -4, 3, -4, 9, -4, -19, -3, -7,
    -3, -4, -3, -2, -3, -1, -3, 0, -3, 1, -3, 2, -3, 4, -3, 6, -3, 33, -3, -14, -2, -10, -2, -5,
    -2, -3, -2, -2, -2, -1, -2, 0, -2, 1, -2, 2, -2, 3, -2, 5, -2, 7, -2, 14, -2, 19, -2, 25, -2,
    43, -2, -7, -1, -3, -1, -2, -1, -1, -1, 0, -1, 1, -1, 2, -1, 3, -1, 10, -1, -5, 0, -3, 0, -2,
    0, -1, 0, 1, 0, 2, 0, 3, 0, 5, 0, 7, 0, -10, 1, -7, 1, -3, 1, -2, 1, -1, 1, 0, 1, 1, 1, 2, 1,
    3, 1, -43, 2, -25, 2, -19, 2, -14, 2, -5, 2, -3, 2, -2, 2, -1, 2, 0, 2, 1, 2, 2, 2, 3, 2, 5, 2,
    7, 2, 10, 2, 14, 2, -33, 3, -6, 3, -4, 3, -2, 3, -1, 3, 0, 3, 1, 3, 2, 3, 4, 3, 19, 3, -9, 4,
    -3, 4, 3, 4, 7, 4, 25, 4, -13, 5, -5, 5, -2, 5, 0, 5, 2, 5, 5, 5, 9, 5, 33, 5, -8, 6, -4, 6, 4,
    6, 13, 6, 43, 6, -18, 7, -2, 7, 0, 7, 2, 7, 7, 7, 18, 7, -24, 8, -6, 8, -42, 9, -11, 9, -4, 9,
    5, 9, 11, 9, 23, 9, -31, 10, -1, 10, 2, 10, -15, 11, -8, 11, 8, 11, 15, 11, 31, 12, -21, 13,
    -5, 13, 5, 13, 41, 13, -1, 14, 1, 14, 21, 14, -12, 15, 12, 15, -39, 17, -28, 17, -18, 17, -8,
    17, 8, 17, 17, 18, -4, 19, 0, 19, 4, 19, 27, 19, 38, 20, -13, 21, 12, 22, -36, 23, -24, 23, -8,
    24, 7, 24, -3, 25, 1, 25, 22, 25, 34, 26, -18, 28, -32, 29, 16, 29, -11, 31, 9, 32, 29, 32, -4,
    33, 2, 33, -26, 34, 23, 36, -19, 39, 16, 40, -13, 41, 9, 42, -6, 43, 1, 43, 0, 0, 0, 0, 0, 0,
    0, 0, 0,
];

/// Helper for translating Blocky16 output from 1555 to RGBA.
fn rgb_from_1555(pixel: u16) -> [u8; 4] {
    let r = ((pixel >> 10) & 0x1F) as u8;
    let g = ((pixel >> 5) & 0x1F) as u8;
    let b = (pixel & 0x1F) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 3) | (g >> 2),
        (b << 3) | (b >> 2),
        255,
    ]
}

pub struct Blocky16Decoder {
    width: usize,
    height: usize,
    frame_size: usize,
    blocks_width: usize,
    blocks_height: usize,
    offset: usize,
    offset1: isize,
    offset2: isize,
    prev_seq: i32,
    last_table_width: usize,
    table_big: Vec<u8>,
    table_small: Vec<u8>,
    table: [i16; 256],
    delta_storage: Vec<u8>,
    delta0_start: usize,
    delta1_start: usize,
    cur_start: usize,
    d_pitch: usize,
}

impl Blocky16Decoder {
    pub fn new(width: u16, height: u16) -> Result<Self> {
        let mut decoder = Self {
            width: 0,
            height: 0,
            frame_size: 0,
            blocks_width: 0,
            blocks_height: 0,
            offset: 0,
            offset1: 0,
            offset2: 0,
            prev_seq: 0,
            last_table_width: usize::MAX,
            table_big: vec![0u8; 256 * 388],
            table_small: vec![0u8; 256 * 128],
            table: [0; 256],
            delta_storage: Vec::new(),
            delta0_start: 0,
            delta1_start: 0,
            cur_start: 0,
            d_pitch: 0,
        };
        decoder.init(width as usize, height as usize)?;
        Ok(decoder)
    }

    /// Reconfigure the decoder for a different surface size.
    pub fn reconfigure(&mut self, width: u16, height: u16) -> Result<()> {
        self.init(width as usize, height as usize)
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.width
    }

    #[inline]
    pub fn height(&self) -> usize {
        self.height
    }

    #[inline]
    pub fn frame_len(&self) -> usize {
        self.frame_size
    }

    #[inline]
    pub fn rgba_len(&self) -> usize {
        self.width * self.height * 4
    }

    fn init(&mut self, width: usize, height: usize) -> Result<()> {
        self.width = width;
        self.height = height;
        self.blocks_width = (width + 7) / 8;
        self.blocks_height = (height + 7) / 8;
        self.frame_size = width
            .checked_mul(height)
            .context("blocky16 frame size overflow")?
            .checked_mul(2)
            .context("blocky16 frame size overflow")?;

        // Allocate a single backing buffer for current and delta frames, matching the
        // original layout (current after delta0/delta1, plus padding for odd dimensions).
        let size = self
            .blocks_width
            .checked_mul(8)
            .and_then(|w| self.blocks_height.checked_mul(8).map(|h| w * h))
            .context("blocky16 backing surface overflow")?
            .checked_mul(2)
            .context("blocky16 backing surface overflow")?;
        self.offset = size.checked_sub(self.frame_size).unwrap_or(0);
        let delta_size = size
            .checked_mul(3)
            .and_then(|value| value.checked_add(200))
            .context("blocky16 delta buffer overflow")?;

        self.delta_storage = vec![0u8; delta_size];
        self.delta0_start = 0;
        self.delta1_start = self.frame_size;
        self.cur_start = self.frame_size * 2;
        self.prev_seq = 0;
        self.d_pitch = self.width * 2;

        self.make_tables_interpolation(4)?;
        self.make_tables_interpolation(8)?;
        self.make_tables_47(width);
        Ok(())
    }

    /// Decode Blocky16 payload into the provided destination buffer (packed 1555 little-endian).
    pub fn decode(&mut self, dst: &mut [u8], src: &[u8]) -> Result<()> {
        ensure!(
            dst.len() >= self.frame_size,
            "blocky16 destination too small: {} < {}",
            dst.len(),
            self.frame_size
        );
        ensure!(
            src.len() >= 560,
            "blocky16 frame payload too short: {}",
            src.len()
        );

        self.offset1 = self.pointer_delta(self.delta1_start, self.cur_start);
        self.offset2 = self.pointer_delta(self.delta0_start, self.cur_start);
        self.d_pitch = self.width * 2;

        let seq = LittleEndian::read_u16(&src[16..18]) as i32;
        let mode = src[18];
        let swap_mode = src[19];
        let param_block = &src[24..40];
        let param67_block = &src[40..560];
        let gfx = &src[560..];

        if seq == 0 {
            self.make_tables_47(self.width);
            if src[32] == src[33] {
                let value = src[32];
                self.fill_surface_u8(self.cur_start, value);
                self.fill_surface_u8(self.delta0_start, value);
                self.fill_surface_u8(self.delta1_start, value);
            } else {
                let value = LittleEndian::read_u16(&src[32..34]);
                self.fill_surface_u16(self.cur_start, value);
                self.fill_surface_u16(self.delta0_start, value);
                self.fill_surface_u16(self.delta1_start, value);
            }
            self.prev_seq = -1;
        }

        match mode {
            0 => {
                let frame_size = self.frame_size;
                ensure!(
                    gfx.len() >= frame_size,
                    "blocky16 mode0 payload too small: {}",
                    gfx.len()
                );
                let start = self.cur_start;
                self.delta_storage[start..start + frame_size].copy_from_slice(&gfx[..frame_size]);
            }
            1 => bail!("blocky16 mode 1 not implemented"),
            2 => {
                if seq == self.prev_seq + 1 {
                    self.decode2(gfx, param_block, param67_block)?;
                }
            }
            3 => {
                self.copy_surface(self.delta1_start, self.cur_start);
            }
            4 => {
                self.copy_surface(self.delta0_start, self.cur_start);
            }
            5 => {
                let size = LittleEndian::read_u32(&src[36..40]) as usize;
                self.bomp_decode_main(gfx, size)?;
            }
            6 => {
                ensure!(
                    gfx.len() >= self.frame_size / 2,
                    "blocky16 mode6 palette payload too small"
                );
                let frame_size = self.frame_size;
                let start = self.cur_start;
                let buffer = &mut self.delta_storage[start..start + frame_size];
                for (i, chunk) in buffer.chunks_exact_mut(2).enumerate() {
                    let index = gfx
                        .get(i)
                        .copied()
                        .context("blocky16 mode6 palette index missing")?;
                    let palette_offset = (index as usize) * 2;
                    let value = param67_block
                        .get(palette_offset..palette_offset + 2)
                        .context("blocky16 mode6 palette entry missing")?;
                    chunk.copy_from_slice(value);
                }
            }
            7 => bail!("blocky16 mode 7 not implemented"),
            8 => {
                let mut bomp = Bomp::new(gfx);
                let frame_size = self.frame_size;
                let start = self.cur_start;
                let buffer = &mut self.delta_storage[start..start + frame_size];
                for chunk in buffer.chunks_exact_mut(2) {
                    chunk[0] = bomp.decode();
                    chunk[1] = bomp.decode();
                }
            }
            _ => bail!("blocky16 unknown mode {}", mode),
        }

        dst[..self.frame_size].copy_from_slice(self.current_buffer());

        if seq == self.prev_seq + 1 {
            match swap_mode {
                1 => {
                    std::mem::swap(&mut self.cur_start, &mut self.delta1_start);
                }
                2 => {
                    let tmp = self.delta0_start;
                    self.delta0_start = self.delta1_start;
                    self.delta1_start = self
                        .cur_start
                        .checked_sub(self.offset)
                        .unwrap_or(self.cur_start);
                    self.cur_start = tmp;
                }
                _ => {}
            }
        }

        self.prev_seq = seq;
        Ok(())
    }

    /// Decode Blocky16 payload directly to RGBA8.
    pub fn decode_rgba(&mut self, dst: &mut [u8], src: &[u8]) -> Result<()> {
        ensure!(
            dst.len() >= self.frame_size * 2,
            "blocky16 RGBA destination too small"
        );
        let mut raw = vec![0u8; self.frame_size];
        self.decode(&mut raw, src)?;
        for (rgba, pixel) in dst.chunks_exact_mut(4).zip(raw.chunks_exact(2)) {
            let value = LittleEndian::read_u16(pixel);
            rgba.copy_from_slice(&rgb_from_1555(value));
        }
        Ok(())
    }

    fn pointer_delta(&self, first: usize, second: usize) -> isize {
        let diff = first as isize - second as isize;
        (diff / 2) * 2
    }

    fn current_buffer(&self) -> &[u8] {
        &self.delta_storage[self.cur_start..self.cur_start + self.frame_size]
    }

    fn decode2(&mut self, gfx: &[u8], param_ptr: &[u8], param67_ptr: &[u8]) -> Result<()> {
        let mut cursor = 0usize;
        let mut dst_index = self.cur_start;
        let next_line = self.width * 2 * 7;
        let mut remaining_rows = self.blocks_height;

        while remaining_rows > 0 {
            let mut blocks = self.blocks_width;
            while blocks > 0 {
                self.level1(dst_index, &mut cursor, gfx, param_ptr, param67_ptr)?;
                dst_index += 16;
                blocks -= 1;
            }
            dst_index += next_line;
            remaining_rows -= 1;
        }

        ensure!(
            cursor <= gfx.len(),
            "blocky16 decode2 cursor overflow: {} > {}",
            cursor,
            gfx.len()
        );
        Ok(())
    }

    fn level1(
        &mut self,
        dst_index: usize,
        cursor: &mut usize,
        gfx: &[u8],
        param_ptr: &[u8],
        param67_ptr: &[u8],
    ) -> Result<()> {
        let code = self.read_byte(cursor, gfx)?;
        if code <= 0xF5 {
            let mut value = if code == 0xF5 {
                let tmp = self.read_i16(cursor, gfx)?;
                (tmp as isize) * 2
            } else {
                (self.table[code as usize] as isize) * 2
            };
            value += self.offset1;
            for row in 0..8 {
                let dest = dst_index + row * self.d_pitch;
                let src = self.index_with_offset(dest, value)?;
                self.copy_line(src, dest, 16)?;
            }
        } else if code == 0xFF {
            self.level2(dst_index, cursor, gfx, param_ptr, param67_ptr)?;
            self.level2(dst_index + 8, cursor, gfx, param_ptr, param67_ptr)?;
            self.level2(
                dst_index + self.d_pitch * 4,
                cursor,
                gfx,
                param_ptr,
                param67_ptr,
            )?;
            self.level2(
                dst_index + self.d_pitch * 4 + 8,
                cursor,
                gfx,
                param_ptr,
                param67_ptr,
            )?;
        } else if code == 0xF6 {
            for row in 0..8 {
                let dest = dst_index + row * self.d_pitch;
                let src = self.index_with_offset(dest, self.offset2)?;
                self.copy_line(src, dest, 16)?;
            }
        } else if code == 0xF7 || code == 0xF8 {
            let selector = self.read_byte(cursor, gfx)?;
            let value = if code == 0xF8 {
                self.read_u32(cursor, gfx)?
            } else {
                let tmp = self.read_u16(cursor, gfx)?;
                let hi = self.read_param67(param67_ptr, (tmp >> 8) as u8)?;
                let lo = self.read_param67(param67_ptr, tmp as u8)?;
                ((hi as u32) << 16) | (lo as u32)
            };

            let base = selector as usize * 388;
            let count_true = self.table_big[base + 384] as usize;
            let count_false = self.table_big[base + 385] as usize;
            let low = value as u16;
            let high = (value >> 16) as u16;

            for idx in 0..count_true {
                let offset =
                    LittleEndian::read_u16(&self.table_big[base + idx * 2..base + idx * 2 + 2])
                        as usize;
                let dest = dst_index + offset * 2;
                self.write_u16(dest, low)?;
            }
            for idx in 0..count_false {
                let offset = LittleEndian::read_u16(
                    &self.table_big[base + 128 + idx * 2..base + 128 + idx * 2 + 2],
                ) as usize;
                let dest = dst_index + offset * 2;
                self.write_u16(dest, high)?;
            }
        } else if code >= 0xF9 {
            let value = self.read_literal(code, cursor, gfx, param_ptr, param67_ptr)?;
            for row in 0..8 {
                let dest = dst_index + row * self.d_pitch;
                self.write_u32(dest, value)?;
                self.write_u32(dest + 4, value)?;
                self.write_u32(dest + 8, value)?;
                self.write_u32(dest + 12, value)?;
            }
        }
        Ok(())
    }

    fn level2(
        &mut self,
        dst_index: usize,
        cursor: &mut usize,
        gfx: &[u8],
        param_ptr: &[u8],
        param67_ptr: &[u8],
    ) -> Result<()> {
        let code = self.read_byte(cursor, gfx)?;
        if code <= 0xF5 {
            let mut value = if code == 0xF5 {
                let tmp = self.read_i16(cursor, gfx)?;
                (tmp as isize) * 2
            } else {
                (self.table[code as usize] as isize) * 2
            };
            value += self.offset1;
            for row in 0..4 {
                let dest = dst_index + row * self.d_pitch;
                let src = self.index_with_offset(dest, value)?;
                self.copy_line(src, dest, 8)?;
            }
        } else if code == 0xFF {
            self.level3(dst_index, cursor, gfx, param_ptr, param67_ptr)?;
            self.level3(dst_index + 4, cursor, gfx, param_ptr, param67_ptr)?;
            self.level3(
                dst_index + self.d_pitch * 2,
                cursor,
                gfx,
                param_ptr,
                param67_ptr,
            )?;
            self.level3(
                dst_index + self.d_pitch * 2 + 4,
                cursor,
                gfx,
                param_ptr,
                param67_ptr,
            )?;
        } else if code == 0xF6 {
            for row in 0..4 {
                let dest = dst_index + row * self.d_pitch;
                let src = self.index_with_offset(dest, self.offset2)?;
                self.copy_line(src, dest, 8)?;
            }
        } else if code == 0xF7 || code == 0xF8 {
            let selector = self.read_byte(cursor, gfx)?;
            let value = if code == 0xF8 {
                self.read_u32(cursor, gfx)?
            } else {
                let tmp = self.read_u16(cursor, gfx)?;
                let hi = self.read_param67(param67_ptr, (tmp >> 8) as u8)?;
                let lo = self.read_param67(param67_ptr, tmp as u8)?;
                ((hi as u32) << 16) | (lo as u32)
            };

            let base = selector as usize * 128;
            let count_true = self.table_small[base + 96] as usize;
            let count_false = self.table_small[base + 97] as usize;
            let low = value as u16;
            let high = (value >> 16) as u16;

            for idx in 0..count_true {
                let offset =
                    LittleEndian::read_u16(&self.table_small[base + idx * 2..base + idx * 2 + 2])
                        as usize;
                let dest = dst_index + offset * 2;
                self.write_u16(dest, low)?;
            }
            for idx in 0..count_false {
                let offset = LittleEndian::read_u16(
                    &self.table_small[base + 32 + idx * 2..base + 32 + idx * 2 + 2],
                ) as usize;
                let dest = dst_index + offset * 2;
                self.write_u16(dest, high)?;
            }
        } else if code >= 0xF9 {
            let value = self.read_literal(code, cursor, gfx, param_ptr, param67_ptr)?;
            for row in 0..4 {
                let dest = dst_index + row * self.d_pitch;
                self.write_u32(dest, value)?;
                self.write_u32(dest + 4, value)?;
            }
        }
        Ok(())
    }

    fn level3(
        &mut self,
        dst_index: usize,
        cursor: &mut usize,
        gfx: &[u8],
        param_ptr: &[u8],
        param67_ptr: &[u8],
    ) -> Result<()> {
        let code = self.read_byte(cursor, gfx)?;
        if code <= 0xF5 {
            let mut value = if code == 0xF5 {
                let tmp = self.read_i16(cursor, gfx)?;
                (tmp as isize) * 2
            } else {
                (self.table[code as usize] as isize) * 2
            };
            value += self.offset1;
            for row in 0..2 {
                let dest = dst_index + row * self.d_pitch;
                let src = self.index_with_offset(dest, value)?;
                self.copy_line(src, dest, 4)?;
            }
        } else if code == 0xFF || code == 0xF8 {
            let values = self.read_exact(cursor, gfx, 8)?;
            self.current_write(dst_index, values)?;
            let next = dst_index + self.d_pitch;
            self.current_write(next, &values[4..])?;
        } else if code == 0xFD {
            let index = self.read_byte(cursor, gfx)?;
            let value = self.read_param67(param67_ptr, index)?;
            let packed = ((value as u32) << 16) | value as u32;
            for row in 0..2 {
                let dest = dst_index + row * self.d_pitch;
                self.write_u32(dest, packed)?;
            }
        } else if code == 0xFE {
            let value = self.read_u16(cursor, gfx)?;
            let packed = ((value as u32) << 16) | value as u32;
            for row in 0..2 {
                let dest = dst_index + row * self.d_pitch;
                self.write_u32(dest, packed)?;
            }
        } else if code == 0xF6 {
            for row in 0..2 {
                let dest = dst_index + row * self.d_pitch;
                let src = self.index_with_offset(dest, self.offset2)?;
                self.copy_line(src, dest, 4)?;
            }
        } else if code == 0xF7 {
            let value = self.read_u32(cursor, gfx)?;
            let mut tmp = value;
            let lo0 = self.read_param67(param67_ptr, (tmp & 0xFF) as u8)?;
            let lo1 = self.read_param67(param67_ptr, ((tmp >> 8) & 0xFF) as u8)?;
            tmp >>= 16;
            let hi0 = self.read_param67(param67_ptr, (tmp & 0xFF) as u8)?;
            let hi1 = self.read_param67(param67_ptr, ((tmp >> 8) & 0xFF) as u8)?;

            let row0 = dst_index;
            let row1 = dst_index + self.d_pitch;
            self.write_u16(row0, lo0)?;
            self.write_u16(row0 + 2, lo1)?;
            self.write_u16(row1, hi0)?;
            self.write_u16(row1 + 2, hi1)?;
        } else if (0xF9..=0xFC).contains(&code) {
            let value = self.read_param(param_ptr, code)?;
            let packed = ((value as u32) << 16) | value as u32;
            for row in 0..2 {
                let dest = dst_index + row * self.d_pitch;
                self.write_u32(dest, packed)?;
            }
        }
        Ok(())
    }

    fn read_literal(
        &mut self,
        code: u8,
        cursor: &mut usize,
        gfx: &[u8],
        param_ptr: &[u8],
        param67_ptr: &[u8],
    ) -> Result<u32> {
        if code == 0xFD {
            let index = self.read_byte(cursor, gfx)?;
            let value = self.read_param67(param67_ptr, index)?;
            Ok(((value as u32) << 16) | value as u32)
        } else if code == 0xFE {
            let value = self.read_u16(cursor, gfx)?;
            Ok(((value as u32) << 16) | value as u32)
        } else if (0xF9..=0xFC).contains(&code) {
            let value = self.read_param(param_ptr, code)?;
            Ok(((value as u32) << 16) | value as u32)
        } else {
            bail!("blocky16 unexpected literal code {:02x}", code);
        }
    }

    fn read_param(&self, param_ptr: &[u8], code: u8) -> Result<u16> {
        ensure!(
            (0xF9..=0xFC).contains(&code),
            "blocky16 param code out of range {:02x}",
            code
        );
        let idx = (code as usize - 0xF9) * 2;
        ensure!(
            param_ptr.len() >= idx + 2,
            "blocky16 param table too small (idx {idx})"
        );
        Ok(LittleEndian::read_u16(&param_ptr[idx..idx + 2]))
    }

    fn read_param67(&self, param67_ptr: &[u8], index: u8) -> Result<u16> {
        let idx = index as usize * 2;
        ensure!(
            param67_ptr.len() >= idx + 2,
            "blocky16 param6/7 table too small (idx {idx})"
        );
        Ok(LittleEndian::read_u16(&param67_ptr[idx..idx + 2]))
    }

    fn read_byte(&self, cursor: &mut usize, data: &[u8]) -> Result<u8> {
        if *cursor >= data.len() {
            bail!("blocky16 stream truncated");
        }
        let value = data[*cursor];
        *cursor += 1;
        Ok(value)
    }

    fn read_exact<'a>(&self, cursor: &mut usize, data: &'a [u8], len: usize) -> Result<&'a [u8]> {
        if *cursor + len > data.len() {
            bail!("blocky16 stream truncated");
        }
        let slice = &data[*cursor..*cursor + len];
        *cursor += len;
        Ok(slice)
    }

    fn read_u16(&self, cursor: &mut usize, data: &[u8]) -> Result<u16> {
        let slice = self.read_exact(cursor, data, 2)?;
        Ok(LittleEndian::read_u16(slice))
    }

    fn read_i16(&self, cursor: &mut usize, data: &[u8]) -> Result<i16> {
        let slice = self.read_exact(cursor, data, 2)?;
        Ok(LittleEndian::read_i16(slice))
    }

    fn read_u32(&self, cursor: &mut usize, data: &[u8]) -> Result<u32> {
        let slice = self.read_exact(cursor, data, 4)?;
        Ok(LittleEndian::read_u32(slice))
    }

    fn index_with_offset(&self, base: usize, offset: isize) -> Result<usize> {
        let value = base as isize + offset;
        if value < 0 || (value as usize) + 16 > self.delta_storage.len() {
            bail!("blocky16 offset outside buffer");
        }
        Ok(value as usize)
    }

    fn copy_line(&mut self, src: usize, dst: usize, len: usize) -> Result<()> {
        let storage_len = self.delta_storage.len();
        ensure!(src + len <= storage_len && dst + len <= storage_len);
        self.delta_storage.copy_within(src..src + len, dst);
        Ok(())
    }

    fn write_u16(&mut self, offset: usize, value: u16) -> Result<()> {
        ensure!(offset + 2 <= self.delta_storage.len());
        LittleEndian::write_u16(&mut self.delta_storage[offset..offset + 2], value);
        Ok(())
    }

    fn write_u32(&mut self, offset: usize, value: u32) -> Result<()> {
        ensure!(offset + 4 <= self.delta_storage.len());
        LittleEndian::write_u32(&mut self.delta_storage[offset..offset + 4], value);
        Ok(())
    }

    fn current_write(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        ensure!(offset + bytes.len() <= self.delta_storage.len());
        self.delta_storage[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    fn fill_surface_u8(&mut self, start: usize, value: u8) {
        let end = start + self.frame_size;
        for byte in &mut self.delta_storage[start..end] {
            *byte = value;
        }
    }

    fn fill_surface_u16(&mut self, start: usize, value: u16) {
        let end = start + self.frame_size;
        for chunk in self.delta_storage[start..end].chunks_mut(2) {
            LittleEndian::write_u16(chunk, value);
        }
    }

    fn copy_surface(&mut self, src: usize, dst: usize) {
        let size = self.frame_size;
        self.delta_storage.copy_within(src..src + size, dst);
    }

    fn bomp_decode_main(&mut self, src: &[u8], size: usize) -> Result<()> {
        let words = size / 2;
        let mut bomp = Bomp::new(src);
        let mut offset = self.cur_start;
        let end = self.cur_start + self.frame_size;
        for _ in 0..words {
            if offset + 1 >= end {
                break;
            }
            let lo = bomp.decode();
            let hi = bomp.decode();
            self.delta_storage[offset] = lo;
            self.delta_storage[offset + 1] = hi;
            offset += 2;
        }
        Ok(())
    }

    fn make_tables_47(&mut self, width: usize) {
        if self.last_table_width == width {
            return;
        }
        self.last_table_width = width;

        for (i, chunk) in BLOCKY16_TABLE.chunks_exact(2).enumerate() {
            let value = chunk[1] * width as i16 + chunk[0];
            self.table[i] = value;
        }

        let mut a = 0usize;
        let mut c = 0usize;
        while c < 32768 {
            let count_true = self.table_small[96 + c] as usize;
            for d in 0..count_true {
                let idx = 64 + c + d;
                let tmp = self.table_small[idx] as i16;
                let linear = ((tmp >> 2) as i16 * width as i16) + (tmp & 3) as i16;
                let dest = c + d * 2;
                self.table_small[dest] = linear as u8;
                self.table_small[dest + 1] = (linear >> 8) as u8;
            }

            let count_false = self.table_small[97 + c] as usize;
            for d in 0..count_false {
                let idx = 80 + c + d;
                let tmp = self.table_small[idx] as i16;
                let linear = ((tmp >> 2) as i16 * width as i16) + (tmp & 3) as i16;
                let dest = 32 + c + d * 2;
                self.table_small[dest] = linear as u8;
                self.table_small[dest + 1] = (linear >> 8) as u8;
            }

            let count_big_true = self.table_big[384 + a] as usize;
            for d in 0..count_big_true {
                let idx = 256 + a + d;
                let tmp = self.table_big[idx] as i16;
                let linear = ((tmp >> 3) as i16 * width as i16) + (tmp & 7) as i16;
                let dest = a + d * 2;
                self.table_big[dest] = linear as u8;
                self.table_big[dest + 1] = (linear >> 8) as u8;
            }

            let count_big_false = self.table_big[385 + a] as usize;
            for d in 0..count_big_false {
                let idx = 320 + a + d;
                let tmp = self.table_big[idx] as i16;
                let linear = ((tmp >> 3) as i16 * width as i16) + (tmp & 7) as i16;
                let dest = 128 + a + d * 2;
                self.table_big[dest] = linear as u8;
                self.table_big[dest + 1] = (linear >> 8) as u8;
            }

            a += 388;
            c += 128;
        }
    }

    fn make_tables_interpolation(&mut self, param: usize) -> Result<()> {
        let (table1, table2, storage, stride) = match param {
            8 => (
                &BLOCKY16_TABLE_BIG1,
                &BLOCKY16_TABLE_BIG2,
                &mut self.table_big,
                388usize,
            ),
            4 => (
                &BLOCKY16_TABLE_SMALL1,
                &BLOCKY16_TABLE_SMALL2,
                &mut self.table_small,
                128usize,
            ),
            other => bail!("blocky16 interpolation unsupported size {}", other),
        };

        for entry in storage.chunks_mut(stride) {
            entry.fill(0);
        }

        let mut s = 0usize;
        for &x_val in table1 {
            for &y_val in table1 {
                let mut grid = [0u8; 64];

                let x1 = x_val as i32;
                let x2 = y_val as i32;
                let y1 = table2[(s / stride) % 16] as i32;
                let y2 = table2[((s / stride) / 16) % 16] as i32;

                let b1 = classify_boundary(x1, y1, param as i32);
                let b2 = classify_boundary(x2, y2, param as i32);

                let mut delta = (y2 - y1).abs();
                delta = delta.max((x2 - x1).abs());

                for variable1 in 0..=delta {
                    let (interp_x, interp_y) = if delta > 0 {
                        (
                            (x1 * variable1 + x2 * (delta - variable1) + delta / 2) / delta,
                            (y1 * variable1 + y2 * (delta - variable1) + delta / 2) / delta,
                        )
                    } else {
                        (x1, y1)
                    };

                    if interp_x >= 0
                        && interp_x < param as i32
                        && interp_y >= 0
                        && interp_y < param as i32
                    {
                        set_grid(&mut grid, param, interp_x as usize, interp_y as usize);
                        fill_edges(&mut grid, param, interp_x, interp_y, b1, b2);
                    }
                }

                if param == 8 {
                    let base = s;
                    let mut true_count = 0usize;
                    let mut false_count = 0usize;
                    for idx in (0..64).rev() {
                        if grid[idx] != 0 {
                            storage[256 + base + true_count] = idx as u8;
                            true_count += 1;
                        } else {
                            storage[320 + base + false_count] = idx as u8;
                            false_count += 1;
                        }
                    }
                    storage[384 + base] = true_count as u8;
                    storage[385 + base] = false_count as u8;
                    s += 388;
                } else {
                    let base = s;
                    let mut true_count = 0usize;
                    let mut false_count = 0usize;
                    for idx in (0..16).rev() {
                        if grid[idx] != 0 {
                            storage[64 + base + true_count] = idx as u8;
                            true_count += 1;
                        } else {
                            storage[80 + base + false_count] = idx as u8;
                            false_count += 1;
                        }
                    }
                    storage[96 + base] = true_count as u8;
                    storage[97 + base] = false_count as u8;
                    s += 128;
                }
            }
        }
        Ok(())
    }
}

fn classify_boundary(value1: i32, value2: i32, param: i32) -> i32 {
    if value2 == 0 {
        0
    } else if value2 == param - 1 {
        1
    } else if value1 == 0 {
        2
    } else if value1 == param - 1 {
        3
    } else {
        4
    }
}

fn set_grid(grid: &mut [u8; 64], param: usize, x: usize, y: usize) {
    let index = y * param + x;
    if index < grid.len() {
        grid[index] = 1;
    }
}

fn fill_edges(grid: &mut [u8; 64], param: usize, x: i32, y: i32, b1: i32, b2: i32) {
    if (b1 == 2 && b2 == 3) || (b2 == 2 && b1 == 3) || (b1 == 0 && b2 != 1) || (b2 == 0 && b1 != 1)
    {
        for row in 0..=y {
            if row >= 0 {
                set_grid(grid, param, x as usize, row as usize);
            }
        }
    } else if (b2 != 0 && b1 == 1) || (b1 != 0 && b2 == 1) {
        for row in y..param as i32 {
            if row >= 0 {
                set_grid(grid, param, x as usize, row as usize);
            }
        }
    } else if (b1 == 2 && b2 != 3) || (b2 == 2 && b1 != 3) {
        for col in 0..=x {
            if col >= 0 {
                set_grid(grid, param, col as usize, y as usize);
            }
        }
    } else if (b1 == 0 && b2 == 1)
        || (b2 == 0 && b1 == 1)
        || (b1 == 3 && b2 != 2)
        || (b2 == 3 && b1 != 2)
    {
        for col in x..param as i32 {
            if col >= 0 {
                set_grid(grid, param, col as usize, y as usize);
            }
        }
    }
}

struct Bomp<'a> {
    src: &'a [u8],
    index: usize,
    left: i32,
    num: i32,
    color: u8,
}

impl<'a> Bomp<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            index: 0,
            left: 2,
            num: 0,
            color: 0,
        }
    }

    fn decode(&mut self) -> u8 {
        if self.left == 2 {
            if self.index >= self.src.len() {
                return 0;
            }
            let code = self.src[self.index];
            self.index += 1;
            self.num = ((code >> 1) + 1) as i32;
            if (code & 1) != 0 {
                self.left = 1;
                if self.index < self.src.len() {
                    self.color = self.src[self.index];
                    self.index += 1;
                }
            } else {
                self.left = 0;
            }
        }

        let result = if self.left != 0 {
            if self.left == 1 { self.color } else { 255 }
        } else if self.index < self.src.len() {
            let value = self.src[self.index];
            self.index += 1;
            value
        } else {
            0
        };

        self.num -= 1;
        if self.num == 0 {
            self.left = 2;
        }
        result
    }
}
