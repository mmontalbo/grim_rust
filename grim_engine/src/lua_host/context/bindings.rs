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

    let root = data_root.to_path_buf();
    let verbose_context = context.clone();
    let system_key = Rc::new(install_runtime_tables(lua, context.clone())?);
    install_actor_scaffold(lua, context.clone(), system_key.clone()).map_err(|err| anyhow!(err))?;
    let dofile_context = context.clone();
    let wrapped_dofile = lua.create_function(move |lua_ctx, path: String| -> LuaResult<Value> {
        if let Some(value) =
            handle_special_dofile(lua_ctx, &path, dofile_context.clone(), system_key.clone())?
        {
            let verbose = verbose_context.borrow().verbose;
            if verbose {
                println!("[lua][dofile] handled {} via host", path);
            }
            {
                let mut ctx = dofile_context.borrow_mut();
                ctx.record_script_name(&path);
            }
            return Ok(value);
        }
        let mut tried = Vec::new();
        let candidates = candidate_paths(&path);
        for candidate in candidates {
            let absolute = if candidate.is_absolute() {
                candidate.clone()
            } else {
                root.join(&candidate)
            };
            tried.push(absolute.clone());
            if let Some(value) = execute_script(lua_ctx, &absolute)? {
                let verbose = verbose_context.borrow().verbose;
                if verbose {
                    println!("[lua][dofile] loaded {}", absolute.display());
                }
                {
                    let mut ctx = dofile_context.borrow_mut();
                    ctx.record_script_path(&absolute);
                }
                return Ok(value);
            }
        }
        if verbose_context.borrow().verbose {
            println!("[lua][dofile] skipped {}", path);
            for attempt in tried {
                println!("  tried {}", attempt.display());
            }
        }
        Ok(Value::Nil)
    })?;
    globals.set("dofile", wrapped_dofile)?;

    install_logging_functions(lua, context.clone())?;
    install_engine_bindings(lua, context.clone())?;

    Ok(())
}

