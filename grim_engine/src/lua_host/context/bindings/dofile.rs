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
    let path = Path::new(file);
    let has_extension = path.extension().is_some();

    variants.push(path.to_path_buf());

    if has_extension {
        if let (Some(stem), Some(ext)) = (path.file_stem(), path.extension()) {
            let stem = stem.to_string_lossy();
            if !stem.ends_with(".decompiled") {
                let mut decompiled = path.to_path_buf();
                decompiled.set_file_name(format!(
                    "{stem}.decompiled.{}",
                    ext.to_string_lossy()
                ));
                variants.push(decompiled);
            }
        }
    } else {
        variants.push(PathBuf::from(format!("{file}.lua")));
        variants.push(PathBuf::from(format!("{file}.decompiled.lua")));
    }
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

#[cfg(test)]
mod tests {
    use super::candidate_paths;

    fn as_strings(paths: &[std::path::PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn includes_decompiled_variant_when_extension_present() {
        let candidates = candidate_paths("setfallback.lua");
        let paths = as_strings(&candidates);
        assert!(paths.iter().any(|p| p.ends_with("setfallback.lua")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with("setfallback.decompiled.lua")));
    }

    #[test]
    fn includes_default_variants_when_no_extension() {
        let candidates = candidate_paths("Scripts/_colors");
        let paths = as_strings(&candidates);
        assert!(paths.iter().any(|p| p.ends_with("Scripts/_colors")));
        assert!(paths.iter().any(|p| p.ends_with("Scripts/_colors.lua")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with("Scripts/_colors.decompiled.lua")));
    }
}
