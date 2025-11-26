use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use grim_analysis::resources::normalize_legacy_lua;
use mlua::{Error as LuaError, Lua, MultiValue, Result as LuaResult, Value};

use crate::lua_host::context::EngineContext;

const SPECIAL_FILES: &[&str] = &[
    "setfallback.lua",
    "_colors.lua",
    "_colors.decompiled.lua",
    "_sfx.lua",
    "_sfx.decompiled.lua",
    "_controls.lua",
    "_controls.decompiled.lua",
    "_dialog.lua",
    "_dialog.decompiled.lua",
    "_music.lua",
    "_music.decompiled.lua",
    "_mouse.lua",
    "_mouse.decompiled.lua",
    "_ui.lua",
    "_ui.decompiled.lua",
    "_achievement.lua",
    "_achievement.decompiled.lua",
    "_actors.lua",
    "_actors.decompiled.lua",
    "_objects.lua",
    "_objects.decompiled.lua",
    "_sets.lua",
    "_sets.decompiled.lua",
    "_inventory.lua",
    "_inventory.decompiled.lua",
    "_cut_scenes.lua",
    "_cut_scenes.decompiled.lua",
    "menu_loading.lua",
    "menu_loading.decompiled.lua",
    "menu_boot_warning.lua",
    "menu_boot_warning.decompiled.lua",
    "menu_dialog.lua",
    "menu_dialog.decompiled.lua",
    "menu_common.lua",
    "menu_common.decompiled.lua",
    "menu_remap_keys.lua",
    "menu_remap_keys.decompiled.lua",
    "menu_prefs.lua",
    "menu_prefs.decompiled.lua",
    "mn_scythe.lua",
    "mn_scythe.decompiled.lua",
];

const SPECIAL_PREFIXES: &[&str] = &["achievementdefinitions_"];
const SPECIAL_SUFFIXES: &[&str] = &["_inv.lua", "_inv.decompiled.lua"];

pub(super) fn handle_special_dofile<'lua>(
    _lua: &'lua Lua,
    path: &str,
    _context: Rc<RefCell<EngineContext>>,
) -> LuaResult<Option<Value<'lua>>> {
    if let Some(filename) = Path::new(path).file_name().and_then(|name| name.to_str()) {
        let lower = filename.to_ascii_lowercase();
        if SPECIAL_FILES.contains(&lower.as_str())
            || SPECIAL_PREFIXES
                .iter()
                .any(|prefix| lower.starts_with(prefix))
            || SPECIAL_SUFFIXES
                .iter()
                .any(|suffix| lower.ends_with(suffix))
        {
            return Ok(Some(Value::Boolean(true)));
        }
    }

    Ok(None)
}

pub(super) fn add_variants(file: &str, variants: &mut Vec<PathBuf>) {
    if file.contains('.') {
        variants.push(PathBuf::from(file));
        return;
    }
    variants.push(PathBuf::from(file));
    variants.push(PathBuf::from(format!("{file}.lua")));
    variants.push(PathBuf::from(format!("{file}.decompiled.lua")));
}

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

pub(super) fn execute_script<'lua>(lua: &'lua Lua, path: &Path) -> LuaResult<Option<Value<'lua>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(mlua::Error::external)?;
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