fn handle_special_dofile<'lua>(
    lua: &'lua Lua,
    path: &str,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<Option<Value<'lua>>> {
    if let Some(filename) = Path::new(path).file_name().and_then(|name| name.to_str()) {
        let lower = filename.to_ascii_lowercase();
        match lower.as_str() {
            "setfallback.lua" => return Ok(Some(Value::Nil)),
            "_colors.lua" | "_colors.decompiled.lua" => {
                install_color_constants(lua)?;
                return Ok(Some(Value::Nil));
            }
            "_sfx.lua" | "_sfx.decompiled.lua" => {
                install_sfx_scaffold(lua, context.clone())?;
                return Ok(Some(Value::Nil));
            }
            "_controls.lua" | "_controls.decompiled.lua" => {
                install_controls_scaffold(lua, context, system_key.clone())?;
                return Ok(Some(Value::Nil));
            }
            "_dialog.lua" | "_dialog.decompiled.lua" => {
                install_dialog_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_music.lua" | "_music.decompiled.lua" => {
                install_music_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_mouse.lua" | "_mouse.decompiled.lua" => {
                install_mouse_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_ui.lua" | "_ui.decompiled.lua" => {
                install_ui_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_achievement.lua" | "_achievement.decompiled.lua" => {
                install_achievement_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_actors.lua" | "_actors.decompiled.lua" => {
                install_actor_scaffold(lua, context, system_key.clone())?;
                return Ok(Some(Value::Nil));
            }
            "menu_loading.lua" | "menu_loading.decompiled.lua" => {
                install_loading_menu(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_boot_warning.lua" | "menu_boot_warning.decompiled.lua" => {
                install_boot_warning_menu(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_dialog.lua" | "menu_dialog.decompiled.lua" => {
                install_menu_dialog(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_common.lua" | "menu_common.decompiled.lua" => {
                install_menu_common(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_remap_keys.lua" | "menu_remap_keys.decompiled.lua" => {
                install_menu_remap(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_prefs.lua" | "menu_prefs.decompiled.lua" => {
                install_menu_prefs(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            _ => {}
        }

        if let Some(base) = lower
            .strip_suffix(".decompiled.lua")
            .or_else(|| lower.strip_suffix(".lua"))
        {
            if base.ends_with("_inv") {
                install_inventory_variant_stub(lua, context.clone(), base)
                    .map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }

            if base == "mn_scythe" {
                install_manny_scythe_stub(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
        }
    }
    Ok(None)
}

fn install_footsteps_table(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("footsteps"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    for profile in FOOTSTEP_PROFILES {
        let entry = lua.create_table()?;
        entry.set("prefix", profile.prefix)?;
        entry.set("left_walk", profile.left_walk)?;
        entry.set("right_walk", profile.right_walk)?;
        if let Some(count) = profile.left_run {
            entry.set("left_run", count)?;
        }
        if let Some(count) = profile.right_run {
            entry.set("right_run", count)?;
        }
        table.set(profile.key, entry)?;
    }

    globals.set("footsteps", table)?;
    Ok(())
}

fn install_color_constants(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    let make_color = |r: f32, g: f32, b: f32| -> LuaResult<Value> {
        let table = lua.create_table()?;
        table.set("r", r)?;
        table.set("g", g)?;
        table.set("b", b)?;
        Ok(Value::Table(table))
    };

    globals.set("White", make_color(1.0, 1.0, 1.0)?)?;
    globals.set("Yellow", make_color(1.0, 0.9, 0.2)?)?;
    globals.set("Magenta", make_color(0.9, 0.1, 0.9)?)?;
    globals.set("Aqua", make_color(0.1, 0.7, 0.9)?)?;

    Ok(())
}

fn install_sfx_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> LuaResult<()> {
    let globals = lua.globals();

    globals.set("IM_GROUP_SFX", 1)?;

    if matches!(globals.get::<_, Value>("sfx"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let sfx = lua.create_table()?;

    let play_context = context.clone();
    sfx.set(
        "play",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            if values.is_empty() {
                return Ok(());
            }
            let cue = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let params = values
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect::<Vec<_>>();
            play_context.borrow_mut().play_sound_effect(cue, params);
            Ok(())
        })?,
    )?;

    let stop_context = context.clone();
    sfx.set(
        "stop",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let target = values.get(0).and_then(|value| value_to_string(value));
            stop_context.borrow_mut().stop_sound_effect(target);
            Ok(())
        })?,
    )?;

    let stop_all_context = context.clone();
    sfx.set(
        "stop_all",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_all_context.borrow_mut().stop_sound_effect(None);
            Ok(())
        })?,
    )?;

    let stop_all_camel_context = context.clone();
    sfx.set(
        "stopAll",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_all_camel_context.borrow_mut().stop_sound_effect(None);
            Ok(())
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            if let Ok(name) = method.to_str() {
                fallback_context
                    .borrow_mut()
                    .log_event(format!("sfx.stub {name}"));
            }
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    sfx.set_metatable(Some(metatable));
    globals.set("sfx", sfx)?;

    let imstart_context = context.clone();
    globals.set(
        "ImStartSound",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let cue_value = values.next().unwrap_or(Value::Nil);
            let Some(cue) = value_to_string(&cue_value) else {
                return Ok(Value::Integer(0));
            };
            let priority = values.next().and_then(|value| value_to_i32(&value));
            let group = values.next().and_then(|value| value_to_i32(&value));
            let handle = {
                let mut ctx = imstart_context.borrow_mut();
                ctx.start_imuse_sound(cue, priority, group)
            };
            Ok(Value::Integer(handle.max(0)))
        })?,
    )?;

    let imstop_context = context.clone();
    globals.set(
        "ImStopSound",
        lua.create_function(move |_, value: Value| {
            if let Some(handle) = value_to_object_handle(&value) {
                imstop_context
                    .borrow_mut()
                    .stop_sound_effect_by_numeric(handle);
            }
            Ok(())
        })?,
    )?;

    let imset_context = context.clone();
    globals.set(
        "ImSetParam",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle = args.get(0).and_then(|value| value_to_object_handle(value));
            let param = args.get(1).and_then(|value| value_to_i32(value));
            let value = args.get(2).and_then(|value| value_to_i32(value));
            if let (Some(handle), Some(param), Some(value)) = (handle, param, value) {
                imset_context
                    .borrow_mut()
                    .set_sound_param(handle, param, value);
            }
            Ok(())
        })?,
    )?;

    let imgp_context = context.clone();
    globals.set(
        "ImGetParam",
        lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<i64> {
            let handle = args.get(0).and_then(|value| value_to_object_handle(value));
            let param = args.get(1).and_then(|value| value_to_i32(value));
            if let (Some(handle), Some(param)) = (handle, param) {
                if let Some(value) = imgp_context.borrow().get_sound_param(handle, param) {
                    return Ok(value as i64);
                }
            }
            Ok(0)
        })?,
    )?;

    let imfade_context = context.clone();
    globals.set(
        "ImFadeParam",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle = args.get(0).and_then(|value| value_to_object_handle(value));
            let param = args.get(1).and_then(|value| value_to_i32(value));
            let value = args.get(2).and_then(|value| value_to_i32(value));
            if let (Some(handle), Some(param), Some(value)) = (handle, param, value) {
                imfade_context
                    .borrow_mut()
                    .set_sound_param(handle, param, value);
            }
            Ok(())
        })?,
    )?;

    let start_sfx_context = context.clone();
    globals.set(
        "start_sfx",
        lua.create_function(move |_, args: Variadic<Value>| {
            let cue = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<unknown>".to_string());
            let priority = args.get(1).and_then(|value| value_to_i32(value));
            let volume = args
                .get(2)
                .and_then(|value| value_to_i32(value))
                .unwrap_or(127);
            let handle = {
                let mut ctx = start_sfx_context.borrow_mut();
                let id = ctx.start_imuse_sound(cue.clone(), priority, Some(1));
                if id >= 0 {
                    ctx.set_sound_param(id, IM_SOUND_VOL, volume);
                }
                id
            };
            Ok(Value::Integer(handle.max(0)))
        })?,
    )?;

    let single_start_context = context.clone();
    globals.set(
        "single_start_sfx",
        lua.create_function(move |_, args: Variadic<Value>| {
            let cue = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<unknown>".to_string());
            let priority = args.get(1).and_then(|value| value_to_i32(value));
            let volume = args
                .get(2)
                .and_then(|value| value_to_i32(value))
                .unwrap_or(127);
            let handle = {
                let mut ctx = single_start_context.borrow_mut();
                let id = ctx.start_imuse_sound(cue.clone(), priority, Some(1));
                if id >= 0 {
                    ctx.set_sound_param(id, IM_SOUND_VOL, volume);
                }
                id
            };
            Ok(Value::Integer(handle.max(0)))
        })?,
    )?;

    let sound_playing_context = context.clone();
    globals.set(
        "sound_playing",
        lua.create_function(move |_, value: Value| {
            let Some(handle) = value_to_object_handle(&value) else {
                return Ok(false);
            };
            let playing = sound_playing_context
                .borrow()
                .get_sound_param(handle, IM_SOUND_PLAY_COUNT)
                .unwrap_or(0)
                > 0;
            Ok(playing)
        })?,
    )?;

    let wait_for_sound_context = context.clone();
    globals.set(
        "wait_for_sound",
        lua.create_function(move |_, value: Value| {
            if let Some(handle) = value_to_object_handle(&value) {
                let mut ctx = wait_for_sound_context.borrow_mut();
                ctx.set_sound_param(handle, IM_SOUND_PLAY_COUNT, 0);
            }
            Ok(())
        })?,
    )?;

    let stop_sound_context = context.clone();
    globals.set(
        "stop_sound",
        lua.create_function(move |_, args: Variadic<Value>| {
            let target = args.get(0);
            if let Some(handle) = target.and_then(|value| value_to_object_handle(value)) {
                stop_sound_context
                    .borrow_mut()
                    .stop_sound_effect_by_numeric(handle);
            } else if let Some(label) = target.and_then(|value| value_to_string(value)) {
                stop_sound_context
                    .borrow_mut()
                    .stop_sound_effect(Some(label));
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

fn install_controls_scaffold(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<()> {
    let globals = lua.globals();
    let system: Table = lua.registry_value(system_key.as_ref())?;

    if system
        .get::<_, Value>("controls")
        .map(|value| !matches!(value, Value::Nil))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let controls = lua.create_table()?;
    let entries = [
        ("AXIS_JOY1_X", 0),
        ("AXIS_JOY1_Y", 1),
        ("AXIS_MOUSE_X", 2),
        ("AXIS_MOUSE_Y", 3),
        ("AXIS_SENSITIVITY", 4),
        ("KEY1", 10),
        ("KEY2", 11),
        ("KEY3", 12),
        ("KEY4", 13),
        ("KEY5", 14),
        ("KEY6", 15),
        ("KEY7", 16),
        ("KEY8", 17),
        ("KEY9", 18),
        ("LCONTROLKEY", 30),
        ("RCONTROLKEY", 31),
    ];
    for (name, value) in entries {
        controls.set(name, value)?;
    }
    system.set("controls", controls)?;

    globals.set("MODE_NORMAL", 0)?;
    globals.set("MODE_MOUSE", 1)?;
    globals.set("MODE_KEYS", 2)?;
    globals.set("MODE_BACKGROUND", 3)?;
    globals.set("CONTROL_MODE", 0)?;

    globals.set("WALK", 0)?;
    globals.set("HOT", 1)?;
    globals.set("CAMERA", 2)?;

    let system_controls = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            if let Ok(name) = method.to_str() {
                fallback_context
                    .borrow_mut()
                    .log_event(format!("system_controls.stub {name}"));
            }
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    system_controls.set_metatable(Some(metatable));
    globals.set("system_controls", system_controls)?;

    system.set("axisHandler", Value::Nil)?;

    Ok(())
}

pub(crate) fn candidate_paths(path: &str) -> Vec<PathBuf> {
    let mut base_candidates = Vec::new();
    base_candidates.push(path.to_string());

    if path.ends_with(".lua") {
        let mut alt = path.to_string();
        alt.truncate(alt.len().saturating_sub(4));
        alt.push_str(".decompiled.lua");
        base_candidates.push(alt);
    } else if path.ends_with(".decompiled.lua") {
        let mut alt = path.to_string();
        alt.truncate(alt.len().saturating_sub(".decompiled.lua".len()));
        alt.push_str(".lua");
        base_candidates.push(alt);
    } else {
        base_candidates.push(format!("{path}.lua"));
        base_candidates.push(format!("{path}.decompiled.lua"));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|existing| existing == &candidate) {
            candidates.push(candidate);
        }
    };

    for candidate in base_candidates {
        let direct = PathBuf::from(&candidate);
        push_unique(direct.clone());
        push_unique(PathBuf::from("Scripts").join(&direct));
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
        Ok(results) => {
            let mut iter = results.into_iter();
            if let Some(value) = iter.next() {
                Ok(Some(value))
            } else {
                Ok(Some(Value::Nil))
            }
        }
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

fn install_logging_functions(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let debug_state = context.clone();
    let print_debug = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if debug_state.borrow().verbose {
                println!("[lua][PrintDebug] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    globals.set("PrintDebug", print_debug)?;

    let logf_state = context.clone();
    let logf = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if logf_state.borrow().verbose {
                println!("[lua][logf] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    globals.set("logf", logf)?;

    Ok(())
}

fn install_engine_bindings(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    if let Ok(string_table) = globals.get::<_, Table>("string") {
        if let Ok(sub) = string_table.get::<_, Function>("sub") {
            globals.set("strsub", sub.clone())?;
        }
        if let Ok(find) = string_table.get::<_, Function>("find") {
            globals.set("strfind", find.clone())?;
        }
        if let Ok(lower) = string_table.get::<_, Function>("lower") {
            globals.set("strlower", lower.clone())?;
        }
        if let Ok(upper) = string_table.get::<_, Function>("upper") {
            globals.set("strupper", upper.clone())?;
        }
        if let Ok(len) = string_table.get::<_, Function>("len") {
            globals.set("strlen", len)?;
        }
    }

    if let Ok(math_table) = globals.get::<_, Table>("math") {
        if let Ok(sqrt_fn) = math_table.get::<_, Function>("sqrt") {
            globals.set("sqrt", sqrt_fn.clone())?;
        }
        if let Ok(abs_fn) = math_table.get::<_, Function>("abs") {
            globals.set("abs", abs_fn)?;
        }
    }

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    let nil_return = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;

    globals.set(
        "LockFont",
        lua.create_function(|_, name: String| Ok(format!("font::{name}")))?,
    )?;
    globals.set(
        "LockCursor",
        lua.create_function(|_, name: String| Ok(format!("cursor::{name}")))?,
    )?;
    globals.set("TRUE", true)?;
    globals.set("FALSE", false)?;
    globals.set("SetSayLineDefaults", noop.clone())?;
    globals.set("GetPlatform", lua.create_function(|_, ()| Ok(1))?)?; // PLATFORM_PC_WIN
    globals.set("ReadRegistryValue", nil_return.clone())?;
    globals.set("ReadRegistryIntValue", nil_return.clone())?;
    globals.set("WriteRegistryValue", noop.clone())?;
    globals.set("enable_basic_remappable_key_set", noop.clone())?;
    globals.set("enable_joystick_controls", noop.clone())?;
    globals.set("enable_mouse_controls", noop.clone())?;
    globals.set(
        "GetControlState",
        lua.create_function(|_, _: Variadic<Value>| Ok(false))?,
    )?;
    globals.set(
        "get_generic_control_state",
        lua.create_function(|_, _: Variadic<Value>| Ok(false))?,
    )?;
    globals.set(
        "AreAchievementsInstalled",
        lua.create_function(|_, ()| Ok(1))?,
    )?;
    globals.set("GlobalSaveResolved", lua.create_function(|_, ()| Ok(1))?)?;
    globals.set(
        "CheckForFile",
        lua.create_function(|_, _args: Variadic<Value>| Ok(true))?,
    )?;
    globals.set(
        "CheckForCD",
        lua.create_function(|_, _args: Variadic<Value>| Ok((false, false)))?,
    )?;
    globals.set("NukeResources", noop.clone())?;
    globals.set("GetSystemFonts", noop.clone())?;
    globals.set("PreloadCursors", noop.clone())?;
    let break_here = lua
        .load("return function(...) return coroutine.yield(...) end")
        .eval::<Function>()?;
    globals.set("break_here", break_here)?;
    globals.set("HideVerbSkull", noop.clone())?;
    let make_set_ctx = context.clone();
    globals.set(
        "MakeCurrentSet",
        lua.create_function(move |_, value: Value| {
            if let Some(set_file) = value_to_set_file(&value) {
                make_set_ctx.borrow_mut().switch_to_set(&set_file);
            } else {
                let description = describe_value(&value);
                make_set_ctx
                    .borrow_mut()
                    .log_event(format!("set.switch <unknown> ({description})"));
            }
            Ok(())
        })?,
    )?;
    let make_setup_ctx = context.clone();
    globals.set(
        "MakeCurrentSetup",
        lua.create_function(move |_, value: Value| {
            let description = describe_value(&value);
            if let Some(setup) = value_to_i32(&value) {
                let mut ctx = make_setup_ctx.borrow_mut();
                if let Some(current) = ctx.set_view().current_set().cloned() {
                    let file = current.set_file.clone();
                    ctx.record_current_setup(&file, setup);
                    ctx.log_event(format!("set.setup.make {file} -> {setup}"));
                } else {
                    ctx.log_event(format!("set.setup.make <none> -> {setup}"));
                }
            } else {
                make_setup_ctx
                    .borrow_mut()
                    .log_event(format!("set.setup.make <invalid> ({description})"));
            }
            Ok(())
        })?,
    )?;
    let get_setup_ctx = context.clone();
    globals.set(
        "GetCurrentSetup",
        lua.create_function(move |_, value: Value| {
            let set_file_opt = value_to_set_file(&value);
            let (label, setup) = {
                let ctx = get_setup_ctx.borrow();
                if let Some(set_file) = set_file_opt.clone() {
                    let setup = ctx.current_setup_for(&set_file).unwrap_or(0);
                    (set_file, setup)
                } else if let Some(current) = ctx.set_view().current_set() {
                    let file = current.set_file.clone();
                    let setup = ctx.current_setup_for(&file).unwrap_or(0);
                    (file, setup)
                } else {
                    ("<none>".to_string(), 0)
                }
            };
            {
                let mut ctx = get_setup_ctx.borrow_mut();
                ctx.log_event(format!("set.setup.get {label} -> {setup}"));
            }
            Ok(Value::Integer(setup as i64))
        })?,
    )?;
    globals.set("SetAmbientLight", noop.clone())?;
    let commentary_ctx = context.clone();
    globals.set(
        "SetActiveCommentary",
        lua.create_function(move |_, args: Variadic<Value>| {
            let value = args.get(0).cloned().unwrap_or(Value::Nil);
            let label = match &value {
                Value::String(text) => Some(text.to_str()?.to_string()),
                _ => None,
            };
            let enabled = match &value {
                Value::Nil => false,
                Value::Boolean(flag) => *flag,
                Value::Integer(i) => *i != 0,
                Value::Number(n) => *n != 0.0,
                Value::String(_) => true,
                other => value_to_bool(other),
            };
            commentary_ctx
                .borrow_mut()
                .set_commentary_active(enabled, label);
            Ok(())
        })?,
    )?;
    let sector_ctx = context.clone();
    globals.set(
        "MakeSectorActive",
        lua.create_function(move |_, args: Variadic<Value>| {
            let name_value = args.get(0).cloned().unwrap_or(Value::Nil);
            let active = args.get(1).map(value_to_bool).unwrap_or(true);
            let set_hint = args.get(2).and_then(|value| value_to_set_file(value));
            let mut ctx = sector_ctx.borrow_mut();
            let Some(sector_name) = value_to_sector_name(&name_value) else {
                let desc = describe_value(&name_value);
                ctx.log_event(format!("sector.active <invalid> ({desc})"));
                return Ok(());
            };
            match ctx.set_sector_active(set_hint.as_deref(), &sector_name, active) {
                SectorToggleResult::Applied {
                    set_file,
                    sector,
                    known_sector,
                    ..
                }
                | SectorToggleResult::NoChange {
                    set_file,
                    sector,
                    known_sector,
                } => {
                    if !known_sector {
                        ctx.log_event(format!("sector.active.unknown {set_file}:{sector}"));
                    }
                }
                SectorToggleResult::NoSet => {
                    ctx.log_event("sector.active <no current set>".to_string());
                }
            }
            Ok(())
        })?,
    )?;
    globals.set("LightMgrSetChange", noop.clone())?;
    globals.set("HideMouseCursor", noop.clone())?;
    globals.set("ShowCursor", noop.clone())?;
    globals.set("SetShadowColor", noop.clone())?;
    globals.set("SetActiveShadow", noop.clone())?;
    globals.set("SetActorShadowPoint", noop.clone())?;
    globals.set("SetActorShadowPlane", noop.clone())?;
    globals.set("AddShadowPlane", noop.clone())?;
    let new_object_state_ctx = context.clone();
    globals.set(
        "NewObjectState",
        lua.create_function(move |_, args: Variadic<Value>| {
            let setup = args
                .get(0)
                .map(|value| describe_value(value))
                .unwrap_or_else(|| "<nil>".to_string());
            let kind = args
                .get(1)
                .map(|value| describe_value(value))
                .unwrap_or_else(|| "<nil>".to_string());
            let bitmap = args
                .get(2)
                .map(|value| value_to_string(value).unwrap_or_else(|| describe_value(value)))
                .unwrap_or_else(|| "<nil>".to_string());
            let zbitmap = args
                .get(3)
                .map(|value| value_to_string(value).unwrap_or_else(|| describe_value(value)))
                .unwrap_or_else(|| "<nil>".to_string());
            let enabled = args
                .get(4)
                .map(|value| value_to_bool(value))
                .unwrap_or(false);
            new_object_state_ctx.borrow_mut().log_event(format!(
                "object.state.new setup={setup} kind={kind} bm={bitmap} zbm={zbitmap} {}",
                if enabled { "enabled" } else { "disabled" }
            ));
            Ok(())
        })?,
    )?;
    let send_front_ctx = context.clone();
    globals.set(
        "SendObjectToFront",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut label = args
                .get(0)
                .map(|value| describe_value(value))
                .unwrap_or_else(|| "<nil>".to_string());
            let mut handle: Option<i64> = None;
            if let Some(Value::Table(table)) = args.get(0) {
                if let Some(name) = table.get::<_, Option<String>>("name").ok().flatten() {
                    label = name;
                }
                if let Some(string_name) =
                    table.get::<_, Option<String>>("string_name").ok().flatten()
                {
                    label = string_name;
                }
                handle = table
                    .get::<_, Option<i64>>("handle")
                    .ok()
                    .flatten()
                    .or_else(|| table.get::<_, Option<i64>>("object_handle").ok().flatten());
                if handle.is_none() {
                    handle = table.get::<_, Option<i64>>("hObject").ok().flatten();
                }
            }
            if handle.is_none() {
                let lookup = {
                    let ctx = send_front_ctx.borrow();
                    ctx.objects.lookup_by_name(&label)
                };
                if let Some(found) = lookup {
                    handle = Some(found);
                }
            }
            let description = handle.map(|id| format!("{label} (#{id})")).unwrap_or(label);
            send_front_ctx
                .borrow_mut()
                .log_event(format!("object.front {description}"));
            Ok(())
        })?,
    )?;
    let constrain_ctx = context.clone();
    globals.set(
        "SetActorConstrain",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let actor = values
                .next()
                .map(|value| describe_value(&value))
                .unwrap_or_else(|| "<nil>".to_string());
            let enabled = values
                .next()
                .map(|value| value_to_bool(&value))
                .unwrap_or(false);
            constrain_ctx.borrow_mut().log_event(format!(
                "actor.constrain {actor} {}",
                if enabled { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;
    let next_script_ctx = context.clone();
    globals.set(
        "next_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            let current = args.get(0).and_then(|value| match value {
                Value::Nil => None,
                Value::Integer(i) if *i >= 0 => Some(*i as u32),
                Value::Number(n) if *n >= 0.0 => Some(*n as u32),
                _ => None,
            });
            let handles = {
                let ctx = next_script_ctx.borrow();
                let mut handles = ctx.active_script_handles();
                handles.sort_unstable();
                handles
            };
            let next = if let Some(current) = current {
                handles.into_iter().find(|handle| *handle > current)
            } else {
                handles.into_iter().next()
            };
            {
                let mut ctx = next_script_ctx.borrow_mut();
                let from = current
                    .map(|handle| format!("#{handle}"))
                    .unwrap_or_else(|| "<nil>".to_string());
                let to = next
                    .map(|handle| format!("#{handle}"))
                    .unwrap_or_else(|| "<nil>".to_string());
                ctx.log_event(format!("script.next {from} -> {to}"));
            }
            if let Some(handle) = next {
                Ok(Value::Integer(handle as i64))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;
    let identify_script_ctx = context.clone();
    globals.set(
        "identify_script",
        lua.create_function(move |lua_ctx, value: Value| {
            let handle = match value {
                Value::Nil => None,
                Value::Integer(i) if i >= 0 => Some(i as u32),
                Value::Number(n) if n >= 0.0 => Some(n as u32),
                _ => None,
            };
            if let Some(handle) = handle {
                if let Some(label) = {
                    let ctx = identify_script_ctx.borrow();
                    ctx.script_label(handle)
                } {
                    return Ok(Value::String(lua_ctx.create_string(&label)?));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;
    globals.set(
        "FunctionName",
        lua.create_function(move |lua_ctx, value: Value| {
            let name = match &value {
                Value::String(text) => text.to_str()?.to_string(),
                Value::Function(func) => {
                    let pointer = func.to_pointer();
                    format!("function {pointer:p}")
                }
                Value::Thread(thread) => format!("thread {:?}", thread.status()),
                other => describe_value(other),
            };
            Ok(Value::String(lua_ctx.create_string(&name)?))
        })?,
    )?;
    globals.set("LoadCostume", noop.clone())?;
    globals.set(
        "tag",
        lua.create_function(|_, _args: Variadic<Value>| Ok(0))?,
    )?;
    globals.set("settagmethod", noop.clone())?;
    globals.set("setfallback", noop.clone())?;
    globals.set(
        "look_up_correct_costume",
        lua.create_function(|_, _args: Variadic<Value>| Ok(String::from("suit")))?,
    )?;
    globals.set("gettagmethod", nil_return.clone())?;
    globals.set("getglobal", nil_return.clone())?;
    globals.set("setglobal", noop.clone())?;
    globals.set("GlobalShrinkEnabled", false)?;
    globals.set("shrinkBoxesEnabled", false)?;
    globals.set(
        "randomseed",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    globals.set("random", lua.create_function(|_, ()| Ok(0.42))?)?;
    let visible_ctx = context.clone();
    globals.set(
        "GetVisibleThings",
        lua.create_function(move |lua_ctx, ()| {
            {
                let mut ctx = visible_ctx.borrow_mut();
                ctx.log_event("scene.get_visible_things".to_string());
            }
            let handles = {
                let ctx = visible_ctx.borrow();
                ctx.visible_object_handles()
            };
            let table = lua_ctx.create_table()?;
            for handle in &handles {
                table.set(*handle, true)?;
            }
            visible_ctx.borrow_mut().record_visible_objects(&handles);
            Ok(table)
        })?,
    )?;
    let walk_vector_context = context.clone();
    globals.set(
        "WalkActorVector",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();

            let actor_handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value))
                .unwrap_or(0);
            // camera handle is ignored in the prototype but advance the iterator
            let _ = values.next();

            let dx = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let dy = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let dz = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let adjust_y = values.next().and_then(|value| value_to_f32(&value));
            // maintained heading flag (ignored)
            let _ = values.next();
            let heading_offset = values.next().and_then(|value| value_to_f32(&value));

            if actor_handle == 0 {
                return Ok(());
            }

            let mut ctx = walk_vector_context.borrow_mut();
            ctx.walk_actor_vector(
                actor_handle,
                Vec3 {
                    x: dx,
                    y: dy,
                    z: dz,
                },
                adjust_y,
                heading_offset,
            );
            Ok(())
        })?,
    )?;

    let walk_to_ctx = context.clone();
    globals.set(
        "WalkActorTo",
        lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<bool> {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(false);
            };
            let x = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let y = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let z = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let mut ctx = walk_to_ctx.borrow_mut();
            let moved = ctx.walk_actor_to_handle(handle, Vec3 { x, y, z });
            Ok(moved)
        })?,
    )?;

    let is_moving_ctx = context.clone();
    globals.set(
        "IsActorMoving",
        lua.create_function(move |_, actor: Value| {
            let handle = value_to_actor_handle(&actor).unwrap_or(0);
            let moving = if handle == 0 {
                false
            } else {
                is_moving_ctx.borrow().is_actor_moving(handle)
            };
            Ok(moving)
        })?,
    )?;

    let sleep_context = context.clone();
    globals.set(
        "sleep_for",
        lua.create_function(move |_, args: Variadic<Value>| {
            let desc = if args.is_empty() {
                "<none>".to_string()
            } else {
                args.iter()
                    .map(|value| describe_value(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            sleep_context
                .borrow_mut()
                .log_event(format!("sleep_for {}", desc));
            Ok(())
        })?,
    )?;

    let set_override_context = context.clone();
    globals.set(
        "set_override",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut ctx = set_override_context.borrow_mut();
            match args.get(0) {
                Some(Value::Nil) | None => {
                    ctx.pop_override();
                }
                Some(value) => {
                    let description = describe_value(value);
                    ctx.push_override(description);
                }
            }
            Ok(())
        })?,
    )?;

    let kill_override_context = context.clone();
    globals.set(
        "kill_override",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut ctx = kill_override_context.borrow_mut();
            ctx.clear_overrides();
            Ok(())
        })?,
    )?;

    let fade_context = context.clone();
    globals.set(
        "FadeInChore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let desc = if args.is_empty() {
                "<none>".to_string()
            } else {
                args.iter()
                    .map(|value| describe_value(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            fade_context
                .borrow_mut()
                .log_event(format!("actor.fade_in {}", desc));
            Ok(())
        })?,
    )?;

    let start_cut_scene_context = context.clone();
    globals.set(
        "START_CUT_SCENE",
        lua.create_function(move |_, args: Variadic<Value>| {
            let label = args.get(0).and_then(|value| value_to_string(value));
            let flags: Vec<String> = args
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect();
            start_cut_scene_context
                .borrow_mut()
                .push_cut_scene(label, flags);
            Ok(())
        })?,
    )?;

    let end_cut_scene_context = context.clone();
    globals.set(
        "END_CUT_SCENE",
        lua.create_function(move |_, _: Variadic<Value>| {
            end_cut_scene_context.borrow_mut().pop_cut_scene();
            Ok(())
        })?,
    )?;

    let wait_context = context.clone();
    globals.set(
        "wait_for_message",
        lua.create_function(move |_, args: Variadic<Value>| {
            let actor_hint = if let Some(Value::Table(table)) = args.get(0) {
                Some(actor_identity(&table)?)
            } else {
                None
            };
            let mut ctx = wait_context.borrow_mut();
            let ended = ctx.finish_dialog_line(actor_hint.as_ref().map(|(id, _)| id.as_str()));
            match ended {
                Some(state) => {
                    ctx.log_event(format!("dialog.wait {} {}", state.actor_label, state.line));
                }
                None => {
                    let label = actor_hint
                        .as_ref()
                        .map(|(_, label)| label.as_str())
                        .unwrap_or("<none>");
                    ctx.log_event(format!("dialog.wait {} <idle>", label));
                }
            }
            Ok(())
        })?,
    )?;

    let message_context = context.clone();
    globals.set(
        "IsMessageGoing",
        lua.create_function(move |_, ()| Ok(message_context.borrow().is_message_active()))?,
    )?;
    globals.set(
        "Load",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;

    let actor_pos_ctx = context.clone();
    globals.set(
        "GetActorPos",
        lua.create_function(move |_, actor: Value| -> LuaResult<(f64, f64, f64)> {
            if let Some(handle) = value_to_actor_handle(&actor) {
                if let Some(pos) = actor_pos_ctx.borrow().actor_position_by_handle(handle) {
                    return Ok((pos.x as f64, pos.y as f64, pos.z as f64));
                }
            }
            Ok((0.0, 0.0, 0.0))
        })?,
    )?;

    let actor_rot_ctx = context.clone();
    globals.set(
        "GetActorRot",
        lua.create_function(move |_, actor: Value| -> LuaResult<(f64, f64, f64)> {
            if let Some(handle) = value_to_actor_handle(&actor) {
                if let Some(rot) = actor_rot_ctx.borrow().actor_rotation_by_handle(handle) {
                    return Ok((rot.x as f64, rot.y as f64, rot.z as f64));
                }
            }
            Ok((0.0, 0.0, 0.0))
        })?,
    )?;

    let set_actor_pos_ctx = context.clone();
    globals.set(
        "SetActorPos",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let x = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let y = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let z = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let mut ctx = set_actor_pos_ctx.borrow_mut();
            ctx.set_actor_position_by_handle(handle, Vec3 { x, y, z });
            Ok(())
        })?,
    )?;

    let set_actor_rot_ctx = context.clone();
    globals.set(
        "SetActorRot",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let x = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let y = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let z = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let mut ctx = set_actor_rot_ctx.borrow_mut();
            ctx.set_actor_rotation_by_handle(handle, Vec3 { x, y, z });
            Ok(())
        })?,
    )?;

    let set_actor_scale_ctx = context.clone();
    globals.set(
        "SetActorScale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let scale = values.next().and_then(|value| value_to_f32(&value));
            let mut ctx = set_actor_scale_ctx.borrow_mut();
            ctx.set_actor_scale_by_handle(handle, scale);
            Ok(())
        })?,
    )?;

    let set_actor_collision_scale_ctx = context.clone();
    globals.set(
        "SetActorCollisionScale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let scale = values.next().and_then(|value| value_to_f32(&value));
            let mut ctx = set_actor_collision_scale_ctx.borrow_mut();
            ctx.set_actor_collision_scale_by_handle(handle, scale);
            Ok(())
        })?,
    )?;

    let angle_between_ctx = context.clone();
    globals.set(
        "GetAngleBetweenActors",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle_a = args.get(0).and_then(value_to_actor_handle);
            let handle_b = args.get(1).and_then(value_to_actor_handle);
            let (mut angle, label) = {
                let ctx = angle_between_ctx.borrow();
                if let (Some(a), Some(b)) = (handle_a, handle_b) {
                    let pos_a = ctx.actor_position_by_handle(a);
                    let pos_b = ctx.actor_position_by_handle(b);
                    if let (Some(a_pos), Some(b_pos)) = (pos_a, pos_b) {
                        let angle = heading_between(a_pos, b_pos);
                        (angle, format!("#{a} -> #{b}"))
                    } else {
                        (0.0, format!("#{a} -> #{b} (no pos)"))
                    }
                } else {
                    (0.0, "<invalid>".to_string())
                }
            };
            if angle.is_nan() {
                angle = 0.0;
            }
            {
                let mut ctx = angle_between_ctx.borrow_mut();
                ctx.log_event(format!("actor.angle_between {label} -> {:.2}", angle));
            }
            Ok(angle)
        })?,
    )?;

    let put_actor_set_ctx = context.clone();
    globals.set(
        "PutActorInSet",
        lua.create_function(move |_, (actor_value, set_value): (Value, Value)| {
            if let Some(handle) = value_to_actor_handle(&actor_value) {
                let set_file = match &set_value {
                    Value::Table(table) => {
                        if let Some(value) = table.get::<_, Option<String>>("setFile")? {
                            value
                        } else if let Some(value) = table.get::<_, Option<String>>("name")? {
                            value
                        } else if let Some(value) = table.get::<_, Option<String>>("label")? {
                            value
                        } else {
                            "<unknown>".to_string()
                        }
                    }
                    Value::String(text) => text.to_str()?.to_string(),
                    _ => "<unknown>".to_string(),
                };
                put_actor_set_ctx
                    .borrow_mut()
                    .put_actor_handle_in_set(handle, &set_file);
            }
            Ok(())
        })?,
    )?;

    let prefs = lua.create_table()?;
    prefs.set("init", noop.clone())?;
    prefs.set("write", noop.clone())?;
    prefs.set(
        "read",
        lua.create_function(|_, (_this, _key): (Table, Value)| Ok(0))?,
    )?;
    let voice_context = context.clone();
    prefs.set(
        "set_voice_effect",
        lua.create_function(move |_, (_this, value): (Table, Value)| {
            let effect = match value {
                Value::String(text) => text.to_str()?.to_string(),
                Value::Nil => "OFF".to_string(),
                other => format!("{:?}", other),
            };
            voice_context.borrow_mut().set_voice_effect(&effect);
            Ok(())
        })?,
    )?;
    globals.set("system_prefs", prefs)?;

    let concept_menu = lua.create_table()?;
    concept_menu.set("unlock_concepts", noop.clone())?;
    globals.set("concept_menu", concept_menu)?;

    let inventory_state = context.clone();
    let inventory = lua.create_table()?;
    inventory.set("unordered_inventory_table", lua.create_table()?)?;
    inventory.set(
        "add_item_to_inventory",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::Table(item)) = args.get(0) {
                if let Ok(name) = item.get::<_, String>("name") {
                    inventory_state.borrow_mut().add_inventory_item(&name);
                    return Ok(());
                }
            }
            if let Some(Value::String(name)) = args.get(0) {
                inventory_state
                    .borrow_mut()
                    .add_inventory_item(name.to_str()?);
            }
            Ok(())
        })?,
    )?;
    globals.set("Inventory", inventory)?;

    let cut_scene = lua.create_table()?;
    let runtime_clone = context.clone();
    cut_scene.set(
        "logos",
        lua.create_function(move |_, ()| {
            runtime_clone
                .borrow_mut()
                .log_event("cut_scene.logos scheduled");
            Ok(())
        })?,
    )?;
    let runtime_clone = context.clone();
    cut_scene.set(
        "intro",
        lua.create_function(move |_, ()| {
            runtime_clone
                .borrow_mut()
                .log_event("cut_scene.intro scheduled");
            Ok(())
        })?,
    )?;
    globals.set("cut_scene", cut_scene)?;

    Ok(())
}

