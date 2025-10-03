use std::env;

use anyhow::{Context, Result};
use grim_formats::LabArchive;

fn main() -> Result<()> {
    let path = env::args().nth(1).context("usage: lab_dump <LAB file>")?;
    let archive = LabArchive::open(&path)?;
    println!(
        "{} entries in {}",
        archive.entries().len(),
        archive.path().display()
    );
    for entry in archive.entries() {
        let type_id = entry
            .type_id
            .as_str()
            .unwrap_or_else(|| String::from("----"));
        println!(
            "{name:<40} {type_id:<4} {offset:>10} {size:>10}",
            name = entry.name,
            type_id = type_id,
            offset = entry.offset,
            size = entry.size
        );
    }
    Ok(())
}
