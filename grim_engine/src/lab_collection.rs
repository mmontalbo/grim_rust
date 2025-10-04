use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use grim_formats::{decode_bm, peek_bm_metadata, LabArchive, LabEntry};
use serde::Serialize;

#[derive(Debug)]
pub struct LabCollection {
    archives: Vec<LabArchive>,
}

impl LabCollection {
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let dir = dir.as_ref();
        let mut archives = Vec::new();

        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }

        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .with_context(|| format!("reading LAB directory {}", dir.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("lab"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();

        for path in paths {
            match LabArchive::open(&path) {
                Ok(archive) => archives.push(archive),
                Err(err) => {
                    eprintln!(
                        "[grim_engine] warning: failed to open {}: {:?}",
                        path.display(),
                        err
                    );
                }
            }
        }

        if archives.is_empty() {
            bail!("no LAB archives found in {}", dir.display());
        }

        Ok(Self { archives })
    }

    pub fn find_entry(&self, name: &str) -> Option<(&LabArchive, &LabEntry)> {
        for archive in &self.archives {
            if let Some(entry) = archive.find_entry(name) {
                return Some((archive, entry));
            }
        }
        None
    }
}

#[derive(Debug, Serialize)]
pub struct AssetReportEntry {
    pub asset_name: String,
    pub archive_path: PathBuf,
    pub offset: u64,
    pub size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AssetMetadata>,
}

#[derive(Debug, Default, Serialize)]
pub struct AssetReport {
    pub found: Vec<AssetReportEntry>,
    pub missing: Vec<String>,
}

impl AssetReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn to_json_string(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

pub fn collect_assets(
    collection: &LabCollection,
    asset_names: &[&str],
    extract_root: Option<&Path>,
) -> Result<AssetReport> {
    let mut report = AssetReport::new();

    if let Some(root) = extract_root {
        fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    }

    for &asset in asset_names {
        match collection.find_entry(asset) {
            Some((archive, entry)) => {
                if let Some(root) = extract_root {
                    let dest_path = root.join(asset.to_ascii_lowercase());
                    if let Some(parent) = dest_path.parent() {
                        fs::create_dir_all(parent)
                            .with_context(|| format!("creating {}", parent.display()))?;
                    }
                    archive
                        .extract_entry(entry, &dest_path)
                        .with_context(|| format!("extracting {}", asset))?;
                }

                let metadata = analyze_asset_metadata(asset, archive, entry);
                report.found.push(AssetReportEntry {
                    asset_name: asset.to_string(),
                    archive_path: archive.path().to_path_buf(),
                    offset: entry.offset,
                    size: entry.size,
                    metadata,
                });
            }
            None => report.missing.push(asset.to_string()),
        }
    }

    Ok(report)
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetMetadata {
    Bitmap {
        codec: u32,
        bits_per_pixel: u32,
        frames: u32,
        width: u32,
        height: u32,
        supported: bool,
    },
}

fn analyze_asset_metadata(
    asset: &str,
    archive: &LabArchive,
    entry: &LabEntry,
) -> Option<AssetMetadata> {
    let lower = asset.to_ascii_lowercase();
    if !(lower.ends_with(".bm") || lower.ends_with(".zbm")) {
        return None;
    }

    let bytes = archive.read_entry_bytes(entry);
    match peek_bm_metadata(bytes) {
        Ok(metadata) => {
            let mut supported = matches!(metadata.codec, 0 | 3);
            if supported {
                if let Err(err) = decode_bm(bytes) {
                    eprintln!(
                        "[grim_engine] warning: failed to decode bitmap {} from {}: {:?}",
                        asset,
                        archive.path().display(),
                        err
                    );
                    supported = false;
                }
            } else {
                eprintln!(
                    "[grim_engine] info: skipping unsupported bitmap {} (codec {})",
                    asset, metadata.codec
                );
            }

            Some(AssetMetadata::Bitmap {
                codec: metadata.codec,
                bits_per_pixel: metadata.bits_per_pixel,
                frames: metadata.image_count,
                width: metadata.width,
                height: metadata.height,
                supported,
            })
        }
        Err(err) => {
            eprintln!(
                "[grim_engine] warning: failed to parse bitmap header {} from {}: {:?}",
                asset,
                archive.path().display(),
                err
            );
            None
        }
    }
}