fn install_set_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> LuaResult<()> {
    let globals = lua.globals();
    let set_table: Table = globals.get("Set")?;
    let original_create: Function = set_table.get("create")?;
    let create_key = lua.create_registry_value(original_create)?;
    let wrapper_context = context.clone();
    let wrapper = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let original: Function = lua_ctx.registry_value(&create_key)?;
        let result = original.call::<_, Value>(args)?;
        if let Value::Table(set_instance) = &result {
            ensure_set_metatable(lua_ctx, &set_instance)?;
            if let Ok(Some(set_file)) = set_instance.get::<_, Option<String>>("setFile") {
                wrapper_context.borrow_mut().mark_set_loaded(&set_file);
            }
        }
        Ok(result)
    })?;
    set_table.set("create", wrapper)?;
    Ok(())
}

fn install_parent_object_hook(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> LuaResult<()> {
    let globals = lua.globals();
    let existing = match globals.get::<_, Value>("parent_object") {
        Ok(Value::Table(table)) => Some(table),
        _ => None,
    };

    let parent_table = lua.create_table()?;
    if let Some(original) = existing {
        for pair in original.pairs::<Value, Value>() {
            let (key, value) = pair?;
            parent_table.raw_set(key, value)?;
        }
    }

    let parent_context = context.clone();
    let parent_handler =
        lua.create_function(move |lua_ctx, (table, key, value): (Table, Value, Value)| {
            if let Some(handle) = value_to_object_handle(&key) {
                match value.clone() {
                    Value::Nil => {
                        parent_context.borrow_mut().unregister_object(handle);
                    }
                    Value::Table(object_table) => {
                        ensure_object_metatable(
                            lua_ctx,
                            &object_table,
                            parent_context.clone(),
                            handle,
                        )?;
                        inject_object_controls(
                            lua_ctx,
                            &object_table,
                            parent_context.clone(),
                            handle,
                        )?;
                        let snapshot = read_object_snapshot(lua_ctx, &object_table, handle)
                            .map_err(LuaError::external)?;
                        parent_context.borrow_mut().register_object(snapshot);
                    }
                    _ => {}
                }
            }
            table.raw_set(key, value)?;
            Ok(())
        })?;
    let parent_meta = lua.create_table()?;
    parent_meta.set("__newindex", parent_handler)?;
    parent_table.set_metatable(Some(parent_meta));
    globals.set("parent_object", parent_table)?;
    Ok(())
}

fn install_runtime_tables(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<RegistryKey> {
    let globals = lua.globals();

    let system = lua.create_table()?;
    system.set("setTable", lua.create_table()?)?;
    system.set("setCount", 0)?;
    system.set("frameTime", 0.016_666_668)?;
    let axis_handler = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    system.set("axisHandler", axis_handler)?;
    globals.set("system", system.clone())?;

    let system_key = lua.create_registry_value(system.clone())?;

    install_menu_infrastructure(lua, context)?;

    Ok(system_key)
}

fn install_actor_scaffold(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<()> {
    let already_installed = {
        let borrow = context.borrow();
        borrow.actors_installed()
    };

    install_footsteps_table(lua)?;

    if already_installed {
        return Ok(());
    }

    ensure_actor_prototype(lua, context.clone(), system_key.clone())?;

    let (manny_id, manny_handle) = {
        let mut ctx = context.borrow_mut();
        ctx.register_actor_with_handle("Manny", Some(1001))
    };

    let manny_table = build_actor_table(
        lua,
        context.clone(),
        system_key.clone(),
        manny_id.clone(),
        "Manny".to_string(),
        manny_handle,
    )?;

    let meche_table = {
        let (meche_id, meche_handle) = {
            let mut ctx = context.borrow_mut();
            ctx.register_actor_with_handle("Meche", Some(1002))
        };
        build_actor_table(
            lua,
            context.clone(),
            system_key.clone(),
            meche_id.clone(),
            "Meche".to_string(),
            meche_handle,
        )?
    };

    let globals = lua.globals();
    globals.set("manny", manny_table.clone())?;
    globals.set("meche", meche_table.clone())?;

    {
        let mut ctx = context.borrow_mut();
        ctx.select_actor(&manny_id, "Manny");
        ctx.mark_actors_installed();
    }

    let system: Table = lua.registry_value(system_key.as_ref())?;
    system.set("currentActor", manny_table.clone())?;
    if matches!(system.get::<_, Value>("rootActor"), Ok(Value::Nil)) {
        system.set("rootActor", manny_table.clone())?;
    }

    Ok(())
}

fn ensure_actor_prototype<'lua>(
    lua: &'lua Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<Table<'lua>> {
    let globals = lua.globals();
    if let Ok(actor) = globals.get::<_, Table>("Actor") {
        return Ok(actor);
    }

    let actor = lua.create_table()?;
    install_actor_methods(lua, &actor, context.clone(), system_key.clone())?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("actor.stub Actor.{}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;

    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    actor.set_metatable(Some(metatable));

    globals.set("Actor", actor.clone())?;
    Ok(actor)
}

fn install_actor_methods(
    lua: &Lua,
    actor: &Table,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<()> {
    let create_context = context.clone();
    let create_system_key = system_key.clone();
    actor.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let mut label = None;
            for value in values.iter().rev() {
                if let Value::String(text) = value {
                    label = Some(text.to_str()?.to_string());
                    break;
                }
            }
            let label = label.unwrap_or_else(|| "actor".to_string());
            let (id, handle) = {
                let mut ctx = create_context.borrow_mut();
                ctx.register_actor_with_handle(&label, None)
            };
            build_actor_table(
                lua_ctx,
                create_context.clone(),
                create_system_key.clone(),
                id,
                label,
                handle,
            )
        })?,
    )?;

    let select_context = context.clone();
    let select_system_key = system_key.clone();
    actor.set(
        "set_selected",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                select_context.borrow_mut().select_actor(&id, &name);
                let system: Table = lua_ctx.registry_value(select_system_key.as_ref())?;
                system.set("currentActor", table)?;
            }
            Ok(())
        })?,
    )?;

    let put_context = context.clone();
    let put_system_key = system_key.clone();
    actor.set(
        "put_in_set",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                if let Some(set_value) = values.get(0) {
                    let set_file = if let Value::Table(set_table) = set_value {
                        if let Ok(Some(value)) = set_table.get::<_, Option<String>>("setFile") {
                            value
                        } else if let Ok(Some(value)) = set_table.get::<_, Option<String>>("name") {
                            value
                        } else if let Ok(Some(value)) = set_table.get::<_, Option<String>>("label")
                        {
                            value
                        } else {
                            "<unknown>".to_string()
                        }
                    } else if let Value::String(text) = set_value {
                        text.to_str()?.to_string()
                    } else {
                        "<unknown>".to_string()
                    };
                    put_context
                        .borrow_mut()
                        .put_actor_in_set(&id, &name, &set_file);
                    let system: Table = lua_ctx.registry_value(put_system_key.as_ref())?;
                    if let Ok(Value::Nil) = system.get::<_, Value>("currentActor") {
                        system.set("currentActor", table.clone())?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    let interest_context = context.clone();
    actor.set(
        "put_at_interest",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                interest_context.borrow_mut().actor_at_interest(&id, &name);
            }
            Ok(())
        })?,
    )?;

    let moveto_context = context.clone();
    let moveto_fn = lua.create_function(move |_, args: Variadic<Value>| {
        let (self_table, values) = split_self(args);
        if let Some(table) = self_table {
            if let Some(position) = value_slice_to_vec3(&values) {
                let (id, name) = actor_identity(&table)?;
                moveto_context
                    .borrow_mut()
                    .set_actor_position(&id, &name, position);
            }
        }
        Ok(())
    })?;
    actor.set("moveto", moveto_fn.clone())?;
    actor.set("moveTo", moveto_fn)?;

    let setpos_context = context.clone();
    actor.set(
        "setpos",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                if let Some(position) = value_slice_to_vec3(&values) {
                    let (id, name) = actor_identity(&table)?;
                    setpos_context
                        .borrow_mut()
                        .set_actor_position(&id, &name, position);
                }
            }
            Ok(())
        })?,
    )?;

    let set_softimage_pos_context = context.clone();
    actor.set(
        "set_softimage_pos",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                if let Some(position) = value_slice_to_vec3(&values) {
                    let (id, name) = actor_identity(&table)?;
                    set_softimage_pos_context
                        .borrow_mut()
                        .set_actor_position(&id, &name, position);
                }
            }
            Ok(())
        })?,
    )?;

    let setrot_context = context.clone();
    actor.set(
        "setrot",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                if let Some(rotation) = value_slice_to_vec3(&values) {
                    let (id, name) = actor_identity(&table)?;
                    setrot_context
                        .borrow_mut()
                        .set_actor_rotation(&id, &name, rotation);
                }
            }
            Ok(())
        })?,
    )?;

    let scale_context = context.clone();
    actor.set(
        "scale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                let scale = values.get(0).and_then(|value| value_to_f32(value));
                scale_context
                    .borrow_mut()
                    .set_actor_scale(&id, &name, scale);
            }
            Ok(())
        })?,
    )?;

    let set_scale_context = context.clone();
    actor.set(
        "set_scale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                let scale = values.get(0).and_then(|value| value_to_f32(value));
                set_scale_context
                    .borrow_mut()
                    .set_actor_scale(&id, &name, scale);
            }
            Ok(())
        })?,
    )?;

    let walkto_object_context = context.clone();
    actor.set(
        "walkto_object",
        lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<bool> {
            let (self_table, values) = split_self(args);
            let Some(actor_table) = self_table else {
                return Ok(false);
            };

            let (actor_id, actor_label) = actor_identity(&actor_table)?;
            let actor_handle = actor_table
                .get::<_, Option<i64>>("hActor")
                .ok()
                .flatten()
                .unwrap_or(0) as u32;
            if actor_handle == 0 {
                return Ok(false);
            }

            let Some(target_value) = values.get(0) else {
                return Ok(false);
            };
            let object_table = match target_value {
                Value::Table(table) => table.clone(),
                _ => return Ok(false),
            };

            let use_out = values
                .get(1)
                .map(|value| value_to_bool(value))
                .unwrap_or(false);
            let run_flag = values
                .get(2)
                .map(|value| value_to_bool(value))
                .unwrap_or(false);

            let object_name = object_table
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .or_else(|| {
                    object_table
                        .get::<_, Option<String>>("string_name")
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| "<object>".to_string());
            let object_handle = object_table
                .get::<_, Option<i64>>("handle")
                .ok()
                .flatten()
                .or_else(|| {
                    object_table
                        .get::<_, Option<i64>>("object_handle")
                        .ok()
                        .flatten()
                })
                .or_else(|| object_table.get::<_, Option<i64>>("hObject").ok().flatten());

            let position = if use_out {
                let x = object_table
                    .get::<_, Option<f32>>("out_pnt_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("out_pnt_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("out_pnt_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            } else {
                let x = object_table
                    .get::<_, Option<f32>>("use_pnt_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("use_pnt_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("use_pnt_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            };

            let rotation = if use_out {
                let x = object_table
                    .get::<_, Option<f32>>("out_rot_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("out_rot_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("out_rot_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            } else {
                let x = object_table
                    .get::<_, Option<f32>>("use_rot_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("use_rot_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("use_rot_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            };

            let destination_label = object_handle
                .map(|handle| format!("{} (#{handle})", object_name))
                .unwrap_or(object_name.clone());

            let moved = {
                let mut ctx = walkto_object_context.borrow_mut();
                if let Some(target) = position {
                    let moved = ctx.walk_actor_to_handle(actor_handle, target);
                    if moved {
                        if let Some(rot) = rotation {
                            ctx.set_actor_rotation_by_handle(actor_handle, rot);
                        }
                        if run_flag {
                            ctx.log_event(format!("actor.run {} true", actor_id));
                        }
                        ctx.log_event(format!(
                            "actor.walkto_object {} -> {}{}",
                            actor_label,
                            destination_label,
                            if use_out { " [out]" } else { "" }
                        ));
                    } else {
                        ctx.log_event(format!(
                            "actor.walkto_object {} failed {}",
                            actor_label, destination_label
                        ));
                    }
                    moved
                } else {
                    ctx.log_event(format!(
                        "actor.walkto_object {} missing target for {}",
                        actor_label, destination_label
                    ));
                    false
                }
            };

            actor_table.set("is_running", run_flag)?;
            Ok(moved)
        })?,
    )?;

    let visibility_context = context.clone();
    actor.set(
        "set_visibility",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let visible = values.get(0).map(value_to_bool).unwrap_or(false);
                table.set("is_visible", visible)?;
                let (id, name) = actor_identity(&table)?;
                visibility_context
                    .borrow_mut()
                    .set_actor_visibility(&id, &name, visible);
            }
            Ok(())
        })?,
    )?;

    let getpos_context = context.clone();
    actor.set(
        "getpos",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            let table = lua_ctx.create_table()?;
            if let Some(actor_table) = self_table {
                let (id, _name) = actor_identity(&actor_table)?;
                if let Some(snapshot) = getpos_context.borrow().actors.get(&id) {
                    if let Some(pos) = snapshot.position {
                        table.set("x", pos.x)?;
                        table.set("y", pos.y)?;
                        table.set("z", pos.z)?;
                        return Ok(table);
                    }
                }
            }
            table.set("x", 0.0)?;
            table.set("y", 0.0)?;
            table.set("z", 0.0)?;
            Ok(table)
        })?,
    )?;

    let getrot_context = context.clone();
    actor.set(
        "getrot",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            let table = lua_ctx.create_table()?;
            if let Some(actor_table) = self_table {
                let (id, _name) = actor_identity(&actor_table)?;
                if let Some(snapshot) = getrot_context.borrow().actors.get(&id) {
                    if let Some(rot) = snapshot.rotation {
                        table.set("x", rot.x)?;
                        table.set("y", rot.y)?;
                        table.set("z", rot.z)?;
                        return Ok(table);
                    }
                }
            }
            table.set("x", 0.0)?;
            table.set("y", 0.0)?;
            table.set("z", 0.0)?;
            Ok(table)
        })?,
    )?;

    let sector_type_context = context.clone();
    actor.set(
        "find_sector_type",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let requested = values.get(0).and_then(|value| value_to_string(value));
                let request_label = requested.clone().unwrap_or_else(|| "<nil>".to_string());
                let hit = {
                    let mut ctx = sector_type_context.borrow_mut();
                    let hit = ctx.default_sector_hit(&id, requested.as_deref());
                    ctx.log_event(format!(
                        "actor.sector {} {} (req={}) -> {}",
                        id, hit.kind, request_label, hit.name
                    ));
                    ctx.record_sector_hit(&id, &label, hit.clone());
                    hit
                };
                let values = vec![
                    Value::Integer(hit.id as i64),
                    Value::String(lua_ctx.create_string(&hit.name)?),
                    Value::String(lua_ctx.create_string(&hit.kind)?),
                ];
                return Ok(MultiValue::from_vec(values));
            }
            Ok(MultiValue::new())
        })?,
    )?;

    let sector_name_context = context.clone();
    actor.set(
        "find_sector_name",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _label) = actor_identity(&table)?;
                let query = values
                    .get(0)
                    .and_then(|value| value_to_string(value))
                    .unwrap_or_default();
                let result = {
                    let mut ctx = sector_name_context.borrow_mut();
                    let hit = ctx.evaluate_sector_name(&id, &query);
                    ctx.log_event(format!(
                        "actor.sector_name {} {} -> {}",
                        id,
                        query,
                        if hit { "true" } else { "false" }
                    ));
                    hit
                };
                return Ok(result);
            }
            Ok(false)
        })?,
    )?;

    let costume_context = context.clone();
    actor.set(
        "set_costume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let costume = values.get(0).and_then(|value| match value {
                    Value::String(text) => Some(text.to_str().ok()?.to_string()),
                    Value::Nil => None,
                    _ => None,
                });
                let (id, name) = actor_identity(&table)?;
                {
                    let mut ctx = costume_context.borrow_mut();
                    ctx.set_actor_base_costume(&id, &name, costume.clone());
                    ctx.set_actor_costume(&id, &name, costume.clone());
                }
                match costume {
                    Some(ref value) => {
                        table.set("base_costume", value.clone())?;
                        table.set("current_costume", value.clone())?;
                    }
                    None => {
                        table.set("base_costume", Value::Nil)?;
                        table.set("current_costume", Value::Nil)?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    let default_context = context.clone();
    actor.set(
        "default",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let costume = values.get(0).and_then(|value| match value {
                    Value::String(text) => Some(text.to_str().ok()?.to_string()),
                    Value::Nil => None,
                    _ => None,
                });
                let (id, name) = actor_identity(&table)?;
                {
                    let mut ctx = default_context.borrow_mut();
                    ctx.set_actor_base_costume(&id, &name, costume.clone());
                    ctx.set_actor_costume(&id, &name, costume.clone());
                }
                match costume {
                    Some(ref value) => {
                        table.set("base_costume", value.clone())?;
                        table.set("current_costume", value.clone())?;
                    }
                    None => {
                        table.set("base_costume", Value::Nil)?;
                        table.set("current_costume", Value::Nil)?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    let get_costume_context = context.clone();
    actor.set(
        "get_costume",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _label) = actor_identity(&table)?;
                if let Some(costume) = get_costume_context.borrow().actor_costume(&id) {
                    return Ok(Value::String(lua_ctx.create_string(costume)?));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;

    let say_context = context.clone();
    let say_system_key = system_key.clone();
    let normal_say_line =
        lua.create_function(move |lua_ctx, args: Variadic<Value>| -> LuaResult<()> {
            let (self_table, values) = split_self(args);
            if let Some(actor_table) = self_table {
                let (id, label) = actor_identity(&actor_table)?;
                let line = values
                    .get(0)
                    .and_then(|value| value_to_string(value))
                    .unwrap_or_else(|| "<nil>".to_string());
                let options_table = values.get(1).and_then(|value| match value {
                    Value::Table(table) => Some(table.clone()),
                    _ => None,
                });

                let mut background = false;
                let mut skip_log = false;

                if let Ok(Value::Table(defaults)) = actor_table.get::<_, Value>("saylineTable") {
                    if let Ok(value) = defaults.get::<_, Value>("background") {
                        background = value_to_bool(&value);
                    }
                    if let Ok(value) = defaults.get::<_, Value>("skip_log") {
                        skip_log = value_to_bool(&value);
                    }
                }

                if let Some(options) = options_table {
                    if let Ok(value) = options.get::<_, Value>("background") {
                        background = value_to_bool(&value);
                    }
                    if let Ok(value) = options.get::<_, Value>("skip_log") {
                        skip_log = value_to_bool(&value);
                    }
                }

                {
                    let mut ctx = say_context.borrow_mut();
                    ctx.log_event(format!("dialog.say {id} {line}"));
                    if !skip_log {
                        ctx.log_event(format!("dialog.log {id} {line}"));
                    }
                    if !background {
                        ctx.begin_dialog_line(&id, &label, &line);
                    }
                }

                if !background {
                    let system: Table = lua_ctx.registry_value(say_system_key.as_ref())?;
                    system.set("lastActorTalking", actor_table.clone())?;
                }
            }
            Ok(())
        })?;
    actor.set("normal_say_line", normal_say_line.clone())?;
    actor.set("say_line", normal_say_line.clone())?;
    actor.set("underwater_say_line", normal_say_line.clone())?;

    let complete_chore_context = context.clone();
    actor.set(
        "complete_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _label) = actor_identity(&table)?;
                let (has_costume, base_costume) = {
                    let ctx = complete_chore_context.borrow();
                    (
                        ctx.actor_costume(&id).is_some(),
                        ctx.actor_base_costume(&id).map(str::to_string),
                    )
                };
                if !has_costume {
                    return Ok(());
                }
                let chore = values
                    .get(0)
                    .and_then(|value| value_to_string(value))
                    .unwrap_or_else(|| "<nil>".to_string());
                let costume_override = values.get(1).and_then(|value| value_to_string(value));
                let costume_label = costume_override
                    .or(base_costume)
                    .unwrap_or_else(|| "<nil>".to_string());
                complete_chore_context
                    .borrow_mut()
                    .log_event(format!("actor.{id}.complete_chore {chore} {costume_label}"));
            }
            Ok(())
        })?,
    )?;

    let speak_context = context.clone();
    actor.set(
        "is_speaking",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _name) = actor_identity(&table)?;
                let speaking = speak_context
                    .borrow()
                    .speaking_actor()
                    .map(|selected| selected.eq_ignore_ascii_case(&id))
                    .unwrap_or(false);
                return Ok(speaking);
            }
            Ok(false)
        })?,
    )?;

    let actor_wait_context = context.clone();
    actor.set(
        "wait_for_message",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let mut ctx = actor_wait_context.borrow_mut();
                if let Some(state) = ctx.finish_dialog_line(Some(&id)) {
                    ctx.log_event(format!("dialog.wait {} {}", state.actor_label, state.line));
                } else {
                    ctx.log_event(format!("dialog.wait {} <idle>", label));
                }
            }
            Ok(())
        })?,
    )?;

    let play_chore_context = context.clone();
    actor.set(
        "play_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| value_to_string(value));
                let costume = values.get(1).and_then(|value| value_to_string(value));
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = play_chore_context.borrow_mut();
                    ctx.set_actor_current_chore(&id, &label, chore.clone(), costume.clone());
                }
                match chore {
                    Some(ref value) => {
                        table.set("last_chore_played", value.clone())?;
                        table.set("current_chore", value.clone())?;
                    }
                    None => {
                        table.set("last_chore_played", Value::Nil)?;
                        table.set("current_chore", Value::Nil)?;
                    }
                }
                match costume {
                    Some(ref value) => table.set("last_cos_played", value.clone())?,
                    None => table.set("last_cos_played", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let pop_costume_context = context.clone();
    actor.set(
        "pop_costume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let success = {
                    let mut ctx = pop_costume_context.borrow_mut();
                    ctx.pop_actor_costume(&id, &label).is_some()
                };
                {
                    let ctx = pop_costume_context.borrow();
                    if let Some(costume) = ctx.actor_costume(&id) {
                        table.set("current_costume", costume.to_string())?;
                    } else {
                        table.set("current_costume", Value::Nil)?;
                    }
                }
                return Ok(success);
            }
            Ok(false)
        })?,
    )?;

    let head_look_context = context.clone();
    actor.set(
        "head_look_at",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let target_label = values
                    .get(0)
                    .map(|value| match value {
                        Value::Table(actor_table) => {
                            if let Ok(name) = actor_table.get::<_, String>("name") {
                                name
                            } else if let Ok(id) = actor_table.get::<_, String>("id") {
                                format!("table:{id}")
                            } else {
                                describe_value(value)
                            }
                        }
                        other => describe_value(other),
                    })
                    .unwrap_or_else(|| "<nil>".to_string());
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = head_look_context.borrow_mut();
                    ctx.set_actor_head_target(&id, &label, Some(target_label.clone()));
                }
                table.set("head_target_label", target_label)?;
            }
            Ok(())
        })?,
    )?;

    let push_costume_context = context.clone();
    actor.set(
        "push_costume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let Some(costume) = values.get(0).and_then(|value| value_to_string(value)) else {
                    return Ok(false);
                };
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = push_costume_context.borrow_mut();
                    ctx.push_actor_costume(&id, &label, costume.clone());
                }
                table.set("current_costume", costume)?;
                return Ok(true);
            }
            Ok(false)
        })?,
    )?;

    let walk_chore_context = context.clone();
    actor.set(
        "set_walk_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let costume = values.get(1).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = walk_chore_context.borrow_mut();
                    ctx.set_actor_walk_chore(&id, &label, chore.clone(), costume.clone());
                }
                match chore {
                    Some(ref value) => table.set("walk_chore", value.clone())?,
                    None => table.set("walk_chore", Value::Nil)?,
                }
                match costume {
                    Some(ref value) => table.set("walk_chore_costume", value.clone())?,
                    None => table.set("walk_chore_costume", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let talk_color_context = context.clone();
    actor.set(
        "set_talk_color",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let color = values.get(0).and_then(|value| value_to_string(value));
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = talk_color_context.borrow_mut();
                    ctx.set_actor_talk_color(&id, &label, color.clone());
                }
                match color {
                    Some(ref value) => table.set("talk_color", value.clone())?,
                    None => table.set("talk_color", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let mumble_chore_context = context.clone();
    actor.set(
        "set_mumble_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let costume = values.get(1).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = mumble_chore_context.borrow_mut();
                    ctx.set_actor_mumble_chore(&id, &label, chore.clone(), costume.clone());
                }
                match chore {
                    Some(ref value) => table.set("mumble_chore", value.clone())?,
                    None => table.set("mumble_chore", Value::Nil)?,
                }
                match costume {
                    Some(ref value) => table.set("mumble_costume", value.clone())?,
                    None => table.set("mumble_costume", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let talk_chore_context = context.clone();
    actor.set(
        "set_talk_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let drop = values.get(1).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let costume = values.get(2).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = talk_chore_context.borrow_mut();
                    ctx.set_actor_talk_chore(
                        &id,
                        &label,
                        chore.clone(),
                        drop.clone(),
                        costume.clone(),
                    );
                }
                match chore {
                    Some(ref value) => table.set("talk_chore", value.clone())?,
                    None => table.set("talk_chore", Value::Nil)?,
                }
                match drop {
                    Some(ref value) => table.set("talk_drop_chore", value.clone())?,
                    None => table.set("talk_drop_chore", Value::Nil)?,
                }
                match costume {
                    Some(ref value) => table.set("talk_chore_costume", value.clone())?,
                    None => table.set("talk_chore_costume", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let set_head_context = context.clone();
    actor.set(
        "set_head",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let params = values
                    .iter()
                    .map(|value| describe_value(value))
                    .collect::<Vec<_>>()
                    .join(", ");
                {
                    let mut ctx = set_head_context.borrow_mut();
                    ctx.set_actor_head_target(&id, &label, Some("manual".to_string()));
                    ctx.log_event(format!("actor.{id}.set_head {params}"));
                }
                table.set("head_control", params)?;
            }
            Ok(())
        })?,
    )?;

    let look_rate_context = context.clone();
    actor.set(
        "set_look_rate",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let rate = values.get(0).and_then(|value| value_to_f32(value));
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = look_rate_context.borrow_mut();
                    ctx.set_actor_head_look_rate(&id, &label, rate);
                }
                if let Some(value) = rate {
                    table.set("head_look_rate", value)?;
                } else {
                    table.set("head_look_rate", Value::Nil)?;
                }
            }
            Ok(())
        })?,
    )?;

    let collision_mode_context = context.clone();
    actor.set(
        "set_collision_mode",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let mode = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = collision_mode_context.borrow_mut();
                    ctx.set_actor_collision_mode(&id, &label, mode.clone());
                }
                match mode {
                    Some(ref value) => table.set("collision_mode", value.clone())?,
                    None => table.set("collision_mode", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let ignore_boxes_context = context.clone();
    actor.set(
        "ignore_boxes",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let flag = values
                    .get(0)
                    .map(|value| value_to_bool(value))
                    .unwrap_or(true);
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = ignore_boxes_context.borrow_mut();
                    ctx.set_actor_ignore_boxes(&id, &label, flag);
                }
                table.set("ignoring_boxes", flag)?;
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

fn build_actor_table<'lua>(
    lua_ctx: &'lua Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
    id: String,
    label: String,
    handle: u32,
) -> LuaResult<Table<'lua>> {
    let actor_table = lua_ctx.create_table()?;
    actor_table.set("name", label.clone())?;
    actor_table.set("id", id.clone())?;
    actor_table.set("hActor", handle as i64)?;

    actor_table.set("is_running", false)?;
    actor_table.set("is_backward", false)?;
    actor_table.set("no_idle_head", false)?;

    let actor_proto: Table = lua_ctx.globals().get("Actor")?;
    actor_table.set("parent", actor_proto.clone())?;

    let metatable = lua_ctx.create_table()?;
    metatable.set("__index", actor_proto.clone())?;
    actor_table.set_metatable(Some(metatable));

    let system: Table = lua_ctx.registry_value(system_key.as_ref())?;
    let registry: Table = match system.get("actorTable") {
        Ok(table) => table,
        Err(_) => {
            let table = lua_ctx.create_table()?;
            system.set("actorTable", table.clone())?;
            table
        }
    };

    let existing = registry
        .get::<_, Value>(label.clone())
        .unwrap_or(Value::Nil);
    if matches!(existing, Value::Nil) {
        let count: i64 = system.get("actorCount").unwrap_or(0);
        system.set("actorCount", count + 1)?;
    }

    registry.set(label.clone(), actor_table.clone())?;
    registry.set(handle as i64, actor_table.clone())?;

    {
        let mut ctx = context.borrow_mut();
        ctx.ensure_actor_mut(&id, &label);
        ctx.log_event(format!("actor.table {} (#{handle})", label));
    }

    Ok(actor_table)
}

fn extract_function<'lua>(lua: &'lua Lua, value: Value<'lua>) -> LuaResult<Option<Function<'lua>>> {
    match value {
        Value::Function(f) => Ok(Some(f)),
        Value::String(name) => {
            let globals = lua.globals();
            let func: Function = globals.get(name.to_str()?)?;
            Ok(Some(func))
        }
        Value::Table(table) => {
            if let Ok(func) = table.get::<_, Function>("run") {
                Ok(Some(func))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

pub(crate) fn strip_self(args: Variadic<Value>) -> Vec<Value> {
    let mut iter = args.into_iter();
    match iter.next() {
        Some(Value::Table(_)) => iter.collect(),
        Some(value) => {
            let mut values = vec![value];
            values.extend(iter);
            values
        }
        None => Vec::new(),
    }
}

fn describe_function(func: &Function) -> String {
    let info = func.info();
    if let Some(name) = info.name.clone() {
        if !name.is_empty() {
            return name;
        }
    }
    if let Some(short) = info.short_src.clone() {
        if let Some(line) = info.line_defined {
            if line > 0 {
                return format!("{short}:{line}");
            }
        }
        return format!("function@{short}");
    }
    if let Some(source) = info.source.clone() {
        if let Some(line) = info.line_defined {
            if line > 0 {
                return format!("{source}:{line}");
            }
        }
        return format!("function@{source}");
    }
    match info.what {
        "C" => "<cfunction>".to_string(),
        other => format!("<{other}>"),
    }
}

fn describe_callable_label(value: &Value) -> LuaResult<String> {
    match value {
        Value::Function(func) => Ok(describe_function(func)),
        Value::String(s) => Ok(s.to_str()?.to_string()),
        Value::Table(table) => {
            if let Ok(name) = table.get::<_, String>("name") {
                if !name.is_empty() {
                    return Ok(name);
                }
            }
            if let Ok(label) = table.get::<_, String>("label") {
                if !label.is_empty() {
                    return Ok(label);
                }
            }
            if let Ok(func) = table.get::<_, Function>("run") {
                return Ok(describe_function(&func));
            }
            Ok(format!("table@{:p}", table.to_pointer()))
        }
        Value::Nil => Ok("<nil>".to_string()),
        other => Ok(describe_value(other)),
    }
}

pub(crate) fn value_to_bool(value: &Value) -> bool {
    match value {
        Value::Boolean(flag) => *flag,
        Value::Integer(i) => *i != 0,
        Value::Number(n) => *n != 0.0,
        Value::String(s) => s
            .to_str()
            .map(|text| text != "0" && text != "false")
            .unwrap_or(false),
        _ => false,
    }
}

pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => text.to_str().ok().map(|s| s.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}
pub(crate) fn describe_value(value: &Value) -> String {
    if let Some(text) = value_to_string(value) {
        return text;
    }
    match value {
        Value::Function(func) => describe_function(func),
        _ => format!("<{value:?}>"),
    }
}

pub(crate) fn split_self<'lua>(
    args: Variadic<Value<'lua>>,
) -> (Option<Table<'lua>>, Vec<Value<'lua>>) {
    let mut iter = args.into_iter();
    match iter.next() {
        Some(Value::Table(table)) => (Some(table), iter.collect()),
        Some(first) => {
            let mut values = vec![first];
            values.extend(iter);
            (None, values)
        }
        None => (None, Vec::new()),
    }
}

fn actor_identity<'lua>(table: &Table<'lua>) -> LuaResult<(String, String)> {
    let id: String = table.get("id")?;
    let name: String = table.get("name")?;
    Ok((id, name))
}

pub(crate) fn value_slice_to_vec3(values: &[Value]) -> Option<Vec3> {
    if values.len() >= 3 {
        let x = value_to_f32(&values[0])?;
        let y = value_to_f32(&values[1])?;
        let z = value_to_f32(&values[2])?;
        return Some(Vec3 { x, y, z });
    }
    if let Some(Value::Table(table)) = values.get(0) {
        let x = table.get::<_, f32>("x").ok()?;
        let y = table.get::<_, f32>("y").ok()?;
        let z = table.get::<_, f32>("z").ok()?;
        return Some(Vec3 { x, y, z });
    }
    None
}

pub(crate) fn value_to_f32(value: &Value) -> Option<f32> {
    match value {
        Value::Integer(i) => Some(*i as f32),
        Value::Number(n) => Some(*n as f32),
        _ => None,
    }
}

fn value_to_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Integer(i) => Some(*i as i32),
        Value::Number(n) => Some(*n as i32),
        Value::String(text) => text.to_str().ok()?.parse().ok(),
        _ => None,
    }
}

fn value_to_set_file(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_str().ok()?.to_string()),
        Value::Table(table) => {
            if let Ok(Some(file)) = table.get::<_, Option<String>>("setFile") {
                return Some(file);
            }
            if let Ok(Some(name)) = table.get::<_, Option<String>>("name") {
                return Some(name);
            }
            if let Ok(Some(label)) = table.get::<_, Option<String>>("label") {
                return Some(label);
            }
            None
        }
        _ => None,
    }
}

