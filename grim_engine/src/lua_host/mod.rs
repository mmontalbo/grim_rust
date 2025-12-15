mod context;
mod legacy_lua;
mod telemetry;

use std::{cell::RefCell, path::Path, rc::Rc};

use anyhow::{Context as AnyhowContext, Result};
use mlua::{Lua, LuaOptions, StdLib};

pub fn log_engine_exit(
    status: &str,
    note: Option<&str>,
    code: Option<i32>,
    signal: Option<i32>,
    cause: Option<&str>,
) {
    telemetry::log_engine_exit(status, note, code, signal, cause);
}

/// Runs the minimal boot sequence needed for parity captures and returns once BOOT completes.
pub fn run_boot_sequence(data_root: &Path, verbose: bool, headless: bool) -> Result<()> {
    telemetry::log_boot_sequence_start();
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(context::EngineContext::new(verbose, headless)));

    context::install_package_path(&lua, data_root)?;
    context::install_globals_pre_system(&lua, data_root, context.clone())?;
    context::load_system_script(&lua, data_root)?;
    context::install_globals_post_system(&lua, context.clone())?;
    context::override_boot_stubs(&lua, context.clone())?;
    context::call_boot(&lua, context)?;
    telemetry::log_boot_sequence_complete(None);

    Ok(())
}

// Parent-cycle scan removed; boot telemetry now covers the boot window only.
