use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::rc::Rc;

use anyhow::{Context, Result};
use grim_analysis::resources::normalize_legacy_lua;
use mlua::{
    Error as LuaError, Function, IntoLua, Lua, MultiValue, RegistryKey, Result as LuaResult, Table,
    Value, Variadic,
};

use crate::lua_host::telemetry::{
    log_bind_global, log_event, log_push_cclosure, log_set_tagmethod, log_store_ref, EventBuilder,
};

use super::EngineContext;

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

fn set_global<'lua, T: IntoLua<'lua>>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    value: T,
) -> LuaResult<()> {
    let value = value.into_lua(lua)?;
    if let Value::Function(ref func) = value {
        let ptr = func.to_pointer();
        log_push_cclosure("lua_pushCclosure", ptr);
        log_bind_global(name, ptr);
    } else {
        log_bind_global(name, ptr::null());
    }
    globals.set(name, value)
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
    if let Ok(math_table) = globals.get::<_, Table>("math") {
        if let Ok(pi_value) = math_table.get::<_, Value>("pi") {
            set_global(lua, globals, "PI", pi_value)?;
        } else {
            set_global(lua, globals, "PI", 3.141592653589793)?;
        }
    } else {
        set_global(lua, globals, "PI", 3.141592653589793)?;
    }
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

fn install_legacy_compat<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let fallbacks = Rc::new(RefCell::new(LegacyFallbacks::new(lua)?));
    install_fallback_globals(lua, globals, fallbacks.clone(), context.clone())?;
    install_index_hook(lua, globals, fallbacks.clone())?;
    install_error_wrapper(lua, globals, fallbacks)?;

    Ok(())
}

fn install_fallback_globals<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    fallbacks: Rc<RefCell<LegacyFallbacks>>,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let setfallback_state = fallbacks.clone();
    let setfallback_ctx = context.clone();
    let setfallback =
        lua.create_function(move |lua_ctx, (event, handler): (String, Function)| {
            let event = event.to_ascii_lowercase();
            if !setfallback_state.borrow().is_known_event(&event)
                && setfallback_ctx.borrow().verbose()
            {
                eprintln!("[lua][setfallback] installing stubbed handler for {event}");
            }
            let previous = setfallback_state
                .borrow_mut()
                .set_fallback_for_all(lua_ctx, &event, handler)?;
            Ok(previous.map(Value::Function).unwrap_or(Value::Nil))
        })?;
    set_global(lua, globals, "setfallback", setfallback)?;

    let gettag_state = fallbacks.clone();
    let gettagmethod = lua.create_function(
        move |lua_ctx, (tag, event): (Value, String)| -> LuaResult<Value> {
            let tag = LegacyFallbacks::parse_tag(tag);
            let event = event.to_ascii_lowercase();
            let method = gettag_state.borrow().get_tag_method(lua_ctx, tag, &event)?;
            Ok(method.map(Value::Function).unwrap_or(Value::Nil))
        },
    )?;
    set_global(lua, globals, "gettagmethod", gettagmethod)?;

    let settag_state = fallbacks.clone();
    let settagmethod = lua.create_function(
        move |lua_ctx, (tag, event, handler): (Value, String, Function)| -> LuaResult<Value> {
            let tag = LegacyFallbacks::parse_tag(tag);
            let event = event.to_ascii_lowercase();
            let previous =
                settag_state
                    .borrow_mut()
                    .set_tag_method(lua_ctx, tag, &event, handler.clone())?;
            if tag == LegacyFallbacks::TAG_NIL {
                settag_state
                    .borrow_mut()
                    .set_fallback_for_tag(lua_ctx, &event, handler)?;
            }
            Ok(previous.map(Value::Function).unwrap_or(Value::Nil))
        },
    )?;
    set_global(lua, globals, "settagmethod", settagmethod)?;

    let seterror_state = fallbacks.clone();
    let seterrormethod =
        lua.create_function(move |lua_ctx, handler: Function| -> LuaResult<Value> {
            let previous = seterror_state
                .borrow_mut()
                .set_fallback_for_all(lua_ctx, "error", handler)?;
            Ok(previous.map(Value::Function).unwrap_or(Value::Nil))
        })?;
    set_global(lua, globals, "seterrormethod", seterrormethod)?;

    let tag =
        lua.create_function(|_, value: Value| Ok(LegacyFallbacks::tag_id_for_value(&value)))?;
    set_global(lua, globals, "tag", tag)?;

    let refs: Rc<RefCell<HashMap<i32, RegistryKey>>> = Rc::new(RefCell::new(HashMap::new()));
    let next_ref = Rc::new(RefCell::new(2i32));
    let refs_state = refs.clone();
    let next_state = next_ref.clone();
    let lua_ref = lua.create_function(move |lua_ctx, value: Value| -> LuaResult<i32> {
        let mut counter = next_state.borrow_mut();
        let handle = *counter;
        *counter = counter.wrapping_add(1).max(1);
        let key = lua_ctx.create_registry_value(value)?;
        refs_state.borrow_mut().insert(handle, key);
        log_store_ref(1, handle, Some("lua_ref".to_string()));
        Ok(handle)
    })?;
    set_global(lua, globals, "lua_ref", lua_ref)?;

    let refs_state = refs.clone();
    let lua_unref = lua.create_function(move |_, handle: i32| {
        refs_state.borrow_mut().remove(&handle);
        log_store_ref(1, handle, Some("lua_unref".to_string()));
        Ok(())
    })?;
    set_global(lua, globals, "lua_unref", lua_unref)?;

    let refs_state = refs.clone();
    let lua_getref = lua.create_function(move |lua_ctx, handle: i32| -> LuaResult<Value> {
        let value = refs_state
            .borrow()
            .get(&handle)
            .map(|key| lua_ctx.registry_value::<Value>(key))
            .transpose()?;
        Ok(value.unwrap_or(Value::Nil))
    })?;
    set_global(lua, globals, "lua_getref", lua_getref)?;

    Ok(())
}

