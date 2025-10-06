use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use grim_formats::{decode_bm, decode_bm_with_seed, BmFrame};
use serde::Serialize;

use crate::lab_collection::LabCollection;

const DESK_BASE_ASSET: &str = "mo_0_ddtws.bm";
const DESK_DEPTH_ASSET: &str = "mo_0_ddtws.zbm";

#[derive(Serialize)]
struct DepthPayload {
    min: u32,
    max: u32,
    min_hex: String,
    max_hex: String,
    zero_pixels: usize,
    nonzero_pixels: usize,
}

#[derive(Serialize)]
struct DepthReport {
    asset: String,
    dimensions: [u32; 2],
    checksum_fnv1a: u64,
    depth: DepthPayload,
}

pub fn write_manny_office_depth_stats(lab_root: &Path, dest: &Path) -> Result<()> {
    let collection = LabCollection::load_from_dir(lab_root)
        .with_context(|| format!("loading LAB archives from {}", lab_root.display()))?;

    let (base_archive, base_entry) = collection
        .find_entry(DESK_BASE_ASSET)
        .ok_or_else(|| anyhow!("missing base bitmap {} in LAB archives", DESK_BASE_ASSET))?;
    let base_bytes = base_archive.read_entry_bytes(base_entry);
    let base_bm = decode_bm(base_bytes)
        .with_context(|| format!("decoding base bitmap {}", DESK_BASE_ASSET))?;
    let base_frame = single_frame(&base_bm, DESK_BASE_ASSET)?;

    let (depth_archive, depth_entry) = collection
        .find_entry(DESK_DEPTH_ASSET)
        .ok_or_else(|| anyhow!("missing depth bitmap {} in LAB archives", DESK_DEPTH_ASSET))?;
    let depth_bytes = depth_archive.read_entry_bytes(depth_entry);
    let depth_bm = decode_bm_with_seed(depth_bytes, Some(base_frame.data.as_slice()))
        .with_context(|| {
            format!(
                "decoding depth bitmap {} with seeded base",
                DESK_DEPTH_ASSET
            )
        })?;
    let depth_frame = single_frame(&depth_bm, DESK_DEPTH_ASSET)?;

    let stats = depth_frame
        .depth_stats(&depth_bm.metadata())
        .with_context(|| format!("computing depth stats for {}", DESK_DEPTH_ASSET))?;
    let checksum = fnv1a64(depth_frame.data.as_slice());

    let payload = DepthPayload {
        min: stats.min as u32,
        max: stats.max as u32,
        min_hex: format!("0x{:04X}", stats.min),
        max_hex: format!("0x{:04X}", stats.max),
        zero_pixels: stats.zero_pixels,
        nonzero_pixels: stats.nonzero_pixels,
    };

    let report = DepthReport {
        asset: DESK_DEPTH_ASSET.to_string(),
        dimensions: [depth_bm.width, depth_bm.height],
        checksum_fnv1a: checksum,
        depth: payload,
    };

    let json = serde_json::to_string_pretty(&report).context("serialising depth stats to JSON")?;
    fs::write(dest, json)
        .with_context(|| format!("writing depth stats JSON to {}", dest.display()))?;
    Ok(())
}

fn single_frame<'a>(bm: &'a grim_formats::BmFile, label: &str) -> Result<&'a BmFrame> {
    bm.frames
        .first()
        .ok_or_else(|| anyhow!("{} has no frames", label))
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut acc = 0xcbf29ce484222325u64;
    for byte in data {
        acc ^= u64::from(*byte);
        acc = acc.wrapping_mul(0x100000001b3);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_known_value() {
        let data = b"codec3";
        let expected = 0xd5f447b35c0f199eu64;
        assert_eq!(fnv1a64(data), expected);
    }
}
