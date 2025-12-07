use std::{
    collections::{HashMap, HashSet},
    ffi::{c_void, CStr},
    mem::MaybeUninit,
    ptr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock,
    },
};

use grim_telemetry_common::{
    EventBuilder, LuaEvent, LuaSemanticEvent, OriginFields, TelemetryConfig, TelemetryLogger,
    UpvaluePreview, ValueFields, ValueType,
};

const ENGINE_ID: &str = "grim_engine";
const VM_ID: &str = "lua";

static FABRICATED_HANDLE: AtomicU32 = AtomicU32::new(1);
static KNOWN_TAGS: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();
static FABRICATED_BY_LABEL: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

static LOGGER: TelemetryLogger = TelemetryLogger::new(TelemetryConfig {
    engine_id: ENGINE_ID,
    vm_id: VM_ID,
    log_env_vars: &["GRIM_ENGINE_LOG"],
    line_prefix: "grim_engine",
    run_id_env: None,
});

pub(crate) fn log_event(event: impl Into<EventBuilder>) {
    LOGGER.log_event(event);
}

pub(crate) fn ptr_to_handle(func: *const c_void) -> String {
    format!("0x{:08x}", func as usize)
}

#[derive(Clone, Debug)]
pub(crate) struct FabricatedHandle {
    pub raw: i32,
}

pub(crate) fn next_fabricated_handle() -> FabricatedHandle {
    let raw = FABRICATED_HANDLE.fetch_add(1, Ordering::Relaxed);
    FabricatedHandle { raw: raw as i32 }
}

pub(crate) fn log_dofile(path: &str) {
    log_event(LuaEvent::Dofile {
        path: path.to_string(),
    });
}

pub(crate) fn log_engine_exit(status: &str, note: Option<&str>) {
    let mut builder = EventBuilder::new("engine_exit").kv("status", status);
    if let Some(text) = note {
        builder = builder.kv("note", text);
    }
    log_event(builder);
}

pub(crate) fn log_push_cclosure(
    label: &str,
    func: *const c_void,
    upvalues: i32,
    symbol_label: Option<&str>,
) {
    let mut origin = origin_fields_for_ptr(func);
    if let Some(symbol) = symbol_label {
        origin.symbol = Some(symbol.to_string());
    }
    log_event(LuaEvent::PushCclosure {
        name: label.to_string(),
        func: ptr_to_handle(func),
        upvalues,
        origin,
    });
}

#[allow(dead_code)]
pub(crate) fn log_push_number(value: &str) {
    log_event(LuaEvent::PushNumber {
        value: value.to_string(),
    });
}

#[allow(dead_code)]
pub(crate) fn log_push_nil() {
    log_event(LuaEvent::PushNil {});
}

#[allow(dead_code)]
pub(crate) fn log_push_string(len: usize, preview: String) {
    log_event(LuaEvent::PushString { len, preview });
}

#[allow(dead_code)]
pub(crate) fn log_push_object(handle: String, values: ValueFields) {
    log_event(LuaEvent::PushObject { handle, values });
}

pub(crate) fn log_push_from_preview(preview: &UpvaluePreview) {
    match preview.kind {
        ValueType::Nil => log_push_nil(),
        ValueType::Number => {
            if let Some(value) = preview.value.as_ref() {
                log_push_number(value);
            }
        }
        ValueType::String => {
            if let Some(len) = preview
                .value_len
                .or_else(|| preview.preview.as_ref().map(|value| value.len()))
            {
                let text = preview
                    .preview
                    .clone()
                    .or_else(|| preview.value.clone())
                    .unwrap_or_default();
                log_push_string(len, text);
            }
        }
        _ => {}
    }
}

pub(crate) fn log_lua_setglobal(
    name: &str,
    handle: String,
    label: Option<String>,
    values: ValueFields,
    origin: OriginFields,
) {
    let bind_handle = handle.clone();
    log_event(LuaEvent::BindGlobal {
        name: name.to_string(),
        handle: bind_handle,
        label,
        values: values.clone(),
        origin,
    });
}

pub(crate) fn log_registered_global(
    name: &str,
    handle: String,
    label: Option<String>,
    upvalues: i32,
    values: ValueFields,
    origin: OriginFields,
) {
    log_event(LuaSemanticEvent::SemanticBindGlobal {
        name: name.to_string(),
        handle: handle.clone(),
        label: label.clone(),
        values: values.clone(),
        upvalues: Some(upvalues),
        origin: origin.clone(),
    });
}