fn value_to_sector_name(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_str().ok()?.to_string()),
        Value::Table(table) => {
            if let Ok(Some(name)) = table.get::<_, Option<String>>("name") {
                return Some(name);
            }
            if let Ok(Some(label)) = table.get::<_, Option<String>>("label") {
                return Some(label);
            }
            None
        }
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn value_to_object_handle(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(handle) => Some(*handle),
        Value::Number(number) => Some(*number as i64),
        Value::String(text) => text.to_str().ok()?.parse().ok(),
        _ => None,
    }
}

fn value_to_actor_handle(value: &Value) -> Option<u32> {
    match value {
        Value::Integer(handle) if *handle >= 0 => Some(*handle as u32),
        Value::Number(number) if *number >= 0.0 => Some(*number as u32),
        Value::Table(table) => {
            if let Ok(Some(id)) = table.get::<_, Option<i64>>("hActor") {
                if id >= 0 {
                    return Some(id as u32);
                }
            }
            if let Ok(Some(id)) = table.get::<_, Option<u32>>("hActor") {
                return Some(id);
            }
            None
        }
        _ => None,
    }
}

fn ensure_object_metatable(
    lua: &Lua,
    object: &Table,
    context: Rc<RefCell<EngineContext>>,
    handle: i64,
) -> LuaResult<()> {
    if let Ok(parent) = object.get::<_, Table>("parent") {
        let metatable = match object.get_metatable() {
            Some(meta) => meta,
            None => lua.create_table()?,
        };
        metatable.set("__index", parent)?;

        let current_newindex = metatable
            .get::<_, Value>("__newindex")
            .unwrap_or(Value::Nil);
        if matches!(current_newindex, Value::Nil) {
            let ctx = context.clone();
            let handler =
                lua.create_function(move |_, (table, key, value): (Table, Value, Value)| {
                    let key_name = match &key {
                        Value::String(text) => Some(text.to_str()?.to_string()),
                        _ => None,
                    };

                    table.raw_set(key.clone(), value.clone())?;

                    if let Some(name) = key_name.as_deref() {
                        match name {
                            "touchable" => {
                                let touchable = value_to_bool(&value);
                                ctx.borrow_mut().set_object_touchable(handle, touchable);
                            }
                            "visible" | "is_visible" => {
                                let visible = value_to_bool(&value);
                                ctx.borrow_mut().set_object_visibility(handle, visible);
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                })?;
            metatable.set("__newindex", handler)?;
        }

        object.set_metatable(Some(metatable));
    }
    Ok(())
}

fn ensure_set_metatable(lua: &Lua, set_instance: &Table) -> LuaResult<()> {
    let globals = lua.globals();
    let prototype: Table = globals.get("Set")?;
    let metatable = match set_instance.get_metatable() {
        Some(meta) => meta,
        None => lua.create_table()?,
    };
    metatable.set("__index", prototype)?;
    set_instance.set_metatable(Some(metatable));
    Ok(())
}

fn inject_object_controls(
    lua: &Lua,
    object: &Table,
    context: Rc<RefCell<EngineContext>>,
    handle: i64,
) -> LuaResult<()> {
    context
        .borrow_mut()
        .log_event(format!("object.prepare #{handle}"));
    let untouchable = object
        .get::<_, Value>("make_untouchable")
        .unwrap_or(Value::Nil);
    if matches!(untouchable, Value::Nil) {
        let func = lua.create_function(move |_, (this,): (Table,)| {
            this.set("touchable", false)?;
            Ok(())
        })?;
        object.set("make_untouchable", func)?;
    }

    let touchable = object
        .get::<_, Value>("make_touchable")
        .unwrap_or(Value::Nil);
    if matches!(touchable, Value::Nil) {
        let func = lua.create_function(move |_, (this,): (Table,)| {
            this.set("touchable", true)?;
            Ok(())
        })?;
        object.set("make_touchable", func)?;
    }

    Ok(())
}

fn read_object_snapshot(_lua: &Lua, object: &Table, handle: i64) -> LuaResult<ObjectSnapshot> {
    let string_name = object.get::<_, Option<String>>("string_name")?;
    let name = object
        .get::<_, Option<String>>("name")?
        .or_else(|| string_name.clone())
        .unwrap_or_else(|| format!("object#{handle}"));
    let set_file = if let Some(set_table) = object.get::<_, Option<Table>>("obj_set")? {
        set_table.get::<_, Option<String>>("setFile")?
    } else {
        None
    };
    let obj_x = object.get::<_, Option<f32>>("obj_x")?;
    let obj_y = object.get::<_, Option<f32>>("obj_y")?;
    let obj_z = object.get::<_, Option<f32>>("obj_z")?;
    let position = match (obj_x, obj_y, obj_z) {
        (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
        _ => None,
    };
    let range = object.get::<_, Option<f32>>("range")?.unwrap_or(0.0);
    let touchable = object.get::<_, Option<bool>>("touchable")?.unwrap_or(false);
    let visible = if let Some(flag) = object.get::<_, Option<bool>>("is_visible")? {
        flag
    } else if let Some(flag) = object.get::<_, Option<bool>>("visible")? {
        flag
    } else {
        true
    };
    let interest_actor = object
        .get::<_, Value>("interest_actor")
        .ok()
        .and_then(|value| value_to_actor_handle(&value));
    Ok(ObjectSnapshot {
        handle,
        name,
        string_name,
        set_file,
        position,
        range,
        touchable,
        visible,
        interest_actor,
        sectors: Vec::new(),
    })
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
    install_parent_object_hook(lua, context.clone()).map_err(|err| anyhow!(err))?;
    install_set_scaffold(lua, context.clone()).map_err(|err| anyhow!(err))?;
    let globals = lua.globals();
    let source_context = context.clone();
    let source_stub = lua.create_function(move |lua_ctx, ()| {
        let globals = lua_ctx.globals();
        if let Ok(load_room_code) = globals.get::<_, Function>("load_room_code") {
            let _: Value = load_room_code.call("mo.lua")?;
        } else if let Ok(dofile) = globals.get::<_, Function>("dofile") {
            let _: Value = dofile.call("mo.lua")?;
        }
        source_context.borrow_mut().mark_set_loaded("mo.set");
        Ok(())
    })?;
    globals.set("source_all_set_files", source_stub)?;

    globals.set("start_script", create_start_script(lua, context.clone())?)?;
    globals.set(
        "single_start_script",
        create_single_start_script(lua, context.clone())?,
    )?;

    let wait_context = context.clone();
    globals.set(
        "wait_for_script",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            for value in args.into_iter() {
                match value {
                    Value::Integer(handle) => {
                        wait_for_handle(lua_ctx, wait_context.clone(), handle as u32)?;
                    }
                    Value::Number(handle) => {
                        wait_for_handle(lua_ctx, wait_context.clone(), handle as u32)?;
                    }
                    Value::Function(func) => {
                        func.call::<_, ()>(MultiValue::new())?;
                    }
                    Value::Table(table) => {
                        if let Ok(func) = table.get::<_, Function>("run") {
                            func.call::<_, ()>(MultiValue::new())?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })?,
    )?;

    let find_context = context.clone();
    globals.set(
        "find_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::String(label)) = args.get(0) {
                if let Some(handle) = find_context.borrow().find_script_handle(label.to_str()?) {
                    return Ok(Value::Integer(handle as i64));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;

    let cache_vector_context = context.clone();
    globals.set(
        "CacheCurrentWalkVector",
        lua.create_function(move |_, _: Variadic<Value>| {
            cache_vector_context
                .borrow_mut()
                .log_event("geometry.cache_walk_vector".to_string());
            Ok(())
        })?,
    )?;

    let stop_context = context.clone();
    globals.set(
        "stop_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            let description = args
                .get(0)
                .map(describe_value)
                .unwrap_or_else(|| "<unknown>".to_string());
            stop_context
                .borrow_mut()
                .log_event(format!("script.stop {description}"));
            Ok(())
        })?,
    )?;

    let current_script_context = context.clone();
    globals.set(
        "GetCurrentScript",
        lua.create_function(move |_, _: Variadic<Value>| {
            current_script_context
                .borrow_mut()
                .log_event("script.current".to_string());
            Ok(Value::Nil)
        })?,
    )?;

    if matches!(
        globals.get::<_, Value>("WalkVector"),
        Ok(Value::Nil) | Err(_)
    ) {
        let walk_vector = lua.create_table()?;
        walk_vector.set("x", 0.0)?;
        walk_vector.set("y", 0.0)?;
        walk_vector.set("z", 0.0)?;
        globals.set("WalkVector", walk_vector)?;
    }

    install_cutscene_helpers(lua, context.clone())?;
    install_idle_scaffold(lua, context.clone())?;

    wrap_start_cut_scene(lua, context.clone())?;
    wrap_end_cut_scene(lua, context.clone())?;
    wrap_set_override(lua, context.clone())?;
    wrap_kill_override(lua, context.clone())?;
    wrap_wait_for_message(lua, context.clone())?;

    Ok(())
}

fn install_cutscene_helpers(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let stop_commentary_context = context.clone();
    globals.set(
        "StopCommentaryImmediately",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_commentary_context
                .borrow_mut()
                .log_event("cut_scene.stop_commentary".to_string());
            Ok(())
        })?,
    )?;

    let kill_render_context = context.clone();
    globals.set(
        "killRenderModeText",
        lua.create_function(move |_, _: Variadic<Value>| {
            kill_render_context
                .borrow_mut()
                .log_event("render.kill_mode_text".to_string());
            Ok(())
        })?,
    )?;

    let destroy_buttons_context = context.clone();
    globals.set(
        "DestroyAllUIButtonsImmediately",
        lua.create_function(move |_, _: Variadic<Value>| {
            destroy_buttons_context
                .borrow_mut()
                .log_event("ui.destroy_buttons_immediate".to_string());
            Ok(())
        })?,
    )?;

    let start_movie_context = context.clone();
    globals.set(
        "StartFullscreenMovie",
        lua.create_function(move |_, args: Variadic<Value>| {
            let movie = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            start_movie_context
                .borrow_mut()
                .log_event(format!("cut_scene.fullscreen.start {movie}"));
            Ok(true)
        })?,
    )?;

    let movie_state_context = context.clone();
    globals.set(
        "IsFullscreenMoviePlaying",
        lua.create_function(move |_, _: Variadic<Value>| {
            movie_state_context
                .borrow_mut()
                .log_event("cut_scene.fullscreen.poll".to_string());
            Ok(false)
        })?,
    )?;

    let hide_skip_context = context.clone();
    globals.set(
        "hideSkipButton",
        lua.create_function(move |_, _: Variadic<Value>| {
            hide_skip_context
                .borrow_mut()
                .log_event("cut_scene.skip.hide".to_string());
            Ok(())
        })?,
    )?;

    let show_skip_context = context;
    globals.set(
        "showSkipButton",
        lua.create_function(move |_, _: Variadic<Value>| {
            show_skip_context
                .borrow_mut()
                .log_event("cut_scene.skip.show".to_string());
            Ok(())
        })?,
    )?;

    Ok(())
}

fn install_idle_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("Idle"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let idle_table = lua.create_table()?;
    let create_context = context.clone();
    idle_table.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let name = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unnamed>".to_string());
            create_context
                .borrow_mut()
                .log_event(format!("idle.create {name}"));

            let state_table = lua_ctx.create_table()?;
            let add_state_context = create_context.clone();
            state_table.set(
                "add_state",
                lua_ctx.create_function(move |_, args: Variadic<Value>| {
                    let state_name = args
                        .get(0)
                        .and_then(value_to_string)
                        .unwrap_or_else(|| "<unnamed>".to_string());
                    add_state_context
                        .borrow_mut()
                        .log_event(format!("idle.state {state_name}"));
                    Ok(())
                })?,
            )?;
            Ok(state_table)
        })?,
    )?;

    globals.set("Idle", idle_table)?;
    Ok(())
}

fn wrap_start_cut_scene(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("START_CUT_SCENE") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let label = values.get(0).and_then(|value| value_to_string(value));
            let flags: Vec<String> = values
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect();
            ctx.borrow_mut().push_cut_scene(label, flags);
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            Ok(result)
        },
    )?;
    globals.set("START_CUT_SCENE", wrapper)?;
    Ok(())
}

