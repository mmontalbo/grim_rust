use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use grim_formats::CosFile;

/// Inspect a Grim costume file and list the referenced components.
#[derive(Parser)]
struct Args {
    /// Path to the `.cos` costume to inspect
    path: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bytes = fs::read(&args.path)?;
    let costume = CosFile::parse_bytes(&bytes)?;

    println!("costume {}", costume.version);
    println!("tags: {}", costume.tags.len());
    println!("components: {}", costume.components.len());

    for component in &costume.components {
        println!(
            "{:>4}  tag {:>3}  parent {:>4}  {}",
            component.id, component.tag_id, component.parent_id, component.name
        );
    }

    Ok(())
}
