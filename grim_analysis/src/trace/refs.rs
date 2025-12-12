use crate::{
    logging::{log_event, log_line, LuaEvent, LuaSemanticEvent, OriginFields},
    lua_api::{
        call_real_lua_getcfunction, call_real_lua_getref, call_real_lua_ref, call_real_lua_unref,
        LuaObject,
    },
};
use libc::c_int;
use std::ffi::c_void;

use super::{
    callfunction_tracker, describe_lua_value, handle_hex, origin_fields, record_non_push_event,
    remember_handle_label_if_missing, resolve_lua_function_label, value_fields_from_details,
    ClosureOrigin,
};

/// Traces releasing a Lua reference and emits both semantic and raw events.
pub(crate) unsafe fn trace_lua_unref(reference: c_int) {
    record_non_push_event();
    let note = if call_real_lua_unref(reference) {
        None
    } else {
        Some("lua_unref_missing".to_string())
    };
    super::log_semantic_event(LuaSemanticEvent::SemanticUnref {
        reference,
        note: note.clone(),
    });
    log_event(LuaEvent::Unref { reference, note });
}

/// Traces storing the top-of-stack value in the Lua reference table.
pub(crate) unsafe fn trace_lua_ref(lock: c_int) -> c_int {
    record_non_push_event();
    match call_real_lua_ref(lock) {
        Some(reference) => {
            let handle = call_real_lua_getref(reference);
            match handle {
                Some(handle) => {
                    let label = resolve_lua_function_label(handle);
                    let origin = call_real_lua_getcfunction(handle)
                        .map(|func| ClosureOrigin::new(func as *const c_void));
                    let value_fields = describe_lua_value(handle)
                        .map(|details| value_fields_from_details(&details));
                    if let Ok(mut tracker) = callfunction_tracker().lock() {
                        tracker.remember_label_if_missing(handle, format!("ref:{reference}"));
                        if let Some(origin) = origin.clone() {
                            tracker.remember_origin_if_missing(handle, origin);
                        }
                    } else {
                        log_line("lua_ref tracker mutex poisoned; skipping cache update");
                    }
                    remember_handle_label_if_missing(handle, label.clone());
                    let handle_hex_str = handle_hex(handle as usize);
                    let origin_fields = origin_fields(origin.as_ref());
                    super::log_semantic_event(LuaSemanticEvent::SemanticStoreRef {
                        lock,
                        reference,
                        handle: Some(handle_hex_str.clone()),
                        label: Some(label.clone()),
                        value_fields: value_fields.clone(),
                        note: None,
                        origin: origin_fields.clone(),
                    });
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some(handle_hex_str),
                        label: Some(label),
                        value_fields,
                        note: None,
                        origin: origin_fields,
                    });
                }
                None => {
                    super::log_semantic_event(LuaSemanticEvent::SemanticStoreRef {
                        lock,
                        reference,
                        handle: Some("<unknown>".to_string()),
                        label: Some(format!("ref:{reference}")),
                        value_fields: None,
                        note: Some("lua_getref_missing".to_string()),
                        origin: OriginFields::default(),
                    });
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some("<unknown>".to_string()),
                        label: Some(format!("ref:{reference}")),
                        value_fields: None,
                        note: Some("lua_getref_missing".to_string()),
                        origin: OriginFields::default(),
                    });
                }
            }
            reference
        }
        None => {
            log_line("lua_ref symbol missing; returning failure to keep engine alive");
            -1
        }
    }
}

/// Traces fetching a value from the Lua reference table.
pub(crate) unsafe fn trace_lua_getref(reference: c_int) -> LuaObject {
    record_non_push_event();
    match call_real_lua_getref(reference) {
        Some(handle) => {
            let label = resolve_lua_function_label(handle);
            let origin = call_real_lua_getcfunction(handle)
                .map(|func| ClosureOrigin::new(func as *const c_void));
            if let Ok(mut tracker) = callfunction_tracker().lock() {
                tracker.remember_label_if_missing(handle, format!("ref:{reference}"));
                if let Some(origin) = origin.clone() {
                    tracker.remember_origin_if_missing(handle, origin);
                }
            } else {
                log_line("lua_getref tracker mutex poisoned; skipping cache update");
            }
            remember_handle_label_if_missing(handle, label.clone());
            let handle_hex_str = handle_hex(handle as usize);
            let origin_fields = origin_fields(origin.as_ref());
            super::log_semantic_event(LuaSemanticEvent::SemanticFetchRef {
                reference,
                handle: Some(handle_hex_str.clone()),
                label: Some(label.clone()),
                note: None,
                origin: origin_fields.clone(),
            });
            log_event(LuaEvent::FetchRef {
                reference,
                handle: Some(handle_hex_str),
                label: Some(label),
                note: None,
                origin: origin_fields,
            });
            handle
        }
        None => {
            super::log_semantic_event(LuaSemanticEvent::SemanticFetchRef {
                reference,
                handle: Some("<unknown>".to_string()),
                label: None,
                note: Some("lua_getref_symbol_missing".to_string()),
                origin: OriginFields::default(),
            });
            log_event(LuaEvent::FetchRef {
                reference,
                handle: Some("<unknown>".to_string()),
                label: None,
                note: Some("lua_getref_symbol_missing".to_string()),
                origin: OriginFields::default(),
            });
            0
        }
    }
}