fn install_index_hook(
    lua: &Lua,
    globals: &Table,
    fallbacks: Rc<RefCell<LegacyFallbacks>>,
) -> LuaResult<()> {
    let index_state = fallbacks.clone();
    let index_fb = lua.create_function(move |lua_ctx, (table, key): (Value, Value)| {
        let handler = index_state.borrow().handler_for_event(lua_ctx, "index")?;
        if let Some(func) = handler {
            return func.call::<_, Value>((table, key));
        }
        Ok(Value::Nil)
    })?;

    let metatable = match globals.get_metatable() {
        Some(table) => table,
        None => lua.create_table()?,
    };
    metatable.set("__index", index_fb)?;
    globals.set_metatable(Some(metatable));
    Ok(())
}

fn install_error_wrapper<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    fallbacks: Rc<RefCell<LegacyFallbacks>>,
) -> LuaResult<()> {
    let original_error: Function = globals.get("error")?;
    let original_error_key = lua.create_registry_value(original_error)?;
    let error_state = fallbacks.clone();
    let wrapped_error = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        if let Some(handler) = error_state.borrow().handler_for_event(lua_ctx, "error")? {
            let _ = handler.call::<_, Value>(args.clone());
        }
        let call_error: Function = lua_ctx.registry_value(&original_error_key)?;
        call_error.call::<_, Value>(args)
    })?;
    set_global(lua, globals, "error", wrapped_error)?;
    Ok(())
}

struct LegacyFallbacks {
    defaults: HashMap<String, RegistryKey>,
    fallbacks: HashMap<String, RegistryKey>,
    tag_methods: HashMap<i64, HashMap<String, RegistryKey>>,
}

impl LegacyFallbacks {
    const TAG_NIL: i64 = -1;
    const TAG_BOOLEAN: i64 = -2;
    const TAG_NUMBER: i64 = 0;
    const TAG_STRING: i64 = 1;
    const TAG_TABLE: i64 = 2;
    const TAG_FUNCTION: i64 = 3;
    const TAG_THREAD: i64 = 4;
    const TAG_USERDATA: i64 = 5;
    const TAG_LIGHTUSERDATA: i64 = 6;
    const TAG_ERROR: i64 = 7;

    fn new(lua: &Lua) -> LuaResult<Self> {
        let mut state = Self {
            defaults: HashMap::new(),
            fallbacks: HashMap::new(),
            tag_methods: HashMap::new(),
        };

        state.install_default(
            lua,
            "gettable",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "indexed expression not a table".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "settable",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "indexed expression not a table".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "index",
            lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
        )?;
        state.install_default(
            lua,
            "getglobal",
            lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
        )?;
        state.install_default(
            lua,
            "arith",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "number expected in arithmetic operation".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "order",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "incompatible types in comparison".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "concat",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "string expected in concatenation".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "gc",
            lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
        )?;
        state.install_default(
            lua,
            "function",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "called expression not a function".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "error",
            lua.create_function(|_, args: Variadic<Value>| {
                if let Some(Value::String(message)) = args.get(0) {
                    eprintln!("[lua][error] {}", message.to_str()?);
                }
                Ok(Value::Nil)
            })?,
        )?;

        Ok(state)
    }

