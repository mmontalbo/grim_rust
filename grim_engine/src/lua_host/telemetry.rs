use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock,
    },
};

pub(crate) use grim_telemetry_schema::trace_utils::origin_fields_for_ptr;
use grim_telemetry_schema::trace_utils::{
    caller_origin_fields as trace_caller_origin_fields, handle_hex, register_tag_alias,
    semantic_set_table_entry, should_skip_caller_frame as common_should_skip_caller_frame,
    tag_alias,
};
use grim_telemetry_schema::{
    BootSequenceTracker, EngineEvent, LuaEvent, LuaSemanticEvent, OriginFields, TelemetryConfig,
    TelemetryLogger, UpvaluePreview, ValueFields, ValueType,
};

// Re-export common helpers so callers keep using the telemetry module surface.
pub(crate) use grim_telemetry_schema::trace_utils::{ptr_to_handle, register_table_label};

const ENGINE_ID: &str = "grim_engine";
const VM_ID: &str = "lua";

static FABRICATED_HANDLE: AtomicU32 = AtomicU32::new(1);
static KNOWN_TAGS: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();
static FABRICATED_BY_LABEL: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static BOOT_SEQUENCE: BootSequenceTracker = BootSequenceTracker::new();

static LOGGER: TelemetryLogger = TelemetryLogger::new(TelemetryConfig {
    engine_id: ENGINE_ID,
    vm_id: VM_ID,
    log_env_vars: &["GRCTL_LOG_PATH"],
    line_prefix: "grim_engine",
    raw_stream_enabled: true,
});

pub(crate) fn log_event(event: impl grim_telemetry_schema::TelemetryEventPayload) {
    LOGGER.log_event(event);
}

pub(crate) fn log_boot_sequence_start() {
    if let Some(event) = BOOT_SEQUENCE.boot_started() {
        log_event(event);
    }
}

