mod context;
mod hotspot;
mod movement;
mod types;

pub use context::AudioCallback;
pub use hotspot::HotspotOptions;
pub use movement::MovementOptions;
#[allow(unused_imports)]
pub use movement::MovementPlan;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use grim_analysis::resources::ResourceGraph;
use mlua::{Lua, LuaOptions, StdLib};

use crate::lab_collection::LabCollection;

#[derive(Debug, Clone)]
pub struct EngineRunSummary {
    events: Vec<String>,
    coverage: BTreeMap<String, u64>,
}

impl EngineRunSummary {
    pub fn events(&self) -> &[String] {
        &self.events
    }

    pub fn coverage(&self) -> &BTreeMap<String, u64> {
        &self.coverage
    }
}

pub fn run_boot_sequence(
    data_root: &Path,
    lab_root: Option<&Path>,
    verbose: bool,
    geometry_json: Option<&Path>,
    audio_callback: Option<Rc<dyn AudioCallback>>,
    movement: Option<MovementOptions>,
    hotspot: Option<HotspotOptions>,
) -> Result<EngineRunSummary> {
    let resources = Rc::new(
        ResourceGraph::from_data_root(data_root)
            .with_context(|| format!("loading resource graph from {}", data_root.display()))?,
    );

    let lab_root_path = lab_root
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("dev-install"));
    let lab_collection = if lab_root_path.is_dir() {
        match LabCollection::load_from_dir(&lab_root_path) {
            Ok(collection) => Some(Rc::new(collection)),
            Err(err) => {
                eprintln!(
                    "[grim_engine] warning: failed to load LAB archives from {}: {:?}",
                    lab_root_path.display(),
                    err
                );
                None
            }
        }
    } else {
        if verbose {
            eprintln!(
                "[grim_engine] info: LAB root {} missing; continuing without geometry",
                lab_root_path.display()
            );
        }
        None
    };

    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(context::EngineContext::new(
        resources,
        verbose,
        lab_collection,
        audio_callback,
    )));
    let context_handle = context::EngineContextHandle::new(context.clone());

    context::install_package_path(&lua, data_root)?;
    context::install_globals(&lua, data_root, context.clone())?;
    context::load_system_script(&lua, data_root)?;
    context::override_boot_stubs(&lua, context.clone())?;
    context::call_boot(&lua, context.clone())?;
    context::drive_active_scripts(&lua, context.clone(), 8, 32)?;

    if let Some(options) = movement.as_ref() {
        movement::simulate_movement(&lua, &context_handle, options)?;
    }

    if let Some(options) = hotspot.as_ref() {
        hotspot::simulate_hotspot_demo(&lua, &context_handle, options)?;
    }

    let snapshot = context.borrow();
    context::dump_runtime_summary(&snapshot);
    let events = snapshot.events().to_vec();
    let coverage = snapshot.coverage_counts().clone();
    if let Some(path) = geometry_json {
        let snapshot_data = snapshot.geometry_snapshot();
        let json = serde_json::to_string_pretty(&snapshot_data)
            .context("serializing Lua geometry snapshot to JSON")?;
        fs::write(path, &json)
            .with_context(|| format!("writing Lua geometry snapshot to {}", path.display()))?;
        println!("Saved Lua geometry snapshot to {}", path.display());
    }
    Ok(EngineRunSummary { events, coverage })
}
