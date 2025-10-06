use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use grim_formats::LabArchive;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(about = "Extract Grim Fandango LAB archives", version)]
struct Args {
    /// LAB archive to extract (may be passed multiple times)
    #[arg(long = "lab", value_name = "PATH", conflicts_with = "root")]
    labs: Vec<PathBuf>,

    /// Directory containing LAB archives (recursively scanned when --lab is not used)
    #[arg(long = "root", value_name = "DIR", conflicts_with = "labs")]
    root: Option<PathBuf>,

    /// Destination directory to materialise assets
    #[arg(long, value_name = "DIR", default_value = "extracted")]
    dest: PathBuf,

    /// Optional newline-delimited list of asset names to extract (case-insensitive)
    #[arg(long, value_name = "FILE")]
    manifest: Option<PathBuf>,

    /// Individual asset names to extract (case-insensitive, may repeat)
    #[arg(long = "asset", value_name = "NAME")]
    assets: Vec<String>,

    /// Overwrite existing files instead of skipping them
    #[arg(long)]
    overwrite: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let labs = resolve_lab_paths(&args)?;
    if labs.is_empty() {
        bail!("no LAB archives to extract");
    }

    let filter = build_asset_filter(&args)?;

    fs::create_dir_all(&args.dest)
        .with_context(|| format!("creating destination {}", args.dest.display()))?;

    for lab_path in labs {
        let archive = LabArchive::open(&lab_path)
            .with_context(|| format!("opening LAB archive {}", lab_path.display()))?;
        extract_archive(&archive, &args.dest, filter.as_ref(), args.overwrite)?;
    }

    Ok(())
}

fn resolve_lab_paths(args: &Args) -> Result<Vec<PathBuf>> {
    let mut labs = Vec::new();

    if !args.labs.is_empty() {
        labs.extend(args.labs.iter().cloned());
    } else if let Some(root) = args.root.as_ref() {
        for entry in WalkDir::new(root).into_iter().filter_map(|res| res.ok()) {
            if entry.file_type().is_file() {
                if entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("lab"))
                    .unwrap_or(false)
                {
                    labs.push(entry.into_path());
                }
            }
        }
    }

    labs.sort();
    labs.dedup();

    Ok(labs)
}

fn build_asset_filter(args: &Args) -> Result<Option<HashSet<String>>> {
    let mut entries: HashSet<String> = HashSet::new();

    if let Some(manifest_path) = args.manifest.as_ref() {
        let file = fs::File::open(manifest_path)
            .with_context(|| format!("opening manifest {}", manifest_path.display()))?;
        for line in io::BufReader::new(file).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            entries.insert(trimmed.to_ascii_lowercase());
        }
    }

    for asset in &args.assets {
        entries.insert(asset.trim().to_ascii_lowercase());
    }

    if entries.is_empty() {
        Ok(None)
    } else {
        Ok(Some(entries))
    }
}

fn extract_archive(
    archive: &LabArchive,
    dest_root: &Path,
    filter: Option<&HashSet<String>>,
    overwrite: bool,
) -> Result<()> {
    let lab_name = archive
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
        .unwrap_or_else(|| "lab".to_string());

    let lab_dest = dest_root.join(lab_name.to_ascii_uppercase());
    fs::create_dir_all(&lab_dest).with_context(|| format!("creating {}", lab_dest.display()))?;

    let mut extracted = 0usize;
    for entry in archive.entries() {
        if let Some(filter) = filter {
            if !filter.contains(&entry.name.to_ascii_lowercase()) {
                continue;
            }
        }

        let raw = PathBuf::from(entry.name.replace('\\', "/"));
        let mut relative = PathBuf::new();
        for component in raw.components() {
            match component {
                Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
                Component::ParentDir => continue,
                Component::Normal(part) => relative.push(part),
            }
        }

        let dest_path = lab_dest.join(&relative);
        if dest_path.exists() && !overwrite {
            continue;
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }

        archive
            .extract_entry(entry, &dest_path)
            .with_context(|| format!("extracting {}", entry.name))?;
        extracted += 1;
    }

    println!(
        "Extracted {} entries from {} into {}",
        extracted,
        archive.path().display(),
        lab_dest.display()
    );

    Ok(())
}