pub(crate) fn log_registered_constant(
    name: &str,
    handle: String,
    label: Option<String>,
    values: ValueFields,
    origin: OriginFields,
) {
    log_event(LuaSemanticEvent::SemanticBindConstant {
        name: name.to_string(),
        handle: handle.clone(),
        label: label.clone(),
        values: values.clone(),
        origin: origin.clone(),
    });
}

pub(crate) fn log_push_usertag(id: i32, tag: i32, payload_hex: String) {
    let mut values = ValueFields::default();
    values.value_type = Some(ValueType::Userdata);
    values.tag = Some(tag);
    values.value = Some(payload_hex);
    log_event(LuaEvent::PushUsertag {
        id,
        values,
        caller: caller_origin_fields(),
    });
}

pub(crate) fn log_create_table(handle: String, mut values: ValueFields) {
    if values.value_type.is_none() {
        values.value_type = Some(ValueType::Table);
    }
    log_event(LuaEvent::CreateTable {
        handle,
        values,
        caller: caller_origin_fields(),
    });
}

pub(crate) fn log_set_table_entry(
    table_handle: String,
    table_handle_label: Option<String>,
    key: UpvaluePreview,
    value: UpvaluePreview,
    note: Option<String>,
    table_fields: Option<ValueFields>,
    value_handle: Option<(String, Option<String>, ValueFields)>,
) {
    let caller = caller_origin_fields();
    let semantic_caller = caller.clone();
    let semantic_key = key.clone();
    let semantic_value = value.clone();
    let semantic_table_handle = table_handle.clone();
    let semantic_table_handle_label = table_handle_label.clone();
    let semantic_note = note.clone();
    // Retail telemetry records pushing the target table before setting entries.
    let table_fields = table_fields.unwrap_or_else(|| {
        let mut fields = ValueFields::default();
        fields.value_type = Some(ValueType::Table);
        fields
    });
    let semantic_table_fields = Some(table_fields.clone());
    let semantic_value_handle = value_handle.as_ref().map(|(handle, _, _)| handle.clone());
    let semantic_value_handle_label = value_handle
        .as_ref()
        .and_then(|(_, label, _)| label.clone());
    let semantic_value_fields = value_handle.as_ref().map(|(_, _, fields)| fields.clone());
    log_push_object(table_handle.clone(), table_fields.clone());
    log_push_from_preview(&key);
    if let Some((value_handle, _, value_fields)) = value_handle {
        log_push_object(value_handle, value_fields);
    } else {
        log_push_from_preview(&value);
    }
    log_event(LuaEvent::SetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    log_event(LuaSemanticEvent::SemanticSetTableEntry {
        table_handle: semantic_table_handle,
        table_handle_label: semantic_table_handle_label,
        table_fields: semantic_table_fields,
        key: semantic_key,
        value: semantic_value,
        value_handle: semantic_value_handle,
        value_handle_label: semantic_value_handle_label,
        value_fields: semantic_value_fields,
        note: semantic_note,
        caller: semantic_caller,
    });
}

pub(crate) fn log_set_tag(tag: i32, note: Option<String>) {
    log_event(LuaEvent::SetTag { tag, note });
}

pub(crate) fn register_tag(tag: i32, note: Option<String>) {
    let cache = KNOWN_TAGS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut tags) = cache.lock() {
        if tags.insert(tag) {
            log_set_tag(tag, note.clone());
        }
    }
}

pub(crate) fn log_store_ref(
    lock: i32,
    reference: i32,
    handle: Option<String>,
    label: Option<String>,
) {
    let origin = caller_origin_fields();
    log_event(LuaSemanticEvent::SemanticStoreRef {
        lock,
        reference,
        handle: handle.clone(),
        label: label.clone(),
        note: None,
        origin: origin.clone(),
    });
    log_event(LuaEvent::StoreRef {
        lock,
        reference,
        handle,
        label: label.clone(),
        note: None,
        origin,
    });
}

