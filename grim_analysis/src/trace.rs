use crate::{
    logging::{
        log_line, LuaEvent, LuaSemanticEvent, OriginFields, UpvaluePreview, ValueFields, ValueType,
    },
    lua_api::{
        call_real_lua_call, call_real_lua_callfunction, call_real_lua_collectgarbage,
        call_real_lua_copytagmethods, call_real_lua_createtable, call_real_lua_dobuffer,
        call_real_lua_dofile, call_real_lua_dostring, call_real_lua_error,
        call_real_lua_getcfunction, call_real_lua_getglobal, call_real_lua_getnumber,
        call_real_lua_getobjname, call_real_lua_getparam, call_real_lua_getref,
        call_real_lua_getstring, call_real_lua_gettable, call_real_lua_iscfunction,
        call_real_lua_isfunction, call_real_lua_isnumber, call_real_lua_isstring,
        call_real_lua_istable, call_real_lua_isuserdata, call_real_lua_newstate,
        call_real_lua_newtag, call_real_lua_newthread, call_real_lua_open,
        call_real_lua_push_c_closure, call_real_lua_pushlstring, call_real_lua_pushnil,
        call_real_lua_pushnumber, call_real_lua_pushobject, call_real_lua_pushstring,
        call_real_lua_pushusertag, call_real_lua_pushvalue, call_real_lua_rawgetglobal,
        call_real_lua_rawgettable, call_real_lua_rawsetglobal, call_real_lua_rawsettable,
        call_real_lua_ref, call_real_lua_setfallback, call_real_lua_setglobal,
        call_real_lua_settable, call_real_lua_settag, call_real_lua_settagmethod,
        call_real_lua_tag, call_real_lua_unref, LuaCFunction, LuaObject, LuaState,
    },
    symbol_map::lookup_symbol_from_map,
    telemetry,
};
use libc::{c_char, c_int, size_t, Dl_info};
use std::{
    collections::{HashMap, VecDeque},
    ffi::{c_void, CStr},
    mem::MaybeUninit,
    ptr,
    sync::{Mutex, OnceLock},
};

const PUSH_RING_CAPACITY: usize = 16;
const MAX_PENDING_REGISTERED_GLOBALS: usize = 8;
static CALLFUNCTION_TRACKER: OnceLock<Mutex<CallfunctionTracker>> = OnceLock::new();
static GLOBAL_ACCESS_TRACKER: OnceLock<Mutex<GlobalAccessTracker>> = OnceLock::new();
static HANDLE_LABELS: OnceLock<Mutex<HandleLabelTracker>> = OnceLock::new();
static PUSH_EVENT_TRACKER: OnceLock<Mutex<PushEventTracker>> = OnceLock::new();
static REGISTERED_GLOBAL_CANDIDATES: OnceLock<Mutex<RegisteredGlobalTracker>> = OnceLock::new();

extern "C" {
    fn backtrace(buffer: *mut *mut c_void, size: c_int) -> c_int;
}

macro_rules! forward_int_result {
    ($label:literal, $result:expr) => {{
        match $result {
            Some(value) => value,
            None => {
                log_line(concat!(
                    $label,
                    " symbol missing; returning failure to keep engine alive"
                ));
                -1
            }
        }
    }};
}

