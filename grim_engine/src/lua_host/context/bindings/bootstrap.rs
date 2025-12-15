use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};
use mlua::{
    Error as LuaError, Function, IntoLuaMulti, Lua, MultiValue, Result as LuaResult, Table, Value,
    Variadic,
};

use crate::lua_host::telemetry::{
    log_create_table, log_dofile, log_event, log_push_cclosure, log_push_number, log_push_object,
    log_push_usertag, log_set_fallback, next_fabricated_handle, ptr_to_handle, register_tag,
};
use grim_telemetry_schema::{LuaEvent, OriginFields, UpvaluePreview, ValueFields, ValueType};

use super::dofile::{candidate_paths, execute_script, handle_special_dofile};
use super::legacy::{install_legacy_compat, install_legacy_math};
use super::util::{
    set_global, set_table_entry_with_telemetry, value_fields_from_lua, value_to_string,
    with_registered_global_hint, with_suppressed_registered_globals, ColorHandle,
    RegisteredGlobalMeta, TaggedHandle, COLOR_TAG,
};
use super::{store_registry_value, PinnedRegistryKeys};
use crate::lua_host::context::EngineContext;

#[derive(Copy, Clone)]
enum GlobalConst {
    Str(&'static str),
    Int(i32),
}

const ACTOR_TAG: i32 = 0x52544341; // 'ACTR'

fn bind_const_globals<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    bindings: &[(&str, GlobalConst)],
) -> LuaResult<()> {
    for (name, value) in bindings {
        match value {
            GlobalConst::Str(text) => set_global(lua, globals, name, *text)?,
            GlobalConst::Int(num) => set_global(lua, globals, name, *num)?,
        }
    }
    Ok(())
}

fn bind_fn_globals<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    bindings: &[(&str, &Function<'lua>)],
) -> LuaResult<()> {
    for (name, func) in bindings {
        set_global(lua, globals, name, (*func).clone())?;
    }
    Ok(())
}

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

    install_constants_and_legacy(lua, &globals, &context)
        .context("pre-system: constants_and_legacy")?;
    install_system_table(lua, &globals).context("pre-system: system_table_and_controls")?;
    install_runtime_bindings(lua, &globals, data_root, &context)
        .context("pre-system: runtime_bindings")?;
    with_suppressed_registered_globals(|| install_stubbed_globals(lua, &globals, &context))
        .context("pre-system: stubs")?;

    Ok(())
}

fn install_constants_and_legacy<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    context: &Rc<RefCell<EngineContext>>,
) -> Result<()> {
    bind_const_globals(
        lua,
        globals,
        &[("_VERSION", GlobalConst::Str("Lua 3.1 (alpha)"))],
    )?;

    install_legacy_io(lua, globals)?;
    let errorfb: Function = globals
        .get("error")
        .context("error handler missing from Lua state")?;
    log_push_cclosure("lua_pushCclosure", errorfb.to_pointer(), 0, Some("errorfb"));
    bind_const_globals(lua, globals, &[("_TRIGMODE", GlobalConst::Str("deg"))])?;
    install_legacy_compat(lua, globals, context.clone())?;
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
    install_legacy_math(lua, globals)?;
    install_pi_constant(lua, globals)?;
    lua.gc_collect()?;
    log_event(LuaEvent::CollectGarbage {});
    Ok(())
}

fn install_runtime_bindings<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    data_root: &Path,
    context: &Rc<RefCell<EngineContext>>,
) -> Result<()> {
    install_dofile(lua, globals, data_root, context.clone())?;
    install_basic_functions_pre_system(lua, globals, context.clone())?;
    Ok(())
}