pub(crate) fn log_set_tagmethod(
    tag: i64,
    event: &str,
    handle: Option<String>,
    values: ValueFields,
    origin: OriginFields,
) {
    log_event(LuaSemanticEvent::SemanticSetTagmethod {
        tag: tag as i32,
        event_name: event.to_string(),
        handle: handle.clone(),
        values: values.clone(),
        origin: origin.clone(),
    });
    log_event(LuaEvent::SetTagmethod {
        tag: tag as i32,
        event_name: event.to_string(),
        handle,
        values,
        origin,
    });
}

pub(crate) fn log_set_fallback(
    fallback: &str,
    handle: String,
    values: ValueFields,
    target_ptr: Option<*const c_void>,
) {
    let origin = target_ptr.map_or_else(OriginFields::default, origin_fields_for_ptr);
    let caller = caller_origin_fields();
    log_event(LuaSemanticEvent::SemanticSetFallback {
        fallback: fallback.to_string(),
        handle: handle.clone(),
        values: values.clone(),
        origin: origin.clone(),
        caller: caller.clone(),
    });
    log_event(LuaEvent::SetFallback {
        fallback: fallback.to_string(),
        handle,
        values,
        origin,
        caller,
    });
}

pub(crate) fn log_fetch_ref(
    reference: i32,
    handle: Option<String>,
    label: Option<String>,
    note: Option<String>,
    origin: OriginFields,
) {
    log_event(LuaSemanticEvent::SemanticFetchRef {
        reference,
        handle: handle.clone(),
        label: label.clone(),
        note: note.clone(),
        origin: origin.clone(),
    });
    log_event(LuaEvent::FetchRef {
        reference,
        handle,
        label,
        note,
        origin,
    });
}

pub(crate) fn log_unref(reference: i32, note: Option<String>) {
    log_event(LuaSemanticEvent::SemanticUnref {
        reference,
        note: note.clone(),
    });
    log_event(LuaEvent::Unref { reference, note });
}

pub(crate) fn normalize_handle(label: &str, preferred: Option<String>) -> String {
    if let Some(handle) = preferred {
        return handle;
    }
    stable_fabricated_handle(label)
}

fn stable_fabricated_handle(label: &str) -> String {
    let map = FABRICATED_BY_LABEL.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut handles) = map.lock() {
        return handles
            .entry(label.to_string())
            .or_insert_with(|| {
                let next = FABRICATED_HANDLE.fetch_add(1, Ordering::Relaxed);
                format!("0x{next:08x}")
            })
            .clone();
    }
    let next = FABRICATED_HANDLE.fetch_add(1, Ordering::Relaxed);
    format!("0x{next:08x}")
}

pub(crate) fn origin_fields_for_ptr(ptr: *const c_void) -> OriginFields {
    let mut fields = OriginFields::default();
    if ptr.is_null() {
        return fields;
    }
    fields.origin = Some(format!("0x{:08x}", ptr as usize));
    let details = describe_closure_target(ptr);
    if let Some(module) = details.module {
        fields.module = Some(module);
    }
    if let Some(symbol) = details.symbol {
        fields.symbol = Some(symbol);
    }
    fields
}

fn caller_origin_fields() -> OriginFields {
    let mut frames: [*mut c_void; 32] = [ptr::null_mut(); 32];
    let depth = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as i32) };
    if depth <= 0 {
        return OriginFields::default();
    }
    for addr in frames.iter().take(depth as usize).skip(1) {
        if addr.is_null() {
            continue;
        }
        return origin_fields_for_ptr(*addr as *const c_void);
    }
    OriginFields::default()
}

struct ClosureDetails {
    module: Option<String>,
    symbol: Option<String>,
}

fn describe_closure_target(ptr: *const c_void) -> ClosureDetails {
    unsafe {
        let mut info = MaybeUninit::<libc::Dl_info>::zeroed();
        if libc::dladdr(ptr, info.as_mut_ptr()) == 0 {
            return ClosureDetails {
                module: None,
                symbol: None,
            };
        }
        let info = info.assume_init();
        let module = cstr_opt(info.dli_fname);
        let symbol = cstr_opt(info.dli_sname);
        ClosureDetails { module, symbol }
    }
}

unsafe fn cstr_opt(ptr: *const libc::c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

extern "C" {
    fn backtrace(buffer: *mut *mut c_void, size: i32) -> i32;
}
