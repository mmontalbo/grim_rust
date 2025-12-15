use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::lua_host::legacy_lua::normalize_legacy_lua;
use mlua::{Error as LuaError, Lua, MultiValue, Result as LuaResult, Value};

use crate::lua_host::context::EngineContext;

pub(super) fn handle_special_dofile<'lua>(
    _lua: &'lua Lua,
    _path: &str,
    _context: Rc<RefCell<EngineContext>>,
) -> LuaResult<Option<Value<'lua>>> {
    Ok(None)
}

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