fn install_stubbed_globals<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    context: &Rc<RefCell<EngineContext>>,
) -> Result<()> {
    install_actor_stubs(lua, globals, context.clone())?;
    install_control_stubs(lua, globals, context.clone())?;
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
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let nil_return = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;

    // Legacy helpers for manipulating globals from Lua 3.1.
    let getglobal = lua.create_function(|lua_ctx, name: String| {
        let globals = lua_ctx.globals();
        globals.get::<_, Value>(name)
    })?;
    with_suppressed_registered_globals(|| set_global(lua, globals, "getglobal", getglobal))?;

    let setglobal = lua.create_function(|lua_ctx, (name, value): (String, Value)| {
        let globals = lua_ctx.globals();
        super::util::set_global(lua_ctx, &globals, &name, value)?;
        Ok(Value::Nil)
    })?;
    with_suppressed_registered_globals(|| set_global(lua, globals, "setglobal", setglobal))?;

    let debug_state = context.clone();
    let print_debug = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.first() {
            if debug_state.borrow().verbose() {
                println!("[lua][PrintDebug] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    with_suppressed_registered_globals(|| set_global(lua, globals, "PrintDebug", print_debug))?;

    if let Ok(type_fn) = globals.get::<_, Function>("type") {
        let type_ptr = type_fn.to_pointer();
        let type_handle = ptr_to_handle(type_ptr);
        log_event(LuaEvent::GetGlobal {
            name: "type".to_string(),
            handle: type_handle.clone(),
            label: "global:type".to_string(),
            count: 1,
        });
        let type_ref = context.borrow_mut().alloc_ref(
            lua,
            Value::Function(type_fn.clone()),
            Some(1),
            Some("global:type".to_string()),
            Some(type_handle.clone()),
            Some("global:type".to_string()),
        )?;
        let type_fetch_ctx = context.clone();
        let type_override = lua.create_function(move |lua_ctx, value: Value| {
            let original: Option<Function> = type_fetch_ctx.borrow().fetch_ref(
                lua_ctx,
                type_ref,
                OriginFields::default(),
                None,
            )?;
            let original = original.ok_or_else(|| {
                LuaError::RuntimeError("type ref missing from registry".to_string())
            })?;
            let primary: Value = original.call(value)?;
            Ok(MultiValue::from_vec(vec![primary, Value::Nil]))
        })?;
        set_global(lua, globals, "type", type_override)?;
    }

    // Sector type / mode constants used during boot before any scripts run.
    bind_const_globals(
        lua,
        globals,
        &[
            ("NONE", GlobalConst::Int(0)),
            ("WALK", GlobalConst::Int(4096)),
            ("CAMERA", GlobalConst::Int(8192)),
            ("SPECIAL", GlobalConst::Int(16384)),
            ("HOT", GlobalConst::Int(32768)),
        ],
    )?;

    register_tag(COLOR_TAG, None, Some("color"));
    let make_color = lua.create_function(|lua_ctx, args: Variadic<Value>| {
        let component = |index: usize| -> u8 {
            args.get(index)
                .and_then(|value| match value {
                    Value::Integer(i) => Some(*i as i64),
                    Value::Number(n) => Some(*n as i64),
                    Value::String(text) => text.to_str().ok()?.trim().parse::<i64>().ok(),
                    _ => None,
                })
                .map(|value| value.clamp(0, 255) as u8)
                .unwrap_or(0)
        };
        let color = ColorHandle::new(component(0), component(1), component(2));
        let userdata = lua_ctx.create_userdata(color)?;
        Ok(Value::UserData(userdata))
    })?;
    with_suppressed_registered_globals(|| set_global(lua, globals, "MakeColor", make_color))?;

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

    with_suppressed_registered_globals(|| -> LuaResult<()> {
        set_global(lua, globals, "ReadRegistryValue", nil_return.clone())?;
        set_global(lua, globals, "ReadRegistryIntValue", nil_return)
    })?;

    Ok(())
}

fn install_basic_functions_post_system(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
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
    bind_fn_globals(
        lua,
        globals,
        &[("SetSayLineDefaults", &noop), ("WriteRegistryValue", &noop)],
    )?;
    set_global(
        lua,
        globals,
        "GetPlatform",
        lua.create_function(|_, ()| Ok(1))?,
    )?; // PLATFORM_PC_WIN
    with_suppressed_registered_globals(|| -> LuaResult<()> {
        set_global(lua, globals, "ReadRegistryValue", nil_return.clone())?;
        set_global(lua, globals, "ReadRegistryIntValue", nil_return.clone())
    })?;
    bind_fn_globals(
        lua,
        globals,
        &[
            ("enable_basic_remappable_key_set", &noop),
            ("enable_joystick_controls", &noop),
            ("enable_mouse_controls", &noop),
        ],
    )?;
    bind_fn_globals(
        lua,
        globals,
        &[
            ("GetControlState", &bool_false),
            ("get_generic_control_state", &bool_false),
        ],
    )?;
    bind_fn_globals(lua, globals, &[("ResetMarioControls", &noop)])?;
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
    bind_fn_globals(
        lua,
        globals,
        &[
            ("NukeResources", &noop),
            ("GetSystemFonts", &noop),
            ("PreloadCursors", &noop),
            ("HideVerbSkull", &noop),
            ("HideMouseCursor", &noop),
            ("ShowCursor", &noop),
            ("SetActiveCommentary", &noop),
            ("SetAmbientLight", &noop),
        ],
    )?;

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
    with_suppressed_registered_globals(|| set_global(lua, globals, "dofile", wrapped_dofile))?;
    Ok(())
}

fn install_system_table(lua: &Lua, globals: &Table) -> LuaResult<()> {
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
        Some(0),
        Some("global:system".to_string()),
        Some(system_handle.clone()),
        system_handle_label.clone(),
    )?;
    let mut system: Table = system_ref.fetch(lua, OriginFields::default(), None)?;

    let controls = lua.create_table()?;
    let controls_handle = ptr_to_handle(controls.to_pointer());
    let controls_fields = value_fields_from_lua(&Value::Table(controls.clone()));
    log_create_table(controls_handle.clone(), controls_fields.clone());

    set_table_entry_with_telemetry(
        &system,
        &system_handle,
        &system_fields,
        system_handle_label.as_ref(),
        Value::String(lua.create_string("controls")?),
        Value::Table(controls.clone()),
        None,
        None,
    )?;
    populate_controls_table(lua, &controls, &controls_handle, &controls_fields)?;

    // Retail fetches the stored system ref before installing default handlers; mirror the fetches without storing the closures.
    let default_cam_change = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    let _cam_change_preview = UpvaluePreview {
        kind: ValueType::Cfunction,
        value: Some(ptr_to_handle(default_cam_change.to_pointer())),
        value_len: None,
        preview: None,
        tag: None,
    };
    system = system_ref.fetch(lua, OriginFields::default(), None)?;
    log_push_cclosure(
        "lua_pushCclosure",
        default_cam_change.to_pointer(),
        0,
        Some("DefaultCamChangeHandlerL"),
    );
    set_table_entry_with_telemetry(
        &system,
        &system_handle,
        &system_fields,
        system_handle_label.as_ref(),
        Value::String(lua.create_string("camChangeHandler")?),
        Value::Function(default_cam_change.clone()),
        None,
        None,
    )?;

    let default_control = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    let _default_control_preview = UpvaluePreview {
        kind: ValueType::Cfunction,
        value: Some(ptr_to_handle(default_control.to_pointer())),
        value_len: None,
        preview: None,
        tag: None,
    };
    for key in ["axisHandler", "inputModeHandler", "buttonHandler"] {
        let system_for_handler: Table = system_ref.fetch(lua, OriginFields::default(), None)?;
        log_push_cclosure(
            "lua_pushCclosure",
            default_control.to_pointer(),
            0,
            Some("DefaultControlHandlerL"),
        );
        set_table_entry_with_telemetry(
            &system_for_handler,
            &system_handle,
            &system_fields,
            system_handle_label.as_ref(),
            Value::String(lua.create_string(key)?),
            Value::Function(default_control.clone()),
            None,
            None,
        )?;
    }

    pinned_refs.push(system_ref.key);
    system.set("setTable", lua.create_table()?)?;
    Ok(())
}

fn populate_controls_table(
    lua: &Lua,
    controls: &Table,
    controls_handle: &String,
    controls_fields: &ValueFields,
) -> LuaResult<()> {
    apply_control_manifest(
        lua,
        controls,
        controls_handle,
        controls_fields,
        &CONTROL_ENTRIES,
    )?;
    Ok(())
}

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

fn apply_control_manifest(
    lua: &Lua,
    controls: &Table,
    controls_handle: &String,
    controls_fields: &ValueFields,
    manifest: &[(&str, i32)],
) -> LuaResult<()> {
    for (name, code) in manifest {
        set_table_entry_with_telemetry(
            controls,
            controls_handle,
            controls_fields,
            None,
            Value::String(lua.create_string(*name)?),
            Value::Integer(*code as i64),
            None,
            None,
        )?;
    }
    Ok(())
}

/// Installs stubbed Lua 3.1-style `io` helpers expected by retail boot scripts.
///
/// Lua 3.1 used a stateful I/O API: `readfrom("file.txt")` opened that file and
/// stored a handle (userdata tagged with an I/O tag) in `_INPUT`; `writeto` and
/// `appendto` similarly set `_OUTPUT`. Plain `read()`/`write()` then operate on
/// the current `_INPUT`/`_OUTPUT` handle instead of returning/accepting file
/// objects.
///
/// This shim (no real file I/O):
/// - registers legacy I/O tags (handle plus retail's unused "fallback" tag) so
///   fabricated handles match retail telemetry;
/// - treats handles as tagged userdata placeholders for the current "open file";
/// - keeps a shared current-handle slot updated by `readfrom`/`writeto`/`appendto`;
/// - provides minimal `read` (nil) and `write` (stderr log) shims for parity;
/// - publishes placeholder `_INPUT`/`_OUTPUT`/`_STDIN`/`_STDOUT`/`_STDERR` globals.
/// Retail also registers a secondary I/O tag labeled "io_fallback"; the stub
/// registers it too to keep tag numbering/labels aligned even though it never
/// produces fallback userdata.
///
/// See also: <https://www.lua.org/manual/3.1/manual.html#2.6> and
/// <https://www.lua.org/ftp/lua-3.1.tar.gz> (iolib.c).
fn install_legacy_io(lua: &Lua, globals: &Table) -> LuaResult<()> {
    // These tags (-16/-17) mirror retail's legacy Lua 3 I/O tagging: one for
    // active handles and one ("fallback") the retail iolib also registers.
    // Keeping both preserves tag ids/labels seen in telemetry even though the
    // stub never fabricates a fallback userdata.
    const IO_HANDLE_TAG: i32 = -16;
    const IO_FALLBACK_TAG: i32 = -17;
    const IO_TAG_ALIAS: &str = "lua_iolibopen";

    register_tag(IO_HANDLE_TAG, Some(IO_TAG_ALIAS), Some("io_handle"));
    // Retail also registers a fallback I/O tag; keep it so tag ids/labels match even
    // though the stubbed I/O never fabricates a fallback userdata.
    register_tag(IO_FALLBACK_TAG, Some(IO_TAG_ALIAS), Some("io_fallback"));

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

    set_global(lua, globals, "readfrom", readfrom)?;
    set_global(lua, globals, "writeto", writeto)?;
    set_global(lua, globals, "appendto", appendto)?;
    set_global(lua, globals, "read", read)?;
    set_global(lua, globals, "write", write)?;

    for name in ["_INPUT", "_OUTPUT", "_STDIN", "_STDOUT", "_STDERR"] {
        let userdata = lua.create_userdata(TaggedHandle::new(IO_HANDLE_TAG))?;
        set_global(lua, globals, name, userdata)?;
    }

    Ok(())
}

fn install_pi_constant(lua: &Lua, globals: &Table) -> LuaResult<()> {
    set_global(lua, globals, "PI", std::f32::consts::PI as f64)?;
    Ok(())
}

fn register_value_stub<'lua, R>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    context: &Rc<RefCell<EngineContext>>,
    name: &str,
    ret: R,
) -> LuaResult<()>
where
    R: Copy + for<'a> IntoLuaMulti<'a> + 'static,
{
    let ctx = context.clone();
    let label = name.to_string();
    let func = lua.create_function(move |_, _: Variadic<Value>| {
        ctx.borrow_mut().log_event(format!("{label} (stub)"));
        Ok(ret)
    })?;
    set_global(lua, globals, name, func)
}

fn register_nil_stub<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    context: &Rc<RefCell<EngineContext>>,
    name: &str,
) -> LuaResult<()> {
    let ctx = context.clone();
    let label = name.to_string();
    let func = lua.create_function(move |_, _: Variadic<Value>| {
        ctx.borrow_mut().log_event(format!("{label} (stub)"));
        Ok(Value::Nil)
    })?;
    set_global(lua, globals, name, func)
}