fn wrap_end_cut_scene(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("END_CUT_SCENE") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            ctx.borrow_mut().pop_cut_scene();
            Ok(result)
        },
    )?;
    globals.set("END_CUT_SCENE", wrapper)?;
    Ok(())
}

fn wrap_set_override(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("set_override") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            {
                let mut ctx = ctx.borrow_mut();
                match values.get(0) {
                    Some(Value::Nil) | None => {
                        ctx.pop_override();
                    }
                    Some(value) => {
                        ctx.push_override(describe_value(value));
                    }
                }
            }
            Ok(result)
        },
    )?;
    globals.set("set_override", wrapper)?;
    Ok(())
}

fn wrap_kill_override(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("kill_override") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            {
                let mut ctx = ctx.borrow_mut();
                ctx.clear_overrides();
            }
            Ok(result)
        },
    )?;
    globals.set("kill_override", wrapper)?;
    Ok(())
}

fn wrap_wait_for_message(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("wait_for_message") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            {
                let mut ctx = ctx.borrow_mut();
                if let Some(state) = ctx.finish_dialog_line(None) {
                    ctx.log_event(format!("dialog.wait {} {}", state.actor_label, state.line));
                } else {
                    ctx.log_event("dialog.wait global <idle>".to_string());
                }
            }
            Ok(result)
        },
    )?;
    globals.set("wait_for_message", wrapper)?;
    Ok(())
}

