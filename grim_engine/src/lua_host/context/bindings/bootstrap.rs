use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};
use mlua::{Function, Lua, MultiValue, Result as LuaResult, Table, Value, Variadic};

use crate::lua_host::telemetry::{
    log_event, log_push_cclosure, log_set_tagmethod, log_store_ref, EventBuilder,
};

use super::dofile::{candidate_paths, execute_script, handle_special_dofile};
use super::legacy::install_legacy_compat;
use super::util::{set_global, value_to_string};
use crate::lua_host::context::EngineContext;

pub(crate) fn install_package_path(lua: &Lua, data_root: &Path) -> Result<()> {
    let globals = lua.globals();
    let package: Table = globals
        .get("package")
        .context("package table missing from Lua state")?;
    let current_path: String = package.get("path")?;
    let mut paths = vec![format!("{}/?.lua", data_root.display())];
    paths.push(format!("{}/?.decompiled.lua", data_root.display()));
    paths.push(current_path);
    let new_path = paths.join(";");
    package.set("path", new_path)?;
    Ok(())
}

pub(crate) fn install_globals(
    lua: &Lua,
    data_root: &Path,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    let globals = lua.globals();

    // Ensure legacy version string is the first bound global to mirror retail traces.
    set_global(lua, &globals, "_VERSION", "Lua 3.1")?;

    install_legacy_io(lua, &globals)?;
    // Retail pushes errorfb before _TRIGMODE is bound; log a stub push to align traces.
    let errorfb = lua.create_function(|_, ()| Ok(Value::Nil))?;
    log_push_cclosure("lua_pushCclosure", errorfb.to_pointer());
    set_global(lua, &globals, "_TRIGMODE", 1)?;
    // Retail pushes math_pow before setting the pow tagmethod; log a stub push to align traces.
    let math_pow = lua.create_function(|_, (a, b): (f64, f64)| Ok(Value::Number(a.powf(b))))?;
    log_push_cclosure("lua_pushCclosure", math_pow.to_pointer());
    log_set_tagmethod(-1, "pow");

    install_pi_constant(lua, &globals)?;
    // Retail triggers the first GC after PI is bound.
    lua.gc_collect()?;
    log_event(EventBuilder::new("collect_garbage"));
    install_system_table(lua, &globals, context.clone())?;
    lua.gc_collect()?;
    log_event(EventBuilder::new("collect_garbage"));
    // Retail pushes default camera/control handlers before rebinding type; log stub pushes to align.
    let default_cam_change = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    log_push_cclosure("lua_pushCclosure", default_cam_change.to_pointer());
    let default_control = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    for _ in 0..3 {
        log_push_cclosure("lua_pushCclosure", default_control.to_pointer());
    }

    install_basic_functions(lua, &globals, context.clone())?;

    install_stubbed_tables(lua, &globals, context.clone())?;

    // Defer legacy compat hooks until after core globals/consts are in place to better mirror
    // retail boot ordering (type/PI/system bound before setfallback/tag helpers).
    install_legacy_compat(lua, &globals, context.clone())?;

    let root = data_root.to_path_buf();
    let verbose = context.borrow().verbose();
    let dofile_context = context.clone();
    let wrapped_dofile = lua.create_function(move |lua_ctx, path: String| -> LuaResult<Value> {
        if let Some(value) = handle_special_dofile(lua_ctx, &path, dofile_context.clone())? {
            if verbose {
                println!("[lua][dofile] handled {} via host", path);
            }
            return Ok(value);
        }

        let mut tried = Vec::new();
        for candidate in candidate_paths(&path) {
            let absolute = if candidate.is_absolute() {
                candidate
            } else {
                root.join(&candidate)
            };
            tried.push(absolute.clone());
            if let Some(value) = execute_script(lua_ctx, &absolute)? {
                if verbose {
                    println!("[lua][dofile] loaded {}", absolute.display());
                }
                return Ok(value);
            }
        }

        if verbose {
            println!("[lua][dofile] skipped {}", path);
            for attempt in tried {
                println!("  tried {}", attempt.display());
            }
        }

        Ok(Value::Nil)
    })?;
    set_global(lua, &globals, "dofile", wrapped_dofile)?;

    Ok(())
}

