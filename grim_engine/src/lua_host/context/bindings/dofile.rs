//! `dofile` wrapper and fallbacks that mirror retail lookup and stub missing scripts.
//!
//! The intro boot path expects a handful of retail Lua files to exist. When
//! those are absent in a local dev install, we provide lightweight stubs so the
//! minimal host can advance while still logging parity-friendly telemetry. File
//! resolution also mirrors retail: compiled first, then decompiled, with
//! basename fallbacks and legacy Lua normalization for decompiled chunks.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::lua_host::legacy_lua::normalize_legacy_lua;
use mlua::{Error as LuaError, Lua, MultiValue, Result as LuaResult, Value};

use crate::lua_host::context::EngineContext;

/// Normalize a path to a lowercase basename for matching stubbed modules.
fn normalized_name(path: &str) -> String {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

fn stub_module<'lua>(
    lua: &'lua Lua,
    chunk_name: &str,
    script: &str,
) -> LuaResult<Option<Value<'lua>>> {
    let value = lua.load(script).set_name(chunk_name).eval::<Value>()?;
    Ok(Some(value))
}

fn stub_checkfirst<'lua>(
    lua: &'lua Lua,
    chunk_name: &str,
    file_id: &str,
) -> LuaResult<Option<Value<'lua>>> {
    let script = format!(r#"CheckFirstTime("{file_id}") return true"#);
    stub_module(lua, chunk_name, &script)
}

fn stub_actors<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_actors.lua(stub)",
        r#"
CheckFirstTime("_actors.lua")
manny = manny or {}
manny.hActor = manny.hActor or 1
function manny:set_selected()
    self.is_selected = true
end
function manny:default(costume)
    self.costume = costume
end
function manny:put_in_set(set)
    system.currentSet = set or system.currentSet
end
function manny:put_at_interest()
end
function manny:getpos()
    return { x = 0, y = 0, z = 0 }
end
function manny:getrot()
    return { x = 0, y = 0, z = 0 }
end
function manny:walkto(x, y, z, rx, ry, rz)
    self.last_walk = { x = x, y = y, z = z, rx = rx, ry = ry, rz = rz }
    return true
end
function manny:wait_for_actor()
    return true
end
mo = mo or {}
mo.scythe = mo.scythe or { shrink_radius = 1 }
function mo.scythe:get()
    manny.is_holding = self
    return self
end
TrackManny = TrackManny or function() end
WalkManny = WalkManny or function() end
look_up_correct_costume = look_up_correct_costume or function(_) return nil end
IN_LIMBO = IN_LIMBO or {}
system.currentActor = manny
return true
"#,
    )
}

fn stub_controls<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_controls.lua(stub)",
        r#"
CheckFirstTime("_controls.lua")
BKEY = BKEY or 1
LKEY = LKEY or 2
AKEY = AKEY or 3
MKEY = MKEY or 4
return true
"#,
    )
}

fn stub_objects<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_objects.lua(stub)",
        r#"
CheckFirstTime("_objects.lua")
IN_LIMBO = IN_LIMBO or {}
return true
"#,
    )
}

fn stub_achievement<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@_achievement.lua(stub)", "_achievement.lua")
}

fn stub_dialog<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@_dialog.lua(stub)", "_dialog.lua")
}

fn stub_sets<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_sets.lua(stub)",
        r#"
CheckFirstTime("_sets.lua")
system.setTable = system.setTable or {}
local set = system.setTable["mo.set"]
if not set then
    set = {
        setFile = "mo.set",
        shrinkable = false,
        boxes_shrunk = false,
    }
    system.setTable["mo.set"] = set
end
function set:switch_to_set()
    system.currentSet = self
    return self
end
function set:short_name()
    return "mo"
end
function set:CommonCameraChange(prevSetup, nextSetup)
end
function set:CommonPostCameraChange(newSetup)
end
system.currentSet = system.currentSet or set
return set
"#,
    )
}