pub(crate) unsafe fn trace_lua_open() -> LuaState {
    match call_real_lua_open() {
        Some(state) => state,
        None => {
            log_line("lua_open symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

pub(crate) unsafe fn trace_lua_newstate() -> LuaState {
    match call_real_lua_newstate() {
        Some(state) => state,
        None => {
            log_line("lua_newstate symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

pub(crate) unsafe fn trace_lua_newthread(state: LuaState) -> LuaState {
    match call_real_lua_newthread(state) {
        Some(thread) => thread,
        None => {
            log_line("lua_newthread symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

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
    let text = cstr_opt(value).unwrap_or_else(|| "<null>".to_string());
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

pub(crate) unsafe fn trace_lua_createtable() -> LuaObject {
    record_non_push_event();
    match call_real_lua_createtable() {
        Some(handle) => {
            let caller = caller_origin_fields();
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::CreateTable {
                handle: format!("0x{handle:08x}"),
                handle_label: handle_label_for(handle),
                values,
                caller,
            });
            handle
        }
        None => {
            log_line("lua_createtable symbol missing; returning null handle");
            0
        }
    }
}

pub(crate) unsafe fn trace_lua_settable() {
    let pushes = take_recent_pushes(3);
    let table_handle = call_real_lua_getparam(-3);
    let caller = caller_origin_fields();
    let succeeded = call_real_lua_settable();
    let note = if succeeded {
        None
    } else {
        Some("lua_settable_missing".to_string())
    };
    log_event_with_seq(LuaEvent::SetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    if succeeded {
        emit_set_table_entry(table_handle, pushes, caller, None);
    }
}

pub(crate) unsafe fn trace_lua_rawsettable() {
    let pushes = take_recent_pushes(3);
    let table_handle = call_real_lua_getparam(-3);
    let caller = caller_origin_fields();
    let succeeded = call_real_lua_rawsettable();
    let note = if succeeded {
        None
    } else {
        Some("lua_rawsettable_missing".to_string())
    };
    log_event_with_seq(LuaEvent::RawsetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    if succeeded {
        emit_set_table_entry(
            table_handle,
            pushes,
            caller,
            Some("via_rawsettable".to_string()),
        );
    }
}

pub(crate) unsafe fn trace_lua_gettable() -> LuaObject {
    record_non_push_event();
    match call_real_lua_gettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::GetTable {
                handle: format!("0x{handle:08x}"),
                handle_label: handle_label_for(handle),
                values,
            });
            handle
        }
        None => {
            log_line("lua_gettable symbol missing; returning null handle");
            0
        }
    }
}

pub(crate) unsafe fn trace_lua_rawgettable() -> LuaObject {
    record_non_push_event();
    match call_real_lua_rawgettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::RawgetTable {
                handle: format!("0x{handle:08x}"),
                handle_label: handle_label_for(handle),
                values,
            });
            handle
        }
        None => {
            log_line("lua_rawgettable symbol missing; returning null handle");
            0
        }
    }
}

pub(crate) unsafe fn trace_lua_rawgetglobal(name: *const c_char) -> LuaObject {
    record_non_push_event();
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
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
                handle_label: Some(handle_label.clone()),
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

pub(crate) unsafe fn trace_lua_rawsetglobal(name: *const c_char) {
    record_non_push_event();
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    let mut handle_field = None;
    let mut handle_label = None;
    let mut values = ValueFields::default();
    let mut note = None;
    let mut computed_label = None;
    let caller = caller_origin_fields();
    if call_real_lua_rawsetglobal(name) {
        if let Some(handle) = call_real_lua_rawgetglobal(name) {
            handle_field = Some(format!("0x{handle:08x}"));
            let resolved_label = format!("global:{label}");
            computed_label = Some(resolved_label.clone());
            handle_label = Some(resolved_label.clone());
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
        handle_label,
        label: computed_label,
        values,
        note,
        caller,
    });
}

pub(crate) unsafe fn trace_lua_unref(reference: c_int) {
    record_non_push_event();
    let note = if call_real_lua_unref(reference) {
        None
    } else {
        Some("lua_unref_missing".to_string())
    };
    log_semantic_event(LuaSemanticEvent::SemanticUnref {
        reference,
        note: note.clone(),
    });
    log_event(LuaEvent::Unref { reference, note });
}

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
            let handle_hex = format!("0x{handle:08x}");
            let origin_fields = origin_fields(Some(&origin));
            log_semantic_event(LuaSemanticEvent::SemanticSetFallback {
                fallback: name.clone(),
                handle: handle_hex.clone(),
                handle_label: Some(handle_label.clone()),
                values: values.clone(),
                origin: origin_fields.clone(),
                caller: caller.clone(),
            });
            log_event(LuaEvent::SetFallback {
                fallback: name,
                handle: handle_hex,
                handle_label: Some(handle_label),
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

pub(crate) unsafe fn trace_lua_newtag() -> c_int {
    record_non_push_event();
    match call_real_lua_newtag() {
        Some(tag) => {
            log_event(LuaEvent::SetTag {
                tag,
                note: Some("created_via_newtag".to_string()),
            });
            tag
        }
        None => {
            log_line("lua_newtag symbol missing; returning 0");
            0
        }
    }
}

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

pub(crate) unsafe fn trace_lua_settag(tag: c_int) {
    record_non_push_event();
    let note = if call_real_lua_settag(tag) {
        None
    } else {
        Some("lua_settag_missing".to_string())
    };
    log_event(LuaEvent::SetTag { tag, note });
}
pub(crate) unsafe fn trace_lua_dofile(path: *const c_char) -> c_int {
    record_non_push_event();
    let label = cstr_opt(path).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dofile { path: label });
    forward_int_result!("lua_dofile", call_real_lua_dofile(path))
}

pub(crate) unsafe fn trace_lua_dostring(chunk: *const c_char) -> c_int {
    record_non_push_event();
    let snippet = cstr_opt(chunk)
        .map(|s| truncate_for_log(&s, 80))
        .unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dostring { snippet });
    forward_int_result!("lua_dostring", call_real_lua_dostring(chunk))
}

pub(crate) unsafe fn trace_lua_dobuffer(
    buffer: *const c_char,
    size: size_t,
    name: *const c_char,
) -> c_int {
    record_non_push_event();
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dobuffer {
        name: label,
        size: size as usize,
    });
    forward_int_result!("lua_dobuffer", call_real_lua_dobuffer(buffer, size, name))
}

pub(crate) unsafe fn trace_lua_call(name: *const c_char) -> c_int {
    record_non_push_event();
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::Call { name: label });
    forward_int_result!("lua_call", call_real_lua_call(name))
}

pub(crate) unsafe fn trace_lua_setglobal(name: *const c_char) {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

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
                handle_label: Some(handle_label.clone()),
                label: Some(handle_label.clone()),
                values: values.clone(),
                origin: origin_fields(origin.as_ref()),
            });

            if is_closure {
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

pub(crate) unsafe fn trace_lua_getglobal(name: *const c_char) -> LuaObject {
    record_non_push_event();
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    let handle = match call_real_lua_getglobal(name) {
        Some(handle) => handle,
        None => {
            log_line("lua_getglobal symbol missing; returning null handle");
            return 0;
        }
    };

    if let Ok(mut tracker) = global_access_tracker().lock() {
        let count = tracker.record(&label);
        let handle_label = format!("global:{label}");
        remember_handle_label(handle, handle_label.clone());
        log_event(LuaEvent::GetGlobal {
            name: label.clone(),
            handle: format!("0x{handle:08x}"),
            handle_label: Some(handle_label.clone()),
            label: handle_label,
            count,
        });
    } else {
        log_line("lua_getglobal tracker mutex poisoned; skipping access log");
    }

    handle
}

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
                    if let Ok(mut tracker) = callfunction_tracker().lock() {
                        tracker.remember_label_if_missing(handle, format!("ref:{reference}"));
                        if let Some(origin) = origin.clone() {
                            tracker.remember_origin_if_missing(handle, origin);
                        }
                    } else {
                        log_line("lua_ref tracker mutex poisoned; skipping cache update");
                    }
                    remember_handle_label_if_missing(handle, label.clone());
                    let handle_hex = format!("0x{handle:08x}");
                    let origin_fields = origin_fields(origin.as_ref());
                    log_semantic_event(LuaSemanticEvent::SemanticStoreRef {
                        lock,
                        reference,
                        handle: Some(handle_hex.clone()),
                        handle_label: Some(label.clone()),
                        label: Some(label.clone()),
                        note: None,
                        origin: origin_fields.clone(),
                    });
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some(handle_hex),
                        handle_label: Some(label.clone()),
                        label: Some(label),
                        note: None,
                        origin: origin_fields,
                    });
                }
                None => {
                    log_semantic_event(LuaSemanticEvent::SemanticStoreRef {
                        lock,
                        reference,
                        handle: Some("<unknown>".to_string()),
                        handle_label: Some(format!("ref:{reference}")),
                        label: Some(format!("ref:{reference}")),
                        note: Some("lua_getref_missing".to_string()),
                        origin: OriginFields::default(),
                    });
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some("<unknown>".to_string()),
                        handle_label: Some(format!("ref:{reference}")),
                        label: Some(format!("ref:{reference}")),
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
            let handle_hex = format!("0x{handle:08x}");
            let origin_fields = origin_fields(origin.as_ref());
            log_semantic_event(LuaSemanticEvent::SemanticFetchRef {
                reference,
                handle: Some(handle_hex.clone()),
                handle_label: Some(label.clone()),
                label: Some(label.clone()),
                note: None,
                origin: origin_fields.clone(),
            });
            log_event(LuaEvent::FetchRef {
                reference,
                handle: Some(handle_hex),
                handle_label: Some(label.clone()),
                label: Some(label),
                note: None,
                origin: origin_fields,
            });
            handle
        }
        None => {
            log_semantic_event(LuaSemanticEvent::SemanticFetchRef {
                reference,
                handle: Some("<unknown>".to_string()),
                handle_label: None,
                label: None,
                note: Some("lua_getref_symbol_missing".to_string()),
                origin: OriginFields::default(),
            });
            log_event(LuaEvent::FetchRef {
                reference,
                handle: Some("<unknown>".to_string()),
                handle_label: None,
                label: None,
                note: Some("lua_getref_symbol_missing".to_string()),
                origin: OriginFields::default(),
            });
            0
        }
    }
}

pub(crate) unsafe fn trace_lua_settagmethod(tag: c_int, event: *const c_char) {
    let event_label = cstr_opt(event).unwrap_or_else(|| "<null>".to_string());
    let top_handle = call_real_lua_getparam(-1);
    let mut values = ValueFields::default();
    let mut handle_field = None;
    let mut handle_label = None;
    let mut origin = None;
    if let Some(handle) = top_handle {
        handle_field = Some(format!("0x{handle:08x}"));
        handle_label = handle_label_for(handle);
        if let Some(details) = describe_lua_value(handle) {
            values = value_fields_from_details(&details);
            if let Some(addr) = call_real_lua_getcfunction(handle) {
                origin = Some(ClosureOrigin::new(addr as *const c_void));
            }
        }
    }
    let origin_fields = origin_fields(origin.as_ref());
    log_semantic_event(LuaSemanticEvent::SemanticSetTagmethod {
        tag,
        event_name: event_label.clone(),
        handle: handle_field.clone(),
        handle_label: handle_label.clone(),
        values: values.clone(),
        origin: origin_fields.clone(),
    });
    if call_real_lua_settagmethod(tag, event) {
        log_event(LuaEvent::SetTagmethod {
            tag,
            event_name: event_label.clone(),
            handle: handle_field.clone(),
            handle_label: handle_label.clone(),
            values: values.clone(),
            origin: origin_fields.clone(),
        });
    } else {
        log_event(LuaEvent::SetTagmethod {
            tag,
            event_name: event_label.clone(),
            handle: handle_field,
            handle_label,
            values,
            origin: origin_fields,
        });
    }
}
pub(crate) unsafe fn trace_lua_collectgarbage() {
    record_non_push_event();
    if call_real_lua_collectgarbage() {
        log_event(LuaEvent::CollectGarbage {});
    }
}

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

pub(crate) unsafe fn trace_lua_callfunction(func: *mut c_void) -> c_int {
    record_non_push_event();
    let handle = func as usize as LuaObject;
    let label = resolve_lua_function_label(handle);
    remember_handle_label_if_missing(handle, label.clone());

    if let Ok(mut tracker) = callfunction_tracker().lock() {
        let sample = tracker.record(handle, &label);
        log_event(LuaEvent::CallFunc {
            handle: format!("0x{handle:08x}"),
            label: label.clone(),
            handle_label: Some(handle_label_for(handle).unwrap_or_else(|| label.clone())),
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

    let result = forward_int_result!("lua_callfunction", call_real_lua_callfunction(handle));

    result
}

fn record_push_preview(log_seq: u64, preview: UpvaluePreview, handle: Option<LuaObject>) {
    if let Ok(mut tracker) = push_event_tracker().lock() {
        tracker.record_push(log_seq, preview, handle);
    } else {
        log_line("push event tracker mutex poisoned; skipping push capture");
    }
}

fn record_non_push_event() {
    if let Ok(mut tracker) = push_event_tracker().lock() {
        tracker.record_non_push();
    } else {
        log_line("push event tracker mutex poisoned; skipping activity record");
    }
}

fn take_recent_pushes(count: usize) -> Option<Vec<TrackedPush>> {
    match push_event_tracker().lock() {
        Ok(mut tracker) => {
            let pushes = tracker.snapshot_recent(count);
            tracker.clear();
            pushes
        }
        Err(_) => {
            log_line("push event tracker mutex poisoned; skipping push capture");
            None
        }
    }
}

fn emit_set_table_entry(
    table_handle: Option<LuaObject>,
    pushes: Option<Vec<TrackedPush>>,
    caller: OriginFields,
    note: Option<String>,
) {
    let pushes = match pushes {
        Some(pushes) if pushes.len() >= 2 => pushes,
        _ => return,
    };
    let handle = match table_handle.or_else(|| table_handle_from_pushes(&pushes)) {
        Some(handle) => handle,
        None => return,
    };
    let table_handle_hex = format!("0x{handle:08x}");
    let table_handle_label = handle_label_for(handle);
    let table_fields = describe_lua_value(handle)
        .map(|value| value_fields_from_details(&value))
        .or_else(|| {
            let mut fields = ValueFields::default();
            fields.value_type = Some(ValueType::Table);
            Some(fields)
        });
    let semantic_note = note.clone();
    let semantic_caller = caller.clone();
    let key_push = &pushes[pushes.len() - 2];
    let value_push = pushes.last().unwrap();
    let value_handle = value_push
        .handle
        .map(|value_handle| format!("0x{value_handle:08x}"));
    let value_handle_label = value_push
        .handle
        .and_then(|value_handle| handle_label_for(value_handle));
    let value_fields = value_push
        .handle
        .and_then(describe_lua_value)
        .map(|details| value_fields_from_details(&details));
    log_semantic_event(LuaSemanticEvent::SemanticSetTableEntry {
        table_handle: table_handle_hex,
        table_handle_label,
        table_fields,
        key: key_push.preview.clone(),
        value: value_push.preview.clone(),
        value_handle,
        value_handle_label,
        value_fields,
        note: semantic_note,
        caller: semantic_caller,
    });
}

fn table_handle_from_pushes(pushes: &[TrackedPush]) -> Option<LuaObject> {
    pushes
        .iter()
        .filter(|push| matches!(push.preview.kind, ValueType::Table) && push.handle.is_some())
        .min_by_key(|push| push.log_seq)
        .and_then(|push| push.handle)
}

fn emit_registered_global(
    name: &str,
    handle: LuaObject,
    handle_label: String,
    upvalues: c_int,
    values: ValueFields,
    origin: Option<ClosureOrigin>,
) {
    let origin_fields = origin_fields(origin.as_ref());
    log_semantic_event(LuaSemanticEvent::SemanticBindGlobal {
        name: name.to_string(),
        handle: format!("0x{handle:08x}"),
        handle_label: Some(handle_label.clone()),
        label: Some(handle_label.clone()),
        values: values.clone(),
        upvalues: Some(upvalues),
        origin: origin_fields.clone(),
    });
}

fn emit_registered_constant(
    name: &str,
    handle: LuaObject,
    handle_label: String,
    values: ValueFields,
    origin: Option<ClosureOrigin>,
) {
    let origin_fields = origin_fields(origin.as_ref());
    log_semantic_event(LuaSemanticEvent::SemanticBindConstant {
        name: name.to_string(),
        handle: format!("0x{handle:08x}"),
        handle_label: Some(handle_label.clone()),
        label: Some(handle_label.clone()),
        values: values.clone(),
        origin: origin_fields.clone(),
    });
}

fn remember_registered_global_candidate(
    func_addr: usize,
    upvalues: c_int,
    origin: Option<ClosureOrigin>,
) {
    match registered_global_tracker().lock() {
        Ok(mut tracker) => {
            tracker.remember(PendingRegisteredGlobal {
                func_addr,
                upvalues,
                origin,
            });
        }
        Err(_) => {
            log_line("registered global tracker mutex poisoned; dropping candidate");
        }
    }
}

fn take_registered_global_candidate(func_addr: usize) -> Option<PendingRegisteredGlobal> {
    match registered_global_tracker().lock() {
        Ok(mut tracker) => tracker.take(func_addr),
        Err(_) => {
            log_line("registered global tracker mutex poisoned; skipping candidate lookup");
            None
        }
    }
}
fn push_event_tracker() -> &'static Mutex<PushEventTracker> {
    PUSH_EVENT_TRACKER.get_or_init(|| Mutex::new(PushEventTracker::new(PUSH_RING_CAPACITY)))
}

fn registered_global_tracker() -> &'static Mutex<RegisteredGlobalTracker> {
    REGISTERED_GLOBAL_CANDIDATES
        .get_or_init(|| Mutex::new(RegisteredGlobalTracker::new(MAX_PENDING_REGISTERED_GLOBALS)))
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

fn describe_lua_value(handle: LuaObject) -> Option<ValueDetails> {
    if handle == 0 {
        return Some(ValueDetails::Nil);
    }
    let tag = call_real_lua_tag(handle);
    if call_real_lua_isnumber(handle) {
        if let Some(value) = call_real_lua_getnumber(handle) {
            return Some(ValueDetails::Number(value));
        }
    }
    if call_real_lua_isstring(handle) {
        if let Some(value) = call_real_lua_getstring(handle) {
            return Some(ValueDetails::String(value));
        }
    }
    if call_real_lua_istable(handle) {
        return Some(ValueDetails::Table { tag });
    }
    if call_real_lua_iscfunction(handle) {
        let func = call_real_lua_getcfunction(handle).map(|f| f as *const c_void as usize);
        return Some(ValueDetails::CFunction { tag, func });
    }
    if call_real_lua_isfunction(handle) {
        return Some(ValueDetails::Function { tag });
    }
    if call_real_lua_isuserdata(handle) {
        return Some(ValueDetails::Userdata { tag });
    }
    Some(ValueDetails::Unknown { tag })
}

fn value_fields_from_details(value: &ValueDetails) -> ValueFields {
    let mut fields = ValueFields::default();
    match value {
        ValueDetails::Number(value) => {
            fields.value_type = Some(ValueType::Number);
            fields.value = Some(format_number_for_log(*value));
        }
        ValueDetails::String(text) => {
            fields.value_type = Some(ValueType::String);
            fields.value_len = Some(text.len());
            fields.value_preview = Some(truncate_for_log(text, 80));
        }
        ValueDetails::Nil => {
            fields.value_type = Some(ValueType::Nil);
        }
        ValueDetails::Table { tag } => {
            fields.value_type = Some(ValueType::Table);
            fields.tag = *tag;
        }
        ValueDetails::Function { tag } => {
            fields.value_type = Some(ValueType::Function);
            fields.tag = *tag;
        }
        ValueDetails::CFunction { tag, func } => {
            fields.value_type = Some(ValueType::Cfunction);
            if let Some(addr) = func {
                fields.func = Some(format!("0x{addr:08x}"));
            }
            fields.tag = *tag;
        }
        ValueDetails::Userdata { tag } => {
            fields.value_type = Some(ValueType::Userdata);
            fields.tag = *tag;
        }
        ValueDetails::Unknown { tag } => {
            fields.value_type = Some(ValueType::Unknown);
            fields.tag = *tag;
        }
    }
    fields
}

fn upvalue_preview_from_details(value: &ValueDetails) -> UpvaluePreview {
    match value {
        ValueDetails::Number(value) => UpvaluePreview {
            kind: ValueType::Number,
            value: Some(format_number_for_log(*value)),
            value_len: None,
            preview: None,
            tag: None,
        },
        ValueDetails::String(text) => UpvaluePreview {
            kind: ValueType::String,
            value: None,
            value_len: Some(text.len()),
            preview: Some(truncate_for_log(text, 80)),
            tag: None,
        },
        ValueDetails::Nil => UpvaluePreview {
            kind: ValueType::Nil,
            value: None,
            value_len: None,
            preview: None,
            tag: None,
        },
        ValueDetails::Table { tag } => UpvaluePreview {
            kind: ValueType::Table,
            value: None,
            value_len: None,
            preview: None,
            tag: *tag,
        },
        ValueDetails::Function { tag } => UpvaluePreview {
            kind: ValueType::Function,
            value: None,
            value_len: None,
            preview: None,
            tag: *tag,
        },
        ValueDetails::CFunction { tag, .. } => UpvaluePreview {
            kind: ValueType::Cfunction,
            value: None,
            value_len: None,
            preview: None,
            tag: *tag,
        },
        ValueDetails::Userdata { tag, .. } => UpvaluePreview {
            kind: ValueType::Userdata,
            value: None,
            value_len: None,
            preview: None,
            tag: *tag,
        },
        ValueDetails::Unknown { tag } => UpvaluePreview {
            kind: ValueType::Unknown,
            value: None,
            value_len: None,
            preview: None,
            tag: *tag,
        },
    }
}

enum ValueDetails {
    Number(f64),
    String(String),
    Nil,
    Table {
        tag: Option<c_int>,
    },
    Function {
        tag: Option<c_int>,
    },
    CFunction {
        tag: Option<c_int>,
        func: Option<usize>,
    },
    Userdata {
        tag: Option<c_int>,
    },
    Unknown {
        tag: Option<c_int>,
    },
}

fn origin_fields(origin: Option<&ClosureOrigin>) -> OriginFields {
    let mut fields = OriginFields::default();
    if let Some(origin) = origin {
        fields.origin = Some(format!("0x{addr:08x}", addr = origin.func_addr));
        if let Some(module) = &origin.module {
            fields.module = Some(module.clone());
        }
        let mut has_symbol = false;
        if let Some(symbol) = &origin.symbol {
            has_symbol = true;
            fields.symbol = Some(symbol.clone());
        }
        if let Some(map_symbol) = &origin.map_symbol {
            if !has_symbol {
                let mut value = map_symbol.name.clone();
                if map_symbol.distance > 0 {
                    value.push_str(&format!("+0x{delta:x}", delta = map_symbol.distance));
                }
                fields.symbol = Some(value);
            }
        }
    }
    fields
}

fn caller_origin_fields() -> OriginFields {
    let mut frames: [*mut c_void; 32] = [ptr::null_mut(); 32];
    let depth = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as c_int) };
    if depth <= 0 {
        return OriginFields::default();
    }
    OriginFields::default()
}

struct ClosureDetails {
    module: Option<String>,
    module_base: Option<usize>,
    symbol: Option<String>,
}

fn describe_closure_target(ptr: *const c_void) -> ClosureDetails {
    unsafe {
        let mut info = MaybeUninit::<Dl_info>::zeroed();
        if libc::dladdr(ptr, info.as_mut_ptr()) == 0 {
            return ClosureDetails {
                module: None,
                module_base: None,
                symbol: None,
            };
        }
        let info = info.assume_init();
        let module = cstr_opt(info.dli_fname);
        let module_base = if info.dli_fbase.is_null() {
            None
        } else {
            Some(info.dli_fbase as usize)
        };
        let symbol = cstr_opt(info.dli_sname);
        ClosureDetails {
            module,
            module_base,
            symbol,
        }
    }
}

unsafe fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

struct PushEventTracker {
    pushes: VecDeque<TrackedPush>,
    capacity: usize,
}

impl PushEventTracker {
    fn new(capacity: usize) -> Self {
        Self {
            pushes: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn record_push(&mut self, log_seq: u64, preview: UpvaluePreview, handle: Option<LuaObject>) {
        if self.pushes.len() == self.capacity {
            self.pushes.pop_front();
        }
        self.pushes.push_back(TrackedPush {
            log_seq,
            preview,
            handle,
        });
    }

    fn record_non_push(&mut self) {
        self.pushes.clear();
    }

    fn snapshot_recent(&self, count: usize) -> Option<Vec<TrackedPush>> {
        if count == 0 {
            return Some(Vec::new());
        }
        if self.pushes.len() < count {
            return None;
        }
        let start = self.pushes.len() - count;
        Some(self.pushes.iter().skip(start).cloned().collect())
    }

    fn clear(&mut self) {
        self.pushes.clear();
    }
}

#[derive(Clone)]
struct TrackedPush {
    log_seq: u64,
    preview: UpvaluePreview,
    handle: Option<LuaObject>,
}

struct PendingRegisteredGlobal {
    func_addr: usize,
    upvalues: c_int,
    origin: Option<ClosureOrigin>,
}

struct RegisteredGlobalTracker {
    pending: HashMap<usize, VecDeque<PendingRegisteredGlobal>>,
    max_per_func: usize,
}

impl RegisteredGlobalTracker {
    fn new(max_per_func: usize) -> Self {
        Self {
            pending: HashMap::new(),
            max_per_func,
        }
    }

    fn remember(&mut self, candidate: PendingRegisteredGlobal) {
        let queue = self
            .pending
            .entry(candidate.func_addr)
            .or_insert_with(VecDeque::new);
        queue.push_back(candidate);
        if queue.len() > self.max_per_func {
            queue.pop_front();
        }
    }

    fn take(&mut self, func_addr: usize) -> Option<PendingRegisteredGlobal> {
        let candidate = self
            .pending
            .get_mut(&func_addr)
            .and_then(|queue| queue.pop_back());
        let should_remove = self
            .pending
            .get(&func_addr)
            .map(|queue| queue.is_empty())
            .unwrap_or(false);
        if should_remove {
            self.pending.remove(&func_addr);
        }
        candidate
    }
}

struct CallfunctionTracker {
    counts: HashMap<LuaObject, u64>,
    labels: HashMap<LuaObject, String>,
    origins: HashMap<LuaObject, ClosureOrigin>,
}

impl CallfunctionTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
            labels: HashMap::new(),
            origins: HashMap::new(),
        }
    }

    fn remember_label<S: Into<String>>(&mut self, handle: LuaObject, label: S) {
        self.labels.insert(handle, label.into());
    }

    fn remember_label_if_missing<S: Into<String>>(&mut self, handle: LuaObject, label: S) {
        self.labels.entry(handle).or_insert_with(|| label.into());
    }

    fn remember_origin(&mut self, handle: LuaObject, origin: ClosureOrigin) {
        self.origins.insert(handle, origin);
    }

    fn remember_origin_if_missing(&mut self, handle: LuaObject, origin: ClosureOrigin) {
        self.origins.entry(handle).or_insert(origin);
    }

    fn label_for(&self, handle: LuaObject) -> Option<String> {
        self.labels.get(&handle).cloned()
    }

    fn origin_for(&self, handle: LuaObject) -> Option<ClosureOrigin> {
        self.origins.get(&handle).cloned()
    }

    fn record(&mut self, handle: LuaObject, label: &str) -> CallSample {
        let count = {
            let entry = self.counts.entry(handle).or_insert(0);
            *entry += 1;
            *entry
        };
        self.labels
            .entry(handle)
            .or_insert_with(|| label.to_string());
        let origin = self.origin_for(handle);
        CallSample { count, origin }
    }
}