fn install_basic_functions(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    if let Ok(type_fn) = globals.get::<_, Function>("type") {
        // Retail saves the stock type and wraps it so userdata can report richer tags.
        let type_ptr = type_fn.to_pointer();
        log_event(
            EventBuilder::new("get_global")
                .kv("name", "type")
                .kv("handle", format!("{type_ptr:p}")),
        );
        let saved_type = lua.create_registry_value(type_fn)?;
        log_store_ref(1, 1, Some("global:type".to_string()));
        let type_key = saved_type;
        let type_override = lua.create_function(move |lua_ctx, value: Value| {
            let original: Function = lua_ctx.registry_value(&type_key)?;
            let primary: Value = original.call(value)?;
            Ok(MultiValue::from_vec(vec![primary, Value::Nil]))
        })?;
        set_global(lua, globals, "type", type_override)?;
    }

    // Sector type / mode constants used during boot before any scripts run.
    set_global(lua, globals, "NONE", 0)?;
    set_global(lua, globals, "WALK", 1)?;
    set_global(lua, globals, "CAMERA", 2)?;
    set_global(lua, globals, "SPECIAL", 3)?;
    set_global(lua, globals, "HOT", 4)?;

    let debug_state = context.clone();
    let print_debug = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if debug_state.borrow().verbose() {
                println!("[lua][PrintDebug] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    set_global(lua, globals, "PrintDebug", print_debug)?;

    let logf_state = context.clone();
    let logf = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if logf_state.borrow().verbose() {
                println!("[lua][logf] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    set_global(lua, globals, "logf", logf)?;

    if let Ok(string_table) = globals.get::<_, Table>("string") {
        if let Ok(sub) = string_table.get::<_, Function>("sub") {
            set_global(lua, globals, "strsub", sub.clone())?;
        }
        if let Ok(find) = string_table.get::<_, Function>("find") {
            set_global(lua, globals, "strfind", find.clone())?;
        }
        if let Ok(lower) = string_table.get::<_, Function>("lower") {
            set_global(lua, globals, "strlower", lower.clone())?;
        }
        if let Ok(upper) = string_table.get::<_, Function>("upper") {
            set_global(lua, globals, "strupper", upper.clone())?;
        }
        if let Ok(len) = string_table.get::<_, Function>("len") {
            set_global(lua, globals, "strlen", len)?;
        }
    }

    if let Ok(math_table) = globals.get::<_, Table>("math") {
        if let Ok(sqrt_fn) = math_table.get::<_, Function>("sqrt") {
            set_global(lua, globals, "sqrt", sqrt_fn.clone())?;
        }
        if let Ok(abs_fn) = math_table.get::<_, Function>("abs") {
            set_global(lua, globals, "abs", abs_fn)?;
        }
    }

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    let bool_false = lua.create_function(|_, _: Variadic<Value>| Ok(false))?;
    let nil_return = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;

    set_global(
        lua,
        globals,
        "LockFont",
        lua.create_function(|_, name: String| Ok(format!("font::{name}")))?,
    )?;
    set_global(
        lua,
        globals,
        "LockCursor",
        lua.create_function(|_, name: String| Ok(format!("cursor::{name}")))?,
    )?;
    set_global(lua, globals, "SetSayLineDefaults", noop.clone())?;
    set_global(
        lua,
        globals,
        "GetPlatform",
        lua.create_function(|_, ()| Ok(1))?,
    )?; // PLATFORM_PC_WIN
    set_global(lua, globals, "ReadRegistryValue", nil_return.clone())?;
    set_global(lua, globals, "ReadRegistryIntValue", nil_return)?;
    set_global(lua, globals, "WriteRegistryValue", noop.clone())?;
    set_global(
        lua,
        globals,
        "enable_basic_remappable_key_set",
        noop.clone(),
    )?;
    set_global(lua, globals, "enable_joystick_controls", noop.clone())?;
    set_global(lua, globals, "enable_mouse_controls", noop.clone())?;
    set_global(lua, globals, "GetControlState", bool_false.clone())?;
    set_global(lua, globals, "get_generic_control_state", bool_false)?;
    set_global(lua, globals, "ResetMarioControls", noop.clone())?;
    set_global(
        lua,
        globals,
        "AreAchievementsInstalled",
        lua.create_function(|_, ()| Ok(1))?,
    )?;
    set_global(
        lua,
        globals,
        "GlobalSaveResolved",
        lua.create_function(|_, ()| Ok(1))?,
    )?;
    set_global(
        lua,
        globals,
        "CheckForFile",
        lua.create_function(|_, _: Variadic<Value>| Ok(false))?,
    )?;
    set_global(
        lua,
        globals,
        "CheckForCD",
        lua.create_function(|_, _: Variadic<Value>| Ok((false, false)))?,
    )?;
    set_global(lua, globals, "NukeResources", noop.clone())?;
    set_global(lua, globals, "GetSystemFonts", noop.clone())?;
    set_global(lua, globals, "PreloadCursors", noop.clone())?;
    set_global(lua, globals, "HideVerbSkull", noop.clone())?;
    set_global(lua, globals, "HideMouseCursor", noop.clone())?;
    set_global(lua, globals, "ShowCursor", noop.clone())?;
    set_global(lua, globals, "SetActiveCommentary", noop.clone())?;
    set_global(lua, globals, "SetAmbientLight", noop.clone())?;

    let break_here = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    set_global(lua, globals, "break_here", break_here)?;

    Ok(())
}

fn install_system_table(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let manny_select_ctx = context.clone();
    let manny = lua.create_table()?;
    manny.set(
        "set_selected",
        lua.create_function(move |_, _: Variadic<Value>| {
            manny_select_ctx
                .borrow_mut()
                .log_event("actor.select manny");
            Ok(())
        })?,
    )?;
    let manny_default_ctx = context.clone();
    manny.set(
        "default",
        lua.create_function(move |_, _: Variadic<Value>| {
            manny_default_ctx
                .borrow_mut()
                .log_event("actor.default manny");
            Ok(())
        })?,
    )?;
    let manny_put_ctx = context.clone();
    manny.set(
        "put_in_set",
        lua.create_function(move |_, args: Variadic<Value>| {
            let set = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            manny_put_ctx
                .borrow_mut()
                .log_event(format!("actor.put_in_set manny {set}"));
            Ok(())
        })?,
    )?;
    manny.set("is_holding", Value::Nil)?;

    let system = lua.create_table()?;
    system.set("setTable", lua.create_table()?)?;
    system.set("currentActor", manny)?;
    set_global(lua, globals, "system", system)?;
    log_store_ref(1, 0, Some("global:system".to_string()));
    Ok(())
}

fn install_legacy_io(lua: &Lua, globals: &Table) -> LuaResult<()> {
    // Legacy Lua 3 I/O shims expected by retail boot scripts.
    let io_handle = Rc::new(RefCell::new(None::<String>));
    let current_input = io_handle.clone();
    set_global(
        lua,
        globals,
        "readfrom",
        lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
            let mut handle_ref = current_input.borrow_mut();
            if let Some(Value::String(path)) = args.get(0) {
                *handle_ref = Some(path.to_str().unwrap_or("<input>").to_string());
                return Ok(Value::String(path.clone()));
            }
            *handle_ref = None;
            Ok(Value::Nil)
        })?,
    )?;

    let current_output = io_handle.clone();
    set_global(
        lua,
        globals,
        "writeto",
        lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
            let mut handle_ref = current_output.borrow_mut();
            if let Some(Value::String(path)) = args.get(0) {
                *handle_ref = Some(path.to_str().unwrap_or("<output>").to_string());
                return Ok(Value::String(path.clone()));
            }
            if let Some(Value::String(handle)) = args.get(0) {
                *handle_ref = Some(handle.to_str().unwrap_or("<output>").to_string());
                return Ok(Value::String(handle.clone()));
            }
            *handle_ref = None;
            Ok(Value::Nil)
        })?,
    )?;

    let append_state = io_handle.clone();
    set_global(
        lua,
        globals,
        "appendto",
        lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
            let mut handle_ref = append_state.borrow_mut();
            if let Some(Value::String(path)) = args.get(0) {
                *handle_ref = Some(path.to_str().unwrap_or("<append>").to_string());
                return Ok(Value::String(path.clone()));
            }
            Ok(Value::Nil)
        })?,
    )?;

    let read_state = io_handle.clone();
    set_global(
        lua,
        globals,
        "read",
        lua.create_function(move |_, args: Variadic<Value>| {
            let _handle = args
                .get(0)
                .and_then(value_to_string)
                .or_else(|| read_state.borrow().clone());
            Ok(Value::Nil)
        })?,
    )?;

    let write_state = io_handle.clone();
    set_global(
        lua,
        globals,
        "write",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle = args
                .get(0)
                .and_then(value_to_string)
                .or_else(|| write_state.borrow().clone())
                .unwrap_or_else(|| "<stdout>".to_string());
            let text: Vec<String> = args.iter().skip(1).filter_map(value_to_string).collect();
            if !text.is_empty() {
                eprintln!("[lua][write] {handle}: {}", text.join(""));
            }
            Ok(())
        })?,
    )?;

    set_global(lua, globals, "_INPUT", "stdin")?;
    set_global(lua, globals, "_OUTPUT", "stdout")?;
    set_global(lua, globals, "_STDIN", "stdin")?;
    set_global(lua, globals, "_STDOUT", "stdout")?;
    set_global(lua, globals, "_STDERR", "stderr")?;

    Ok(())
}

