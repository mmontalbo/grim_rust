use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{ensure, Context, Result};
use mlua::{Function, Lua, Value, Variadic};

use super::dofile::execute_script;
use super::util::{describe_callable_label, describe_value, set_global};
use crate::lua_host::context::EngineContext;
use crate::lua_host::telemetry::log_dofile;

pub(crate) fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let compiled = data_root.join("_system.lua");
    let decompiled = data_root.join("_system.decompiled.lua");
    ensure!(
        compiled.is_file() || decompiled.is_file(),
        "missing _system.lua under {}",
        data_root.display()
    );

    // Retail logs _system.lua via dofile; mirror that telemetry even though we execute directly.
    log_dofile("_system.lua");

    let path = if compiled.is_file() {
        compiled
    } else {
        decompiled
    };
    let executed =
        execute_script(lua, &path).with_context(|| format!("executing {}", path.display()))?;
    ensure!(executed.is_some(), "failed to execute {}", path.display());
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
    Ok(())
}
