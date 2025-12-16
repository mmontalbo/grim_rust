use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{ensure, Context, Result};
use mlua::{Function, Lua, MultiValue, RegistryKey, Value, Variadic};

use super::util::{
    describe_callable_label, describe_value, function_provenance, function_source_hint, set_global,
};
use crate::lua_host::context::EngineContext;
use crate::lua_host::telemetry::log_boot_sequence_complete;
use grim_telemetry_schema::trace_utils::LuaFunctionProvenance;

pub(crate) fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let compiled = data_root.join("_system.lua");
    let decompiled = data_root.join("_system.decompiled.lua");
    ensure!(
        compiled.is_file() || decompiled.is_file(),
        "missing _system.lua under {}",
        data_root.display()
    );

    let globals = lua.globals();
    let dofile: Function = globals
        .get("dofile")
        .context("dofile missing from Lua state")?;
    // Execute via the Lua-facing dofile so telemetry, search order, and handling
    // of compiled/decompiled variants match retail behavior.
    let _: Value = dofile
        .call("_system.lua")
        .context("executing dofile(\"_system.lua\")")?;
    Ok(())
}

pub(crate) fn wrap_boot(lua: &Lua, data_root: &Path) -> Result<()> {
    let globals = lua.globals();
    let boot: Function = globals
        .get("BOOT")
        .context("BOOT function missing after loading _system")?;

    let provenance = function_provenance(&boot, data_root);
    if !matches!(provenance, LuaFunctionProvenance::GameScript(_)) {
        return Ok(());
    }

    let note = function_source_hint(&boot, data_root);
    let boot_key: RegistryKey = lua.create_registry_value(boot.clone())?;
    let wrapped_boot = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let boot_fn: Function = lua_ctx.registry_value(&boot_key)?;
        let results: MultiValue = boot_fn.call(args)?;
        log_boot_sequence_complete(note.as_deref());
        Ok(results)
    })?;

    globals.set("BOOT", wrapped_boot)?;
    Ok(())
}

pub(crate) fn override_boot_stubs(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let source_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "source_all_set_files",
        lua.create_function(move |_, _: Variadic<Value>| {
            source_ctx
                .borrow_mut()
                .log_event("sets.source_all_set_files");
            Ok(())
        })?,
    )?;

    let start_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "start_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            if args.is_empty() {
                return Ok(0u32);
            }
            let label = describe_callable_label(args.first().unwrap());
            let handle = {
                let mut state = start_ctx.borrow_mut();
                state.start_script(label)
            };
            start_ctx.borrow_mut().complete_script(handle);
            Ok(handle)
        })?,
    )?;

    let single_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "single_start_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            if args.is_empty() {
                return Ok(0u32);
            }
            let label = describe_callable_label(args.first().unwrap());
            let handle = {
                let mut state = single_ctx.borrow_mut();
                state.start_script(label)
            };
            single_ctx.borrow_mut().complete_script(handle);
            Ok(handle)
        })?,
    )?;

    set_global(
        lua,
        &globals,
        "wait_for_script",
        lua.create_function(|_, _: Variadic<Value>| Ok(()))?,
    )?;

    let stop_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "stop_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            let description = args
                .first()
                .map(describe_value)
                .unwrap_or_else(|| "<unknown>".to_string());
            stop_ctx
                .borrow_mut()
                .log_event(format!("script.stop {description}"));
            Ok(())
        })?,
    )?;

    set_global(
        lua,
        &globals,
        "GetCurrentScript",
        lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
    )?;

    Ok(())
}

pub(crate) fn call_boot(lua: &Lua, _context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let boot: Function = globals
        .get("BOOT")
        .context("BOOT function missing after loading _system")?;
    boot.call::<_, ()>((false, Value::Nil))
        .context("executing BOOT(false)")?;
    log_boot_sequence_complete(None);
    Ok(())
}