fn stub_inventory<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_inventory.lua(stub)",
        r#"
CheckFirstTime("_inventory.lua")
inv_sets = inv_sets or {}
return true
"#,
    )
}

fn stub_music<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@_music.lua(stub)", "_music.lua")
}

fn stub_mouse<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@_mouse.lua(stub)", "_mouse.lua")
}

fn stub_ui<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_ui.lua(stub)",
        r#"
CheckFirstTime("_ui.lua")
concept_menu = concept_menu or {}
function concept_menu:unlock_concepts(count)
    self.unlocked = count
end
return concept_menu
"#,
    )
}

fn stub_cut_scenes<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_cut_scenes.lua(stub)",
        r#"
CheckFirstTime("_cut_scenes.lua")
cut_scene = cut_scene or {}
cut_scene.logos = cut_scene.logos or function() return true end
return cut_scene
"#,
    )
}

fn stub_menu_common<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@menu_common.lua(stub)", "menu_common.lua")
}

fn stub_menu_dialog<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@menu_dialog.lua(stub)", "menu_dialog.lua")
}

fn stub_menu_prefs<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@menu_prefs.lua(stub)", "menu_prefs.lua")
}

fn stub_menu_loading<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@menu_loading.lua(stub)",
        r#"
CheckFirstTime("menu_loading.lua")
loading_menu = loading_menu or { is_visible = false }
function loading_menu:run(mode)
    self.is_visible = true
    return self
end
return loading_menu
"#,
    )
}

fn stub_menu_boot_warning<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@menu_boot_warning.lua(stub)",
        r#"
CheckFirstTime("menu_boot_warning.lua")
boot_warning_menu = boot_warning_menu or { is_visible = false }
function boot_warning_menu:run(mode)
    self.is_visible = true
    return self
end
boot_warning_menu.check_timeout = boot_warning_menu.check_timeout or function() return true end
return boot_warning_menu
"#,
    )
}

fn stub_menu_remap_keys<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@menu_remap_keys.lua(stub)", "menu_remap_keys.lua")
}

fn stub_local<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@_local.lua(stub)", "_local.lua")
}

fn stub_sfx<'lua>(lua: &'lua Lua) -> LuaResult<Option<Value<'lua>>> {
    stub_module(
        lua,
        "@_sfx.lua(stub)",
        r#"
CheckFirstTime("_sfx.lua")
StopMovie = StopMovie or function() end
return true
"#,
    )
}

fn stub_achievement_definitions<'lua>(
    lua: &'lua Lua,
    file_name: &str,
) -> LuaResult<Option<Value<'lua>>> {
    stub_checkfirst(lua, "@achievement_definitions.lua(stub)", file_name)
}