pub(crate) fn call_boot(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let boot: Function = globals
        .get("BOOT")
        .context("BOOT function missing after loading _system")?;
    boot.call::<_, ()>((false, Value::Nil))
        .context("executing BOOT(false)")?;
    if context.borrow().verbose {
        println!("[lua-runtime] BOOT completed");
    }
    Ok(())
}

fn install_achievement_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("achievement"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;

    let set_context = context.clone();
    table.set(
        "setEligible",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let id = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let eligible = values.get(1).map(value_to_bool).unwrap_or(true);
            set_context
                .borrow_mut()
                .set_achievement_eligibility(&id, eligible);
            Ok(())
        })?,
    )?;

    let established_context = context.clone();
    table.set(
        "hasEligibilityBeenEstablished",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let id = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let established = {
                let ctx = established_context.borrow();
                ctx.achievement_has_been_established(&id)
            };
            established_context.borrow_mut().log_event(format!(
                "achievement.check_established {id} -> {established}"
            ));
            Ok(established)
        })?,
    )?;

    let query_context = context.clone();
    table.set(
        "isEligible",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let id = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let eligible = {
                let ctx = query_context.borrow();
                ctx.achievement_is_eligible(&id)
            };
            query_context
                .borrow_mut()
                .log_event(format!("achievement.query {id} -> {eligible}"));
            Ok(eligible)
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("achievement.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));

    globals.set("achievement", table)?;

    match globals.get::<_, Value>("ACHIEVE_CLASSIC_DRIVER") {
        Ok(Value::Nil) | Err(_) => {
            globals.set("ACHIEVE_CLASSIC_DRIVER", "ACHIEVE_CLASSIC_DRIVER")?;
        }
        _ => {}
    }

    Ok(())
}

fn install_mouse_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("mouse"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let mouse = lua.create_table()?;

    let mode_context = context.clone();
    mouse.set(
        "set_mode",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mode = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<none>".to_string());
            mode_context
                .borrow_mut()
                .log_event(format!("mouse.set_mode {mode}"));
            Ok(())
        })?,
    )?;

    let show_context = context.clone();
    mouse.set(
        "show",
        lua.create_function(move |_, _: Variadic<Value>| {
            show_context.borrow_mut().log_event("mouse.show");
            Ok(())
        })?,
    )?;

    let hide_context = context.clone();
    mouse.set(
        "hide",
        lua.create_function(move |_, _: Variadic<Value>| {
            hide_context.borrow_mut().log_event("mouse.hide");
            Ok(())
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("mouse.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    mouse.set_metatable(Some(metatable));

    globals.set("mouse", mouse)?;
    Ok(())
}

fn install_ui_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("UI"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let ui = lua.create_table()?;
    ui.set("screens", lua.create_table()?)?;

    let screen_ctor_context = context.clone();
    ui.set(
        "create_screen",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let name = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "anonymous".to_string());
            screen_ctor_context
                .borrow_mut()
                .log_event(format!("ui.screen.create {name}"));
            let table = lua_ctx.create_table()?;
            let fallback_context = screen_ctor_context.clone();
            let fallback =
                lua_ctx.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
                    if let Value::String(method) = key {
                        fallback_context
                            .borrow_mut()
                            .log_event(format!("ui.screen.stub {}", method.to_str()?));
                    }
                    let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
                    Ok(Value::Function(noop))
                })?;
            let metatable = lua_ctx.create_table()?;
            metatable.set("__index", fallback)?;
            table.set_metatable(Some(metatable));
            Ok(table)
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("ui.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    ui.set_metatable(Some(metatable));

    globals.set("UI", ui)?;

    let rebuild_context = context.clone();
    globals.set(
        "rebuildButtons",
        lua.create_function(move |_, _: mlua::Variadic<mlua::Value>| {
            rebuild_context
                .borrow_mut()
                .log_event("ui.rebuildButtons".to_string());
            Ok(())
        })?,
    )?;

    let update_buttons_context = context;
    globals.set(
        "UpdateUIButtons",
        lua.create_function(move |_, _: mlua::Variadic<mlua::Value>| {
            update_buttons_context
                .borrow_mut()
                .log_event("ui.update_buttons".to_string());
            Ok(())
        })?,
    )?;

    Ok(())
}

fn install_inventory_variant_stub(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    base: &str,
) -> Result<()> {
    let globals = lua.globals();
    let room_id = base
        .split(&['\\', '/'][..])
        .last()
        .unwrap_or(base)
        .to_string();
    context.borrow_mut().register_inventory_room(&room_id);

    // expose a stub table under the global named after the script (e.g., mn_inv)
    let global_name = room_id.replace('.', "_");

    if !matches!(
        globals.get::<_, Value>(global_name.as_str()),
        Ok(Value::Table(_))
    ) {
        let table = lua.create_table()?;
        let fallback_context = context.clone();
        let fallback_name = global_name.clone();
        let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
            if let Value::String(method) = key {
                fallback_context.borrow_mut().log_event(format!(
                    "inventory.variant.stub {}.{}",
                    fallback_name,
                    method.to_str()?
                ));
            }
            let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
            Ok(Value::Function(noop))
        })?;
        let metatable = lua.create_table()?;
        metatable.set("__index", fallback)?;
        table.set_metatable(Some(metatable));
        globals.set(global_name, table)?;
    }

    Ok(())
}

