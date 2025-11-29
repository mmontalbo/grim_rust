use std::ptr;

use grim_telemetry_common::{ValueFields, ValueType};
use mlua::{IntoLua, Lua, Result as LuaResult, Table, Value};

use crate::lua_host::telemetry::{log_lua_setglobal, log_push_cclosure};

pub(super) fn set_global<'lua, T: IntoLua<'lua>>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    value: T,
) -> LuaResult<()> {
    let value = value.into_lua(lua)?;
    let value_fields = value_fields_from_lua(&value);

    if let Value::Function(ref func) = value {
        let ptr = func.to_pointer();
        log_push_cclosure("lua_pushCclosure", ptr);
        log_lua_setglobal(name, ptr, value_fields);
    } else {
        log_lua_setglobal(name, ptr::null(), value_fields);
    }
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

fn value_fields_from_lua(value: &Value) -> ValueFields {
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
        Value::Table(_) => {
            fields.value_type = Some(ValueType::Table);
        }
        Value::Function(func) => {
            fields.value_type = Some(ValueType::Cfunction);
            let ptr = func.to_pointer();
            fields.func = Some(format!("0x{:08x}", ptr as usize));
        }
        Value::UserData(_) => {
            fields.value_type = Some(ValueType::Userdata);
        }
        Value::Thread(_) | Value::LightUserData(_) | Value::Error(_) => {
            fields.value_type = Some(ValueType::Unknown);
        }
    }
    fields
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