struct CallSample {
    count: u64,
    origin: Option<ClosureOrigin>,
}

struct GlobalAccessTracker {
    counts: HashMap<String, u64>,
}

impl GlobalAccessTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    fn record(&mut self, label: &str) -> u64 {
        let count = {
            let entry = self.counts.entry(label.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        count
    }
}

struct HandleLabelTracker {
    labels: HashMap<LuaObject, String>,
}

impl HandleLabelTracker {
    fn new() -> Self {
        Self {
            labels: HashMap::new(),
        }
    }

    fn remember_label(&mut self, handle: LuaObject, label: String) {
        self.labels.insert(handle, label);
    }

    fn remember_label_if_missing(&mut self, handle: LuaObject, label: String) {
        self.labels.entry(handle).or_insert(label);
    }

    fn label_for(&self, handle: LuaObject) -> Option<String> {
        self.labels.get(&handle).cloned()
    }
}

fn resolve_lua_function_label(handle: LuaObject) -> String {
    if let Ok(tracker) = callfunction_tracker().lock() {
        if let Some(label) = tracker.label_for(handle) {
            return label;
        }
    }

    if let Some((kind, name)) = call_real_lua_getobjname(handle) {
        match (kind.as_deref(), name.as_deref()) {
            (Some(kind), Some(name)) if !kind.is_empty() => {
                return format!("{kind}:{name}");
            }
            (_, Some(name)) => {
                return format!("function:{name}");
            }
            (Some(kind), None) if !kind.is_empty() => {
                return format!("{kind} handle=0x{handle:08x}");
            }
            _ => {}
        }
    }
    format!("handle=0x{handle:08x}")
}

fn callfunction_tracker() -> &'static Mutex<CallfunctionTracker> {
    CALLFUNCTION_TRACKER.get_or_init(|| Mutex::new(CallfunctionTracker::new()))
}