fn install_manny_scythe_stub(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("mn_scythe"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("mn_scythe.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));
    globals.set("mn_scythe", table)?;
    Ok(())
}

pub(crate) fn install_render_helpers(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let render_context = context.clone();
    globals.set(
        "SetGameRenderMode",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let description = values
                .get(0)
                .map(describe_value)
                .unwrap_or_else(|| "<nil>".to_string());
            render_context
                .borrow_mut()
                .log_event(format!("render.mode {description}"));
            Ok(())
        })?,
    )?;

    let display_context = context;
    globals.set(
        "EngineDisplay",
        lua.create_function(move |_, args: Variadic<Value>| {
            let description = args
                .iter()
                .map(describe_value)
                .collect::<Vec<_>>()
                .join(", ");
            display_context
                .borrow_mut()
                .log_event(format!("render.display [{}]", description));
            Ok(())
        })?,
    )?;
    Ok(())
}

pub(crate) fn dump_runtime_summary(state: &EngineContext) {
    println!("Lua runtime summary:");
    let sets_snapshot = state.set_view().snapshot();
    match &sets_snapshot.current_set {
        Some(set) => {
            let display = set.display_name.as_deref().unwrap_or(&set.variable_name);
            println!("  Current set: {} ({})", set.set_file, display);
        }
        None => println!("  Current set: <none>"),
    }
    println!(
        "  Selected actor: {}",
        state
            .actors
            .selected_actor_id()
            .map(|id| id.as_str())
            .unwrap_or("<none>")
    );
    if let Some(effect) = &state.voice_effect {
        println!("  Voice effect: {}", effect);
    }
    let music = state.audio_view().music().clone();
    if let Some(current) = &music.current {
        if current.parameters.is_empty() {
            println!("  Music playing: {}", current.name);
        } else {
            println!(
                "  Music playing: {} [{}]",
                current.name,
                current.parameters.join(", ")
            );
        }
    } else {
        println!("  Music playing: <none>");
    }
    if !music.queued.is_empty() {
        let queued: Vec<_> = music
            .queued
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        println!("  Music queued: {}", queued.join(", "));
    }
    if music.paused {
        println!("  Music paused");
    }
    if state.pause_view().active() {
        println!("  Game paused");
    }
    if let Some(state_name) = &music.current_state {
        println!("  Music state: {}", state_name);
    }
    if !music.state_stack.is_empty() {
        println!("  Music state stack: {}", music.state_stack.join(" -> "));
    }
    if !music.muted_groups.is_empty() {
        let groups: Vec<_> = music
            .muted_groups
            .iter()
            .map(|group| group.as_str())
            .collect();
        println!("  Music muted groups: {}", groups.join(", "));
    }
    if let Some(volume) = music.volume {
        println!("  Music volume: {:.3}", volume);
    }
    let sfx = state.audio_view().sfx().clone();
    if !sfx.active.is_empty() {
        println!("  Active SFX:");
        for instance in sfx.active.values() {
            if instance.parameters.is_empty() {
                println!("    - {} ({})", instance.cue, instance.handle);
            } else {
                println!(
                    "    - {} ({}) [{}]",
                    instance.cue,
                    instance.handle,
                    instance.parameters.join(", ")
                );
            }
        }
    }
    if let Some(manny) = state.actors.get("manny") {
        if let Some(set) = &manny.current_set {
            println!("  Manny in set: {set}");
        }
        if let Some(costume) = &manny.costume {
            println!("  Manny costume: {costume}");
        }
        if let Some(pos) = manny.position {
            println!(
                "  Manny position: ({:.3}, {:.3}, {:.3})",
                pos.x, pos.y, pos.z
            );
        }
        if let Some(rot) = manny.rotation {
            println!(
                "  Manny rotation: ({:.3}, {:.3}, {:.3})",
                rot.x, rot.y, rot.z
            );
        }
        if !manny.sectors.is_empty() {
            for (kind, hit) in &manny.sectors {
                println!("  Manny sector {kind}: {} (id {})", hit.name, hit.id);
            }
        }
    }
    if let Some(commentary) = state.cutscenes.commentary() {
        let status = if commentary.active {
            "active".to_string()
        } else {
            commentary
                .suppressed_reason
                .as_deref()
                .unwrap_or("suppressed")
                .to_string()
        };
        println!("  Commentary: {} ({})", commentary.display_label(), status);
    }
    let cut_scenes = state.cutscenes.cut_scene_stack();
    if !cut_scenes.is_empty() {
        println!("  Cut scenes:");
        for record in cut_scenes {
            let status = if record.suppressed {
                "blocked"
            } else {
                "active"
            };
            let label = if record.flags.is_empty() {
                record.display_label().to_string()
            } else {
                format!("{} ({})", record.display_label(), record.flags.join(", "))
            };
            match (&record.set_file, &record.sector) {
                (Some(set), Some(sector)) => {
                    println!("    {} [{}] {}:{}", label, status, set, sector)
                }
                (Some(set), None) => println!("    {} [{}] {}", label, status, set),
                (None, Some(sector)) => println!("    {} [{}] sector={}", label, status, sector),
                (None, None) => println!("    {} [{}]", label, status),
            }
        }
    }
    if !state.inventory.items().is_empty() {
        let mut items: Vec<_> = state.inventory.items().iter().collect();
        items.sort();
        let display = items
            .iter()
            .map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory: {}", display);
    }
    if !state.inventory.rooms().is_empty() {
        let mut rooms: Vec<_> = state.inventory.rooms().iter().collect();
        rooms.sort();
        let display = rooms
            .iter()
            .map(|room| room.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory rooms: {}", display);
    }
    if let Some(current) = &sets_snapshot.current_set {
        if let Some(states) = sets_snapshot.sector_states.get(&current.set_file) {
            if let Some(geometry) = sets_snapshot.set_geometry.get(&current.set_file) {
                let mut overrides: Vec<(String, bool)> = Vec::new();
                for sector in &geometry.sectors {
                    if let Some(active) = states.get(&sector.name) {
                        if *active != sector.default_active {
                            overrides.push((sector.name.clone(), *active));
                        }
                    }
                }
                if !overrides.is_empty() {
                    overrides.sort_by(|a, b| a.0.cmp(&b.0));
                    println!("  Sector overrides:");
                    for (name, active) in overrides {
                        println!(
                            "    - {}: {}",
                            name,
                            if active { "active" } else { "inactive" }
                        );
                    }
                }
            }
        }
    }
    if !state.objects.visible_objects().is_empty() {
        println!("  Visible objects:");
        for info in state.objects.visible_objects() {
            let mut details: Vec<String> = Vec::new();
            if let Some(distance) = info.distance {
                details.push(format!("dist={distance:.3}"));
            }
            if let Some(angle) = info.angle {
                details.push(format!("angle={angle:.2}°"));
            }
            if let Some(within) = info.within_range {
                if within {
                    details.push("in-range".to_string());
                } else {
                    details.push("out-of-range".to_string());
                }
                if info.range > 0.0 {
                    details.push(format!("range={:.3}", info.range));
                }
            } else if info.range > 0.0 {
                details.push(format!("range={:.3}", info.range));
            }
            if info.in_hotlist {
                details.push("HOT".to_string());
            }
            let suffix = if details.is_empty() {
                String::new()
            } else {
                format!(" [{}]", details.join(", "))
            };
            println!("    - {} (#{}{})", info.display_name(), info.handle, suffix);
        }
    }
    let menus = state.menu_view();
    if !menus.is_empty() {
        println!("  Menus:");
        for (name, menu_state) in menus.iter() {
            let snapshot = menu_state.borrow();
            let visibility = if snapshot.visible {
                "visible"
            } else {
                "hidden"
            };
            let mut details = Vec::new();
            if snapshot.auto_freeze {
                details.push("autoFreeze".to_string());
            }
            if let Some(mode) = &snapshot.last_run_mode {
                details.push(format!("run={mode}"));
            }
            if let Some(action) = &snapshot.last_action {
                details.push(format!("last={action}"));
            }
            let extra = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            println!("    - {}: {}{}", name, visibility, extra);
        }
    }
    if !state.scripts.is_empty() {
        println!("  Pending scripts:");
        for (handle, record) in state.scripts.iter() {
            println!(
                "    - {} (#{handle}) yields={}",
                record.label(),
                record.yields()
            );
        }
    }
    if !state.events.is_empty() {
        println!("  Event log:");
        for event in &state.events {
            println!("    - {event}");
        }
    }
}

fn create_start_script(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<Function<'_>> {
    let start_state = context.clone();
    let func = lua.create_function(move |lua_ctx, mut args: Variadic<Value>| {
        if args.is_empty() {
            return Ok(0u32);
        }
        let callable = args.remove(0);
        let label = describe_callable_label(&callable)?;
        let function = extract_function(lua_ctx, callable)?;
        let callable_key = if let Some(func) = function.as_ref() {
            Some(lua_ctx.create_registry_value(func.clone())?)
        } else {
            None
        };
        let handle = {
            let mut state = start_state.borrow_mut();
            state.start_script(label.clone(), callable_key)
        };
        if let Some(func) = function {
            let thread = lua_ctx.create_thread(func.clone())?;
            let thread_key = lua_ctx.create_registry_value(thread.clone())?;
            {
                let mut state = start_state.borrow_mut();
                state.attach_script_thread(handle, thread_key);
            }
            let params: Vec<Value> = args.into_iter().collect();
            let initial_args = MultiValue::from_vec(params);
            resume_script(
                lua_ctx,
                start_state.clone(),
                handle,
                Some(thread),
                Some(initial_args),
            )?;
        } else {
            let cleanup = start_state.borrow_mut().complete_script(handle);
            if let Some(key) = cleanup.thread {
                lua_ctx.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua_ctx.remove_registry_value(key)?;
            }
        }
        Ok(handle)
    })?;
    Ok(func)
}

fn create_single_start_script(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<Function<'_>> {
    let single_state = context.clone();
    let func = lua.create_function(move |lua_ctx, mut args: Variadic<Value>| {
        if args.is_empty() {
            return Ok(0u32);
        }
        let callable = args.remove(0);
        let label = describe_callable_label(&callable)?;
        if single_state.borrow().has_script_with_label(&label) {
            return Ok(0u32);
        }
        let function = extract_function(lua_ctx, callable)?;
        let callable_key = if let Some(func) = function.as_ref() {
            Some(lua_ctx.create_registry_value(func.clone())?)
        } else {
            None
        };
        let handle = {
            let mut state = single_state.borrow_mut();
            state.start_script(label.clone(), callable_key)
        };
        if let Some(func) = function {
            let thread = lua_ctx.create_thread(func.clone())?;
            let thread_key = lua_ctx.create_registry_value(thread.clone())?;
            {
                let mut state = single_state.borrow_mut();
                state.attach_script_thread(handle, thread_key);
            }
            let params: Vec<Value> = args.into_iter().collect();
            let initial_args = MultiValue::from_vec(params);
            resume_script(
                lua_ctx,
                single_state.clone(),
                handle,
                Some(thread),
                Some(initial_args),
            )?;
        } else {
            let cleanup = single_state.borrow_mut().complete_script(handle);
            if let Some(key) = cleanup.thread {
                lua_ctx.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua_ctx.remove_registry_value(key)?;
            }
        }
        Ok(handle)
    })?;
    Ok(func)
}

enum ScriptStep {
    Yielded,
    Completed,
}

pub(crate) fn drive_active_scripts(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    max_passes: usize,
    max_yields_per_script: u32,
) -> LuaResult<()> {
    for _ in 0..max_passes {
        let handles = {
            let state = context.borrow();
            state.active_script_handles()
        };
        if handles.is_empty() {
            break;
        }
        let mut progressed = false;
        for handle in handles {
            let yield_count = {
                let state = context.borrow();
                state.script_yield_count(handle).unwrap_or(0)
            };
            if yield_count >= max_yields_per_script {
                continue;
            }
            match resume_script(lua, context.clone(), handle, None, None)? {
                ScriptStep::Yielded | ScriptStep::Completed => {
                    progressed = true;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    Ok(())
}

fn resume_script(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    handle: u32,
    thread_override: Option<Thread>,
    initial_args: Option<MultiValue>,
) -> LuaResult<ScriptStep> {
    let thread = if let Some(thread) = thread_override {
        thread
    } else {
        let thread = {
            let state = context.borrow();
            let maybe_thread = state.with_script_thread_key(handle, |maybe_key| {
                maybe_key
                    .map(|key| lua.registry_value::<Thread>(key))
                    .transpose()
            })?;
            if let Some(thread) = maybe_thread {
                thread
            } else {
                return Ok(ScriptStep::Completed);
            }
        };
        thread
    };

    if !matches!(thread.status(), ThreadStatus::Resumable) {
        let cleanup = {
            let mut state = context.borrow_mut();
            state.complete_script(handle)
        };
        if let Some(key) = cleanup.thread {
            lua.remove_registry_value(key)?;
        }
        if let Some(key) = cleanup.callable {
            lua.remove_registry_value(key)?;
        }
        return Ok(ScriptStep::Completed);
    }

    let resume_result = if let Some(args) = initial_args {
        thread.resume::<_, MultiValue>(args)
    } else {
        thread.resume::<_, MultiValue>(MultiValue::new())
    };

    match resume_result {
        Ok(_) => match thread.status() {
            ThreadStatus::Resumable => {
                context.borrow_mut().increment_script_yield(handle);
                Ok(ScriptStep::Yielded)
            }
            ThreadStatus::Unresumable | ThreadStatus::Error => {
                let cleanup = {
                    let mut state = context.borrow_mut();
                    state.complete_script(handle)
                };
                if let Some(key) = cleanup.thread {
                    lua.remove_registry_value(key)?;
                }
                if let Some(key) = cleanup.callable {
                    lua.remove_registry_value(key)?;
                }
                Ok(ScriptStep::Completed)
            }
        },
        Err(LuaError::CoroutineInactive) => {
            let cleanup = {
                let mut state = context.borrow_mut();
                state.complete_script(handle)
            };
            if let Some(key) = cleanup.thread {
                lua.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua.remove_registry_value(key)?;
            }
            Ok(ScriptStep::Completed)
        }
        Err(err) => {
            let label = {
                let state = context.borrow();
                state
                    .script_label(handle)
                    .unwrap_or_else(|| format!("#{handle}"))
            };
            let message = err.to_string();
            context
                .borrow_mut()
                .log_event(format!("script.error {label}: {message}"));
            let cleanup = {
                let mut state = context.borrow_mut();
                state.complete_script(handle)
            };
            if let Some(key) = cleanup.thread {
                lua.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua.remove_registry_value(key)?;
            }
            Err(err)
        }
    }
}

fn wait_for_handle(lua: &Lua, context: Rc<RefCell<EngineContext>>, handle: u32) -> LuaResult<()> {
    const MAX_STEPS: u32 = 10_000;
    let mut steps = 0;
    while context.borrow().is_script_running(handle) {
        resume_script(lua, context.clone(), handle, None, None)?;
        steps += 1;
        if steps >= MAX_STEPS {
            let label = {
                let state = context.borrow();
                state
                    .script_label(handle)
                    .unwrap_or_else(|| format!("#{handle}"))
            };
            return Err(LuaError::external(format!(
                "wait_for_script exceeded {MAX_STEPS} resumes for {label}"
            )));
        }
    }
    Ok(())
}
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{anyhow, Context, Result};
use grim_analysis::resources::normalize_legacy_lua;
use mlua::{
    Error as LuaError, Function, Lua, MultiValue, RegistryKey, Result as LuaResult, Table, Thread,
    ThreadStatus, Value, Variadic,
};

use super::super::types::Vec3;
use super::audio::{install_music_scaffold, FOOTSTEP_PROFILES, IM_SOUND_PLAY_COUNT, IM_SOUND_VOL};
use super::menus::{
    install_boot_warning_menu, install_dialog_scaffold, install_loading_menu, install_menu_common,
    install_menu_dialog, install_menu_infrastructure, install_menu_prefs, install_menu_remap,
};
use super::objects::ObjectSnapshot;
use super::{heading_between, EngineContext, SectorToggleResult};
