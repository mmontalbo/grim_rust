use std::ptr;

use mlua::{IntoLua, Lua, Result as LuaResult, Table, Value};

use crate::lua_host::telemetry::{log_bind_global, log_push_cclosure};

pub(super) fn set_global<'lua, T: IntoLua<'lua>>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    name: &str,
    value: T,
) -> LuaResult<()> {
    let value = value.into_lua(lua)?;
    if let Value::Function(ref func) = value {
        let ptr = func.to_pointer();
        log_push_cclosure("lua_pushCclosure", ptr);
        log_bind_global(name, ptr);
    } else {
        log_bind_global(name, ptr::null());
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
