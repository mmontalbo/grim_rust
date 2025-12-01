use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, ensure};
use grim_formats::{LabArchive, decode_bm};
use grim_stream::StateUpdate;

const OVERLAY_WIDTH: u32 = 640;
const OVERLAY_HEIGHT: u32 = 480;
const BACKGROUND_COLOR: [u8; 4] = [32, 36, 44, 255];
const MANNY_COLOR: [u8; 4] = [51, 242, 217, 240];
const MIN_SPAN: f32 = 1.0;
const BOUNDS_MARGIN: f32 = 0.5;
const SET_FILE: &str = "mo.set";

pub struct EngineFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

pub struct LiveSceneState {
    install_root: PathBuf,
    lab_paths: Vec<PathBuf>,
    width: u32,
    height: u32,
    buffer: Vec<u8>,
    background_pixels: Option<Arc<[u8]>>,
    background_cache: HashMap<String, CachedBackground>,
    setup_backgrounds: HashMap<String, String>,
    missing_backgrounds: HashSet<String>,
    current_setup: Option<String>,
    last_position: Option<[f32; 3]>,
    bounds: Option<PositionBounds>,
}

#[derive(Clone)]
struct CachedBackground {
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
}

#[derive(Clone, Copy)]
struct PositionBounds {
    min_x: f32,
    max_x: f32,
    min_z: f32,
    max_z: f32,
}

impl PositionBounds {
    fn new(x: f32, z: f32) -> Self {
        Self {
            min_x: x - BOUNDS_MARGIN,
            max_x: x + BOUNDS_MARGIN,
            min_z: z - BOUNDS_MARGIN,
            max_z: z + BOUNDS_MARGIN,
        }
    }

    fn include(&mut self, x: f32, z: f32) {
        self.min_x = self.min_x.min(x - BOUNDS_MARGIN);
        self.max_x = self.max_x.max(x + BOUNDS_MARGIN);
        self.min_z = self.min_z.min(z - BOUNDS_MARGIN);
        self.max_z = self.max_z.max(z + BOUNDS_MARGIN);
    }

    fn span_x(&self) -> f32 {
        (self.max_x - self.min_x).abs().max(MIN_SPAN)
    }

    fn span_z(&self) -> f32 {
        (self.max_z - self.min_z).abs().max(MIN_SPAN)
    }
}

impl LiveSceneState {
    pub fn new(install_root: PathBuf) -> Self {
        let lab_paths = collect_lab_paths(&install_root);
        let setup_backgrounds =
            load_setup_backgrounds(&install_root, &lab_paths).unwrap_or_else(|err| {
                eprintln!(
                    "[grim_viewer] failed to load setup background map from {}: {err:?}",
                    install_root.display()
                );
                HashMap::new()
            });

        let mut state = Self {
            install_root,
            lab_paths,
            width: OVERLAY_WIDTH,
            height: OVERLAY_HEIGHT,
            buffer: Vec::new(),
            background_pixels: None,
            background_cache: HashMap::new(),
            setup_backgrounds,
            missing_backgrounds: HashSet::new(),
            current_setup: None,
            last_position: None,
            bounds: None,
        };
        state.fill_with_default_color();
        state
    }

