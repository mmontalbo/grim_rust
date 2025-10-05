use anyhow::{Result, anyhow};
use grim_formats::decode_bm;
use std::{collections::HashMap, env};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let zbm_path = args.next().ok_or_else(|| {
        anyhow!("usage: cargo run -p grim_formats --example zbm_stats -- <zbm> [base.bm]")
    })?;
    let base_path = args.next();

    let zbm_bytes = std::fs::read(&zbm_path)?;
    let zbm = decode_bm(&zbm_bytes)?;
    let zbm_frame = zbm
        .frames
        .first()
        .ok_or_else(|| anyhow!("zbm has no frames"))?;

    let base_data = if let Some(path) = base_path {
        let bm_bytes = std::fs::read(path)?;
        let bm = decode_bm(&bm_bytes)?;
        let frame = bm
            .frames
            .first()
            .ok_or_else(|| anyhow!("base bm has no frames"))?;
        Some(frame.data.clone())
    } else {
        None
    };

    print_stats(&zbm_frame.data, base_data.as_deref())?;
    Ok(())
}

fn print_stats(zbm: &[u8], base: Option<&[u8]>) -> Result<()> {
    let mut zero_pixels = 0usize;
    let mut nonzero_pixels = 0usize;
    let mut min_value = u16::MAX;
    let mut max_value = u16::MIN;
    let mut diff_from_base = 0usize;
    let mut diff_hist = HashMap::new();

    for (idx, chunk) in zbm.chunks_exact(2).enumerate() {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
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

        if let Some(base) = base {
            let base_chunk = &base[idx * 2..idx * 2 + 2];
            if base_chunk != chunk {
                diff_from_base += 1;
            }
            let base_value = u16::from_le_bytes([base_chunk[0], base_chunk[1]]) as i32;
            let delta_value = value as i32;
            let diff = delta_value - base_value;
            *diff_hist.entry(diff).or_insert(0usize) += 1;
        }
    }

    let total = zero_pixels + nonzero_pixels;
    println!(
        "pixels: {total} (zero: {zero_pixels}, nonzero: {nonzero_pixels}) min=0x{min_value:04X} max=0x{max_value:04X}"
    );
    if base.is_some() {
        println!("pixels differing from base: {diff_from_base}");
    }

    println!("first 8 pixel values (hex):");
    for chunk in zbm.chunks_exact(2).take(8) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        print!("{:04X} ", value);
    }
    println!();

    if base.is_some() {
        let mut entries: Vec<_> = diff_hist.into_iter().collect();
        entries.sort_by_key(|&(diff, count)| (std::cmp::Reverse(count), diff));
        println!("top diffs (value:count):");
        for (diff, count) in entries.into_iter().take(8) {
            println!("  {diff:+5} -> {count}");
        }
    }

    Ok(())
}