    fn install_default(&mut self, lua: &Lua, event: &str, func: Function) -> LuaResult<()> {
        let key = lua.create_registry_value(func)?;
        self.defaults.insert(event.to_string(), key);
        Ok(())
    }

    fn is_known_event(&self, event: &str) -> bool {
        self.defaults.contains_key(event)
    }

    fn handler_for_event<'lua>(
        &self,
        lua: &'lua Lua,
        event: &str,
    ) -> LuaResult<Option<Function<'lua>>> {
        if let Some(key) = self.fallbacks.get(event) {
            return lua.registry_value(key).map(Some);
        }
        if let Some(key) = self.defaults.get(event) {
            return lua.registry_value(key).map(Some);
        }
        Ok(None)
    }

    fn set_fallback_for_all<'lua>(
        &mut self,
        lua: &'lua Lua,
        event: &str,
        handler: Function<'lua>,
    ) -> LuaResult<Option<Function<'lua>>> {
        let previous = self.get_tag_method(lua, Self::TAG_NIL, event)?;
        let key = lua.create_registry_value(handler.clone())?;
        self.fallbacks.insert(event.to_string(), key);

        for tag in Self::default_tags() {
            let func = handler.clone();
            self.set_tag_method(lua, tag, event, func)?;
        }

        Ok(previous)
    }

    fn set_fallback_for_tag<'lua>(
        &mut self,
        lua: &'lua Lua,
        event: &str,
        handler: Function<'lua>,
    ) -> LuaResult<()> {
        let key = lua.create_registry_value(handler)?;
        self.fallbacks.insert(event.to_string(), key);
        Ok(())
    }

    fn set_tag_method<'lua>(
        &mut self,
        lua: &'lua Lua,
        tag: i64,
        event: &str,
        handler: Function<'lua>,
    ) -> LuaResult<Option<Function<'lua>>> {
        let previous = self.get_tag_method(lua, tag, event)?;
        let key = lua.create_registry_value(handler)?;
        self.tag_methods
            .entry(tag)
            .or_default()
            .insert(event.to_string(), key);
        log_set_tagmethod(tag, event);
        Ok(previous)
    }

    fn get_tag_method<'lua>(
        &self,
        lua: &'lua Lua,
        tag: i64,
        event: &str,
    ) -> LuaResult<Option<Function<'lua>>> {
        if let Some(methods) = self.tag_methods.get(&tag) {
            if let Some(key) = methods.get(event) {
                return lua.registry_value(key).map(Some);
            }
        }
        self.handler_for_event(lua, event)
    }

    fn parse_tag(value: Value) -> i64 {
        match value {
            Value::Integer(id) => id,
            Value::Number(id) => id.trunc() as i64,
            other => Self::tag_id_for_value(&other),
        }
    }

    fn tag_id_for_value(value: &Value) -> i64 {
        match value {
            Value::Nil => Self::TAG_NIL,
            Value::Boolean(_) => Self::TAG_BOOLEAN,
            Value::Integer(_) | Value::Number(_) => Self::TAG_NUMBER,
            Value::String(_) => Self::TAG_STRING,
            Value::Table(_) => Self::TAG_TABLE,
            Value::Function(_) => Self::TAG_FUNCTION,
            Value::Thread(_) => Self::TAG_THREAD,
            Value::UserData(_) => Self::TAG_USERDATA,
            Value::LightUserData(_) => Self::TAG_LIGHTUSERDATA,
            Value::Error(_) => Self::TAG_ERROR,
        }
    }

    fn default_tags() -> Vec<i64> {
        let mut tags = vec![
            0,
            Self::TAG_NIL,
            Self::TAG_NUMBER,
            Self::TAG_STRING,
            Self::TAG_TABLE,
            Self::TAG_FUNCTION,
        ];
        tags.sort_unstable();
        tags.dedup();
        tags
    }
}