    pub fn compose_frame(&mut self) -> Option<EngineFrame<'_>> {
        self.render_engine_overlay()
    }

    pub fn ingest_state_update<'a>(&'a mut self, update: &StateUpdate) -> Option<EngineFrame<'a>> {
        if let Some(setup) = update.active_setup.as_deref() {
            if let Err(err) = self.ensure_background(setup) {
                if self.missing_backgrounds.insert(setup.to_string()) {
                    eprintln!("[grim_viewer] background unavailable for setup {setup}: {err:?}");
                }
            } else {
                self.missing_backgrounds.remove(setup);
            }
        }

        if let Some(position) = update.position {
            self.last_position = Some(position);
            match self.bounds.as_mut() {
                Some(bounds) => bounds.include(position[0], position[2]),
                None => self.bounds = Some(PositionBounds::new(position[0], position[2])),
            }
        }

        self.render_engine_overlay()
    }

    fn ensure_background(&mut self, setup: &str) -> Result<()> {
        if self.current_setup.as_deref() == Some(setup) {
            return Ok(());
        }

        let background_name = match self.setup_backgrounds.get(setup) {
            Some(name) => name.clone(),
            None => {
                return Err(anyhow!(
                    "no background mapping found for setup {setup} in {SET_FILE}"
                ));
            }
        };

        if !self.background_cache.contains_key(setup) {
            let cached = self.load_background(setup, &background_name)?;
            self.background_cache.insert(setup.to_string(), cached);
        }

        if let Some(cached) = self.background_cache.get(setup).cloned() {
            self.apply_background(setup, &cached);
        }

        Ok(())
    }

    fn load_background(&self, setup: &str, asset: &str) -> Result<CachedBackground> {
        let bytes = self
            .load_asset_bytes(asset)
            .with_context(|| format!("loading background asset {asset} for setup {setup}"))?;

        let bm = decode_bm(&bytes).with_context(|| format!("decoding BM asset {asset}"))?;
        let metadata = bm.metadata();
        ensure!(!bm.frames.is_empty(), "BM asset {asset} contains no frames");
        let frame = &bm.frames[0];
        let pixels = frame
            .as_rgba8888(&metadata)
            .with_context(|| format!("converting BM asset {asset} to RGBA"))?;

        Ok(CachedBackground {
            width: metadata.width,
            height: metadata.height,
            pixels: Arc::from(pixels.into_boxed_slice()),
        })
    }

    fn apply_background(&mut self, setup: &str, cached: &CachedBackground) {
        self.width = cached.width.max(1);
        self.height = cached.height.max(1);
        self.background_pixels = Some(cached.pixels.clone());
        self.buffer = cached.pixels.as_ref().to_vec();
        self.current_setup = Some(setup.to_string());
        self.last_position = None;
        self.bounds = None;
    }

    fn load_asset_bytes(&self, asset: &str) -> Result<Vec<u8>> {
        let direct_path = self.install_root.join(asset);
        if direct_path.is_file() {
            return fs::read(&direct_path)
                .with_context(|| format!("reading asset from {}", direct_path.display()));
        }

        for lab_path in &self.lab_paths {
            match LabArchive::open(lab_path) {
                Ok(archive) => {
                    if let Some(entry) = archive.find_entry(asset) {
                        return Ok(archive.read_entry_bytes(entry).to_vec());
                    }
                }
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to open LAB archive {}: {err:?}",
                        lab_path.display()
                    );
                }
            }
        }

        let fallback = fallback_assets_dir().join(asset);
        if fallback.is_file() {
            return fs::read(&fallback)
                .with_context(|| format!("reading fallback asset {}", fallback.display()));
        }

        Err(anyhow!(
            "asset {asset} not found in install or fallback paths"
        ))
    }

    fn render_engine_overlay(&mut self) -> Option<EngineFrame<'_>> {
        self.reset_canvas();

        if let Some(position) = self.last_position {
            self.draw_manny(position);
        }

        Some(EngineFrame {
            width: self.width,
            height: self.height,
            pixels: &self.buffer,
        })
    }

    fn reset_canvas(&mut self) {
        if let Some(base) = self.background_pixels.as_ref() {
            if self.buffer.len() != base.len() {
                self.buffer = base.as_ref().to_vec();
            } else {
                self.buffer.copy_from_slice(base.as_ref());
            }
        } else {
            self.fill_with_default_color();
        }
    }

    fn fill_with_default_color(&mut self) {
        let expected_len = self
            .width
            .max(1)
            .saturating_mul(self.height.max(1))
            .saturating_mul(4) as usize;
        if self.buffer.len() != expected_len {
            self.buffer = vec![0u8; expected_len];
        }
        for chunk in self.buffer.chunks_mut(4) {
            chunk.copy_from_slice(&BACKGROUND_COLOR);
        }
    }

    fn draw_manny(&mut self, position: [f32; 3]) {
        if let Some(bounds) = self.bounds
            && let Some((px, py)) = self.project(bounds, position)
        {
            stamp_point(
                &mut self.buffer,
                self.width,
                self.height,
                px,
                py,
                MANNY_COLOR,
                4,
            );
        }
    }

    fn project(&self, bounds: PositionBounds, position: [f32; 3]) -> Option<(i32, i32)> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let span_x = bounds.span_x();
        let span_z = bounds.span_z();
        let x_norm = ((position[0] - bounds.min_x) / span_x).clamp(0.0, 1.0);
        let z_norm = ((position[2] - bounds.min_z) / span_z).clamp(0.0, 1.0);

        let px = (x_norm * (self.width as f32 - 1.0)).round() as i32;
        let py = ((1.0 - z_norm) * (self.height as f32 - 1.0)).round() as i32;
        Some((px, py))
    }
}

