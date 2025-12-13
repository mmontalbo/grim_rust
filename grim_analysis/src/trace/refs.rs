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
    record_non_push_event_skip_ref_batch, record_ref_batch, ref_alias,
    remember_handle_label_if_missing, remember_ref_alias, resolve_lua_function_label,
    take_ref_alias_candidate, value_fields_from_details, ClosureOrigin, RefHandleMeta,
};

/// Traces releasing a Lua reference and emits both semantic and raw events.
pub(crate) unsafe fn trace_lua_unref(reference: c_int) {
    record_non_push_event();
    let note = if call_real_lua_unref(reference) {
        None
    } else {
        Some("lua_unref_missing".to_string())
    };
    let alias_meta = ref_alias(reference);
    let alias = alias_meta.as_ref().and_then(|meta| meta.alias.clone());
    let value_kind = alias_meta.and_then(|meta| meta.value_kind.clone());
    super::log_semantic_event(LuaSemanticEvent::SemanticUnref {
        reference,
        alias: alias.clone(),
        value_kind: value_kind.clone(),
        note: note.clone(),
        origin: OriginFields::default(),
    });
    log_event(LuaEvent::Unref { reference, note });
}

/// Traces storing the top-of-stack value in the Lua reference table.
pub(crate) unsafe fn trace_lua_ref(lock: c_int) -> c_int {
    record_non_push_event_skip_ref_batch();
    let alias = take_ref_alias_candidate();
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
                    let value_kind = value_fields
                        .as_ref()
                        .and_then(|fields| fields.value_type.clone());
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
                    remember_ref_alias(reference, alias.clone(), value_kind.clone());
                    if let Ok(mut tracker) = callfunction_tracker().lock() {
                        tracker.remember_ref_meta(
                            handle,
                            RefHandleMeta {
                                ref_id: reference,
                                alias: alias.clone(),
                                value_kind: value_kind.clone(),
                            },
                        );
                    }
                    super::log_semantic_event(LuaSemanticEvent::SemanticStoreRef {
                        lock,
                        reference,
                        handle: Some(handle_hex_str.clone()),
                        label: Some(label.clone()),
                        alias: alias.clone(),
                        value_kind: value_kind.clone(),
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
                    record_ref_batch(alias.as_deref(), reference);
                }
                None => {
                    super::log_semantic_event(LuaSemanticEvent::SemanticStoreRef {
                        lock,
                        reference,
                        handle: Some("<unknown>".to_string()),
                        label: Some(format!("ref:{reference}")),
                        alias: alias.clone(),
                        value_kind: None,
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
                    record_ref_batch(alias.as_deref(), reference);
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
            let value_fields =
                describe_lua_value(handle).map(|details| value_fields_from_details(&details));
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
            let alias_meta = ref_alias(reference);
            let alias = alias_meta.as_ref().and_then(|meta| meta.alias.clone());
            let value_kind = value_fields
                .as_ref()
                .and_then(|fields| fields.value_type.clone())
                .or_else(|| alias_meta.and_then(|meta| meta.value_kind.clone()));
            remember_ref_alias(reference, alias.clone(), value_kind.clone());
            if let Ok(mut tracker) = callfunction_tracker().lock() {
                tracker.remember_ref_meta(
                    handle,
                    RefHandleMeta {
                        ref_id: reference,
                        alias: alias.clone(),
                        value_kind: value_kind.clone(),
                    },
                );
            }
            super::log_semantic_event(LuaSemanticEvent::SemanticLoadRef {
                reference,
                handle: Some(handle_hex_str.clone()),
                label: Some(label.clone()),
                alias: alias.clone(),
                value_kind: value_kind.clone(),
                note: None,
                origin: origin_fields.clone(),
            });
            log_event(LuaEvent::LoadRef {
                reference,
                handle: Some(handle_hex_str),
                label: Some(label),
                alias,
                value_kind,
                note: None,
                origin: origin_fields,
            });
            handle
        }
        None => {
            super::log_semantic_event(LuaSemanticEvent::SemanticLoadRef {
                reference,
                handle: Some("<unknown>".to_string()),
                label: None,
                alias: None,
                value_kind: None,
                note: Some("lua_getref_symbol_missing".to_string()),
                origin: OriginFields::default(),
            });
            log_event(LuaEvent::LoadRef {
                reference,
                handle: Some("<unknown>".to_string()),
                label: None,
                alias: None,
                value_kind: None,
                note: Some("lua_getref_symbol_missing".to_string()),
                origin: OriginFields::default(),
            });
            0
        }
    }
}
