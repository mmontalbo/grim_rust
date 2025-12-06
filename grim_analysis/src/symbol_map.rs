//! Simple symbol-map lookup used to enrich telemetry with module-relative names.
//!
//! Maps are loaded from paths specified by `GRIM_SHIM_SYMBOL_MAP` and
//! `GRIM_SHIM_SYMBOL_MAP_LUALIB` (with optional module filters), then used to
//! resolve addresses into function names when native symbols are missing.
use crate::logging::log_line;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    sync::OnceLock,
};

#[derive(Clone)]
pub(crate) struct SymbolMapHit {
    pub(crate) name: String,
    pub(crate) distance: usize,
}

struct SymbolEntry {
    addr: usize,
    name: String,
}

struct SymbolMaps {
    maps: Vec<LoadedSymbolMap>,
}

struct LoadedSymbolMap {
    entries: Vec<SymbolEntry>,
    module_filter: Option<String>,
}

/// Resolves an address to the closest symbol using any configured symbol maps.
pub(crate) fn lookup_symbol_from_map(
    addr: usize,
    module_path: Option<&str>,
    module_base: Option<usize>,
) -> Option<SymbolMapHit> {
    let maps = symbol_maps()?;
    maps.lookup(addr, module_path, module_base)
}

/// Lazily loads and caches symbol maps; returns None when no maps are configured.
fn symbol_maps() -> Option<&'static SymbolMaps> {
    static MAPS: OnceLock<Option<SymbolMaps>> = OnceLock::new();
    MAPS.get_or_init(load_symbol_maps).as_ref()
}

/// Reads symbol maps specified via environment variables and returns them if any parsed successfully.
fn load_symbol_maps() -> Option<SymbolMaps> {
    let mut maps = Vec::new();
    if let Some(map) = load_symbol_map("GRIM_SHIM_SYMBOL_MAP", "GRIM_SHIM_SYMBOL_MAP_MODULE") {
        maps.push(map);
    }
    if let Some(map) = load_symbol_map(
        "GRIM_SHIM_SYMBOL_MAP_LUALIB",
        "GRIM_SHIM_SYMBOL_MAP_LUALIB_MODULE",
    ) {
        maps.push(map);
    }
    if maps.is_empty() {
        None
    } else {
        Some(SymbolMaps { maps })
    }
}

/// Attempts to load a single symbol map based on env vars for path and module filter.
fn load_symbol_map(path_var: &str, module_var: &str) -> Option<LoadedSymbolMap> {
    let path = env::var(path_var).ok()?;
    let module_filter = env::var(module_var).ok().filter(|value| !value.is_empty());
    let path_ref = Path::new(&path);
    load_symbol_map_at_path(path_ref, module_filter)
}

/// Opens a symbol map file, logs failures, and collects parsed entries.
fn load_symbol_map_at_path(
    path_ref: &Path,
    module_filter: Option<String>,
) -> Option<LoadedSymbolMap> {
    let path_display = path_ref.display().to_string();
    let file = match File::open(path_ref) {
        Ok(file) => file,
        Err(err) => {
            log_line(&format!(
                "failed to open symbol map at {}: {err}",
                path_display
            ));
            return None;
        }
    };

    let mut entries = Vec::new();
    let reader = BufReader::new(file);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if let Some(entry) = parse_symbol_line(&line) {
                    entries.push(entry);
                }
            }
            Err(err) => {
                log_line(&format!("error while reading {}: {err}", path_display));
                break;
            }
        }
    }

    if entries.is_empty() {
        log_line(&format!(
            "symbol map {} contained no parseable entries; skipping",
            path_display
        ));
        return None;
    }

    entries.sort_by_key(|entry| entry.addr);
    Some(LoadedSymbolMap {
        entries,
        module_filter,
    })
}

/// Parses a whitespace-delimited address/symbol line, ignoring comments and blanks.
fn parse_symbol_line(line: &str) -> Option<SymbolEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let addr_token = parts.next()?;
    let addr = parse_hex_addr(addr_token)?;

    let remainder: Vec<&str> = parts.collect();
    if remainder.is_empty() {
        return None;
    }

    let mut name_start = 0;
    if remainder[0].len() == 1 && remainder.len() > 1 && remainder[0].is_ascii() {
        name_start = 1;
    }
    let name = remainder[name_start..].join(" ");
    if name.is_empty() {
        return None;
    }

    Some(SymbolEntry { addr, name })
}

/// Converts a hex address token (with or without 0x) into a usize.
fn parse_hex_addr(token: &str) -> Option<usize> {
    usize::from_str_radix(token.trim_start_matches("0x"), 16).ok()
}

impl SymbolMaps {
    /// Searches all loaded maps, preferring module-specific ones, for the best match.
    fn lookup(
        &self,
        addr: usize,
        module_path: Option<&str>,
        module_base: Option<usize>,
    ) -> Option<SymbolMapHit> {
        if let Some(path) = module_path {
            for map in self.maps.iter().filter(|map| map.module_filter.is_some()) {
                if map.matches_module(Some(path)) {
                    if let Some(hit) = map.lookup(addr, module_base) {
                        return Some(hit);
                    }
                }
            }
        }

        for map in self.maps.iter().filter(|map| map.module_filter.is_none()) {
            if let Some(hit) = map.lookup(addr, module_base) {
                return Some(hit);
            }
        }

        None
    }
}

impl LoadedSymbolMap {
    /// Checks whether this map should be used for the provided module path.
    fn matches_module(&self, module_path: Option<&str>) -> bool {
        match (&self.module_filter, module_path) {
            (Some(filter), Some(path)) => path.contains(filter),
            (Some(_), None) => false,
            _ => true,
        }
    }

    /// Looks up an address using both module-relative and absolute offsets.
    fn lookup(&self, addr: usize, module_base: Option<usize>) -> Option<SymbolMapHit> {
        // First try relative to module base (useful for shared libraries).
        let mut candidates = Vec::new();
        if let Some(base) = module_base {
            if let Some(offset) = addr.checked_sub(base) {
                candidates.push(offset);
            }
        }
        // Also try absolute addresses so maps generated from nm on the main executable work.
        candidates.push(addr);

        for candidate in candidates {
            if let Some(hit) = self.lookup_addr(candidate) {
                return Some(hit);
            }
        }
        None
    }

    /// Finds the nearest symbol at or before the requested offset within this map.
    fn lookup_addr(&self, offset: usize) -> Option<SymbolMapHit> {
        let idx = match self
            .entries
            .binary_search_by_key(&offset, |entry| entry.addr)
        {
            Ok(idx) => idx,
            Err(0) => return None,
            Err(idx) => idx - 1,
        };
        let entry = &self.entries[idx];
        let distance = offset.saturating_sub(entry.addr);
        if distance > 0x4000 {
            return None;
        }
        Some(SymbolMapHit {
            name: entry.name.clone(),
            distance,
        })
    }
}
