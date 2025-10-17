use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub enum MovieSource {
    Snm(PathBuf),
    Ogv(PathBuf),
}

#[derive(Debug, Clone)]
pub struct CatalogStats {
    pub total: usize,
    pub snm: usize,
    pub ogv: usize,
}

#[derive(Debug, Default, Clone)]
pub struct MovieCatalog {
    index: HashMap<String, MovieSource>,
}

impl MovieCatalog {
    pub fn from_root(root: &Path) -> Result<Self> {
        let mut catalog = MovieCatalog::default();
        catalog.extend_from_root(root)?;
        Ok(catalog)
    }

    pub fn from_roots<I, P>(roots: I) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut catalog = MovieCatalog::default();
        for root in roots {
            catalog.extend_from_root(root.as_ref())?;
        }
        Ok(catalog)
    }

    pub fn extend_from_root(&mut self, root: &Path) -> Result<()> {
        if !root.exists() {
            return Ok(());
        }
        for entry in WalkDir::new(root).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to traverse {}: {err}",
                        root.display()
                    );
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(ext) = entry.path().extension() else {
                continue;
            };
            if ext.eq_ignore_ascii_case("snm") {
                self.insert_snm(entry.path());
            } else if ext.eq_ignore_ascii_case("ogv") {
                self.insert_ogv(entry.path());
            }
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&MovieSource> {
        self.index.get(key)
    }

    pub fn stats(&self) -> CatalogStats {
        let mut snm = 0;
        let mut ogv = 0;
        for value in self.index.values() {
            match value {
                MovieSource::Snm(_) => snm += 1,
                MovieSource::Ogv(_) => ogv += 1,
            }
        }
        CatalogStats {
            total: self.index.len(),
            snm,
            ogv,
        }
    }

    fn insert_snm(&mut self, path: &Path) {
        let key = normalize_movie_key(&path.file_name().unwrap_or_default().to_string_lossy());
        match self.index.get_mut(&key) {
            None => {
                self.index.insert(key, MovieSource::Snm(path.to_path_buf()));
            }
            Some(existing @ MovieSource::Ogv(_)) => {
                println!(
                    "[grim_viewer] preferring SNM '{}' over OGV fallback",
                    path.display()
                );
                *existing = MovieSource::Snm(path.to_path_buf());
            }
            Some(MovieSource::Snm(previous)) => {
                eprintln!(
                    "[grim_viewer] warning: duplicate SNM key '{}' (keeping {}, skipping {})",
                    key,
                    previous.display(),
                    path.display()
                );
            }
        }
    }

    fn insert_ogv(&mut self, path: &Path) {
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase()
            });
        let key = format!("{stem}.snm");
        match self.index.get(&key) {
            Some(MovieSource::Snm(_)) => {
                // Prefer SNM entries when both exist; no log necessary.
            }
            Some(MovieSource::Ogv(previous)) => {
                eprintln!(
                    "[grim_viewer] warning: duplicate OGV fallback key '{}' (keeping {}, skipping {})",
                    key,
                    previous.display(),
                    path.display()
                );
            }
            None => {
                self.index.insert(key, MovieSource::Ogv(path.to_path_buf()));
            }
        }
    }
}

pub fn normalize_movie_key(movie: &str) -> String {
    let trimmed = movie.trim();
    let replaced = trimmed.replace('\\', "/");
    let segment = replaced.rsplit('/').next().unwrap_or(&replaced);
    let mut key = segment.to_ascii_lowercase();
    if !key.ends_with(".snm") {
        key.push_str(".snm");
    }
    key
}
