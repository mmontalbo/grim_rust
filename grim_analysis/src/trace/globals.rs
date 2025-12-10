use crate::{
    logging::{log_event, log_event_with_seq, log_line, LuaEvent, ValueFields, ValueType},
    lua_api::{
        call_real_lua_getcfunction, call_real_lua_getglobal, call_real_lua_rawgetglobal,
        call_real_lua_rawsetglobal, call_real_lua_setglobal, LuaObject,
    },
};
use libc::c_char;
use std::ffi::c_void;

use super::{
    callfunction_tracker, describe_lua_value, emit_registered_constant, emit_registered_global,
    origin_fields, record_non_push_event, remember_handle_label, take_registered_global_candidate,
    value_fields_from_details, ClosureOrigin,
};

/// Traces a raw global read (no metamethods) and records the returned handle/value.
pub(crate) unsafe fn trace_lua_rawgetglobal(name: *const c_char) -> LuaObject {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    match call_real_lua_rawgetglobal(name) {
        Some(handle) => {
            let handle_label = format!("global:{label}");
            remember_handle_label(handle, handle_label.clone());
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::RawGetGlobal {
                name: label.clone(),
                handle: format!("0x{handle:08x}"),
                label: Some(handle_label),
                values,
            });
            handle
        }
        None => {
            log_line("lua_rawgetglobal symbol missing; returning null handle");
            0
        }
    }
}

/// Traces a raw global write (no metamethods) and emits metadata about the stored value.
pub(crate) unsafe fn trace_lua_rawsetglobal(name: *const c_char) {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    let mut handle_field = None;
    let mut values = ValueFields::default();
    let mut note = None;
    let mut computed_label = None;
    let caller = super::caller_origin_fields();
    if call_real_lua_rawsetglobal(name) {
        if let Some(handle) = call_real_lua_rawgetglobal(name) {
            handle_field = Some(format!("0x{handle:08x}"));
            let resolved_label = format!("global:{label}");
            computed_label = Some(resolved_label.clone());
            remember_handle_label(handle, resolved_label);
            if let Some(details) = describe_lua_value(handle) {
                values = value_fields_from_details(&details);
            }
        } else {
            note = Some("lua_rawgetglobal_missing_after_set".to_string());
        }
    } else {
        note = Some("lua_rawsetglobal_missing".to_string());
    }
    log_event(LuaEvent::RawSetGlobal {
        name: label,
        handle: handle_field,
        label: computed_label,
        values,
        note,
        caller,
    });
}

/// Traces a global set, emitting semantic bind events for functions/constants.
pub(crate) unsafe fn trace_lua_setglobal(name: *const c_char) {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    if call_real_lua_setglobal(name) {
        if let Some(handle) = call_real_lua_getglobal(name) {
            let func_ptr = call_real_lua_getcfunction(handle);
            let origin = func_ptr.map(|func| ClosureOrigin::new(func as *const c_void));
            let value_fields = describe_lua_value(handle);
            let handle_label = format!("global:{label}");

            if let Ok(mut tracker) = callfunction_tracker().lock() {
                tracker.remember_label(handle, handle_label.clone());
                if let Some(origin) = origin.clone() {
                    tracker.remember_origin(handle, origin);
                }
            } else {
                log_line("lua_setglobal tracker mutex poisoned; skipping cache update");
            }
            remember_handle_label(handle, handle_label.clone());

            let values = value_fields
                .as_ref()
                .map(value_fields_from_details)
                .unwrap_or_default();
            let is_closure = matches!(
                values.value_type,
                Some(ValueType::Cfunction | ValueType::Function)
            );

            log_event_with_seq(LuaEvent::BindGlobal {
                name: label.clone(),
                handle: format!("0x{handle:08x}"),
                label: Some(handle_label.clone()),
                values: values.clone(),
                origin: origin_fields(origin.as_ref()),
            });

            if is_closure {
                // If this closure was just pushed, consume the pending candidate to emit a
                // SemanticBindGlobal (with upvalue count + origin) instead of a constant bind.
                if let Some(func_addr) = func_ptr.map(|func| func as *const c_void as usize) {
                    if let Some(mut candidate) = take_registered_global_candidate(func_addr) {
                        let merged_origin = candidate.origin.take().or(origin.clone());
                        emit_registered_global(
                            &label,
                            handle,
                            handle_label.clone(),
                            candidate.upvalues,
                            values,
                            merged_origin,
                        );
                        return;
                    }
                }
            }

            emit_registered_constant(&label, handle, handle_label, values, origin);
        }
    }
}

/// Traces a global read and increments access counters for the symbol.
pub(crate) unsafe fn trace_lua_getglobal(name: *const c_char) -> LuaObject {
    record_non_push_event();
    let label = super::cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    let handle = match call_real_lua_getglobal(name) {
        Some(handle) => handle,
        None => {
            log_line("lua_getglobal symbol missing; returning null handle");
            return 0;
        }
    };

    if let Ok(mut tracker) = super::global_access_tracker().lock() {
        let count = tracker.record(&label);
        let handle_label = format!("global:{label}");
        remember_handle_label(handle, handle_label.clone());
        log_event(LuaEvent::GetGlobal {
            name: label.clone(),
            handle: format!("0x{handle:08x}"),
            label: handle_label,
            count,
        });
    } else {
        log_line("lua_getglobal tracker mutex poisoned; skipping access log");
    }

    handle
}