/// Handle well-known boot-time modules with in-process stubs so the intro can continue without assets.
pub(super) fn handle_special_dofile<'lua>(
    lua: &'lua Lua,
    path: &str,
    _context: Rc<RefCell<EngineContext>>,
) -> LuaResult<Option<Value<'lua>>> {
    let name = normalized_name(path);
    match name.as_str() {
        "_actors.lua" | "_actors.decompiled.lua" => stub_actors(lua),
        "_controls.lua" | "_controls.decompiled.lua" => stub_controls(lua),
        "_objects.lua" | "_objects.decompiled.lua" => stub_objects(lua),
        "_achievement.lua" | "_achievement.decompiled.lua" => stub_achievement(lua),
        "_dialog.lua" | "_dialog.decompiled.lua" => stub_dialog(lua),
        "_sets.lua" | "_sets.decompiled.lua" => stub_sets(lua),
        "_inventory.lua" | "_inventory.decompiled.lua" => stub_inventory(lua),
        "_music.lua" | "_music.decompiled.lua" => stub_music(lua),
        "_mouse.lua" | "_mouse.decompiled.lua" => stub_mouse(lua),
        "_ui.lua" | "_ui.decompiled.lua" => stub_ui(lua),
        "_cut_scenes.lua" | "_cut_scenes.decompiled.lua" => stub_cut_scenes(lua),
        "menu_common.lua" | "menu_common.decompiled.lua" => stub_menu_common(lua),
        "menu_dialog.lua" | "menu_dialog.decompiled.lua" => stub_menu_dialog(lua),
        "menu_prefs.lua" | "menu_prefs.decompiled.lua" => stub_menu_prefs(lua),
        "menu_loading.lua" | "menu_loading.decompiled.lua" => stub_menu_loading(lua),
        "menu_boot_warning.lua" | "menu_boot_warning.decompiled.lua" => stub_menu_boot_warning(lua),
        "menu_remap_keys.lua" | "menu_remap_keys.decompiled.lua" => stub_menu_remap_keys(lua),
        "_local.lua" | "_local.decompiled.lua" => stub_local(lua),
        "_sfx.lua" | "_sfx.decompiled.lua" => stub_sfx(lua),
        other if other.starts_with("achievementdefinitions_") && other.ends_with(".lua") => {
            stub_achievement_definitions(lua, &name)
        }
        _ => Ok(None),
    }
}

/// Expand a requested file into the set of compiled/decompiled variants retail would try.
pub(super) fn add_variants(file: &str, variants: &mut Vec<PathBuf>) {
    let mut push_unique = |path: PathBuf| {
        if !variants.contains(&path) {
            variants.push(path);
        }
    };

    // Always try the provided path first.
    push_unique(PathBuf::from(file));

    // If the caller already specified a .decompiled.lua file, also allow the
    // compiled variant to match retail's preference for the compiled chunk.
    if let Some(stripped) = file.strip_suffix(".decompiled.lua") {
        push_unique(PathBuf::from(format!("{stripped}.lua")));
        return;
    }

    // If the caller asked for a .lua file, fall back to the .decompiled.lua
    // source when the compiled chunk is missing.
    if let Some(stripped) = file.strip_suffix(".lua") {
        push_unique(PathBuf::from(format!("{stripped}.decompiled.lua")));
        return;
    }

    // For extension-less paths, mirror retail lookup order: plain name,
    // compiled .lua, then decompiled source.
    if !file.contains('.') {
        push_unique(PathBuf::from(format!("{file}.lua")));
        push_unique(PathBuf::from(format!("{file}.decompiled.lua")));
        return;
    }

    // For other dotted filenames, still try a decompiled suffix fallback.
    push_unique(PathBuf::from(format!("{file}.decompiled.lua")));
}

/// Compute candidate file paths mirroring retail `dofile` search order.
pub(super) fn candidate_paths(path: &str) -> Vec<PathBuf> {
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

/// Load and execute a Lua chunk, handling compiled/decompiled detection and returning the first result.
pub(super) fn execute_script<'lua>(lua: &'lua Lua, path: &Path) -> LuaResult<Option<Value<'lua>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(mlua::Error::external)?;
    let chunk_name = format!("@{}", path.display());
    let eval_result = if path.to_string_lossy().ends_with(".decompiled.lua") {
        let source = String::from_utf8_lossy(&bytes);
        let script = normalize_legacy_lua(&source);
        lua.load(&script).set_name(&chunk_name).eval::<MultiValue>()
    } else if is_precompiled_chunk(&bytes) {
        lua.load(&bytes).set_name(&chunk_name).eval::<MultiValue>()
    } else {
        let source = String::from_utf8_lossy(&bytes).into_owned();
        lua.load(&source).set_name(&chunk_name).eval::<MultiValue>()
    };

    match eval_result {
        Ok(results) => {
            let first = results.into_iter().next().unwrap_or(Value::Nil);
            let value = if matches!(first, Value::Nil) {
                Value::Boolean(true)
            } else {
                first
            };
            Ok(Some(value))
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
