use std::cell::RefCell;

use grim_telemetry_common::{OriginFields, UpvaluePreview, ValueFields, ValueType};
use mlua::{IntoLua, Lua, Result as LuaResult, Table, UserData, Value};

use crate::lua_host::telemetry::{
    log_lua_setglobal, log_push_cclosure, log_push_nil, log_push_number, log_push_object,
    log_push_string, log_registered_constant, log_registered_global, normalize_handle,
    origin_fields_for_ptr, ptr_to_handle, register_table_label,
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

thread_local! {
    static REGISTERED_GLOBAL_HINT: RefCell<Option<RegisteredGlobalMeta>> = RefCell::new(None);
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

pub(super) fn set_global<'lua, T: IntoLua<'lua>>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    value: T,
) -> LuaResult<()> {
    let value = value.into_lua(lua)?;
    let value_fields = value_fields_from_lua(&value);
    let hint = take_registered_global_hint();
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
            let preview = truncate_for_log(&rendered, 80);
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

pub(super) fn set_global_silent<'lua, T: IntoLua<'lua>>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    value: T,
) -> LuaResult<()> {
    let value = value.into_lua(lua)?;
    globals.set(name, value)
}

pub(super) fn value_to_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Integer(i) if *i >= 0 => Some(*i as u32),
        Value::Number(n) if *n >= 0.0 => Some(n.trunc() as u32),
        Value::String(text) => text.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
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
    let mut fields = ValueFields::default();
    match value {
        Value::Nil => {
            fields.value_type = Some(ValueType::Nil);
        }
        Value::Boolean(flag) => {
            fields.value_type = Some(ValueType::Unknown);
            fields.value = Some(flag.to_string());
        }
        Value::Integer(num) => {
            fields.value_type = Some(ValueType::Number);
            fields.value = Some(num.to_string());
        }
        Value::Number(num) => {
            fields.value_type = Some(ValueType::Number);
            fields.value = Some(format_number_for_log(*num));
        }
        Value::String(text) => {
            fields.value_type = Some(ValueType::String);
            let bytes = text.as_bytes();
            let rendered = String::from_utf8_lossy(bytes).into_owned();
            fields.value_len = Some(bytes.len());
            fields.value_preview = Some(truncate_for_log(&rendered, 80));
        }
        Value::Table(_table) => {
            fields.value_type = Some(ValueType::Table);
        }
        Value::Function(func) => {
            let info = func.info();
            let what = info.what.as_ref();
            fields.value_type = Some(match what {
                "C" | "Rust" => ValueType::Cfunction,
                _ => ValueType::Function,
            });
            let ptr = func.to_pointer();
            fields.func = Some(format!("0x{:08x}", ptr as usize));
        }
        Value::UserData(data) => {
            fields.value_type = Some(ValueType::Userdata);
            if let Ok(handle) = data.borrow::<TaggedHandle>() {
                fields.tag = Some(handle.tag);
            }
        }
        Value::Thread(_) | Value::LightUserData(_) | Value::Error(_) => {
            fields.value_type = Some(ValueType::Unknown);
        }
    }
    fields
}

pub(crate) fn value_to_upvalue_preview(value: &Value) -> UpvaluePreview {
    let fields = value_fields_from_lua(value);
    UpvaluePreview {
        kind: fields.value_type.unwrap_or(ValueType::Unknown),
        value: fields.value,
        value_len: fields.value_len,
        preview: fields.value_preview,
        tag: fields.tag,
    }
}

fn format_number_for_log(value: f64) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn truncate_for_log(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut truncated = text[..max_len].to_string();
    truncated.push_str("...");
    truncated
}
