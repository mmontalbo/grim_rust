use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use grim_formats::{CosFile, LabArchive};
use walkdir::WalkDir;

/// Extract mesh assets referenced by a costume (.cos) file.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Costume file to parse (e.g. artifacts/manny_assets/ma_note_type.cos)
    #[arg(long)]
    cos: PathBuf,

    /// Destination directory for the extracted meshes
    #[arg(long)]
    dest: PathBuf,

    /// Directory containing LAB archives (defaults to $GRIM_INSTALL_PATH)
    #[arg(long)]
    root: Option<PathBuf>,

    /// Overwrite any existing files in the destination directory
    #[arg(long)]
    force: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let lab_root = match args.root {
        Some(path) => path,
        None => std::env::var("GRIM_INSTALL_PATH")
            .map(PathBuf::from)
            .context("GRIM_INSTALL_PATH not set; pass --root")?,
    };

    let costume_bytes =
        fs::read(&args.cos).with_context(|| format!("reading costume {}", args.cos.display()))?;
    let costume = CosFile::parse_bytes(&costume_bytes)
        .with_context(|| format!("parsing {}", args.cos.display()))?;

    let mesh_components: BTreeSet<String> = costume
        .components
        .iter()
        .filter_map(|component| {
            let name = component.name.to_ascii_lowercase();
            if name.ends_with(".3do") {
                Some(component.name.clone())
            } else {
                None
            }
        })
        .collect();

    if mesh_components.is_empty() {
        bail!("no .3do components found in {}", args.cos.display());
    }

    let archives = load_lab_archives(&lab_root)?;
    if archives.is_empty() {
        bail!("no LAB archives found under {}", lab_root.display());
    }

    fs::create_dir_all(&args.dest).with_context(|| format!("creating {}", args.dest.display()))?;

    for asset_name in mesh_components {
        let asset_ref = asset_name.as_str();
        let (archive_index, entry_index) = find_asset(&archives, asset_ref)
            .with_context(|| format!("locating {} in LAB archives", asset_ref))?;
        let archive = &archives[archive_index];
        let entry = &archive.entries()[entry_index];

        let dest_path = args.dest.join(&asset_name);
        if dest_path.exists() && !args.force {
            println!(
                "skip {} (already exists, use --force to overwrite)",
                dest_path.display()
            );
            continue;
        }

        println!(
            "extract {} from {} -> {}",
            asset_ref,
            archive.path().display(),
            dest_path.display()
        );
        archive
            .extract_entry(entry, &dest_path)
            .with_context(|| format!("extracting {}", asset_name))?;
    }

    Ok(())
}

fn load_lab_archives(root: &Path) -> Result<Vec<LabArchive>> {
    let mut archives = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_lab = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("lab"))
            .unwrap_or(false);
        if !is_lab {
            continue;
        }
        let archive = LabArchive::open(path)
            .with_context(|| format!("opening LAB archive {}", path.display()))?;
        archives.push(archive);
    }
    archives.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(archives)
}

fn find_asset(archives: &[LabArchive], asset_name: &str) -> Result<(usize, usize)> {
    for (index, archive) in archives.iter().enumerate() {
        if let Some((entry_index, _)) = archive
            .entries()
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.name.eq_ignore_ascii_case(asset_name))
        {
            return Ok((index, entry_index));
        }
    }
    bail!("asset {} not present in provided LAB archives", asset_name);
}
