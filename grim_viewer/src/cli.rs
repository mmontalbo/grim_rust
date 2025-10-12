use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;

#[derive(Parser, Debug)]
#[command(about = "Minimal viewer stub that boots wgpu and rodio", version)]
pub struct Args {
    /// Asset manifest JSON produced by grim_engine --asset-manifest
    #[arg(long, default_value = "artifacts/manny_office_assets.json")]
    pub manifest: PathBuf,

    /// Asset to load from the LAB archives for inspection
    #[arg(long, default_value = "mo_0_ddtws.bm")]
    pub asset: String,

    /// Optional boot timeline manifest produced by grim_engine --timeline-json
    #[arg(long)]
    pub timeline: Option<PathBuf>,

    /// When set, stream audio cue updates from the given log file
    #[arg(long)]
    pub audio_log: Option<PathBuf>,

    /// When set, overlay Manny's movement trace captured via --movement-log-json
    #[arg(long)]
    pub movement_log: Option<PathBuf>,

    /// When set, overlay hotspot event log captured via --event-log-json
    #[arg(long)]
    pub event_log: Option<PathBuf>,

    /// When set, load Lua runtime geometry to align Manny/desk/tube markers
    #[arg(long)]
    pub lua_geometry_json: Option<PathBuf>,

    /// When set, write the decoded bitmap to disk (PNG) before launching the viewer
    #[arg(long)]
    pub dump_frame: Option<PathBuf>,

    /// Skip creating a winit window/event loop; useful for headless automation
    #[arg(long)]
    pub headless: bool,

    /// Optional layout preset JSON describing overlay sizes and minimap constraints
    #[arg(long)]
    pub layout_preset: Option<PathBuf>,

    /// Optional Manny mesh JSON exported via three_do_export for 3D rendering
    #[arg(long)]
    pub manny_mesh_json: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LayoutPreset {
    #[serde(default)]
    pub audio: Option<PanelPreset>,
    #[serde(default)]
    pub timeline: Option<PanelPreset>,
    #[serde(default)]
    pub scrubber: Option<PanelPreset>,
    #[serde(default)]
    pub minimap: Option<MinimapPreset>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PanelPreset {
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub padding_x: Option<u32>,
    #[serde(default)]
    pub padding_y: Option<u32>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl PanelPreset {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MinimapPreset {
    #[serde(default)]
    pub min_side: Option<f32>,
    #[serde(default)]
    pub preferred_fraction: Option<f32>,
    #[serde(default)]
    pub max_fraction: Option<f32>,
}

pub fn load_layout_preset(path: &Path) -> Result<LayoutPreset> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("reading layout preset {}", path.display()))?;
    let preset: LayoutPreset = serde_json::from_str(&data)
        .with_context(|| format!("parsing layout preset {}", path.display()))?;
    Ok(preset)
}
