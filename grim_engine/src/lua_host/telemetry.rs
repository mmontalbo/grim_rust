use std::{
    collections::{HashMap, HashSet},
    ffi::{c_void, CStr},
    mem::MaybeUninit,
    ptr,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

use grim_telemetry_common::{
    EventBuilder, LuaEvent, OriginFields, SeqRange, TelemetryConfig, TelemetryLogger,
    UpvaluePreview, ValueFields, ValueType,
};

const ENGINE_ID: &str = "grim_engine";
const VM_ID: &str = "lua";

static PUSH_SEQ: AtomicU64 = AtomicU64::new(0);
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

fn log_event_with_seq(event: impl Into<EventBuilder>) -> u64 {
    LOGGER.log_event_with_seq(event)
}

fn log_event_with_seq_display(event: impl Into<EventBuilder>, seq_display: String) {
    LOGGER.log_event_with_seq_display(event, seq_display);
}

fn next_push_seq() -> u64 {
    PUSH_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LoggedPushCclosure {
    pub push_seq: u64,
    pub log_seq: u64,
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

pub(crate) fn log_push_cclosure(
    label: &str,
    func: *const c_void,
    upvalues: i32,
    symbol_label: Option<&str>,
) -> LoggedPushCclosure {
    let push_seq = next_push_seq();
    let mut origin = origin_fields_for_ptr(func);
    if let Some(symbol) = symbol_label {
        origin.symbol = Some(symbol.to_string());
    }
    let log_seq = log_event_with_seq(LuaEvent::PushCclosure {
        name: label.to_string(),
        func: ptr_to_handle(func),
        push_seq,
        upvalues,
        origin,
    });
    LoggedPushCclosure { push_seq, log_seq }
}

#[allow(dead_code)]
pub(crate) fn log_push_number(value: &str) -> u64 {
    log_event_with_seq(LuaEvent::PushNumber {
        value: value.to_string(),
    })
}

#[allow(dead_code)]
pub(crate) fn log_push_nil() -> u64 {
    log_event_with_seq(LuaEvent::PushNil {})
}

#[allow(dead_code)]
pub(crate) fn log_push_string(len: usize, preview: String) -> u64 {
    log_event_with_seq(LuaEvent::PushString { len, preview })
}

#[allow(dead_code)]
pub(crate) fn log_push_object(
    handle: String,
    handle_label: Option<String>,
    values: ValueFields,
) -> u64 {
    log_event_with_seq(LuaEvent::PushObject {
        handle,
        handle_label,
        values,
    })
}

pub(crate) fn log_push_from_preview(preview: &UpvaluePreview) -> Option<u64> {
    match preview.kind {
        ValueType::Nil => Some(log_push_nil()),
        ValueType::Number => preview.value.as_ref().map(|value| log_push_number(value)),
        ValueType::String => {
            let len = preview
                .value_len
                .or_else(|| preview.preview.as_ref().map(|value| value.len()))?;
            let text = preview
                .preview
                .clone()
                .or_else(|| preview.value.clone())
                .unwrap_or_default();
            Some(log_push_string(len, text))
        }
        _ => None,
    }
}

pub(crate) fn log_lua_setglobal(
    name: &str,
    handle: String,
    handle_label: Option<String>,
    values: ValueFields,
    origin: OriginFields,
) -> u64 {
    let label = handle_label.clone();
    let bind_handle = handle.clone();
    log_event_with_seq(LuaEvent::BindGlobal {
        name: name.to_string(),
        handle: bind_handle,
        handle_label: handle_label.clone(),
        label,
        values: values.clone(),
        origin,
    })
}

pub(crate) fn log_registered_global(
    name: &str,
    handle: String,
    handle_label: Option<String>,
    push_seq: u64,
    func: String,
    upvalues: i32,
    upvalue_previews: Option<Vec<UpvaluePreview>>,
    values: ValueFields,
    seq_range: Option<SeqRange>,
    origin: OriginFields,
) {
    let label = handle_label.clone();
    let event = LuaEvent::RegisteredGlobal {
        name: name.to_string(),
        handle,
        handle_label: handle_label.clone(),
        label,
        push_seq,
        func,
        upvalues,
        upvalue_previews,
        values,
        origin,
    };
    if let Some(seq_range) = seq_range {
        log_event_with_seq_display(event, seq_range.display());
    } else {
        log_event(event);
    }
}

pub(crate) fn log_registered_constant(
    name: &str,
    handle: String,
    handle_label: Option<String>,
    values: ValueFields,
    seq_range: Option<SeqRange>,
    origin: OriginFields,
) {
    let event = LuaEvent::RegisteredConstant {
        name: name.to_string(),
        handle,
        handle_label: handle_label.clone(),
        label: handle_label,
        values,
        origin,
    };
    if let Some(seq_range) = seq_range {
        log_event_with_seq_display(event, seq_range.display());
    } else {
        log_event(event);
    }
}

pub(crate) fn log_push_usertag(id: i32, tag: i32, payload_hex: String) -> u64 {
    let mut values = ValueFields::default();
    values.value_type = Some(ValueType::Userdata);
    values.tag = Some(tag);
    values.value = Some(payload_hex);
    log_event_with_seq(LuaEvent::PushUsertag {
        id,
        values,
        caller: caller_origin_fields(),
    })
}

pub(crate) fn log_create_table(
    handle: String,
    handle_label: Option<String>,
    mut values: ValueFields,
) {
    if values.value_type.is_none() {
        values.value_type = Some(ValueType::Table);
    }
    log_event(LuaEvent::CreateTable {
        handle,
        handle_label,
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
) {
    let caller = caller_origin_fields();
    let mut seqs = Vec::new();
    if let Some(seq) = log_push_from_preview(&key) {
        seqs.push(seq);
    }
    if let Some(seq) = log_push_from_preview(&value) {
        seqs.push(seq);
    }
    let set_seq = log_event_with_seq(LuaEvent::SetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    seqs.push(set_seq);
    let seq_range = SeqRange::from_seqs(seqs).unwrap_or_else(|| SeqRange::new(set_seq, set_seq));
    log_event_with_seq_display(
        LuaEvent::SetTableEntry {
            table_handle,
            table_handle_label,
            key,
            value,
            note,
            seq_min: Some(seq_range.min),
            seq_max: Some(seq_range.max),
            caller,
        },
        seq_range.display(),
    );
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
    handle_label: Option<String>,
    label: Option<String>,
) {
    log_event(LuaEvent::StoreRef {
        lock,
        reference,
        handle,
        handle_label,
        label: label.clone(),
        note: None,
        origin: caller_origin_fields(),
    });
}

pub(crate) fn log_set_tagmethod(
    tag: i64,
    event: &str,
    handle: Option<String>,
    handle_label: Option<String>,
    values: ValueFields,
    origin: OriginFields,
) {
    log_event(LuaEvent::SetTagmethod {
        tag: tag as i32,
        event_name: event.to_string(),
        handle,
        handle_label,
        values,
        origin,
    });
}

pub(crate) fn log_set_fallback(
    fallback: &str,
    handle: String,
    handle_label: Option<String>,
    values: ValueFields,
    target_ptr: Option<*const c_void>,
) {
    let origin = target_ptr.map_or_else(OriginFields::default, origin_fields_for_ptr);
    log_event(LuaEvent::SetFallback {
        fallback: fallback.to_string(),
        handle,
        handle_label,
        values,
        origin,
        caller: caller_origin_fields(),
    });
}

pub(crate) fn log_fetch_ref(
    reference: i32,
    handle: Option<String>,
    handle_label: Option<String>,
    label: Option<String>,
    note: Option<String>,
    origin: OriginFields,
) {
    log_event(LuaEvent::FetchRef {
        reference,
        handle,
        handle_label,
        label,
        note,
        origin,
    });
}

pub(crate) fn log_unref(reference: i32, note: Option<String>) {
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