fn handle_special_dofile<'lua>(
    _lua: &'lua Lua,
    path: &str,
    _context: Rc<RefCell<EngineContext>>,
) -> LuaResult<Option<Value<'lua>>> {
    if let Some(filename) = Path::new(path).file_name().and_then(|name| name.to_str()) {
        let lower = filename.to_ascii_lowercase();
        match lower.as_str() {
            "setfallback.lua"
            | "_colors.lua"
            | "_colors.decompiled.lua"
            | "_sfx.lua"
            | "_sfx.decompiled.lua"
            | "_controls.lua"
            | "_controls.decompiled.lua"
            | "_dialog.lua"
            | "_dialog.decompiled.lua"
            | "_music.lua"
            | "_music.decompiled.lua"
            | "_mouse.lua"
            | "_mouse.decompiled.lua"
            | "_ui.lua"
            | "_ui.decompiled.lua"
            | "_achievement.lua"
            | "_achievement.decompiled.lua"
            | "_actors.lua"
            | "_actors.decompiled.lua"
            | "_objects.lua"
            | "_objects.decompiled.lua"
            | "_sets.lua"
            | "_sets.decompiled.lua"
            | "_inventory.lua"
            | "_inventory.decompiled.lua"
            | "_cut_scenes.lua"
            | "_cut_scenes.decompiled.lua"
            | "menu_loading.lua"
            | "menu_loading.decompiled.lua"
            | "menu_boot_warning.lua"
            | "menu_boot_warning.decompiled.lua"
            | "menu_dialog.lua"
            | "menu_dialog.decompiled.lua"
            | "menu_common.lua"
            | "menu_common.decompiled.lua"
            | "menu_remap_keys.lua"
            | "menu_remap_keys.decompiled.lua"
            | "menu_prefs.lua"
            | "menu_prefs.decompiled.lua" => return Ok(Some(Value::Boolean(true))),
            _ => {}
        }

        if lower.starts_with("achievementdefinitions_") {
            return Ok(Some(Value::Boolean(true)));
        }

        if lower.ends_with("_inv.lua") || lower.ends_with("_inv.decompiled.lua") {
            return Ok(Some(Value::Boolean(true)));
        }

        if lower == "mn_scythe.lua" || lower == "mn_scythe.decompiled.lua" {
            return Ok(Some(Value::Boolean(true)));
        }
    }

    if path.to_ascii_lowercase().contains("telemetry.lua") {
        return Ok(Some(Value::Boolean(true)));
    }

    Ok(None)
}

fn add_variants(file: &str, variants: &mut Vec<PathBuf>) {
    if file.contains('.') {
        variants.push(PathBuf::from(file));
        return;
    }
    variants.push(PathBuf::from(file));
    variants.push(PathBuf::from(format!("{file}.lua")));
    variants.push(PathBuf::from(format!("{file}.decompiled.lua")));
}

fn candidate_paths(path: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if Path::new(path).is_absolute() {
        candidates.push(PathBuf::from(path));
        return candidates;
    }
    add_variants(path, &mut candidates);
    if let Some(file_name) = Path::new(path).file_name().and_then(|name| name.to_str()) {
        add_variants(file_name, &mut candidates);
    }
    candidates
}

fn execute_script<'lua>(lua: &'lua Lua, path: &Path) -> LuaResult<Option<Value<'lua>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(LuaError::external)?;
    let chunk_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("script");
    let eval_result = if path.to_string_lossy().ends_with(".decompiled.lua") {
        let source = String::from_utf8_lossy(&bytes);
        let script = normalize_legacy_lua(&source);
        lua.load(&script).set_name(chunk_name).eval::<MultiValue>()
    } else if is_precompiled_chunk(&bytes) {
        lua.load(&bytes).set_name(chunk_name).eval::<MultiValue>()
    } else {
        let source = String::from_utf8_lossy(&bytes).into_owned();
        lua.load(&source).set_name(chunk_name).eval::<MultiValue>()
    };

    match eval_result {
        Ok(results) => Ok(Some(results.into_iter().next().unwrap_or(Value::Nil))),
        Err(LuaError::SyntaxError { message, .. })
            if message.contains("bad header in precompiled chunk") =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn is_precompiled_chunk(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == 0x1B && bytes[1] == b'L' && bytes[2] == b'u' && bytes[3] == b'a'
}

pub(crate) fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let system_path = data_root.join("_system.decompiled.lua");
    let source = fs::read_to_string(&system_path)
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

fn value_to_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Integer(i) if *i >= 0 => Some(*i as u32),
        Value::Number(n) if *n >= 0.0 => Some(n.trunc() as u32),
        Value::String(text) => text.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
}

