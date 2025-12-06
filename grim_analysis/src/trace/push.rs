use crate::{
    logging::{log_line, LuaEvent, UpvaluePreview, ValueFields, ValueType},
    lua_api::{
        call_real_lua_getparam, call_real_lua_push_c_closure, call_real_lua_pushlstring,
        call_real_lua_pushnil, call_real_lua_pushnumber, call_real_lua_pushobject,
        call_real_lua_pushstring, call_real_lua_pushusertag, call_real_lua_pushvalue, LuaCFunction,
        LuaObject,
    },
    telemetry,
};
use libc::{c_char, c_int, size_t};
use std::ffi::c_void;

use super::{
    caller_origin_fields, describe_lua_value, format_number_for_log, handle_label_for,
    log_event_with_seq, origin_fields, record_non_push_event, record_push_preview,
    remember_registered_global_candidate, truncate_for_log, upvalue_preview_from_details,
    value_fields_from_details, ClosureOrigin,
};

pub(crate) unsafe fn trace_lua_push_closure(label: &str, func: LuaCFunction, upvalues: c_int) {
    let func_addr = func as *const c_void as usize;
    let origin = Some(ClosureOrigin::new(func as *const c_void));
    let event = LuaEvent::PushCclosure {
        name: label.to_string(),
        func: format!("0x{func_addr:08x}"),
        upvalues,
        origin: origin_fields(origin.as_ref()),
    };
    let closure_log_seq = log_event_with_seq(event);
    record_push_preview(
        closure_log_seq,
        UpvaluePreview {
            kind: ValueType::Cfunction,
            value: Some(format!("0x{func_addr:08x}")),
            value_len: None,
            preview: None,
            tag: None,
        },
        None,
    );
    remember_registered_global_candidate(func_addr, upvalues, origin.clone());
    record_non_push_event();

    if !call_real_lua_push_c_closure(func, upvalues) {
        log_line("unable to forward lua_pushCclosure call; retail VM may misbehave");
    }
}

pub(crate) unsafe fn trace_lua_pushnumber(value: f32) {
    let rendered = format_number_for_log(value as f64);
    telemetry::record_pushed_number(value.into());
    let log_seq = log_event_with_seq(LuaEvent::PushNumber {
        value: rendered.clone(),
    });
    record_push_preview(
        log_seq,
        UpvaluePreview {
            kind: ValueType::Number,
            value: Some(rendered.clone()),
            value_len: None,
            preview: None,
            tag: None,
        },
        None,
    );
    if !call_real_lua_pushnumber(value) {
        log_line("lua_pushnumber symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushnil() {
    telemetry::record_pushed_nil();
    let log_seq = log_event_with_seq(LuaEvent::PushNil {});
    record_push_preview(
        log_seq,
        UpvaluePreview {
            kind: ValueType::Nil,
            value: None,
            value_len: None,
            preview: None,
            tag: None,
        },
        None,
    );
    if !call_real_lua_pushnil() {
        log_line("lua_pushnil symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushstring(value: *const c_char) {
    let text = super::cstr_opt(value).unwrap_or_else(|| "<null>".to_string());
    let preview = truncate_for_log(&text, 80);
    let log_seq = log_event_with_seq(LuaEvent::PushString {
        len: text.len(),
        preview: preview.clone(),
    });
    record_push_preview(
        log_seq,
        UpvaluePreview {
            kind: ValueType::String,
            value: None,
            value_len: Some(text.len()),
            preview: Some(preview),
            tag: None,
        },
        None,
    );
    if !call_real_lua_pushstring(value) {
        log_line("lua_pushstring symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushlstring(value: *const c_char, len: size_t) {
    let text = if value.is_null() {
        "<null>".to_string()
    } else {
        let bytes = std::slice::from_raw_parts(value as *const u8, len as usize);
        String::from_utf8_lossy(bytes).into_owned()
    };
    let preview = truncate_for_log(&text, 80);
    let log_seq = log_event_with_seq(LuaEvent::PushLstring {
        len: len as usize,
        preview: preview.clone(),
    });
    record_push_preview(
        log_seq,
        UpvaluePreview {
            kind: ValueType::String,
            value: None,
            value_len: Some(text.len()),
            preview: Some(preview),
            tag: None,
        },
        None,
    );
    if !call_real_lua_pushlstring(value, len) {
        log_line("lua_pushlstring symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushusertag(id: c_int, tag: c_int) {
    let mut values = ValueFields::default();
    values.value_type = Some(ValueType::Userdata);
    values.tag = Some(tag);
    let caller = caller_origin_fields();
    let log_seq = log_event_with_seq(LuaEvent::PushUsertag { id, values, caller });
    record_push_preview(
        log_seq,
        UpvaluePreview {
            kind: ValueType::Userdata,
            value: None,
            value_len: None,
            preview: None,
            tag: Some(tag),
        },
        None,
    );
    if !call_real_lua_pushusertag(id, tag) {
        log_line("lua_pushusertag symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushobject(object: LuaObject) {
    let value_details = describe_lua_value(object);
    let values = value_details
        .as_ref()
        .map(|value| value_fields_from_details(value))
        .unwrap_or_default();
    let log_seq = log_event_with_seq(LuaEvent::PushObject {
        handle: format!("0x{object:08x}"),
        handle_label: handle_label_for(object),
        values,
    });
    if let Some(preview) = value_details
        .as_ref()
        .map(|value| upvalue_preview_from_details(value))
    {
        record_push_preview(log_seq, preview, Some(object));
    }
    if !call_real_lua_pushobject(object) {
        log_line("lua_pushobject symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushvalue(index: c_int) {
    let source = call_real_lua_getparam(index);
    let mut values = ValueFields::default();
    let mut preview = None;
    let mut note = None;
    if let Some(handle) = source {
        if let Some(details) = describe_lua_value(handle) {
            values = value_fields_from_details(&details);
            preview = Some(upvalue_preview_from_details(&details));
        } else {
            note = Some("value_unknown".to_string());
        }
    } else {
        note = Some(if index < 0 {
            "source_missing_or_out_of_range".to_string()
        } else {
            "source_missing".to_string()
        });
    }
    let log_seq = log_event_with_seq(LuaEvent::PushValue {
        index,
        note: note.clone(),
        values: values.clone(),
    });
    let effective_preview = preview.unwrap_or(UpvaluePreview {
        kind: ValueType::Unknown,
        value: None,
        value_len: None,
        preview: None,
        tag: None,
    });
    record_push_preview(log_seq, effective_preview, source);
    if !call_real_lua_pushvalue(index) {
        log_line("lua_pushvalue symbol missing; skipping push");
    }
}