pub(crate) fn log_boot_sequence_complete(note: Option<&str>) {
    let note_owned = note.map(str::to_string);
    if let Some(event) = BOOT_SEQUENCE.boot_complete(note_owned) {
        log_event(event);
    }
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

pub(crate) fn log_engine_exit(
    status: &str,
    note: Option<&str>,
    code: Option<i32>,
    signal: Option<i32>,
    cause: Option<&str>,
) {
    log_event(EngineEvent::EngineExit {
        status: status.to_string(),
        note: note.map(str::to_string),
        code,
        signal,
        cause: cause.map(str::to_string),
        component: Some(ENGINE_ID.to_string()),
    });
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
    let caller = caller_origin_fields();
    log_event(LuaEvent::PushCclosure {
        name: label.to_string(),
        func: ptr_to_handle(func),
        upvalues,
        origin,
        caller,
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
    let caller = caller_origin_fields();
    log_event(LuaEvent::BindGlobal {
        name: name.to_string(),
        handle: bind_handle,
        label,
        values: values.clone(),
        origin,
        caller,
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
    log_event(LuaSemanticEvent::SemanticBindGlobalClosure {
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
    log_event(LuaSemanticEvent::SemanticBindGlobalConstant {
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
    // Retail telemetry records pushing the target table before setting entries.
    let table_fields = table_fields.unwrap_or_else(|| {
        let mut fields = ValueFields::default();
        fields.value_type = Some(ValueType::Table);
        fields
    });
    log_push_object(table_handle.clone(), table_fields.clone());
    log_push_from_preview(&key);
    if let Some((value_handle, _, value_fields)) = value_handle.clone() {
        log_push_object(value_handle, value_fields);
    } else {
        log_push_from_preview(&value);
    }
    log_event(LuaEvent::SetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    let semantic_value_handle = value_handle.as_ref().map(|(handle, _, _)| handle.clone());
    let semantic_value_handle_label = value_handle
        .as_ref()
        .and_then(|(_, label, _)| label.clone());
    let semantic_value_fields = value_handle.as_ref().map(|(_, _, fields)| fields.clone());
    let semantic_event = semantic_set_table_entry(
        table_handle,
        table_handle_label,
        Some(table_fields),
        key,
        value,
        semantic_value_handle,
        semantic_value_handle_label,
        semantic_value_fields,
        note,
        caller,
    );
    log_event(semantic_event);
}

pub(crate) fn log_set_tag(tag: i32, alias: Option<String>, note: Option<String>) {
    log_event(LuaEvent::SetTag {
        tag,
        note,
        tag_alias: alias,
    });
}

pub(crate) fn register_tag(tag: i32, alias: Option<&str>, note: Option<&str>) {
    let cache = KNOWN_TAGS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut tags) = cache.lock() {
        if tags.insert(tag) {
            let alias_owned = alias.map(|text| text.to_string());
            let note_owned = note.map(|text| text.to_string());
            if let Some(alias_value) = alias_owned.as_ref() {
                register_tag_alias(tag, alias_value.clone());
                log_event(LuaSemanticEvent::SemanticTagAlias {
                    tag,
                    alias: alias_value.clone(),
                    origin: caller_origin_fields(),
                });
            }
            log_set_tag(tag, alias_owned, note_owned);
        }
    }
}

pub(crate) fn log_store_ref(
    lock: i32,
    reference: i32,
    handle: Option<String>,
    label: Option<String>,
    value_fields: Option<ValueFields>,
) {
    let origin = caller_origin_fields();
    log_event(LuaSemanticEvent::SemanticStoreRef {
        lock,
        reference,
        handle: handle.clone(),
        label: label.clone(),
        alias: None,
        value_kind: value_fields
            .as_ref()
            .and_then(|fields| fields.value_type.clone()),
        value_fields: value_fields.clone(),
        note: None,
        origin: origin.clone(),
    });
    log_event(LuaEvent::StoreRef {
        lock,
        reference,
        handle,
        label: label.clone(),
        value_fields,
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
    let tag_alias_value = tag_alias(tag as i32);
    log_event(LuaSemanticEvent::SemanticSetTagmethod {
        tag: tag as i32,
        event_name: event.to_string(),
        handle: handle.clone(),
        values: values.clone(),
        tag_alias: tag_alias_value.clone(),
        origin: origin.clone(),
    });
    log_event(LuaEvent::SetTagmethod {
        tag: tag as i32,
        event_name: event.to_string(),
        handle,
        values,
        tag_alias: tag_alias_value,
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
    let semantic_event = LuaSemanticEvent::SemanticSetFallback {
        fallback: fallback.to_string(),
        handle: handle.clone(),
        values: values.clone(),
        origin: origin.clone(),
        caller: caller.clone(),
    };
    log_event(semantic_event);
    log_event(LuaEvent::SetFallback {
        fallback: fallback.to_string(),
        handle,
        values,
        origin,
        caller,
    });
}

pub(crate) fn log_load_ref(
    reference: i32,
    handle: Option<String>,
    label: Option<String>,
    note: Option<String>,
    origin: OriginFields,
) {
    log_event(LuaSemanticEvent::SemanticLoadRef {
        reference,
        handle: handle.clone(),
        label: label.clone(),
        alias: None,
        value_kind: None,
        note: note.clone(),
        origin: origin.clone(),
    });
    log_event(LuaEvent::LoadRef {
        reference,
        handle,
        label,
        alias: None,
        value_kind: None,
        note,
        origin,
    });
}

pub(crate) fn log_unref(reference: i32, note: Option<String>) {
    log_event(LuaSemanticEvent::SemanticUnref {
        reference,
        alias: None,
        value_kind: None,
        note: note.clone(),
        origin: OriginFields::default(),
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
                handle_hex(next as usize)
            })
            .clone();
    }
    let next = FABRICATED_HANDLE.fetch_add(1, Ordering::Relaxed);
    handle_hex(next as usize)
}

fn caller_origin_fields() -> OriginFields {
    trace_caller_origin_fields(should_skip_caller_frame)
}

fn should_skip_caller_frame(module_path: Option<&str>, symbol: Option<&str>) -> bool {
    common_should_skip_caller_frame(module_path, symbol, |_, symbol| {
        symbol
            .map(|sym| sym.to_ascii_lowercase().contains("lua_host::telemetry"))
            .unwrap_or(false)
    })
}