pub(crate) fn describe_callable_label(value: &Value) -> String {
    match value {
        Value::String(text) => text.to_str().unwrap_or("<string>").to_string(),
        Value::Function(_) => "<function>".to_string(),
        Value::Table(_) => "<table>".to_string(),
        other => describe_value(other),
    }
}

pub(crate) fn describe_value(value: &Value) -> String {
    match value {
        Value::Nil => "<nil>".to_string(),
        Value::Boolean(flag) => flag.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(text) => text.to_str().unwrap_or("<string>").to_string(),
        Value::Table(_) => "<table>".to_string(),
        Value::Function(_) => "<function>".to_string(),
        Value::Thread(_) => "<thread>".to_string(),
        Value::UserData(_) => "<userdata>".to_string(),
        Value::Error(err) => err.to_string(),
        Value::LightUserData(_) => "<lightuserdata>".to_string(),
    }
}

pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_str().ok()?.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{LuaOptions, Result as LuaResult, StdLib};
    use std::path::Path;

    fn setup_lua() -> Lua {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        let context = Rc::new(RefCell::new(EngineContext::new(true, true)));
        install_globals(&lua, Path::new("."), context).unwrap();
        lua
    }

    #[test]
    fn setfallback_returns_previous_handler() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();

        let handler_one = lua.create_function(|_, ()| Ok("first")).unwrap();
        let previous = setfallback
            .call::<_, Value>(("index", handler_one.clone()))
            .unwrap();
        if let Value::Function(default_fb) = previous {
            let default_result: Value = default_fb.call((Value::Nil, Value::Nil)).unwrap();
            assert!(matches!(default_result, Value::Nil));
        } else {
            panic!("expected function from default index fallback");
        }

        let handler_two = lua.create_function(|_, ()| Ok("second")).unwrap();
        let returned = setfallback
            .call::<_, Value>(("index", handler_two.clone()))
            .unwrap();
        let previous_fn = match returned {
            Value::Function(func) => func,
            other => panic!("expected function from previous handler, got {other:?}"),
        };
        assert_eq!(previous_fn.to_pointer(), handler_one.to_pointer());
    }

    #[test]
    fn setfallback_rejects_non_function() {
        let lua = setup_lua();
        let result = lua.load("return setfallback('index', 42)").eval::<Value>();
        assert!(result.is_err());
    }

    #[test]
    fn error_fallback_runs_before_error() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let flag = Rc::new(RefCell::new(false));
        let flag_ref = flag.clone();
        let handler = lua
            .create_function(move |_, _: Variadic<Value>| {
                *flag_ref.borrow_mut() = true;
                Ok(Value::Nil)
            })
            .unwrap();
        setfallback.call::<_, Value>(("error", handler)).unwrap();
        let result: LuaResult<()> = lua.load("error('boom')").exec();
        assert!(result.is_err());
        assert!(*flag.borrow());
    }

    #[test]
    fn index_fallback_applies_to_missing_globals() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua
            .create_function(
                |lua_ctx, (_table, key): (Value, Value)| -> LuaResult<Value> {
                    let key_name = match key {
                        Value::String(text) => text.to_str().unwrap_or("<key>").to_string(),
                        other => describe_value(&other),
                    };
                    Ok(Value::String(
                        lua_ctx.create_string(&format!("fb::{key_name}"))?,
                    ))
                },
            )
            .unwrap();
        setfallback.call::<_, Value>(("index", handler)).unwrap();
        let value: String = lua.load("return missing_global_name").eval().unwrap();
        assert_eq!(value, "fb::missing_global_name");
    }

    #[test]
    fn gettable_fallback_available_via_tag_lookup() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua
            .create_function(|_, (_table, _key): (Value, Value)| Ok("handled"))
            .unwrap();
        setfallback.call::<_, Value>(("gettable", handler)).unwrap();
        let value: String = lua
            .load("local fb = gettagmethod(tag(nil), 'gettable'); return fb(nil, 'field')")
            .eval()
            .unwrap();
        assert_eq!(value, "handled");
    }

    #[test]
    fn unknown_fallbacks_can_be_installed() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua.create_function(|_, ()| Ok(123)).unwrap();
        let previous: Value = setfallback.call(("mystery", handler)).unwrap();
        assert!(matches!(previous, Value::Nil));
        let value: i32 = lua
            .load("local fb = gettagmethod(tag(nil), 'mystery'); return fb()")
            .eval()
            .unwrap();
        assert_eq!(value, 123);
    }
}
