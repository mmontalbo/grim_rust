//! Lua host glue for running the minimal intro boot path.
//!
//! The host sets up an `mlua` runtime with the safe standard library, installs
//! just enough globals and stubs to make the retail boot scripts happy, and
//! emits telemetry as the intro sequence progresses.

mod context;
mod legacy_lua;
mod telemetry;

use std::{cell::RefCell, path::Path, rc::Rc};

use anyhow::{Context as AnyhowContext, Result};
use mlua::{Lua, LuaOptions, StdLib};

pub use telemetry::log_engine_exit;

/// Runs the minimal boot sequence needed for parity captures and returns once BOOT completes.
pub fn run_boot_sequence(data_root: &Path, verbose: bool, headless: bool) -> Result<()> {
    telemetry::log_boot_sequence_start();
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initializing Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(context::EngineContext::new(verbose, headless)));

    context::install_package_path(&lua, data_root)?;
    context::install_globals_pre_system(&lua, data_root, context.clone())?;
    context::load_system_script(&lua, data_root)?;
    context::wrap_boot(&lua, data_root)?;
    context::install_globals_post_system(&lua, context.clone())?;
    context::override_boot_stubs(&lua, context.clone())?;
    context::call_boot(&lua, context)?;

    Ok(())
}
