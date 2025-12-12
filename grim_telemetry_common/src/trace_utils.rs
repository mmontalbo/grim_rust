use crate::{LuaSemanticEvent, OriginFields, UpvaluePreview, ValueFields, ValueType};
use libc::{c_char, c_int, c_void, Dl_info};
use std::{
    collections::HashMap,
    ffi::CStr,
    mem::MaybeUninit,
    ptr,
    sync::{Mutex, OnceLock},
};

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
    format!("0x{:08x}", ptr as usize)
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

/// Builds `OriginFields` from resolved closure details.
pub fn origin_fields_from_details(details: &ClosureDetails) -> OriginFields {
    let mut fields = OriginFields::default();
    fields.origin = Some(format!("0x{:08x}", details.address as usize));
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

static TABLE_LABELS: OnceLock<Mutex<HashMap<usize, String>>> = OnceLock::new();

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
        value_preview: Some(truncate_for_log(text, 80)),
        ..ValueFields::default()
    }
}

pub fn value_fields_for_nil() -> ValueFields {
    ValueFields {
        value_type: Some(ValueType::Nil),
        ..ValueFields::default()
    }
}
