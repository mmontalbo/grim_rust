use crate::{
    logging::{log_event, log_line, LuaEvent, OriginFields},
    lua_api::{
        call_real_lua_call, call_real_lua_callfunction, call_real_lua_collectgarbage,
        call_real_lua_dobuffer, call_real_lua_dofile, call_real_lua_dostring, call_real_lua_error,
    },
    telemetry,
};
use libc::{c_char, c_int, size_t};
use std::ffi::c_void;

use super::{
    callfunction_tracker, origin_fields, record_non_push_event, remember_handle_label_if_missing,
    resolve_lua_function_label,
};

/// Normalizes optional integer returns, logging when symbols are missing.
fn forward_int_result(label: &str, result: Option<c_int>) -> c_int {
    match result {
        Some(value) => value,
        None => {
            log_line(&format!(
                "{} symbol missing; returning failure to keep engine alive",
                label
            ));
            -1
        }
    }
}

/// Executes a Lua file while logging the call and forwarding to the retail VM.
pub(crate) unsafe fn trace_lua_dofile(path: *const c_char) -> c_int {
    record_non_push_event();
    let label = super::cstr_opt(path).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dofile { path: label });
    forward_int_result("lua_dofile", call_real_lua_dofile(path))
}

/// Executes a Lua string chunk while logging and forwarding to the retail VM.
pub(crate) unsafe fn trace_lua_dostring(chunk: *const c_char) -> c_int {
    record_non_push_event();
    let snippet = super::cstr_opt(chunk)
        .map(|s| super::truncate_for_log(&s, 80))
        .unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dostring { snippet });
    forward_int_result("lua_dostring", call_real_lua_dostring(chunk))
}

pub(crate) unsafe fn trace_lua_dobuffer(
    buffer: *const c_char,
    size: size_t,
    name: *const c_char,
) -> c_int {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dobuffer { name: label, size });
    forward_int_result("lua_dobuffer", call_real_lua_dobuffer(buffer, size, name))
}

/// Traces a `lua_call` invocation by name.
pub(crate) unsafe fn trace_lua_call(name: *const c_char) -> c_int {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::Call { name: label });
    forward_int_result("lua_call", call_real_lua_call(name))
}

/// Traces a `lua_callfunction` invocation by handle, recording metadata and call counts.
pub(crate) unsafe fn trace_lua_callfunction(func: *mut c_void) -> c_int {
    record_non_push_event();
    let handle = func as usize as crate::lua_api::LuaObject;
    let label = resolve_lua_function_label(handle);
    remember_handle_label_if_missing(handle, label.clone());

    if let Ok(mut tracker) = callfunction_tracker().lock() {
        let sample = tracker.record(handle, &label);
        log_event(LuaEvent::CallFunc {
            handle: format!("0x{handle:08x}"),
            label: label.clone(),
            handle_label: None,
            calls: Some(sample.count),
            note: None,
            origin: origin_fields(sample.origin.as_ref()),
        });
    } else {
        log_line("lua_callfunction tracker mutex poisoned; falling back to minimal log");
        log_event(LuaEvent::CallFunc {
            handle: format!("0x{handle:08x}"),
            label: label.clone(),
            handle_label: None,
            calls: None,
            note: Some("tracker_poisoned".to_string()),
            origin: OriginFields::default(),
        });
    }

    forward_int_result("lua_callfunction", call_real_lua_callfunction(handle))
}

/// Traces an explicit GC invocation.
pub(crate) unsafe fn trace_lua_collectgarbage() {
    record_non_push_event();
    if call_real_lua_collectgarbage() {
        log_event(LuaEvent::CollectGarbage {});
    }
}

/// Traces a `lua_error` call, including the truncated error message.
pub(crate) unsafe fn trace_lua_error(message: *const c_char) {
    record_non_push_event();
    let text = super::cstr_opt(message)
        .map(|s| super::truncate_for_log(&s, 120))
        .unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::LuaError { message: text });
    if !call_real_lua_error(message) {
        log_line("lua_error symbol missing; unable to propagate error to Lua VM");
    }
}
