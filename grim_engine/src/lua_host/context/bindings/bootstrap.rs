use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};
use mlua::{
    Error as LuaError, Function, Lua, MultiValue, Result as LuaResult, Table, Value, Variadic,
};

use crate::lua_host::telemetry::{
    log_create_table, log_dofile, log_event, log_push_cclosure, log_push_number, log_push_object,
    log_push_usertag, log_set_fallback, log_set_table_entry, next_fabricated_handle, ptr_to_handle,
    register_tag,
};
use grim_telemetry_common::{LuaEvent, OriginFields, ValueFields};

use super::dofile::{candidate_paths, execute_script, handle_special_dofile};
use super::legacy::install_legacy_compat;
use super::util::{
    set_global, set_global_silent, value_fields_from_lua, value_to_string,
    value_to_upvalue_preview, with_registered_global_hint, RegisteredGlobalMeta, TaggedHandle,
};
use super::{store_registry_value, PinnedRegistryKeys};
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

pub(crate) fn install_globals_pre_system(
    lua: &Lua,
    data_root: &Path,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    let globals = lua.globals();

    set_global(lua, &globals, "_VERSION", "Lua 3.1 (alpha)")?;

    install_legacy_io(lua, &globals)?;
    let errorfb: Function = globals
        .get("error")
        .context("error handler missing from Lua state")?;
    log_push_cclosure("lua_pushCclosure", errorfb.to_pointer(), 0, Some("errorfb"));
    set_global(lua, &globals, "_TRIGMODE", "deg")?;
    install_legacy_compat(lua, &globals, context.clone())?;
    if let (Ok(settagmethod), Some(pow_fn)) = (
        globals.get::<_, Function>("settagmethod"),
        globals
            .get::<_, Table>("math")
            .ok()
            .and_then(|math| math.get::<_, Function>("pow").ok())
            .or_else(|| {
                lua.create_function(|_, (a, b): (f64, f64)| Ok(Value::Number(a.powf(b))))
                    .ok()
            }),
    ) {
        log_push_cclosure("lua_pushCclosure", pow_fn.to_pointer(), 0, Some("math_pow"));
        log_push_number("0");
        let _ = settagmethod.call::<_, Value>((-1, "pow", pow_fn));
    }
    if let Ok(math) = globals.get::<_, Table>("math") {
        if let Ok(random) = math.get::<_, Function>("random") {
            set_global(lua, &globals, "random", random)?;
        }
        if let Ok(randomseed) = math.get::<_, Function>("randomseed") {
            set_global(lua, &globals, "randomseed", randomseed)?;
        }
    }
    install_pi_constant(lua, &globals)?;
    lua.gc_collect()?;
    log_event(LuaEvent::CollectGarbage {});
    install_system_table(lua, &globals, context.clone())?;

    install_dofile(lua, &globals, data_root, context.clone())?;
    install_basic_functions_pre_system(lua, &globals, context.clone())?;

    Ok(())
}