#[derive(Copy, Clone)]
enum ActorStub {
    Nil(&'static str),
    Bool(&'static str),
    Int(&'static str),
    Float(&'static str),
    Vec3(&'static str),
}

// Keep stub order explicit so parity diffs point at the first divergence.
const ACTOR_STUBS: &[ActorStub] = &[
    ActorStub::Nil("SetActorColormap"),
    ActorStub::Nil("SetActorTalkColor"),
    ActorStub::Nil("SetActorWalkChore"),
    ActorStub::Nil("SetActorTurnChores"),
    ActorStub::Nil("SetActorRestChore"),
    ActorStub::Nil("SetActorMumblechore"),
    ActorStub::Nil("SetActorTalkChore"),
    ActorStub::Nil("SetActorTurnRate"),
    ActorStub::Nil("SetActorWalkRate"),
    ActorStub::Nil("SetActorHead"),
    ActorStub::Nil("SetActorLookRate"),
    ActorStub::Nil("SetActorVisibility"),
    ActorStub::Nil("SetActorFrustrumCull"),
    ActorStub::Nil("SetActorFollowBoxes"),
    ActorStub::Nil("SetActorScale"),
    ActorStub::Nil("SetActorConstrain"),
    ActorStub::Nil("SetActorCollisionMode"),
    ActorStub::Nil("SetActorCollisionScale"),
    ActorStub::Nil("SetActorTimeScale"),
    ActorStub::Nil("SetActorChoreLooping"),
    ActorStub::Nil("SetSelectedActor"),
    ActorStub::Nil("PutActorAt"),
    ActorStub::Nil("PutActorAtInterest"),
    ActorStub::Nil("PutActorInSet"),
    ActorStub::Nil("PlayActorChore"),
    ActorStub::Nil("PlayActorChoreLooping"),
    ActorStub::Nil("StopActorChore"),
    ActorStub::Nil("CompleteActorChore"),
    ActorStub::Nil("PushActorCostume"),
    ActorStub::Nil("PopActorCostume"),
    ActorStub::Nil("PrintActorCostumes"),
    ActorStub::Nil("ActorLookAt"),
    ActorStub::Nil("ShutUpActor"),
    ActorStub::Bool("IsActorMoving"),
    ActorStub::Bool("IsActorResting"),
    ActorStub::Bool("IsActorTurning"),
    ActorStub::Bool("IsActorChoring"),
    ActorStub::Bool("IsActorInSector"),
    ActorStub::Bool("TurnActorTo"),
    ActorStub::Bool("WalkActorTo"),
    ActorStub::Bool("WalkActorForward"),
    ActorStub::Vec3("GetActorPos"),
    ActorStub::Vec3("GetActorRot"),
    ActorStub::Vec3("GetActorNodeLocation"),
    ActorStub::Int("GetActorSector"),
    ActorStub::Float("GetActorYawToPoint"),
    ActorStub::Nil("GetActorCostume"),
    ActorStub::Int("GetActorCostumeDepth"),
    ActorStub::Int("GetActorLookRate"),
    ActorStub::Int("GetActorWalkRate"),
    ActorStub::Float("GetAngleBetweenActors"),
];

fn install_actor_stubs(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    register_tag(ACTOR_TAG, None, Some("actor"));
    let load_actor_ctx = context.clone();
    let load_actor = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let name = args
            .first()
            .and_then(value_to_string)
            .unwrap_or_else(|| "<unnamed>".to_string());
        let fabricated = next_fabricated_handle();
        let userdata = lua_ctx.create_userdata(TaggedHandle::new(ACTOR_TAG))?;
        let handle = ptr_to_handle(userdata.to_pointer());
        load_actor_ctx
            .borrow_mut()
            .log_event(format!("actor.load {name} -> {handle}"));
        log_push_usertag(fabricated.raw, ACTOR_TAG, handle.clone());
        Ok(userdata)
    })?;
    set_global(lua, globals, "LoadActor", load_actor)?;

    let set_actor_costume_ctx = context.clone();
    let set_actor_costume = lua.create_function(move |_, args: Variadic<Value>| {
        set_actor_costume_ctx
            .borrow_mut()
            .log_event("SetActorCostume (stub)");
        Ok(args.get(1).cloned().unwrap_or(Value::Nil))
    })?;
    set_global(lua, globals, "SetActorCostume", set_actor_costume)?;

    for stub in ACTOR_STUBS {
        match stub {
            ActorStub::Nil(name) => register_nil_stub(lua, globals, &context, name)?,
            ActorStub::Bool(name) => register_value_stub(lua, globals, &context, name, false)?,
            ActorStub::Int(name) => register_value_stub(lua, globals, &context, name, 0)?,
            ActorStub::Float(name) => register_value_stub(lua, globals, &context, name, 0.0)?,
            ActorStub::Vec3(name) => {
                register_value_stub(lua, globals, &context, name, (0.0, 0.0, 0.0))?
            }
        }
    }

    Ok(())
}

fn install_control_stubs(
    lua: &Lua,
    globals: &Table,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    register_nil_stub(lua, globals, &context, "EnableControl")?;
    register_nil_stub(lua, globals, &context, "DisableControl")?;
    register_value_stub(lua, globals, &context, "GetControlState", 0.0)?;
    register_nil_stub(lua, globals, &context, "ResetMarioControls")?;
    register_nil_stub(lua, globals, &context, "MarioStyleControl")?;
    register_nil_stub(lua, globals, &context, "TombRaiderControl")?;
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
        install_system_table(lua, globals)?;
    }

    Ok(())
}
