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
    callfunction_tracker, forward_int_result, origin_fields, record_non_push_event,
    remember_handle_label_if_missing, resolve_lua_function_label,
};

pub(crate) unsafe fn trace_lua_dofile(path: *const c_char) -> c_int {
    record_non_push_event();
    let label = super::cstr_opt(path).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dofile { path: label });
    forward_int_result("lua_dofile", call_real_lua_dofile(path))
}

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

pub(crate) unsafe fn trace_lua_call(name: *const c_char) -> c_int {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::Call { name: label });
    forward_int_result("lua_call", call_real_lua_call(name))
}

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
            handle_label: Some(super::handle_label_for(handle).unwrap_or_else(|| label.clone())),
            calls: Some(sample.count),
            note: None,
            origin: origin_fields(sample.origin.as_ref()),
        });
    } else {
        log_line("lua_callfunction tracker mutex poisoned; falling back to minimal log");
        log_event(LuaEvent::CallFunc {
            handle: format!("0x{handle:08x}"),
            label: label.clone(),
            handle_label: Some(label.clone()),
            calls: None,
            note: Some("tracker_poisoned".to_string()),
            origin: OriginFields::default(),
        });
    }

    forward_int_result("lua_callfunction", call_real_lua_callfunction(handle))
}

pub(crate) unsafe fn trace_lua_collectgarbage() {
    record_non_push_event();
    if call_real_lua_collectgarbage() {
        log_event(LuaEvent::CollectGarbage {});
    }
}

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
