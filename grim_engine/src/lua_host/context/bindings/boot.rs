use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};
use grim_analysis::resources::normalize_legacy_lua;
use mlua::{Function, Lua, Result as LuaResult, Value, Variadic};

use super::util::{
    describe_callable_label, describe_value, set_global, value_to_string, value_to_u32,
};
use crate::lua_host::context::EngineContext;

pub(crate) fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let system_path = data_root.join("_system.decompiled.lua");
    let source = std::fs::read_to_string(&system_path)
        .with_context(|| format!("reading {}", system_path.display()))?;
    let normalized = normalize_legacy_lua(&source);
    let chunk = lua.load(&normalized).set_name("_system.decompiled.lua");
    chunk.exec().context("executing _system.decompiled.lua")?;
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
            let label = describe_callable_label(args.get(0).unwrap());
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
            let label = describe_callable_label(args.get(0).unwrap());
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
                .get(0)
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

    let start_movie_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "StartFullscreenMovie",
        lua.create_function(move |_, args: Variadic<Value>| {
            let movie = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let yields = args.get(1).and_then(value_to_u32);
            Ok(start_movie_ctx
                .borrow_mut()
                .start_fullscreen_movie(movie, yields))
        })?,
    )?;

    let run_movie_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "RunFullscreenMovie",
        lua.create_function(move |_, args: Variadic<Value>| {
            let movie = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let yields = args.get(1).and_then(value_to_u32);
            Ok(run_movie_ctx
                .borrow_mut()
                .start_fullscreen_movie(movie, yields))
        })?,
    )?;

    let legacy_movie_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "StartMovie",
        lua.create_function(move |_, args: Variadic<Value>| {
            let movie = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            Ok(legacy_movie_ctx
                .borrow_mut()
                .start_fullscreen_movie(movie, None))
        })?,
    )?;

    let stop_movie_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "StopMovie",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut ctx = stop_movie_ctx.borrow_mut();
            ctx.request_cutscene_skip();
            ctx.stop_fullscreen_movie();
            Ok(())
        })?,
    )?;

    let poll_movie_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "IsFullscreenMoviePlaying",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut ctx = poll_movie_ctx.borrow_mut();
            Ok(ctx.poll_fullscreen_movie())
        })?,
    )?;

    let poll_movie_ctx = context.clone();
    set_global(
        lua,
        &globals,
        "IsMoviePlaying",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut ctx = poll_movie_ctx.borrow_mut();
            Ok(ctx.poll_fullscreen_movie())
        })?,
    )?;

    set_global(
        lua,
        &globals,
        "hideSkipButton",
        lua.create_function(|_, _: Variadic<Value>| Ok(()))?,
    )?;

    set_global(
        lua,
        &globals,
        "showSkipButton",
        lua.create_function(|_, _: Variadic<Value>| Ok(()))?,
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

pub(crate) fn drive_active_scripts(
    _lua: &Lua,
    _context: Rc<RefCell<EngineContext>>,
    _max_passes: usize,
    _max_yields_per_script: u32,
) -> LuaResult<()> {
    Ok(())
}

pub(crate) fn ensure_intro_cutscene(
    _lua: &Lua,
    _context: Rc<RefCell<EngineContext>>,
    _defer_playback: bool,
) -> Result<bool> {
    Ok(false)
}

pub(crate) fn dump_runtime_summary(state: &EngineContext) {
    println!("Lua runtime summary:");
    if let Some(name) = state.active_fullscreen_movie() {
        println!("  Active movie: {name}");
    } else {
        println!("  Active movie: <none>");
    }
    if !state.events().is_empty() {
        println!("  Event log:");
        for event in state.events() {
            println!("    - {event}");
        }
    }
}
