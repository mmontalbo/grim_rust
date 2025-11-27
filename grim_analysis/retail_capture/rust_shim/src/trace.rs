use crate::{
    logging::{log_event, log_line, LuaEvent, OriginFields, ValueFields, ValueType},
    lua_api::{
        call_real_lua_call, call_real_lua_callfunction, call_real_lua_collectgarbage,
        call_real_lua_copytagmethods, call_real_lua_createtable, call_real_lua_dobuffer,
        call_real_lua_dofile, call_real_lua_dostring, call_real_lua_error,
        call_real_lua_getcfunction, call_real_lua_getglobal, call_real_lua_getnumber,
        call_real_lua_getobjname, call_real_lua_getref, call_real_lua_getstring,
        call_real_lua_gettable, call_real_lua_getuserdata, call_real_lua_iscfunction,
        call_real_lua_isfunction, call_real_lua_isnumber, call_real_lua_isstring,
        call_real_lua_istable, call_real_lua_isuserdata, call_real_lua_newtag,
        call_real_lua_push_c_closure, call_real_lua_pushlstring, call_real_lua_pushnil,
        call_real_lua_pushnumber, call_real_lua_pushobject, call_real_lua_pushstring,
        call_real_lua_pushusertag, call_real_lua_rawgetglobal, call_real_lua_rawgettable,
        call_real_lua_rawsetglobal, call_real_lua_rawsettable, call_real_lua_ref,
        call_real_lua_setfallback, call_real_lua_setglobal, call_real_lua_settable,
        call_real_lua_settag, call_real_lua_settagmethod, call_real_lua_tag, call_real_lua_unref,
        LuaCFunction, LuaObject,
    },
    symbol_map::lookup_symbol_from_map,
    telemetry,
};
use libc::{c_char, c_int, size_t, Dl_info};
use std::{
    collections::HashMap,
    ffi::{c_void, CStr, CString},
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

static CLOSURE_PUSH_COUNTER: AtomicU64 = AtomicU64::new(0);
static CALLFUNCTION_TRACKER: OnceLock<Mutex<CallfunctionTracker>> = OnceLock::new();
static GLOBAL_ACCESS_TRACKER: OnceLock<Mutex<GlobalAccessTracker>> = OnceLock::new();

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

pub(crate) unsafe fn trace_lua_push_closure(label: &str, func: LuaCFunction, upvalues: c_int) {
    let func_addr = func as *const c_void as usize;
    let origin = Some(ClosureOrigin::new(func as *const c_void));
    let sequence = CLOSURE_PUSH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let event = LuaEvent::PushCclosure {
        name: label.to_string(),
        func: format!("0x{func_addr:08x}"),
        push_seq: sequence,
        upvalues,
        origin: origin_fields(origin.as_ref()),
    };
    log_event(event);

    if !call_real_lua_push_c_closure(func, upvalues) {
        log_line("unable to forward lua_pushCclosure call; retail VM may misbehave");
    }
}

pub(crate) unsafe fn trace_lua_pushnumber(value: f64) {
    telemetry::record_pushed_number(value);
    log_event(LuaEvent::PushNumber {
        value: format_number_for_log(value),
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
    log_event(LuaEvent::PushString {
        len: text.len(),
        preview: truncate_for_log(&text, 80),
    });
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
    log_event(LuaEvent::PushLstring {
        len: len as usize,
        preview: truncate_for_log(&text, 80),
    });
    if !call_real_lua_pushlstring(value, len) {
        log_line("lua_pushlstring symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushusertag(id: c_int, tag: c_int) {
    log_event(LuaEvent::PushUsertag { id, tag });
    if !call_real_lua_pushusertag(id, tag) {
        log_line("lua_pushusertag symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_pushobject(object: LuaObject) {
    let values = describe_lua_value(object)
        .map(|value| value_fields_from_details(&value))
        .unwrap_or_default();
    log_event(LuaEvent::PushObject {
        handle: format!("0x{object:08x}"),
        values,
    });
    if !call_real_lua_pushobject(object) {
        log_line("lua_pushobject symbol missing; skipping push");
    }
}

pub(crate) unsafe fn trace_lua_createtable() -> LuaObject {
    match call_real_lua_createtable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::CreateTable {
                handle: format!("0x{handle:08x}"),
                values,
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
    let note = if call_real_lua_settable() {
        None
    } else {
        Some("lua_settable_missing".to_string())
    };
    log_event(LuaEvent::SetTable { note });
}

pub(crate) unsafe fn trace_lua_rawsettable() {
    let note = if call_real_lua_rawsettable() {
        None
    } else {
        Some("lua_rawsettable_missing".to_string())
    };
    log_event(LuaEvent::RawsetTable { note });
}

pub(crate) unsafe fn trace_lua_gettable() -> LuaObject {
    match call_real_lua_gettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::GetTable {
                handle: format!("0x{handle:08x}"),
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
            log_event(LuaEvent::RawGetGlobal {
                name: label.clone(),
                handle: format!("0x{handle:08x}"),
                label: Some(format!("global:{label}")),
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
    let mut handle_field = None;
    let mut values = ValueFields::default();
    let mut note = None;
    let mut computed_label = None;
    if call_real_lua_rawsetglobal(name) {
        if let Some(handle) = call_real_lua_rawgetglobal(name) {
            handle_field = Some(format!("0x{handle:08x}"));
            computed_label = Some(format!("global:{label}"));
            if let Some(details) = describe_lua_value(handle) {
                values = value_fields_from_details(&details);
            }
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
    let origin = ClosureOrigin::new(func as *const c_void);
    match call_real_lua_setfallback(event_name, func) {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::SetFallback {
                fallback: name,
                handle: format!("0x{handle:08x}"),
                values,
                origin: origin_fields(Some(&origin)),
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
        Some(tag) => tag,
        None => {
            log_line("lua_newtag symbol missing; returning 0");
            0
        }
    }
}

pub(crate) unsafe fn trace_lua_copytagmethods(tagto: c_int, tagfrom: c_int) -> c_int {
    match call_real_lua_copytagmethods(tagto, tagfrom) {
        Some(result) => {
            log_event(LuaEvent::CopyTagmethods {
                to: tagto,
                from: tagfrom,
                result: Some(result),
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
    log_event(LuaEvent::SetTag { tag, note });
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

pub(crate) unsafe fn trace_lua_dobuffer(
    buffer: *const c_char,
    size: size_t,
    name: *const c_char,
) -> c_int {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    telemetry::observe_lua_activity();
    log_event(LuaEvent::Dobuffer {
        name: label,
        size: size as usize,
    });
    forward_int_result!("lua_dobuffer", call_real_lua_dobuffer(buffer, size, name))
}

pub(crate) unsafe fn trace_lua_call(name: *const c_char) -> c_int {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    log_event(LuaEvent::Call { name: label });
    forward_int_result!("lua_call", call_real_lua_call(name))
}

pub(crate) unsafe fn trace_lua_setglobal(name: *const c_char) {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    if call_real_lua_setglobal(name) {
        if let Some(handle) = call_real_lua_getglobal(name) {
            let origin = call_real_lua_getcfunction(handle)
                .map(|func| ClosureOrigin::new(func as *const c_void));
            let value_fields = describe_lua_value(handle);

            if let Ok(mut tracker) = callfunction_tracker().lock() {
                tracker.remember_label(handle, format!("global:{label}"));
                if let Some(origin) = origin.clone() {
                    tracker.remember_origin(handle, origin);
                }
            } else {
                log_line("lua_setglobal tracker mutex poisoned; skipping cache update");
            }

            let values = value_fields
                .as_ref()
                .map(value_fields_from_details)
                .unwrap_or_default();
            log_event(LuaEvent::BindGlobal {
                name: label.clone(),
                handle: format!("0x{handle:08x}"),
                label: Some(format!("global:{label}")),
                values,
                origin: origin_fields(origin.as_ref()),
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

    if let Ok(mut tracker) = global_access_tracker().lock() {
        let count = tracker.record(&label);
        log_event(LuaEvent::GetGlobal {
            name: label.clone(),
            handle: format!("0x{handle:08x}"),
            label: format!("global:{label}"),
            count,
        });
    } else {
        log_line("lua_getglobal tracker mutex poisoned; skipping access log");
    }

    handle
}

pub(crate) unsafe fn trace_lua_ref(lock: c_int) -> c_int {
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
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some(format!("0x{handle:08x}")),
                        label: Some(label),
                        note: None,
                        origin: origin_fields(origin.as_ref()),
                    });
                }
                None => {
                    log_event(LuaEvent::StoreRef {
                        lock,
                        reference,
                        handle: Some("<unknown>".to_string()),
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
            log_event(LuaEvent::FetchRef {
                reference,
                handle: Some(format!("0x{handle:08x}")),
                label: Some(label),
                note: None,
                origin: origin_fields(origin.as_ref()),
            });
            handle
        }
        None => {
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

pub(crate) unsafe fn trace_lua_settagmethod(tag: c_int, event: *const c_char) {
    let event_label = cstr_opt(event).unwrap_or_else(|| "<null>".to_string());
    if call_real_lua_settagmethod(tag, event) {
        log_event(LuaEvent::SetTagmethod {
            tag,
            event_name: event_label.clone(),
        });
    }
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

    if let Ok(mut tracker) = callfunction_tracker().lock() {
        let sample = tracker.record(handle, &label);
        log_event(LuaEvent::CallFunc {
            handle: format!("0x{handle:08x}"),
            label: label.clone(),
            calls: Some(sample.count),
            note: None,
            origin: origin_fields(sample.origin.as_ref()),
        });
    } else {
        log_line("lua_callfunction tracker mutex poisoned; falling back to minimal log");
        log_event(LuaEvent::CallFunc {
            handle: format!("0x{handle:08x}"),
            label: label.clone(),
            calls: None,
            note: Some("tracker_poisoned".to_string()),
            origin: OriginFields::default(),
        });
    }

    let result = forward_int_result!("lua_callfunction", call_real_lua_callfunction(handle));

    result
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
            if let Some(demangled) = &origin.demangled {
                fields.demangled = Some(demangled.clone());
            }
        }
        if let Some(map_symbol) = &origin.map_symbol {
            if !has_symbol {
                let mut value = map_symbol.name.clone();
                if map_symbol.distance > 0 {
                    value.push_str(&format!("+0x{delta:x}", delta = map_symbol.distance));
                }
                fields.symbol = Some(value);
                fields.symbol_source = Some(
                    map_symbol
                        .source_label
                        .clone()
                        .unwrap_or_else(|| "map".to_string()),
                );
            } else if let Some(source) = &map_symbol.source_label {
                fields.map_source = Some(source.clone());
            }
        }
    }
    fields
}

struct ClosureDetails {
    module: Option<String>,
    module_base: Option<usize>,
    symbol: Option<String>,
    demangled: Option<String>,
}

fn describe_closure_target(ptr: *const c_void) -> ClosureDetails {
    unsafe {
        let mut info = MaybeUninit::<Dl_info>::zeroed();
        if libc::dladdr(ptr, info.as_mut_ptr()) == 0 {
            return ClosureDetails {
                module: None,
                module_base: None,
                symbol: None,
                demangled: None,
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
        let demangled = symbol
            .as_ref()
            .and_then(|symbol| demangle_symbol(symbol.as_str()));
        ClosureDetails {
            module,
            module_base,
            symbol,
            demangled,
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

fn demangle_symbol(symbol: &str) -> Option<String> {
    // retail binaries are C++ and often expose mangled names; try to recover readable signatures
    let Ok(symbol) = CString::new(symbol) else {
        return None;
    };
    let mut status: c_int = 0;
    let ptr = unsafe {
        __cxa_demangle(
            symbol.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut status,
        )
    };
    if ptr.is_null() || status != 0 {
        return None;
    }
    let demangled = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
    unsafe {
        libc::free(ptr as *mut c_void);
    }
    Some(demangled)
}

extern "C" {
    fn __cxa_demangle(
        mangled_name: *const c_char,
        output_buffer: *mut c_char,
        length: *mut usize,
        status: *mut c_int,
    ) -> *mut c_char;
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

#[derive(Clone)]
struct ClosureOrigin {
    func_addr: usize,
    module: Option<String>,
    symbol: Option<String>,
    demangled: Option<String>,
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
                    source_label: hit.source_label,
                });
        Self {
            func_addr: ptr as usize,
            module: details.module,
            symbol: details.symbol,
            demangled: details.demangled,
            map_symbol,
        }
    }
}

#[derive(Clone)]
struct MapSymbol {
    name: String,
    distance: usize,
    source_label: Option<String>,
}
