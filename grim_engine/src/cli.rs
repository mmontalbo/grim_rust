use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    about = "Prototype host that inspects the new-game boot sequence",
    version
)]
pub struct Args {
    /// Path to the extracted DATA000 directory
    #[arg(long, default_value = "extracted/DATA000")]
    pub data_root: PathBuf,

    /// Optional JSON registry file to read/write while simulating the boot
    #[arg(long)]
    pub registry: Option<PathBuf>,

    /// Print all hook summaries instead of the compact view
    #[arg(long)]
    pub verbose: bool,

    /// Directory containing LAB archives (default: dev-install)
    #[arg(long)]
    pub lab_root: Option<PathBuf>,

    /// Optional directory to extract Manny's office assets into
    #[arg(long)]
    pub extract_assets: Option<PathBuf>,

    /// Path to write the boot timeline JSON report
    #[arg(long)]
    pub timeline_json: Option<PathBuf>,

    /// Path to write the Manny's Office asset scan JSON manifest
    #[arg(long)]
    pub asset_manifest: Option<PathBuf>,

    /// Simulate the boot-time scheduler to show execution cadence
    #[arg(long)]
    pub simulate_scheduler: bool,

    /// Path to write the boot-time scheduler queues as JSON
    #[arg(long)]
    pub scheduler_json: Option<PathBuf>,

    /// Compare a Lua geometry snapshot against the static timeline (requires --timeline-json or default analysis run)
    #[arg(long)]
    pub geometry_diff: Option<PathBuf>,

    /// Path to write the geometry diff summary as JSON (requires --geometry-diff)
    #[arg(long)]
    pub geometry_diff_json: Option<PathBuf>,

    /// Execute the embedded Lua VM prototype instead of the static analysis pipeline
    #[arg(long)]
    pub run_lua: bool,

    /// Path to write the embedded Lua geometry/visibility snapshot as JSON (with --run-lua or --verify-geometry)
    #[arg(long)]
    pub lua_geometry_json: Option<PathBuf>,

    /// Generate a runtime geometry snapshot and diff it against the static timeline
    #[arg(long)]
    pub verify_geometry: bool,

    /// Path to write the audio event log emitted by the Lua runtime (requires --run-lua)
    #[arg(long)]
    pub audio_log_json: Option<PathBuf>,

    /// Path to write the hotspot event log emitted by the Lua runtime (requires --run-lua)
    #[arg(long)]
    pub event_log_json: Option<PathBuf>,

    /// Run the built-in Manny movement demo after boot (requires --run-lua)
    #[arg(long)]
    pub movement_demo: bool,

    /// Path to write the movement trajectory log as JSON (with --movement-demo)
    #[arg(long)]
    pub movement_log_json: Option<PathBuf>,

    /// Path to write codec3 depth stats JSON (requires --run-lua)
    #[arg(long)]
    pub depth_stats_json: Option<PathBuf>,

    /// Run a Manny hotspot demo after boot (requires --run-lua)
    #[arg(long, value_name = "SLUG")]
    pub hotspot_demo: Option<String>,
}

#[derive(Debug)]
pub enum Command {
    RunLua(RunLuaArgs),
    Analyze(AnalyzeArgs),
}

#[derive(Debug)]
pub struct RunLuaArgs {
    pub data_root: PathBuf,
    pub verbose: bool,
    pub lab_root: Option<PathBuf>,
    pub lua_geometry_json: Option<PathBuf>,
    pub audio_log_json: Option<PathBuf>,
    pub event_log_json: Option<PathBuf>,
    pub movement_demo: bool,
    pub movement_log_json: Option<PathBuf>,
    pub hotspot_demo: Option<String>,
    pub depth_stats_json: Option<PathBuf>,
    pub verify_geometry: bool,
    pub geometry_diff: Option<PathBuf>,
    pub geometry_diff_json: Option<PathBuf>,
}

#[derive(Debug)]
pub struct AnalyzeArgs {
    pub data_root: PathBuf,
    pub registry: Option<PathBuf>,
    pub verbose: bool,
    pub lab_root: Option<PathBuf>,
    pub extract_assets: Option<PathBuf>,
    pub timeline_json: Option<PathBuf>,
    pub asset_manifest: Option<PathBuf>,
    pub simulate_scheduler: bool,
    pub scheduler_json: Option<PathBuf>,
    pub geometry_diff: Option<PathBuf>,
    pub geometry_diff_json: Option<PathBuf>,
    pub lua_geometry_json: Option<PathBuf>,
    pub verify_geometry: bool,
    pub audio_log_json: Option<PathBuf>,
    pub event_log_json: Option<PathBuf>,
    pub depth_stats_json: Option<PathBuf>,
}

pub fn parse() -> Result<Command> {
    let args = Args::parse();
    args.into_command()
}

impl Args {
    fn into_command(self) -> Result<Command> {
        if !self.run_lua && self.hotspot_demo.is_some() {
            bail!("--hotspot-demo requires --run-lua");
        }

        if self.run_lua {
            Ok(Command::RunLua(RunLuaArgs {
                data_root: self.data_root,
                verbose: self.verbose,
                lab_root: self.lab_root,
                lua_geometry_json: self.lua_geometry_json,
                audio_log_json: self.audio_log_json,
                event_log_json: self.event_log_json,
                movement_demo: self.movement_demo,
                movement_log_json: self.movement_log_json,
                hotspot_demo: self.hotspot_demo,
                depth_stats_json: self.depth_stats_json,
                verify_geometry: self.verify_geometry,
                geometry_diff: self.geometry_diff,
                geometry_diff_json: self.geometry_diff_json,
            }))
        } else {
            Ok(Command::Analyze(AnalyzeArgs {
                data_root: self.data_root,
                registry: self.registry,
                verbose: self.verbose,
                lab_root: self.lab_root,
                extract_assets: self.extract_assets,
                timeline_json: self.timeline_json,
                asset_manifest: self.asset_manifest,
                simulate_scheduler: self.simulate_scheduler,
                scheduler_json: self.scheduler_json,
                geometry_diff: self.geometry_diff,
                geometry_diff_json: self.geometry_diff_json,
                lua_geometry_json: self.lua_geometry_json,
                verify_geometry: self.verify_geometry,
                audio_log_json: self.audio_log_json,
                event_log_json: self.event_log_json,
                depth_stats_json: self.depth_stats_json,
            }))
        }
    }
}
