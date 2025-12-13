use crate::{
    logging::{log_event, log_line, LuaEvent, LuaSemanticEvent, OriginFields, ValueFields},
    lua_api::{
        call_real_lua_copytagmethods, call_real_lua_getcfunction, call_real_lua_getparam,
        call_real_lua_newtag, call_real_lua_setfallback, call_real_lua_settag,
        call_real_lua_settagmethod, LuaCFunction, LuaObject,
    },
};
use grim_telemetry_common::trace_utils::{cstr_opt, register_tag_alias, tag_alias};
use libc::{c_char, c_int};
use std::ffi::c_void;

use super::{
    caller_origin_fields, describe_lua_value, handle_hex, origin_fields, record_non_push_event,
    remember_handle_label, value_fields_from_details, ClosureOrigin,
};

/// Traces installing a fallback handler and emits both raw and semantic events.
pub(crate) unsafe fn trace_lua_setfallback(
    event_name: *const c_char,
    func: LuaCFunction,
) -> LuaObject {
    record_non_push_event();
    let name = cstr_opt(event_name).unwrap_or_else(|| "<null>".to_string());
    let origin = ClosureOrigin::new(func as *const c_void);
    let caller = caller_origin_fields();
    match call_real_lua_setfallback(event_name, func) {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            let handle_label = format!("fallback:{name}");
            remember_handle_label(handle, handle_label.clone());
            let handle_hex_str = handle_hex(handle as usize);
            let origin_fields = origin_fields(Some(&origin));
            super::log_semantic_event(LuaSemanticEvent::SemanticSetFallback {
                fallback: name.clone(),
                handle: handle_hex_str.clone(),
                values: values.clone(),
                origin: origin_fields.clone(),
                caller: caller.clone(),
            });
            log_event(LuaEvent::SetFallback {
                fallback: name,
                handle: handle_hex_str,
                values,
                origin: origin_fields,
                caller,
            });
            handle
        }
        None => {
            log_line("lua_setfallback symbol missing; returning null handle");
            0
        }
    }
}

/// Traces tag creation and records the assigned tag id.
pub(crate) unsafe fn trace_lua_newtag() -> c_int {
    record_non_push_event();
    let caller = caller_origin_fields();
    match call_real_lua_newtag() {
        Some(tag) => {
            let alias = derive_tag_alias(tag, &caller);
            register_tag_alias(tag, alias.clone());
            super::log_semantic_event(LuaSemanticEvent::SemanticTagAlias {
                tag,
                alias: alias.clone(),
                origin: caller.clone(),
            });
            log_event(LuaEvent::SetTag {
                tag,
                note: Some("created_via_newtag".to_string()),
                tag_alias: Some(alias),
            });
            tag
        }
        None => {
            log_line("lua_newtag symbol missing; returning 0");
            0
        }
    }
}

/// Traces copying tag methods between tags, noting the caller and result.
pub(crate) unsafe fn trace_lua_copytagmethods(tagto: c_int, tagfrom: c_int) -> c_int {
    record_non_push_event();
    match call_real_lua_copytagmethods(tagto, tagfrom) {
        Some(result) => {
            log_event(LuaEvent::CopyTagmethods {
                to: tagto,
                from: tagfrom,
                to_label: None,
                from_label: None,
                result: Some(result),
                caller: caller_origin_fields(),
            });
            result
        }
        None => {
            log_line("lua_copytagmethods symbol missing; returning 0");
            0
        }
    }
}

/// Traces setting a tag on the top-of-stack value.
pub(crate) unsafe fn trace_lua_settag(tag: c_int) {
    record_non_push_event();
    let note = if call_real_lua_settag(tag) {
        None
    } else {
        Some("lua_settag_missing".to_string())
    };
    log_event(LuaEvent::SetTag {
        tag,
        note,
        tag_alias: tag_alias(tag),
    });
}

/// Traces installing a tag method handler for a given event.
pub(crate) unsafe fn trace_lua_settagmethod(tag: c_int, event: *const c_char) {
    let event_label = cstr_opt(event).unwrap_or_else(|| "<null>".to_string());
    let top_handle = call_real_lua_getparam(-1);
    let mut values = ValueFields::default();
    let mut handle_field = None;
    let mut origin = None;
    if let Some(handle) = top_handle {
        handle_field = Some(handle_hex(handle as usize));
        if let Some(details) = describe_lua_value(handle) {
            values = value_fields_from_details(&details);
            if let Some(addr) = call_real_lua_getcfunction(handle) {
                origin = Some(ClosureOrigin::new(addr as *const c_void));
            }
        }
    }
    let origin_fields = origin_fields(origin.as_ref());
    let tag_alias_value = tag_alias(tag);
    super::log_semantic_event(LuaSemanticEvent::SemanticSetTagmethod {
        tag,
        event_name: event_label.clone(),
        handle: handle_field.clone(),
        values: values.clone(),
        tag_alias: tag_alias_value.clone(),
        origin: origin_fields.clone(),
    });
    if call_real_lua_settagmethod(tag, event) {
        log_event(LuaEvent::SetTagmethod {
            tag,
            event_name: event_label.clone(),
            handle: handle_field.clone(),
            values: values.clone(),
            tag_alias: tag_alias_value.clone(),
            origin: origin_fields.clone(),
        });
    } else {
        log_event(LuaEvent::SetTagmethod {
            tag,
            event_name: event_label.clone(),
            handle: handle_field,
            values,
            tag_alias: tag_alias_value,
            origin: origin_fields,
        });
    }
}

fn derive_tag_alias(tag: i32, origin: &OriginFields) -> String {
    if let Some(symbol) = origin.symbol.as_ref() {
        let alias = clean_alias(symbol);
        if !alias.is_empty() {
            return alias;
        }
    }
    if let Some(module) = origin.module.as_ref() {
        if let Some(last) = module.rsplit(['/', '\\']).next() {
            let alias = clean_alias(last);
            if !alias.is_empty() {
                return alias;
            }
        }
    }
    format!("tag_{tag}")
}

fn clean_alias(text: &str) -> String {
    let head = text
        .split(|c: char| c == '+' || c == '(')
        .next()
        .unwrap_or(text);
    let normalized: String = head
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    normalized.trim_matches('_').to_ascii_lowercase()
}
