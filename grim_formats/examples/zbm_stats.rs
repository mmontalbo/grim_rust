use anyhow::{Result, anyhow, ensure};
use grim_formats::{BmFrame, BmMetadata, decode_bm};
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
    let metadata = zbm.metadata();

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

    print_stats(zbm_frame, &metadata, base_data.as_deref())?;
    Ok(())
}

fn print_stats(frame: &BmFrame, metadata: &BmMetadata, base: Option<&[u8]>) -> Result<()> {
    ensure!(
        metadata.format == 5,
        "expected depth buffer format (got {})",
        metadata.format
    );

    let stats = frame.depth_stats(metadata)?;
    let data = frame.data.as_slice();

    if let Some(base) = base {
        ensure!(
            base.len() == data.len(),
            "base bitmap does not match depth map dimensions"
        );
    }

    let mut diff_from_base = 0usize;
    let mut diff_hist = HashMap::new();
    let mut first_values = Vec::with_capacity(8);

    for (idx, chunk) in data.chunks_exact(2).enumerate() {
        let value = normalize_depth(u16::from_le_bytes([chunk[0], chunk[1]]));
        if idx < 8 {
            first_values.push(value);
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

    let total = stats.total_pixels();
    println!(
        "pixels: {total} (zero: {zero}, nonzero: {nonzero}) min=0x{min:04X} max=0x{max:04X}",
        zero = stats.zero_pixels,
        nonzero = stats.nonzero_pixels,
        min = stats.min,
        max = stats.max
    );
    if base.is_some() {
        println!("pixels differing from base: {diff_from_base}");
    }

    println!("first 8 pixel values (hex):");
    for value in first_values {
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

fn normalize_depth(value: u16) -> u16 {
    if value == 0xF81F { 0 } else { value }
}
