use crate::{
    logging::{log_boot_sequence_complete, log_event, log_line, LuaEvent, OriginFields},
    lua_api::{
        call_real_lua_call, call_real_lua_callfunction, call_real_lua_collectgarbage,
        call_real_lua_dobuffer, call_real_lua_dofile, call_real_lua_dostring, call_real_lua_error,
    },
    telemetry,
};
use grim_telemetry_schema::trace_utils::cstr_opt;
use grim_telemetry_schema::trace_utils::truncate_for_log;
use libc::{c_char, c_int, size_t};
use std::ffi::c_void;

use super::{
    callfunction_tracker, handle_hex, origin_fields, record_non_push_event,
    remember_handle_label_if_missing, resolve_lua_function_label, LOG_PREVIEW_MAX_LEN,
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
    let label = cstr_opt(path).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dofile {
        path: label.clone(),
    });
    if is_system_boot_script(&label) {
        log_boot_sequence_complete(None);
    }
    forward_int_result("lua_dofile", call_real_lua_dofile(path))
}

/// Executes a Lua string chunk while logging and forwarding to the retail VM.
pub(crate) unsafe fn trace_lua_dostring(chunk: *const c_char) -> c_int {
    record_non_push_event();
    let snippet = cstr_opt(chunk)
        .map(|s| truncate_for_log(&s, LOG_PREVIEW_MAX_LEN))
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
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dobuffer { name: label, size });
    forward_int_result("lua_dobuffer", call_real_lua_dobuffer(buffer, size, name))
}

/// Traces a `lua_call` invocation by name.
pub(crate) unsafe fn trace_lua_call(name: *const c_char) -> c_int {
    record_non_push_event();
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
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
            handle: handle_hex(handle as usize),
            label: label.clone(),
            calls: Some(sample.count),
            note: None,
            ref_id: sample.ref_meta.as_ref().map(|meta| meta.ref_id),
            ref_alias: sample.ref_meta.as_ref().and_then(|meta| meta.alias.clone()),
            ref_value_kind: sample
                .ref_meta
                .as_ref()
                .and_then(|meta| meta.value_kind.clone()),
            origin: origin_fields(sample.origin.as_ref()),
        });
    } else {
        log_line("lua_callfunction tracker mutex poisoned; falling back to minimal log");
        log_event(LuaEvent::CallFunc {
            handle: handle_hex(handle as usize),
            label: label.clone(),
            calls: None,
            note: Some("tracker_poisoned".to_string()),
            ref_id: None,
            ref_alias: None,
            ref_value_kind: None,
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
    let text = cstr_opt(message)
        .map(|s| truncate_for_log(&s, 120))
        .unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::LuaError { message: text });
    if !call_real_lua_error(message) {
        log_line("lua_error symbol missing; unable to propagate error to Lua VM");
    }
}

fn is_system_boot_script(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    normalized.ends_with("_system.lua") || normalized.ends_with("_system.decompiled.lua")
}