pub(crate) fn install_globals_post_system(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    let globals = lua.globals();
    install_basic_functions_post_system(lua, &globals, context.clone())?;
    install_stubbed_tables(lua, &globals, context)?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn install_globals(
    lua: &Lua,
    data_root: &Path,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    install_globals_pre_system(lua, data_root, context.clone())?;
    install_globals_post_system(lua, context)?;
    Ok(())
}

fn install_basic_functions_pre_system(
    lua: &Lua,
    globals: &Table,
    _context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let nil_return = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;

    if let Ok(type_fn) = globals.get::<_, Function>("type") {
        let type_ptr = type_fn.to_pointer();
        let type_handle = ptr_to_handle(type_ptr);
        log_event(LuaEvent::GetGlobal {
            name: "type".to_string(),
            handle: type_handle.clone(),
            label: "global:type".to_string(),
            count: 1,
        });
        let saved_type = store_registry_value(
            lua,
            Value::Function(type_fn),
            1,
            Some(1),
            Some("global:type".to_string()),
            Some(type_handle),
            Some("global:type".to_string()),
        )?;
        let type_key = saved_type.key;
        let type_override = lua.create_function(move |lua_ctx, value: Value| {
            let original: Function = lua_ctx.registry_value(&type_key)?;
            let primary: Value = original.call(value)?;
            Ok(MultiValue::from_vec(vec![primary, Value::Nil]))
        })?;
        set_global(lua, globals, "type", type_override)?;
    }

    // Sector type / mode constants used during boot before any scripts run.
    set_global(lua, globals, "NONE", 0)?;
    set_global(lua, globals, "WALK", 4096)?;
    set_global(lua, globals, "CAMERA", 8192)?;
    set_global(lua, globals, "SPECIAL", 12288)?;
    set_global(lua, globals, "HOT", 16384)?;
    set_global(lua, globals, "ReadRegistryValue", nil_return.clone())?;
    set_global(lua, globals, "ReadRegistryIntValue", nil_return.clone())?;

    let concat_fallback = lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
        Err(LuaError::RuntimeError(
            "string expected in concatenation".to_string(),
        ))
    })?;
    let concat_handle = ptr_to_handle(concat_fallback.to_pointer());
    let concat_fields = value_fields_from_lua(&Value::Function(concat_fallback.clone()));
    log_set_fallback(
        "concat",
        concat_handle,
        concat_fields,
        Some(concat_fallback.to_pointer()),
    );

    // Retail stashes the concat fallback (typeFB) and a set of text property strings as refs
    // before binding additional globals; mirror that burst so ref ids and lua_getref calls line up.
    let lua_ref: Function = globals.get("lua_ref")?;
    let _ = lua_ref.call::<_, i32>(Value::Function(concat_fallback.clone()))?;
    for key in [
        "x",
        "y",
        "cache",
        "font",
        "width",
        "leftclip",
        "height",
        "fgcolor",
        "bgcolor",
        "fxcolor",
        "hicolor",
        "duration",
        "center",
        "ljustify",
        "rjustify",
        "layer",
        "highlight",
        "coords",
        "volume",
        "pan",
        "background",
        "alpha",
        "fade",
        "mirrormode",
    ] {
        let value = Value::String(lua.create_string(key)?);
        let _ = lua_ref.call::<_, i32>(value)?;
    }

    Ok(())
}