fn global_access_tracker() -> &'static Mutex<GlobalAccessTracker> {
    GLOBAL_ACCESS_TRACKER.get_or_init(|| Mutex::new(GlobalAccessTracker::new()))
}

fn handle_label_tracker() -> &'static Mutex<HandleLabelTracker> {
    HANDLE_LABELS.get_or_init(|| Mutex::new(HandleLabelTracker::new()))
}

fn remember_handle_label(handle: LuaObject, label: impl Into<String>) {
    if handle == 0 {
        return;
    }
    if let Ok(mut tracker) = handle_label_tracker().lock() {
        tracker.remember_label(handle, label.into());
    } else {
        log_line("handle label tracker mutex poisoned; skipping label update");
    }
}

fn remember_handle_label_if_missing(handle: LuaObject, label: impl Into<String>) {
    if handle == 0 {
        return;
    }
    if let Ok(mut tracker) = handle_label_tracker().lock() {
        tracker.remember_label_if_missing(handle, label.into());
    } else {
        log_line("handle label tracker mutex poisoned; skipping label update");
    }
}

fn handle_label_for(handle: LuaObject) -> Option<String> {
    if handle == 0 {
        return None;
    }
    handle_label_tracker()
        .lock()
        .ok()
        .and_then(|tracker| tracker.label_for(handle))
}

#[derive(Clone)]
struct ClosureOrigin {
    func_addr: usize,
    module: Option<String>,
    symbol: Option<String>,
    map_symbol: Option<MapSymbol>,
}

impl ClosureOrigin {
    fn new(ptr: *const c_void) -> Self {
        let details = describe_closure_target(ptr);
        let map_symbol =
            lookup_symbol_from_map(ptr as usize, details.module.as_deref(), details.module_base)
                .map(|hit| MapSymbol {
                    name: hit.name,
                    distance: hit.distance,
                });
        Self {
            func_addr: ptr as usize,
            module: details.module,
            symbol: details.symbol,
            map_symbol,
        }
    }
}

#[derive(Clone)]
struct MapSymbol {
    name: String,
    distance: usize,
}

fn log_event(event: LuaEvent) {
    crate::logging::log_event(event);
}

fn log_semantic_event(event: LuaSemanticEvent) {
    crate::logging::log_event(event);
}

fn log_event_with_seq(event: LuaEvent) -> u64 {
    crate::logging::log_event_with_seq(event)
}
