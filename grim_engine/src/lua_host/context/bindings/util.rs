use std::cell::RefCell;

use grim_telemetry_schema::{
    trace_utils::{
        format_number_for_log, handle_hex, truncate_for_log, upvalue_preview_from_meta,
        value_fields_from_meta, ValueMeta, LOG_PREVIEW_MAX_LEN,
    },
    OriginFields, UpvaluePreview, ValueFields, ValueType,
};
use mlua::{IntoLua, Lua, Result as LuaResult, Table, UserData, Value};

use crate::lua_host::telemetry::{
    log_lua_setglobal, log_push_cclosure, log_push_nil, log_push_number, log_push_object,
    log_push_string, log_registered_constant, log_registered_global, log_set_table_entry,
    normalize_handle, origin_fields_for_ptr, ptr_to_handle, register_table_label,
};

#[derive(Clone)]
pub(crate) struct TaggedHandle {
    pub tag: i32,
}

impl TaggedHandle {
    pub(crate) fn new(tag: i32) -> Self {
        Self { tag }
    }
}

impl UserData for TaggedHandle {}

#[derive(Clone, Default)]
pub(crate) struct RegisteredGlobalMeta {
    pub upvalues: i32,
}

pub(crate) const COLOR_TAG: i32 = 0x434f4c52;

#[derive(Clone)]
pub(crate) struct ColorHandle {
    encoded: u32,
}

impl ColorHandle {
    pub(crate) fn new(r: u8, g: u8, b: u8) -> Self {
        let encoded = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        Self { encoded }
    }

    pub(crate) fn encoded(&self) -> u32 {
        self.encoded
    }
}

impl UserData for ColorHandle {}

thread_local! {
    static REGISTERED_GLOBAL_HINT: RefCell<Option<RegisteredGlobalMeta>> = RefCell::new(None);
    static REGISTERED_GLOBAL_SUPPRESSION: RefCell<bool> = RefCell::new(false);
}

pub(crate) fn with_registered_global_hint<R>(
    meta: RegisteredGlobalMeta,
    f: impl FnOnce() -> R,
) -> R {
    REGISTERED_GLOBAL_HINT.with(|cell| {
        let previous = cell.replace(Some(meta));
        let result = f();
        cell.replace(previous);
        result
    })
}

fn take_registered_global_hint() -> Option<RegisteredGlobalMeta> {
    REGISTERED_GLOBAL_HINT.with(|cell| cell.replace(None))
}

pub(crate) struct RegisteredGlobalTelemetryGuard {
    previous: bool,
}

impl Drop for RegisteredGlobalTelemetryGuard {
    fn drop(&mut self) {
        REGISTERED_GLOBAL_SUPPRESSION.with(|cell| {
            cell.replace(self.previous);
        });
    }
}

pub(crate) fn suppress_registered_global_logging() -> RegisteredGlobalTelemetryGuard {
    let previous = REGISTERED_GLOBAL_SUPPRESSION.with(|cell| cell.replace(true));
    RegisteredGlobalTelemetryGuard { previous }
}

pub(crate) fn with_suppressed_registered_globals<R>(f: impl FnOnce() -> R) -> R {
    let _guard = suppress_registered_global_logging();
    f()
}

fn registered_global_telemetry_suppressed() -> bool {
    REGISTERED_GLOBAL_SUPPRESSION.with(|cell| *cell.borrow())
}

pub(super) fn handle_from_value(value: &Value) -> Option<String> {
    match value {
        Value::Function(func) => Some(ptr_to_handle(func.to_pointer())),
        Value::Table(table) => Some(ptr_to_handle(table.to_pointer())),
        Value::UserData(data) => Some(ptr_to_handle(data.to_pointer())),
        Value::String(text) => Some(ptr_to_handle(text.to_pointer())),
        Value::Thread(thread) => Some(ptr_to_handle(thread.to_pointer())),
        _ => None,
    }
}

/// Logs a semantic table entry mutation and performs the actual set on the table.
pub(crate) fn set_table_entry_with_telemetry<'lua>(
    table: &Table<'lua>,
    table_handle: &str,
    table_fields: &ValueFields,
    table_label: Option<&String>,
    key: Value<'lua>,
    value: Value<'lua>,
    value_handle_label: Option<String>,
    note: Option<String>,
) -> LuaResult<()> {
    let key_preview = value_to_upvalue_preview(&key);
    let value_preview = value_to_upvalue_preview(&value);
    let value_fields = value_fields_from_lua(&value);
    let value_handle = handle_from_value(&value);
    log_set_table_entry(
        table_handle.to_string(),
        table_label.cloned(),
        key_preview,
        value_preview,
        note,
        Some(table_fields.clone()),
        value_handle.map(|handle| (handle, value_handle_label, value_fields.clone())),
    );
    table.set(key, value)
}

