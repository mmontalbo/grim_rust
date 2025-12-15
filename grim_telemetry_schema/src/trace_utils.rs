use crate::{LuaSemanticEvent, OriginFields, UpvaluePreview, ValueFields, ValueType};
use libc::{c_char, c_int, c_void, Dl_info};
use std::path::{Path, PathBuf};
use std::{
    collections::HashMap,
    ffi::CStr,
    mem::MaybeUninit,
    ptr,
    sync::{Mutex, OnceLock},
};

/// Default maximum length for value previews in telemetry logs.
pub const LOG_PREVIEW_MAX_LEN: usize = 80;

/// Details about a resolved closure/function pointer.
#[derive(Clone, Debug)]
pub struct ClosureDetails {
    pub address: usize,
    pub module: Option<String>,
    pub module_base: Option<usize>,
    pub symbol: Option<String>,
}

/// Formats a pointer value into the canonical hex handle representation.
pub fn ptr_to_handle(ptr: *const c_void) -> String {
    handle_hex(ptr as usize)
}

/// Formats an address/handle into the canonical hex string representation.
pub fn handle_hex(handle: usize) -> String {
    format!("0x{handle:08x}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LuaFunctionProvenance {
    GameScript(PathBuf),
    Native(String),
    Other(String),
    Unknown,
}

impl LuaFunctionProvenance {
    pub fn source_hint(&self) -> Option<String> {
        match self {
            LuaFunctionProvenance::GameScript(path) => Some(path.display().to_string()),
            LuaFunctionProvenance::Native(kind) => Some(kind.clone()),
            LuaFunctionProvenance::Other(source) => Some(source.clone()),
            LuaFunctionProvenance::Unknown => None,
        }
    }
}

fn normalize_script_path(source: &str, data_root: &Path) -> PathBuf {
    let path = Path::new(source);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        data_root.join(path)
    }
}

fn path_within_root(path: &Path, root: &Path) -> bool {
    let normalized_root = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    match path.canonicalize() {
        Ok(actual) => actual.starts_with(&normalized_root),
        Err(_) => path.starts_with(&normalized_root),
    }
}

pub fn classify_lua_function_provenance(
    source: Option<&str>,
    what: Option<&str>,
    data_root: &Path,
) -> LuaFunctionProvenance {
    if let Some(kind) = what {
        if matches!(kind, "C" | "Rust") {
            return LuaFunctionProvenance::Native(kind.to_string());
        }
    }

    if let Some(raw_source) = source {
        if let Some(path) = raw_source.strip_prefix('@') {
            let normalized = normalize_script_path(path, data_root);
            if path_within_root(&normalized, data_root) {
                return LuaFunctionProvenance::GameScript(normalized);
            }
            return LuaFunctionProvenance::Other(normalized.display().to_string());
        }
        return LuaFunctionProvenance::Other(raw_source.to_string());
    }

    LuaFunctionProvenance::Unknown
}

/// Converts a potentially null C string into an owned `String`.
pub fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        unsafe { Some(CStr::from_ptr(ptr).to_string_lossy().into_owned()) }
    }
}

/// Resolves module/symbol information for a closure pointer using `dladdr`.
pub fn describe_closure_target(ptr: *const c_void) -> ClosureDetails {
    unsafe {
        let mut info = MaybeUninit::<Dl_info>::zeroed();
        if libc::dladdr(ptr, info.as_mut_ptr()) == 0 {
            return ClosureDetails {
                address: ptr as usize,
                module: None,
                module_base: None,
                symbol: None,
            };
        }
        let info = info.assume_init();
        let module = cstr_opt(info.dli_fname);
        let module_base = if info.dli_fbase.is_null() {
            None
        } else {
            Some(info.dli_fbase as usize)
        };
        let symbol = cstr_opt(info.dli_sname);
        ClosureDetails {
            address: ptr as usize,
            module,
            module_base,
            symbol,
        }
    }
}