fn install_basic_functions_post_system(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let debug_state = context.clone();
    let print_debug = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.first() {
            if debug_state.borrow().verbose() {
                println!("[lua][PrintDebug] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    set_global(lua, globals, "PrintDebug", print_debug)?;

    let logf_state = context.clone();
    let logf = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.first() {
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

fn install_dofile(
    lua: &Lua,
    globals: &Table,
    data_root: &Path,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    let root = data_root.to_path_buf();
    let verbose = context.borrow().verbose();
    let dofile_context = context.clone();
    let wrapped_dofile = lua.create_function(move |lua_ctx, path: String| -> LuaResult<Value> {
        log_dofile(&path);
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
    // Avoid logging an extra semantic bind here so ordering matches retail tagmethod burst.
    set_global_silent(lua, globals, "dofile", wrapped_dofile)?;
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
                .first()
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            manny_put_ctx
                .borrow_mut()
                .log_event(format!("actor.put_in_set manny {set}"));
            Ok(())
        })?,
    )?;
    manny.set("is_holding", Value::Nil)?;
    // Retail exposes the player actor globally; mirror that so BOOT can call manny:* helpers.
    set_global(lua, globals, "manny", manny.clone())?;

    let system = lua.create_table()?;
    let system_handle = ptr_to_handle(system.to_pointer());
    let system_handle_label = Some("global:system".to_string());
    let system_fields = value_fields_from_lua(&Value::Table(system.clone()));
    // Keep bootstrap registry refs pinned for the lifetime of the process to mirror retail.
    let mut pinned_refs = PinnedRegistryKeys::default();
    log_create_table(system_handle.clone(), system_fields.clone());
    set_global(lua, globals, "system", system.clone())?;

    // Mirror retail bootstrap: stash system in the registry, then set controls via lua_getref flow.
    log_push_object(system_handle.clone(), system_fields.clone());
    let system_ref = store_registry_value(
        lua,
        Value::Table(system.clone()),
        1,
        None,
        Some("global:system".to_string()),
        Some(system_handle.clone()),
        system_handle_label.clone(),
    )?;
    let mut system: Table = system_ref.fetch(lua, OriginFields::default(), None)?;

    let controls = lua.create_table()?;
    let controls_handle = ptr_to_handle(controls.to_pointer());
    let controls_fields = value_fields_from_lua(&Value::Table(controls.clone()));
    log_create_table(controls_handle.clone(), controls_fields.clone());

    let key_preview = value_to_upvalue_preview(&Value::String(lua.create_string("controls")?));
    let value_preview = value_to_upvalue_preview(&Value::Table(controls.clone()));
    log_set_table_entry(
        system_handle.clone(),
        system_handle_label.clone(),
        key_preview,
        value_preview,
        None,
        Some(system_fields.clone()),
        Some((controls_handle.clone(), None, controls_fields.clone())),
    );
    system.set("controls", controls.clone())?;
    populate_controls_table(lua, &controls, &controls_handle, &controls_fields)?;

    // Retail fetches the stored system ref before installing default handlers; mirror the fetches without storing the closures.
    let default_cam_change = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    system = system_ref.fetch(lua, OriginFields::default(), None)?;
    log_push_cclosure(
        "lua_pushCclosure",
        default_cam_change.to_pointer(),
        0,
        Some("DefaultCamChangeHandlerL"),
    );
    system.set("camChangeHandler", default_cam_change)?;

    let default_control = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    for key in ["axisHandler", "inputModeHandler", "buttonHandler"] {
        let system_for_handler: Table = system_ref.fetch(lua, OriginFields::default(), None)?;
        log_push_cclosure(
            "lua_pushCclosure",
            default_control.to_pointer(),
            0,
            Some("DefaultControlHandlerL"),
        );
        system_for_handler.set(key, default_control.clone())?;
    }

    pinned_refs.push(system_ref.key);
    system.set("setTable", lua.create_table()?)?;
    system.set("currentActor", manny)?;
    Ok(())
}

fn populate_controls_table(
    lua: &Lua,
    controls: &Table,
    controls_handle: &String,
    controls_fields: &ValueFields,
) -> LuaResult<()> {
    // Retail populates system.controls before scripts run; mirror that list (and telemetry) here.
    const CONTROL_ENTRIES: [(&str, i32); 163] = [
        ("KEY_ESCAPE", 0),
        ("KEY_1", 1),
        ("KEY_2", 2),
        ("KEY_3", 3),
        ("KEY_4", 4),
        ("KEY_5", 5),
        ("KEY_6", 6),
        ("KEY_7", 7),
        ("KEY_8", 8),
        ("KEY_9", 9),
        ("KEY_0", 10),
        ("KEY_MINUS", 11),
        ("KEY_EQUALS", 12),
        ("KEY_BACK", 13),
        ("KEY_TAB", 14),
        ("KEY_Q", 15),
        ("KEY_W", 16),
        ("KEY_E", 17),
        ("KEY_R", 18),
        ("KEY_T", 19),
        ("KEY_Y", 20),
        ("KEY_U", 21),
        ("KEY_I", 22),
        ("KEY_O", 23),
        ("KEY_P", 24),
        ("KEY_LBRACKET", 25),
        ("KEY_RBRACKET", 26),
        ("KEY_RETURN", 27),
        ("KEY_LCONTROL", 28),
        ("KEY_A", 29),
        ("KEY_S", 30),
        ("KEY_D", 31),
        ("KEY_F", 32),
        ("KEY_G", 33),
        ("KEY_H", 34),
        ("KEY_J", 35),
        ("KEY_K", 36),
        ("KEY_L", 37),
        ("KEY_SEMICOLON", 38),
        ("KEY_APOSTROPHE", 39),
        ("KEY_GRAVE", 40),
        ("KEY_LSHIFT", 41),
        ("KEY_BACKSLASH", 42),
        ("KEY_Z", 43),
        ("KEY_X", 44),
        ("KEY_C", 45),
        ("KEY_V", 46),
        ("KEY_B", 47),
        ("KEY_N", 48),
        ("KEY_M", 49),
        ("KEY_COMMA", 50),
        ("KEY_PERIOD", 51),
        ("KEY_SLASH", 52),
        ("KEY_RSHIFT", 53),
        ("KEY_MULTIPLY", 54),
        ("KEY_LMENU", 55),
        ("KEY_SPACE", 56),
        ("KEY_CAPITAL", 57),
        ("KEY_F1", 58),
        ("KEY_F2", 59),
        ("KEY_F3", 60),
        ("KEY_F4", 61),
        ("KEY_F5", 62),
        ("KEY_F6", 63),
        ("KEY_F7", 64),
        ("KEY_F8", 65),
        ("KEY_F9", 66),
        ("KEY_F11", 67),
        ("KEY_NUMLOCK", 68),
        ("KEY_SCROLL", 69),
        ("KEY_NUMPAD7", 70),
        ("KEY_NUMPAD8", 71),
        ("KEY_NUMPAD9", 72),
        ("KEY_SUBTRACT", 73),
        ("KEY_NUMPAD4", 74),
        ("KEY_NUMPAD5", 75),
        ("KEY_NUMPAD6", 76),
        ("KEY_ADD", 77),
        ("KEY_NUMPAD1", 78),
        ("KEY_NUMPAD2", 79),
        ("KEY_NUMPAD3", 80),
        ("KEY_NUMPAD0", 81),
        ("KEY_DECIMAL", 82),
        ("KEY_F11", 83),
        ("KEY_F12", 84),
        ("KEY_F13", 85),
        ("KEY_F14", 86),
        ("KEY_F15", 87),
        ("KEY_NUMPADEQUALS", 88),
        ("KEY_AT", 89),
        ("KEY_COLON", 90),
        ("KEY_UNDERLINE", 91),
        ("KEY_STOP", 92),
        ("KEY_NUMPADENTER", 93),
        ("KEY_RCONTROL", 94),
        ("KEY_NUMPADCOMMA", 95),
        ("KEY_DIVIDE", 96),
        ("KEY_SYSRQ", 97),
        ("KEY_RMENU", 98),
        ("KEY_HOME", 99),
        ("KEY_UP", 100),
        ("KEY_PRIOR", 101),
        ("KEY_LEFT", 102),
        ("KEY_RIGHT", 103),
        ("KEY_END", 104),
        ("KEY_DOWN", 105),
        ("KEY_NEXT", 106),
        ("KEY_INSERT", 107),
        ("KEY_DELETE", 108),
        ("KEY_LWIN", 109),
        ("KEY_RWIN", 110),
        ("KEY_APS", 111),
        ("KEY_JOY1_B1", 112),
        ("KEY_JOY1_B2", 113),
        ("KEY_JOY1_B3", 114),
        ("KEY_JOY1_B4", 115),
        ("KEY_JOY1_B5", 116),
        ("KEY_JOY1_B6", 117),
        ("KEY_JOY1_B7", 118),
        ("KEY_JOY1_B8", 119),
        ("KEY_JOY1_B9", 120),
        ("KEY_JOY1_B10", 121),
        ("KEY_JOY1_B11", 122),
        ("KEY_JOY1_B12", 123),
        ("KEY_JOY1_HLEFT", 124),
        ("KEY_JOY1_HUP", 125),
        ("KEY_JOY1_HRIGHT", 126),
        ("KEY_JOY1_HDOWN", 127),
        ("KEY_JOY2_B1", 128),
        ("KEY_JOY2_B2", 129),
        ("KEY_JOY2_B3", 130),
        ("KEY_JOY2_B4", 131),
        ("KEY_JOY2_B5", 132),
        ("KEY_JOY2_B6", 133),
        ("KEY_JOY2_B7", 134),
        ("KEY_JOY2_B8", 135),
        ("KEY_JOY2_B9", 136),
        ("KEY_JOY2_B10", 137),
        ("KEY_JOY2_HLEFT", 138),
        ("KEY_JOY2_HUP", 139),
        ("KEY_JOY2_HRIGHT", 140),
        ("KEY_JOY2_HDOWN", 141),
        ("KEY_MOUSE_B1", 142),
        ("KEY_MOUSE_B2", 143),
        ("KEY_MOUSE_B3", 144),
        ("KEY_MOUSE_B4", 145),
        ("KEY_MOUSE_LONG", 146),
        ("KEY_MOUSE_PING", 147),
        ("AXIS_JOY1_X", 148),
        ("AXIS_JOY1_Y", 149),
        ("AXIS_JOY1_Z", 150),
        ("AXIS_JOY1_R", 151),
        ("AXIS_JOY1_U", 152),
        ("AXIS_JOY1_V", 153),
        ("AXIS_JOY2_X", 154),
        ("AXIS_JOY2_Y", 155),
        ("AXIS_JOY2_Z", 156),
        ("AXIS_JOY2_R", 157),
        ("AXIS_JOY2_U", 158),
        ("AXIS_JOY2_V", 159),
        ("AXIS_MOUSE_X", 160),
        ("AXIS_MOUSE_Y", 161),
        ("AXIS_MOUSE_Z", 162),
    ];

    for (name, code) in CONTROL_ENTRIES {
        let key_value = Value::String(lua.create_string(name)?);
        let key_preview = value_to_upvalue_preview(&key_value);
        let value_preview = value_to_upvalue_preview(&Value::Integer(code as i64));
        log_set_table_entry(
            controls_handle.clone(),
            None,
            key_preview,
            value_preview,
            None,
            Some(controls_fields.clone()),
            None,
        );
        controls.set(name, code)?;
    }

    Ok(())
}

fn install_legacy_io(lua: &Lua, globals: &Table) -> LuaResult<()> {
    // Legacy Lua 3 I/O shims expected by retail boot scripts.
    const IO_HANDLE_TAG: i32 = -16;
    const IO_FALLBACK_TAG: i32 = -17;

    register_tag(IO_HANDLE_TAG, Some("io_handle".to_string()));
    register_tag(IO_FALLBACK_TAG, Some("io_fallback".to_string()));

    let io_handle = Rc::new(RefCell::new(None::<String>));
    let current_input = io_handle.clone();
    let readfrom = lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
        let mut handle_ref = current_input.borrow_mut();
        if let Some(Value::String(path)) = args.first() {
            *handle_ref = Some(path.to_str().unwrap_or("<input>").to_string());
            return Ok(Value::String(path.clone()));
        }
        *handle_ref = None;
        Ok(Value::Nil)
    })?;

    let current_output = io_handle.clone();
    let writeto = lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
        let mut handle_ref = current_output.borrow_mut();
        if let Some(Value::String(path)) = args.first() {
            *handle_ref = Some(path.to_str().unwrap_or("<output>").to_string());
            return Ok(Value::String(path.clone()));
        }
        if let Some(Value::String(handle)) = args.first() {
            *handle_ref = Some(handle.to_str().unwrap_or("<output>").to_string());
            return Ok(Value::String(handle.clone()));
        }
        *handle_ref = None;
        Ok(Value::Nil)
    })?;

    let append_state = io_handle.clone();
    let appendto = lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
        let mut handle_ref = append_state.borrow_mut();
        if let Some(Value::String(path)) = args.first() {
            *handle_ref = Some(path.to_str().unwrap_or("<append>").to_string());
            return Ok(Value::String(path.clone()));
        }
        Ok(Value::Nil)
    })?;

    let read_state = io_handle.clone();
    let read = lua.create_function(move |_, args: Variadic<Value>| {
        let _handle = args
            .first()
            .and_then(value_to_string)
            .or_else(|| read_state.borrow().clone());
        Ok(Value::Nil)
    })?;

    let write_state = io_handle.clone();
    let write = lua.create_function(move |_, args: Variadic<Value>| {
        let handle = args
            .first()
            .and_then(value_to_string)
            .or_else(|| write_state.borrow().clone())
            .unwrap_or_else(|| "<stdout>".to_string());
        let text: Vec<String> = args.iter().skip(1).filter_map(value_to_string).collect();
        if !text.is_empty() {
            eprintln!("[lua][write] {handle}: {}", text.join(""));
        }
        Ok(())
    })?;

    const IO_UPVALUES: i32 = 2;

    bind_io_function(lua, globals, "readfrom", readfrom, IO_UPVALUES)?;
    bind_io_function(lua, globals, "writeto", writeto, IO_UPVALUES)?;
    bind_io_function(lua, globals, "appendto", appendto, IO_UPVALUES)?;
    bind_io_function(lua, globals, "read", read, IO_UPVALUES)?;
    bind_io_function(lua, globals, "write", write, IO_UPVALUES)?;

    for name in ["_INPUT", "_OUTPUT", "_STDIN", "_STDOUT", "_STDERR"] {
        let fabricated = next_fabricated_handle();
        let userdata = lua.create_userdata(TaggedHandle::new(IO_HANDLE_TAG))?;
        let handle = ptr_to_handle(userdata.to_pointer());
        let meta = RegisteredGlobalMeta {
            ..Default::default()
        };
        log_push_usertag(fabricated.raw, IO_HANDLE_TAG, handle);
        with_registered_global_hint(meta, || set_global(lua, globals, name, userdata))?;
    }

    Ok(())
}

fn bind_io_function<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    func: Function<'lua>,
    upvalues: i32,
) -> LuaResult<()> {
    let meta = RegisteredGlobalMeta { upvalues };
    with_registered_global_hint(meta, || set_global(lua, globals, name, func))
}

fn install_pi_constant(lua: &Lua, globals: &Table) -> LuaResult<()> {
    let fallback = Value::Number(std::f64::consts::PI);
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
                .first()
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
                .first()
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
                .first()
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