pub(super) fn set_global<'lua, T: IntoLua<'lua>>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    value: T,
) -> LuaResult<()> {
    let value = value.into_lua(lua)?;
    let hint = take_registered_global_hint();

    if registered_global_telemetry_suppressed() {
        return globals.set(name, value);
    }

    let value_fields = value_fields_from_lua(&value);
    let upvalues = hint.as_ref().map(|meta| meta.upvalues).unwrap_or(0);

    let handle_label = format!("global:{name}");
    let handle = normalize_handle(&handle_label, handle_from_value(&value));

    if let Value::Table(table) = &value {
        register_table_label(table.to_pointer(), handle_label.clone());
    }

    let mut origin = OriginFields::default();
    match &value {
        Value::Function(func) => {
            origin = origin_fields_for_ptr(func.to_pointer());
            log_push_cclosure("lua_pushCclosure", func.to_pointer(), upvalues, None);
        }
        Value::Nil => {
            log_push_nil();
        }
        Value::Integer(num) => {
            log_push_number(&format_number_for_log(*num as f64));
        }
        Value::Number(num) => {
            log_push_number(&format_number_for_log(*num));
        }
        Value::String(text) => {
            let bytes = text.as_bytes();
            let rendered = String::from_utf8_lossy(bytes).into_owned();
            let preview = truncate_for_log(&rendered, LOG_PREVIEW_MAX_LEN);
            log_push_string(bytes.len(), preview);
        }
        Value::Table(_) => {
            log_push_object(handle.clone(), value_fields.clone());
        }
        _ => {}
    }

    log_lua_setglobal(
        name,
        handle.clone(),
        Some(handle_label.clone()),
        value_fields.clone(),
        origin.clone(),
    );

    if matches!(value, Value::Function(_)) {
        log_registered_global(
            name,
            handle,
            Some(handle_label),
            upvalues,
            value_fields,
            origin,
        );
    } else {
        log_registered_constant(name, handle, Some(handle_label), value_fields, origin);
    }

    globals.set(name, value)
}

pub(crate) fn describe_callable_label(value: &Value) -> String {
    match value {
        Value::String(text) => text.to_str().unwrap_or("<string>").to_string(),
        Value::Function(_) => "<function>".to_string(),
        Value::Table(_) => "<table>".to_string(),
        other => describe_value(other),
    }
}

pub(crate) fn describe_value(value: &Value) -> String {
    match value {
        Value::Nil => "<nil>".to_string(),
        Value::Boolean(flag) => flag.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(text) => text.to_str().unwrap_or("<string>").to_string(),
        Value::Table(_) => "<table>".to_string(),
        Value::Function(_) => "<function>".to_string(),
        Value::Thread(_) => "<thread>".to_string(),
        Value::UserData(_) => "<userdata>".to_string(),
        Value::Error(err) => err.to_string(),
        Value::LightUserData(_) => "<lightuserdata>".to_string(),
    }
}

pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_str().ok()?.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

pub(crate) fn value_fields_from_lua(value: &Value) -> ValueFields {
    value_fields_from_meta(&value_meta_from_lua(value))
}

pub(crate) fn value_to_upvalue_preview(value: &Value) -> UpvaluePreview {
    upvalue_preview_from_meta(&value_meta_from_lua(value))
}

fn value_meta_from_lua(value: &Value) -> ValueMeta {
    match value {
        Value::Nil => ValueMeta {
            kind: ValueType::Nil,
            ..ValueMeta::default()
        },
        Value::Boolean(flag) => ValueMeta {
            kind: ValueType::Unknown,
            value: Some(flag.to_string()),
            ..ValueMeta::default()
        },
        Value::Integer(num) => ValueMeta {
            kind: ValueType::Number,
            value: Some(num.to_string()),
            ..ValueMeta::default()
        },
        Value::Number(num) => ValueMeta {
            kind: ValueType::Number,
            value: Some(format_number_for_log(*num)),
            ..ValueMeta::default()
        },
        Value::String(text) => {
            let rendered = String::from_utf8_lossy(text.as_bytes());
            ValueMeta {
                kind: ValueType::String,
                value_len: Some(text.as_bytes().len()),
                preview: Some(truncate_for_log(&rendered, LOG_PREVIEW_MAX_LEN)),
                ..ValueMeta::default()
            }
        }
        Value::Table(_) => ValueMeta {
            kind: ValueType::Table,
            ..ValueMeta::default()
        },
        Value::Function(func) => {
            let info = func.info();
            let what = info.what.as_ref();
            let kind = match what {
                "C" | "Rust" => ValueType::Cfunction,
                _ => ValueType::Function,
            };
            let ptr = func.to_pointer();
            ValueMeta {
                kind,
                func: Some(handle_hex(ptr as usize)),
                ..ValueMeta::default()
            }
        }
        Value::UserData(data) => {
            let mut meta = ValueMeta {
                kind: ValueType::Userdata,
                ..ValueMeta::default()
            };
            if let Ok(handle) = data.borrow::<TaggedHandle>() {
                meta.tag = Some(handle.tag);
            } else if let Ok(color) = data.borrow::<ColorHandle>() {
                meta.tag = Some(COLOR_TAG);
                meta.value = Some(format!("0x{:06x}", color.encoded()));
            }
            meta
        }
        Value::Thread(_) | Value::LightUserData(_) | Value::Error(_) => ValueMeta {
            kind: ValueType::Unknown,
            ..ValueMeta::default()
        },
    }
}