fn install_pi_constant(lua: &Lua, globals: &Table) -> LuaResult<()> {
    let fallback = Value::Number(3.141592653589793);
    let pi_value = globals
        .get::<_, Table>("math")
        .ok()
        .and_then(|math| math.get::<_, Value>("pi").ok())
        .unwrap_or(fallback);
    set_global(lua, globals, "PI", pi_value)?;
    Ok(())
}

fn install_stubbed_tables(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let prefs_init_ctx = context.clone();
    let prefs_write_ctx = context.clone();
    let prefs_voice_ctx = context.clone();
    let system_prefs = lua.create_table()?;
    system_prefs.set(
        "init",
        lua.create_function(move |_, _: Variadic<Value>| {
            prefs_init_ctx.borrow_mut().log_event("system_prefs.init");
            Ok(())
        })?,
    )?;
    system_prefs.set(
        "write",
        lua.create_function(move |_, args: Variadic<Value>| {
            let key = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            prefs_write_ctx
                .borrow_mut()
                .log_event(format!("system_prefs.write {key}"));
            Ok(())
        })?,
    )?;
    system_prefs.set(
        "set_voice_effect",
        lua.create_function(move |_, args: Variadic<Value>| {
            let effect = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            prefs_voice_ctx
                .borrow_mut()
                .log_event(format!("system_prefs.voice_effect {effect}"));
            Ok(())
        })?,
    )?;
    set_global(lua, globals, "system_prefs", system_prefs)?;

    if !matches!(globals.get::<_, Value>("system"), Ok(Value::Table(_))) {
        install_system_table(lua, globals, context.clone())?;
    }

    let logos_ctx = context.clone();
    let intro_ctx = context.clone();
    let cut_scene = lua.create_table()?;
    cut_scene.set(
        "logos",
        lua.create_function(move |_, _: Variadic<Value>| {
            logos_ctx
                .borrow_mut()
                .start_fullscreen_movie("logos".to_string(), Some(2));
            Ok(())
        })?,
    )?;
    cut_scene.set(
        "intro",
        lua.create_function(move |_, _: Variadic<Value>| {
            intro_ctx
                .borrow_mut()
                .start_fullscreen_movie("intro".to_string(), Some(4));
            Ok(())
        })?,
    )?;
    set_global(lua, globals, "cut_scene", cut_scene)?;

    let loading_ctx = context.clone();
    let loading_menu = lua.create_table()?;
    loading_menu.set(
        "run",
        lua.create_function(move |_, _: Variadic<Value>| {
            loading_ctx.borrow_mut().log_event("menu.loading.run");
            Ok(())
        })?,
    )?;
    loading_menu.set("is_visible", false)?;
    set_global(lua, globals, "loading_menu", loading_menu)?;

    let boot_ctx = context.clone();
    let boot_warning_menu = lua.create_table()?;
    boot_warning_menu.set(
        "run",
        lua.create_function(move |_, _: Variadic<Value>| {
            boot_ctx.borrow_mut().log_event("menu.boot_warning.run");
            Ok(())
        })?,
    )?;
    boot_warning_menu.set(
        "check_timeout",
        lua.create_function(|_, _: Variadic<Value>| Ok(()))?,
    )?;
    boot_warning_menu.set(
        "destroy",
        lua.create_function(|_, _: Variadic<Value>| Ok(()))?,
    )?;
    set_global(lua, globals, "boot_warning_menu", boot_warning_menu)?;

    let concepts_ctx = context.clone();
    let concept_menu = lua.create_table()?;
    concept_menu.set(
        "unlock_concepts",
        lua.create_function(move |_, args: Variadic<Value>| {
            let value = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            concepts_ctx
                .borrow_mut()
                .log_event(format!("concept_menu.unlock {value}"));
            Ok(())
        })?,
    )?;
    set_global(lua, globals, "concept_menu", concept_menu)?;

    if !matches!(globals.get::<_, Value>("footsteps"), Ok(Value::Table(_))) {
        let table = lua.create_table()?;
        let entry = lua.create_table()?;
        entry.set("prefix", "fs")?;
        entry.set("left_walk", 1)?;
        entry.set("right_walk", 1)?;
        table.set("default", entry)?;
        set_global(lua, globals, "footsteps", table)?;
    }

    Ok(())
}