fn stamp_point(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    px: i32,
    py: i32,
    color: [u8; 4],
    radius: i32,
) {
    let width_i = width as i32;
    let height_i = height as i32;

    for y in (py - radius)..=(py + radius) {
        if y < 0 || y >= height_i {
            continue;
        }
        for x in (px - radius)..=(px + radius) {
            if x < 0 || x >= width_i {
                continue;
            }
            let offset = ((y as u32 * width) + x as u32) as usize * 4;
            buffer[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

fn collect_lab_paths(install_root: &Path) -> Vec<PathBuf> {
    let mut labs = Vec::new();
    if let Ok(entries) = fs::read_dir(install_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(ext) = path.extension().and_then(|ext| ext.to_str())
                && ext.eq_ignore_ascii_case("lab")
            {
                labs.push(path);
            }
        }
    }
    labs.sort();
    labs
}

fn load_setup_backgrounds(
    install_root: &Path,
    labs: &[PathBuf],
) -> Result<HashMap<String, String>> {
    let bytes = load_asset_bytes_with_paths(install_root, labs, SET_FILE)?;
    parse_setup_backgrounds(&bytes)
}

fn load_asset_bytes_with_paths(
    install_root: &Path,
    labs: &[PathBuf],
    asset: &str,
) -> Result<Vec<u8>> {
    let direct_path = install_root.join(asset);
    if direct_path.is_file() {
        return fs::read(&direct_path)
            .with_context(|| format!("reading asset from {}", direct_path.display()));
    }

    for lab_path in labs {
        match LabArchive::open(lab_path) {
            Ok(archive) => {
                if let Some(entry) = archive.find_entry(asset) {
                    return Ok(archive.read_entry_bytes(entry).to_vec());
                }
            }
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: failed to open LAB archive {}: {err:?}",
                    lab_path.display()
                );
            }
        }
    }

    let fallback = fallback_assets_dir().join(asset);
    if fallback.is_file() {
        return fs::read(&fallback)
            .with_context(|| format!("reading fallback asset {}", fallback.display()));
    }

    Err(anyhow!(
        "asset {asset} not found in install or fallback paths"
    ))
}

fn parse_setup_backgrounds(bytes: &[u8]) -> Result<HashMap<String, String>> {
    let text = String::from_utf8(bytes.to_vec()).context("decoding mo.set as UTF-8 text")?;
    let mut map = HashMap::new();
    let mut current_setup: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let Some(keyword) = parts.next() else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "setup" => {
                current_setup = parts.next().map(|value| value.to_string());
            }
            "background" => {
                if let (Some(setup), Some(background)) = (current_setup.as_ref(), parts.next()) {
                    map.insert(setup.clone(), background.to_string());
                }
            }
            _ => {}
        }
    }

    if map.is_empty() {
        Err(anyhow!("no background entries found in {SET_FILE}"))
    } else {
        Ok(map)
    }
}

fn fallback_assets_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(parent) = manifest_dir.parent() {
        parent.join("artifacts").join("manny_assets")
    } else {
        PathBuf::from("artifacts/manny_assets")
    }
}
