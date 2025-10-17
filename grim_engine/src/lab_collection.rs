use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use grim_formats::{LabArchive, LabEntry};

/// Slim wrapper around the retail LAB archives we need for the intro sequence.
#[derive(Debug)]
pub struct LabCollection {
    archives: Vec<LabArchive>,
}

impl LabCollection {
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }

        let mut archives: Vec<_> = fs::read_dir(dir)
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
        archives.sort();

        let mut opened = Vec::new();
        for path in archives {
            match LabArchive::open(&path) {
                Ok(archive) => opened.push(archive),
                Err(err) => {
                    eprintln!(
                        "[grim_engine] warning: failed to open {}: {:?}",
                        path.display(),
                        err
                    );
                }
            }
        }

        if opened.is_empty() {
            bail!("no LAB archives found in {}", dir.display());
        }

        Ok(Self { archives: opened })
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