/// Walks the current call stack and returns the first frame that is not skipped by `skip_frame`.
pub fn caller_origin_details(
    skip_frame: impl Fn(Option<&str>, Option<&str>) -> bool,
) -> Option<ClosureDetails> {
    let mut frames: [*mut c_void; 32] = [ptr::null_mut(); 32];
    let depth = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as c_int) };
    if depth <= 0 {
        return None;
    }

    for addr in frames.iter().take(depth as usize).skip(1) {
        if addr.is_null() {
            continue;
        }
        let ptr = *addr as *const c_void;
        let details = describe_closure_target(ptr);
        let module = details.module.as_deref();
        let symbol = details.symbol.as_deref();
        if skip_frame(module, symbol) {
            continue;
        }
        return Some(details);
    }
    None
}

/// Returns `true` when a frame belongs to common runtime helpers that should not be
/// considered the caller (libc/dl/loader/vdso).
pub fn is_runtime_frame(module_path: Option<&str>) -> bool {
    match module_path {
        Some(path) => {
            let normalized = path.to_ascii_lowercase();
            normalized.contains("libc.so")
                || normalized.contains("libdl.so")
                || normalized.contains("ld-linux")
                || normalized.contains("linux-vdso")
        }
        None => false,
    }
}

/// Baseline caller skip: filters out runtime frames, then defers to a crate-specific predicate.
/// Use this in both engine and shim to avoid subtle divergence in caller attribution.
pub fn should_skip_caller_frame(
    module_path: Option<&str>,
    symbol: Option<&str>,
    extra_skip: impl Fn(Option<&str>, Option<&str>) -> bool,
) -> bool {
    if is_runtime_frame(module_path) {
        return true;
    }
    extra_skip(module_path, symbol)
}

/// Resolves module/symbol info for a pointer into `OriginFields`.
pub fn origin_fields_for_ptr(ptr: *const c_void) -> OriginFields {
    if ptr.is_null() {
        return OriginFields::default();
    }
    let details = describe_closure_target(ptr);
    origin_fields_from_details(&details)
}

/// Captures the immediate non-filtered caller as `OriginFields`.
pub fn caller_origin_fields(
    skip_frame: impl Fn(Option<&str>, Option<&str>) -> bool,
) -> OriginFields {
    caller_origin_details(skip_frame)
        .map(|details| origin_fields_from_details(&details))
        .unwrap_or_default()
}

/// Builds `OriginFields` from resolved closure details.
pub fn origin_fields_from_details(details: &ClosureDetails) -> OriginFields {
    let mut fields = OriginFields::default();
    fields.origin = Some(handle_hex(details.address));
    if let Some(module) = details.module.as_ref() {
        fields.module = Some(module.clone());
    }
    if let Some(symbol) = details.symbol.as_ref() {
        fields.symbol = Some(symbol.clone());
    }
    fields
}

/// Helper to construct a semantic set-table entry event with sensible defaults.
pub fn semantic_set_table_entry(
    table_handle: String,
    table_handle_label: Option<String>,
    table_fields: Option<ValueFields>,
    key: UpvaluePreview,
    value: UpvaluePreview,
    value_handle: Option<String>,
    value_handle_label: Option<String>,
    value_fields: Option<ValueFields>,
    note: Option<String>,
    caller: OriginFields,
) -> LuaSemanticEvent {
    let table_fields = table_fields.or_else(|| {
        let mut fields = ValueFields::default();
        fields.value_type = Some(ValueType::Table);
        Some(fields)
    });
    LuaSemanticEvent::SemanticSetTableEntry {
        table_handle,
        table_handle_label,
        table_fields,
        key,
        value,
        value_handle,
        value_handle_label,
        value_fields,
        note,
        caller,
    }
}

static TAG_ALIASES: OnceLock<Mutex<HashMap<i32, String>>> = OnceLock::new();
static REF_ALIASES: OnceLock<Mutex<HashMap<i32, RefAlias>>> = OnceLock::new();
static TABLE_LABELS: OnceLock<Mutex<HashMap<usize, String>>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct RefAlias {
    pub alias: Option<String>,
    pub value_kind: Option<ValueType>,
}

/// Records a friendly alias for a tag if one is not already present.
pub fn register_tag_alias(tag: i32, alias: impl Into<String>) {
    let aliases = TAG_ALIASES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = aliases.lock() {
        map.entry(tag).or_insert_with(|| alias.into());
    }
}

