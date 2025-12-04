use crate::{
    logging::{log_line, EventBuilder, LuaEvent, OriginFields, ValueFields, ValueType},
    lua_api::{
        call_real_lua_callfunction, call_real_lua_collectgarbage, call_real_lua_copytagmethods,
        call_real_lua_createtable, call_real_lua_dofile, call_real_lua_dostring,
        call_real_lua_error, call_real_lua_getcfunction, call_real_lua_getglobal,
        call_real_lua_getnumber, call_real_lua_getobjname, call_real_lua_getparam,
        call_real_lua_getref, call_real_lua_getstring, call_real_lua_gettable,
        call_real_lua_getuserdata, call_real_lua_iscfunction, call_real_lua_isfunction,
        call_real_lua_isnumber, call_real_lua_isstring, call_real_lua_istable,
        call_real_lua_isuserdata, call_real_lua_newstate, call_real_lua_newtag, call_real_lua_open,
        call_real_lua_push_c_closure, call_real_lua_pushnil, call_real_lua_pushnumber,
        call_real_lua_pushobject, call_real_lua_pushstring, call_real_lua_pushusertag,
        call_real_lua_rawgetglobal, call_real_lua_rawgettable, call_real_lua_rawsetglobal,
        call_real_lua_rawsettable, call_real_lua_ref, call_real_lua_setfallback,
        call_real_lua_setglobal, call_real_lua_settable, call_real_lua_settag,
        call_real_lua_settagmethod, call_real_lua_tag, call_real_lua_unref, lua_state_global_addr,
        LuaCFunction, LuaObject, LuaState,
    },
    telemetry, vm_state,
};
use libc::{c_char, c_int};
use std::{
    ffi::{c_void, CStr},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

static CLOSURE_PUSH_COUNTER: AtomicU64 = AtomicU64::new(0);
static GLOBAL_ACCESS_COUNT: AtomicU64 = AtomicU64::new(0);

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
        Some(state) => {
            let recorded = lua_state_global_addr()
                .map(|addr| addr as LuaState)
                .unwrap_or(state);
            vm_state::record_main_state(recorded);
            state
        }
        None => {
            log_line("lua_open symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

pub(crate) unsafe fn trace_lua_newstate() -> LuaState {
    match call_real_lua_newstate() {
        Some(state) => {
            let recorded = lua_state_global_addr()
                .map(|addr| addr as LuaState)
                .unwrap_or(state);
            vm_state::record_main_state(recorded);
            state
        }
        None => {
            log_line("lua_newstate symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

pub(crate) unsafe fn trace_lua_push_closure(label: &str, func: LuaCFunction, upvalues: c_int) {
    let func_addr = func as *const c_void as usize;
    let sequence = CLOSURE_PUSH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    log_event_with_seq(LuaEvent::PushCclosure {
        name: label.to_string(),
        func: format!("0x{func_addr:08x}"),
        push_seq: sequence,
        upvalues,
        origin: origin_from_ptr(func as *const c_void),
    });
    if !call_real_lua_push_c_closure(func, upvalues) {
        log_line("unable to forward lua_pushCclosure call; retail VM may misbehave");
    }
}

pub(crate) unsafe fn trace_lua_pushnumber(value: f32) {
    telemetry::record_pushed_number(value.into());
    log_event(LuaEvent::PushNumber {
        value: format_number_for_log(value as f64),
    });
    if !call_real_lua_pushnumber(value) {
        log_line("lua_pushnumber symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushnil() {
    telemetry::record_pushed_nil();
    log_event(LuaEvent::PushNil {});
    if !call_real_lua_pushnil() {
        log_line("lua_pushnil symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushstring(value: *const c_char) {
    let text = cstr_opt(value).unwrap_or_else(|| "<null>".to_string());
    let preview = truncate_for_log(&text, 80);
    log_event(LuaEvent::PushString {
        len: text.len(),
        preview,
    });
    if !call_real_lua_pushstring(value) {
        log_line("lua_pushstring symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushusertag(id: c_int, tag: c_int) {
    let mut values = ValueFields::default();
    values.value_type = Some(ValueType::Userdata);
    values.tag = Some(tag);
    let caller = caller_origin_fields();
    log_event(LuaEvent::PushUsertag { id, values, caller });
    if !call_real_lua_pushusertag(id, tag) {
        log_line("lua_pushusertag symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushobject(object: LuaObject) {
    let values = describe_lua_value(object)
        .as_ref()
        .map(value_fields_from_details)
        .unwrap_or_default();
    log_event(LuaEvent::PushObject {
        handle: format!("0x{object:08x}"),
        handle_label: None,
        values,
    });
    if !call_real_lua_pushobject(object) {
        log_line("lua_pushobject symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_createtable() -> LuaObject {
    match call_real_lua_createtable() {
        Some(handle) => {
            let caller = caller_origin_fields();
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::CreateTable {
                handle: format!("0x{handle:08x}"),
                handle_label: None,
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
    let caller = caller_origin_fields();
    let note = if call_real_lua_settable() {
        None
    } else {
        Some("lua_settable_missing".to_string())
    };
    log_event(LuaEvent::SetTable { note, caller });
}

pub(crate) unsafe fn trace_lua_rawsettable() {
    let caller = caller_origin_fields();
    let note = if call_real_lua_rawsettable() {
        None
    } else {
        Some("lua_rawsettable_missing".to_string())
    };
    log_event(LuaEvent::RawsetTable { note, caller });
}

pub(crate) unsafe fn trace_lua_gettable() -> LuaObject {
    match call_real_lua_gettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::GetTable {
                handle: format!("0x{handle:08x}"),
                handle_label: None,
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
    match call_real_lua_rawgettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::RawgetTable {
                handle: format!("0x{handle:08x}"),
                handle_label: None,
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
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    match call_real_lua_rawgetglobal(name) {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            let handle_label = format!("global:{label}");
            log_event(LuaEvent::RawGetGlobal {
                name: label,
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
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    let caller = caller_origin_fields();
    let mut handle_field = None;
    let mut handle_label = None;
    let mut values = ValueFields::default();
    let mut note = None;

    if call_real_lua_rawsetglobal(name) {
        if let Some(handle) = call_real_lua_rawgetglobal(name) {
            handle_field = Some(format!("0x{handle:08x}"));
            let resolved_label = format!("global:{label}");
            handle_label = Some(resolved_label.clone());
            if let Some(details) = describe_lua_value(handle) {
                values = value_fields_from_details(&details);
            }
        } else {
            note = Some("lua_rawgetglobal_missing_after_set".to_string());
        }
    } else {
        note = Some("lua_rawsetglobal_missing".to_string());
    }

    let label_field = handle_label
        .clone()
        .or_else(|| Some(format!("global:{label}")));
    log_event(LuaEvent::RawSetGlobal {
        name: label,
        handle: handle_field,
        handle_label,
        label: label_field,
        values,
        note,
        caller,
    });
}

pub(crate) unsafe fn trace_lua_unref(reference: c_int) {
    let note = if call_real_lua_unref(reference) {
        None
    } else {
        Some("lua_unref_missing".to_string())
    };
    log_event(LuaEvent::Unref { reference, note });
}

pub(crate) unsafe fn trace_lua_setfallback(
    event_name: *const c_char,
    func: LuaCFunction,
) -> LuaObject {
    let name = cstr_opt(event_name).unwrap_or_else(|| "<null>".to_string());
    let origin = origin_from_func(Some(func));
    let caller = caller_origin_fields();
    match call_real_lua_setfallback(event_name, func) {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::SetFallback {
                fallback: name,
                handle: format!("0x{handle:08x}"),
                handle_label: None,
                values,
                origin,
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
    match call_real_lua_newtag() {
        Some(tag) => {
            log_event(LuaEvent::SetTag {
                tag,
                note: Some("created_via_newtag".to_string()),
                tag_label: None,
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
    match call_real_lua_copytagmethods(tagto, tagfrom) {
        Some(result) => {
            let caller = caller_origin_fields();
            log_event(LuaEvent::CopyTagmethods {
                to: tagto,
                from: tagfrom,
                to_label: None,
                from_label: None,
                result: Some(result),
                caller,
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
    let note = if call_real_lua_settag(tag) {
        None
    } else {
        Some("lua_settag_missing".to_string())
    };
    log_event(LuaEvent::SetTag {
        tag,
        note,
        tag_label: None,
    });
}

pub(crate) unsafe fn trace_lua_dofile(path: *const c_char) -> c_int {
    let label = cstr_opt(path).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dofile { path: label });
    forward_int_result!("lua_dofile", call_real_lua_dofile(path))
}

pub(crate) unsafe fn trace_lua_dostring(chunk: *const c_char) -> c_int {
    let snippet = cstr_opt(chunk)
        .map(|s| truncate_for_log(&s, 80))
        .unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dostring { snippet });
    forward_int_result!("lua_dostring", call_real_lua_dostring(chunk))
}

pub(crate) unsafe fn trace_lua_setglobal(name: *const c_char) {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    if call_real_lua_setglobal(name) {
        if let Some(handle) = call_real_lua_getglobal(name) {
            let origin = origin_from_func(call_real_lua_getcfunction(handle));
            let values = describe_lua_value(handle)
                .as_ref()
                .map(value_fields_from_details)
                .unwrap_or_default();
            let handle_label = format!("global:{label}");
            log_event(LuaEvent::BindGlobal {
                name: label,
                handle: format!("0x{handle:08x}"),
                handle_label: Some(handle_label.clone()),
                label: Some(handle_label),
                values,
                origin,
            });
        }
    }
}

pub(crate) unsafe fn trace_lua_getglobal(name: *const c_char) -> LuaObject {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    let handle = match call_real_lua_getglobal(name) {
        Some(handle) => handle,
        None => {
            log_line("lua_getglobal symbol missing; returning null handle");
            return 0;
        }
    };

    let count = GLOBAL_ACCESS_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let handle_label = format!("global:{label}");
    let func_addr = call_real_lua_getcfunction(handle).map(|func| func as *const c_void as usize);
    let event = LuaEvent::GetGlobal {
        name: label.clone(),
        handle: format!("0x{handle:08x}"),
        handle_label: Some(handle_label.clone()),
        label: handle_label,
        count,
    };
    let mut builder: EventBuilder = event.into();
    if let Some(addr) = func_addr {
        builder.kv_mut("func", format!("0x{addr:08x}"));
    }
    log_builder(builder, None);

    handle
}

pub(crate) unsafe fn trace_lua_ref(lock: c_int) -> c_int {
    match call_real_lua_ref(lock) {
        Some(reference) => {
            let handle = call_real_lua_getref(reference);
            match handle {
                Some(handle) => {
                    let label = resolve_lua_function_label(handle);
                    let origin = origin_from_func(call_real_lua_getcfunction(handle));
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some(format!("0x{handle:08x}")),
                        handle_label: Some(format!("ref:{reference}")),
                        label: Some(label),
                        note: None,
                        origin,
                    });
                }
                None => {
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
    match call_real_lua_getref(reference) {
        Some(handle) => {
            let label = resolve_lua_function_label(handle);
            let origin = origin_from_func(call_real_lua_getcfunction(handle));
            log_event(LuaEvent::FetchRef {
                reference,
                handle: Some(format!("0x{handle:08x}")),
                handle_label: Some(format!("ref:{reference}")),
                label: Some(label),
                note: None,
                origin,
            });
            handle
        }
        None => {
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
    let mut origin = OriginFields::default();
    if let Some(handle) = top_handle {
        handle_field = Some(format!("0x{handle:08x}"));
        handle_label = Some(format!("tagmethod:{event_label}"));
        if let Some(details) = describe_lua_value(handle) {
            values = value_fields_from_details(&details);
        }
        origin = origin_from_func(call_real_lua_getcfunction(handle));
    }
    call_real_lua_settagmethod(tag, event);
    log_event(LuaEvent::SetTagmethod {
        tag,
        event_name: event_label,
        tag_label: None,
        handle: handle_field,
        handle_label,
        values,
        origin,
    });
}

pub(crate) unsafe fn trace_lua_collectgarbage() {
    if call_real_lua_collectgarbage() {
        log_event(LuaEvent::CollectGarbage {});
    }
}

pub(crate) unsafe fn trace_lua_error(message: *const c_char) {
    let text = cstr_opt(message)
        .map(|s| truncate_for_log(&s, 120))
        .unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::LuaError { message: text });
    if !call_real_lua_error(message) {
        log_line("lua_error symbol missing; unable to propagate error to Lua VM");
    }
}

pub(crate) unsafe fn trace_lua_callfunction(func: *mut c_void) -> c_int {
    let handle = func as usize as LuaObject;
    let label = resolve_lua_function_label(handle);
    let origin = call_real_lua_getcfunction(handle)
        .map(|f| origin_from_ptr(f as *const c_void))
        .unwrap_or_else(|| origin_from_ptr(func));

    log_event(LuaEvent::CallFunc {
        handle: format!("0x{handle:08x}"),
        label: label.clone(),
        handle_label: Some(label),
        calls: None,
        note: None,
        origin,
    });

    forward_int_result!("lua_callfunction", call_real_lua_callfunction(handle))
}

fn origin_from_ptr(ptr: *const c_void) -> OriginFields {
    if ptr.is_null() {
        OriginFields::default()
    } else {
        let mut fields = OriginFields::default();
        fields.origin = Some(format!("0x{addr:08x}", addr = ptr as usize));
        fields
    }
}

fn origin_from_func(func: Option<LuaCFunction>) -> OriginFields {
    func.map(|f| origin_from_ptr(f as *const c_void))
        .unwrap_or_default()
}

fn caller_origin_fields() -> OriginFields {
    let mut frames: [*mut c_void; 16] = [ptr::null_mut(); 16];
    let depth = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as c_int) };
    if depth <= 1 {
        return OriginFields::default();
    }
    frames
        .iter()
        .take(depth as usize)
        .skip(1)
        .find(|addr| !addr.is_null())
        .map(|addr| origin_from_ptr(*addr as *const c_void))
        .unwrap_or_default()
}

fn format_number_for_log(value: f64) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn format_pointer_hex(value: c_int) -> String {
    format!("0x{addr:08x}", addr = value as u32)
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
        let payload = call_real_lua_getuserdata(handle);
        return Some(ValueDetails::Userdata { tag, payload });
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
        ValueDetails::Userdata { tag, payload } => {
            fields.value_type = Some(ValueType::Userdata);
            fields.tag = *tag;
            fields.payload_hex = payload.map(format_pointer_hex);
            fields.payload = *payload;
        }
        ValueDetails::Unknown { tag } => {
            fields.value_type = Some(ValueType::Unknown);
            fields.tag = *tag;
        }
    }
    fields
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
        payload: Option<c_int>,
    },
    Unknown {
        tag: Option<c_int>,
    },
}

fn resolve_lua_function_label(handle: LuaObject) -> String {
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

fn log_event(event: LuaEvent) {
    log_event_with_state(event, None);
}

fn log_event_with_seq(event: LuaEvent) -> u64 {
    log_event_with_seq_state(event, None)
}

fn log_event_with_state(event: LuaEvent, preferred_state: Option<LuaState>) {
    let mut builder: EventBuilder = event.into();
    inject_state_field(&mut builder, preferred_state);
    crate::logging::log_event(builder);
}

fn log_event_with_seq_state(event: LuaEvent, preferred_state: Option<LuaState>) -> u64 {
    let mut builder: EventBuilder = event.into();
    inject_state_field(&mut builder, preferred_state);
    crate::logging::log_event_with_seq(builder)
}

fn log_builder(mut builder: EventBuilder, preferred_state: Option<LuaState>) {
    inject_state_field(&mut builder, preferred_state);
    crate::logging::log_event(builder);
}

fn inject_state_field(builder: &mut EventBuilder, preferred_state: Option<LuaState>) {
    if let Some(addr) = active_state(preferred_state) {
        builder.kv_mut("lua_state", format!("0x{addr:08x}"));
    }
}

fn active_state(preferred_state: Option<LuaState>) -> Option<usize> {
    preferred_state
        .map(|ptr| ptr as usize)
        .or_else(lua_state_global_addr)
}

unsafe fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}
