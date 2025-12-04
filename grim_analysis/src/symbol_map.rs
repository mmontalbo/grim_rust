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
    pub(crate) source_label: Option<String>,
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
    source_label: Option<String>,
}

pub(crate) fn lookup_symbol_from_map(
    addr: usize,
    module_path: Option<&str>,
    module_base: Option<usize>,
) -> Option<SymbolMapHit> {
    let maps = symbol_maps()?;
    maps.lookup(addr, module_path, module_base)
}

fn symbol_maps() -> Option<&'static SymbolMaps> {
    static MAPS: OnceLock<Option<SymbolMaps>> = OnceLock::new();
    MAPS.get_or_init(load_symbol_maps).as_ref()
}

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

fn load_symbol_map(path_var: &str, module_var: &str) -> Option<LoadedSymbolMap> {
    let path = env::var(path_var).ok()?;
    let module_filter = env::var(module_var).ok().filter(|value| !value.is_empty());
    let path_ref = Path::new(&path);
    load_symbol_map_at_path(path_ref, module_filter)
}

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
    let source_label = path_ref
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string());

    Some(LoadedSymbolMap {
        entries,
        module_filter,
        source_label,
    })
}

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

fn parse_hex_addr(token: &str) -> Option<usize> {
    usize::from_str_radix(token.trim_start_matches("0x"), 16).ok()
}

impl SymbolMaps {
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
    fn matches_module(&self, module_path: Option<&str>) -> bool {
        match (&self.module_filter, module_path) {
            (Some(filter), Some(path)) => path.contains(filter),
            (Some(_), None) => false,
            _ => true,
        }
    }

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
            source_label: self.source_label.clone(),
        })
    }
}