/// Retrieves a recorded alias for a tag, when available.
pub fn tag_alias(tag: i32) -> Option<String> {
    TAG_ALIASES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|map| map.get(&tag).cloned())
}

/// Stores alias metadata for a reference, including the value kind when known.
pub fn remember_ref_alias(reference: i32, alias: Option<String>, value_kind: Option<ValueType>) {
    if alias.is_none() && value_kind.is_none() {
        return;
    }
    let aliases = REF_ALIASES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = aliases.lock() {
        map.insert(reference, RefAlias { alias, value_kind });
    }
}

/// Fetches the alias/value-kind pair for a reference.
pub fn ref_alias(reference: i32) -> Option<RefAlias> {
    REF_ALIASES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|map| map.get(&reference).cloned())
}

/// Registers a human-readable label for a table pointer if one is not already stored.
pub fn register_table_label(ptr: *const c_void, label: impl Into<String>) {
    if ptr.is_null() {
        return;
    }
    let labels = TABLE_LABELS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = labels.lock() {
        map.entry(ptr as usize).or_insert_with(|| label.into());
    }
}

/// Retrieves a stored label for a table pointer, if present.
pub fn table_label(ptr: *const c_void) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let labels = TABLE_LABELS.get_or_init(|| Mutex::new(HashMap::new()));
    labels
        .lock()
        .ok()
        .and_then(|map| map.get(&(ptr as usize)).cloned())
}

/// Formats numbers for logs, collapsing integral floats to integers.
pub fn format_number_for_log(value: f64) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// Truncates long strings for logging while indicating they were shortened.
pub fn truncate_for_log(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut truncated = text[..max_len].to_string();
    truncated.push_str("...");
    truncated
}

/// A uniform description of a value for telemetry rendering.
#[derive(Clone, Debug)]
pub struct ValueMeta {
    pub kind: ValueType,
    pub value: Option<String>,
    pub value_len: Option<usize>,
    pub preview: Option<String>,
    pub tag: Option<i32>,
    pub func: Option<String>,
}

impl Default for ValueMeta {
    fn default() -> Self {
        Self {
            kind: ValueType::Unknown,
            value: None,
            value_len: None,
            preview: None,
            tag: None,
            func: None,
        }
    }
}

/// Builds `ValueFields` from a `ValueMeta` descriptor.
pub fn value_fields_from_meta(meta: &ValueMeta) -> ValueFields {
    ValueFields {
        value_type: Some(meta.kind.clone()),
        value: meta.value.clone(),
        value_len: meta.value_len,
        value_preview: meta.preview.clone(),
        tag: meta.tag,
        tag_label: meta.tag.and_then(tag_alias),
        func: meta.func.clone(),
    }
}

/// Builds an `UpvaluePreview` from a `ValueMeta` descriptor.
pub fn upvalue_preview_from_meta(meta: &ValueMeta) -> UpvaluePreview {
    UpvaluePreview {
        kind: meta.kind.clone(),
        value: meta.value.clone(),
        value_len: meta.value_len,
        preview: meta.preview.clone(),
        tag: meta.tag,
    }
}

extern "C" {
    fn backtrace(buffer: *mut *mut c_void, size: c_int) -> c_int;
}

/// Converts raw data into `ValueFields` for telemetry, covering common Lua-ish primitives.
pub fn value_fields_from_number(value: f64) -> ValueFields {
    ValueFields {
        value_type: Some(ValueType::Number),
        value: Some(format_number_for_log(value)),
        ..ValueFields::default()
    }
}

pub fn value_fields_from_string(text: &str) -> ValueFields {
    ValueFields {
        value_type: Some(ValueType::String),
        value_len: Some(text.len()),
        value_preview: Some(truncate_for_log(text, LOG_PREVIEW_MAX_LEN)),
        ..ValueFields::default()
    }
}

pub fn value_fields_for_nil() -> ValueFields {
    ValueFields {
        value_type: Some(ValueType::Nil),
        ..ValueFields::default()
    }
}
