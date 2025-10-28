use crate::{
    bootstrap::stringify_lua_object,
    env::telemetry_stack_dump_enabled,
    logging::{log_line, sanitize_lua_string_fragment},
};
use libc::{c_char, c_int, c_void};
use std::{ffi::CStr, sync::OnceLock};

pub(crate) type LuaObject = *mut c_void;
pub(crate) type LuaCFunction = unsafe extern "C" fn();
pub(crate) type LuaDofileFn = unsafe extern "C" fn(*mut c_char) -> c_int;
type LuaPushCClosureFn = unsafe extern "C" fn(LuaCFunction, c_int);
type LuaSetGlobalFn = unsafe extern "C" fn(*mut c_char);
type LuaGetGlobalFn = unsafe extern "C" fn(*const c_char) -> LuaObject;
type LuaGetParamFn = unsafe extern "C" fn(c_int) -> LuaObject;
type LuaIsStringFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsFunctionFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaGetStringFn = unsafe extern "C" fn(LuaObject) -> *const c_char;
type LuaPushNumberFn = unsafe extern "C" fn(f64);
type LuaPopFn = unsafe extern "C" fn() -> LuaObject;
type LuaPushObjectFn = unsafe extern "C" fn(LuaObject);
type LuaTagFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaBeginBlockFn = unsafe extern "C" fn();
type LuaEndBlockFn = unsafe extern "C" fn();
type LuaCallFunctionFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaStrlibopenFn = unsafe extern "C" fn();

static LUA_DOFILE: OnceLock<Option<LuaDofileFn>> = OnceLock::new();
static LUA_PUSH_CLOSURE: OnceLock<Option<LuaPushCClosureFn>> = OnceLock::new();
static LUA_SET_GLOBAL: OnceLock<Option<LuaSetGlobalFn>> = OnceLock::new();
static LUA_GET_GLOBAL: OnceLock<Option<LuaGetGlobalFn>> = OnceLock::new();
static LUA_GET_PARAM: OnceLock<Option<LuaGetParamFn>> = OnceLock::new();
static LUA_IS_STRING: OnceLock<Option<LuaIsStringFn>> = OnceLock::new();
static LUA_IS_FUNCTION: OnceLock<Option<LuaIsFunctionFn>> = OnceLock::new();
static LUA_GET_STRING: OnceLock<Option<LuaGetStringFn>> = OnceLock::new();
static LUA_PUSH_NUMBER: OnceLock<Option<LuaPushNumberFn>> = OnceLock::new();
static LUA_POP: OnceLock<Option<LuaPopFn>> = OnceLock::new();
static LUA_PUSH_OBJECT: OnceLock<Option<LuaPushObjectFn>> = OnceLock::new();
static LUA_TAG: OnceLock<Option<LuaTagFn>> = OnceLock::new();
static LUA_BEGIN_BLOCK: OnceLock<Option<LuaBeginBlockFn>> = OnceLock::new();
static LUA_END_BLOCK: OnceLock<Option<LuaEndBlockFn>> = OnceLock::new();
static LUA_CALL_FUNCTION: OnceLock<Option<LuaCallFunctionFn>> = OnceLock::new();
static LUA_STRLIBOPEN: OnceLock<Option<LuaStrlibopenFn>> = OnceLock::new();

#[derive(Clone, Copy)]
struct SymbolVariant {
    symbol: &'static [u8],
    label: &'static str,
    success_message: Option<&'static str>,
}

unsafe fn resolve_symbol_ptr(symbol: &[u8], label: &str) -> Option<*mut c_void> {
    let ptr = libc::dlsym(libc::RTLD_NEXT, symbol.as_ptr() as *const c_char);
    if ptr.is_null() {
        log_line(&format!("failed to resolve {label} via dlsym"));
        None
    } else {
        Some(ptr)
    }
}

unsafe fn resolve_symbol_with_variants(variants: &[SymbolVariant]) -> Option<*mut c_void> {
    for variant in variants {
        if let Some(ptr) = resolve_symbol_ptr(variant.symbol, variant.label) {
            if let Some(message) = variant.success_message {
                log_line(message);
            }
            return Some(ptr);
        }
    }
    None
}

macro_rules! resolve_fn {
    ($name:ident, $slot:ident, $ty:ty, $variants:expr) => {
        pub(crate) unsafe fn $name() -> Option<$ty> {
            $slot
                .get_or_init(|| {
                    resolve_symbol_with_variants($variants)
                        .map(|ptr| std::mem::transmute::<*mut c_void, $ty>(ptr))
                })
                .clone()
        }
    };
}

resolve_fn!(
    resolve_lua_dofile,
    LUA_DOFILE,
    LuaDofileFn,
    &[SymbolVariant {
        symbol: b"lua_dofile\0",
        label: "lua_dofile",
        success_message: Some("resolved lua_dofile symbol"),
    }]
);
resolve_fn!(
    resolve_lua_pushcclosure,
    LUA_PUSH_CLOSURE,
    LuaPushCClosureFn,
    &[
        SymbolVariant {
            symbol: b"lua_pushcclosure\0",
            label: "lua_pushcclosure",
            success_message: None,
        },
        SymbolVariant {
            symbol: b"lua_pushCclosure\0",
            label: "lua_pushCclosure",
            success_message: Some("resolved lua_pushcclosure via alias lua_pushCclosure"),
        },
    ]
);
resolve_fn!(
    resolve_lua_setglobal,
    LUA_SET_GLOBAL,
    LuaSetGlobalFn,
    &[SymbolVariant {
        symbol: b"lua_setglobal\0",
        label: "lua_setglobal",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_getglobal,
    LUA_GET_GLOBAL,
    LuaGetGlobalFn,
    &[SymbolVariant {
        symbol: b"lua_getglobal\0",
        label: "lua_getglobal",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_getparam,
    LUA_GET_PARAM,
    LuaGetParamFn,
    &[SymbolVariant {
        symbol: b"lua_getparam\0",
        label: "lua_getparam",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_isstring,
    LUA_IS_STRING,
    LuaIsStringFn,
    &[SymbolVariant {
        symbol: b"lua_isstring\0",
        label: "lua_isstring",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_isfunction,
    LUA_IS_FUNCTION,
    LuaIsFunctionFn,
    &[SymbolVariant {
        symbol: b"lua_isfunction\0",
        label: "lua_isfunction",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_getstring,
    LUA_GET_STRING,
    LuaGetStringFn,
    &[SymbolVariant {
        symbol: b"lua_getstring\0",
        label: "lua_getstring",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_pushnumber,
    LUA_PUSH_NUMBER,
    LuaPushNumberFn,
    &[SymbolVariant {
        symbol: b"lua_pushnumber\0",
        label: "lua_pushnumber",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_pop,
    LUA_POP,
    LuaPopFn,
    &[SymbolVariant {
        symbol: b"lua_pop\0",
        label: "lua_pop",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_pushobject,
    LUA_PUSH_OBJECT,
    LuaPushObjectFn,
    &[SymbolVariant {
        symbol: b"lua_pushobject\0",
        label: "lua_pushobject",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_tag,
    LUA_TAG,
    LuaTagFn,
    &[SymbolVariant {
        symbol: b"lua_tag\0",
        label: "lua_tag",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_beginblock,
    LUA_BEGIN_BLOCK,
    LuaBeginBlockFn,
    &[SymbolVariant {
        symbol: b"lua_beginblock\0",
        label: "lua_beginblock",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_endblock,
    LUA_END_BLOCK,
    LuaEndBlockFn,
    &[SymbolVariant {
        symbol: b"lua_endblock\0",
        label: "lua_endblock",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_callfunction,
    LUA_CALL_FUNCTION,
    LuaCallFunctionFn,
    &[SymbolVariant {
        symbol: b"lua_callfunction\0",
        label: "lua_callfunction",
        success_message: None,
    }]
);
resolve_fn!(
    resolve_lua_strlibopen,
    LUA_STRLIBOPEN,
    LuaStrlibopenFn,
    &[SymbolVariant {
        symbol: b"lua_strlibopen\0",
        label: "lua_strlibopen",
        success_message: Some("lua_strlibopen available"),
    }]
);

pub(crate) fn filename_from_ptr(filename: *mut c_char) -> Option<String> {
    if filename.is_null() {
        return None;
    }

    unsafe { Some(CStr::from_ptr(filename).to_string_lossy().into_owned()) }
}

pub(crate) fn call_real_lua_dofile(filename: *mut c_char) -> c_int {
    unsafe {
        match resolve_lua_dofile() {
            Some(real) => real(filename),
            None => {
                log_line("lua_dofile unavailable; returning error");
                -1
            }
        }
    }
}

pub(crate) fn is_system_script(path: &str) -> bool {
    let trimmed = path.trim();
    let filename = trimmed
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(trimmed);
    filename.eq_ignore_ascii_case("_system.lua")
}

#[derive(Clone)]
pub(crate) enum BootstrapGlobalSnapshot {
    Message(String),
    Detail(String),
}

pub(crate) fn snapshot_bootstrap_error_global(global: &[u8]) -> BootstrapGlobalSnapshot {
    unsafe {
        let Some(get_global) = resolve_lua_getglobal() else {
            return BootstrapGlobalSnapshot::Detail("lua_getglobal missing".to_string());
        };
        let Some(is_string) = resolve_lua_isstring() else {
            return BootstrapGlobalSnapshot::Detail("lua_isstring missing".to_string());
        };
        let Some(get_string) = resolve_lua_getstring() else {
            return BootstrapGlobalSnapshot::Detail("lua_getstring missing".to_string());
        };
        let obj = get_global(global.as_ptr() as *const c_char);
        if obj.is_null() {
            return BootstrapGlobalSnapshot::Detail("nil".to_string());
        }
        if is_string(obj) == 0 {
            let mut components = Vec::new();
            if let Some(tag_fn) = resolve_lua_tag() {
                let tag = tag_fn(obj);
                components.push(format!("tag={tag}"));
            }
            if let Some(text) = stringify_lua_object(obj) {
                if !text.is_empty() {
                    let sanitized = sanitize_lua_string_fragment(&text);
                    components.push(format!("tostring={sanitized}"));
                }
            }
            let detail = if components.is_empty() {
                "non-string object".to_string()
            } else {
                format!("non-string object ({})", components.join(", "))
            };
            return BootstrapGlobalSnapshot::Detail(detail);
        }
        let ptr = get_string(obj);
        if ptr.is_null() {
            return BootstrapGlobalSnapshot::Detail("string pointer null".to_string());
        }
        let message = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        BootstrapGlobalSnapshot::Message(message)
    }
}

pub(crate) fn log_bootstrap_error_global(context: &str, global: &[u8]) -> BootstrapGlobalSnapshot {
    let label = format!("telemetry bootstrap error global ({context})");
    let snapshot = snapshot_bootstrap_error_global(global);
    match &snapshot {
        BootstrapGlobalSnapshot::Message(message) => {
            if message.is_empty() {
                log_line(&format!("{label}: <empty>"));
            } else {
                let sanitized = sanitize_lua_string_fragment(message);
                log_line(&format!("{label}: {sanitized}"));
            }
        }
        BootstrapGlobalSnapshot::Detail(detail) => {
            if detail == "non-string object (tag=-7)" || detail == "nil" {
                return snapshot;
            }
            log_line(&format!("{label}: {detail}"));
        }
    }
    snapshot
}

pub(crate) fn log_lua_stack_snapshot(context: &str, limit: usize) {
    let prefix = format!("lua stack snapshot ({context})");
    if !telemetry_stack_dump_enabled() {
        log_line(&format!(
            "{prefix}: stack dump disabled (set GRIM_TELEMETRY_STACK_DUMP=1 to enable)"
        ));
        return;
    }
    unsafe {
        let Some(pop) = resolve_lua_pop() else {
            log_line(&format!("{prefix}: lua_pop missing; skipping dump"));
            return;
        };
        let Some(push_object) = resolve_lua_pushobject() else {
            log_line(&format!("{prefix}: lua_pushobject missing; skipping dump"));
            return;
        };
        let Some(tag_fn) = resolve_lua_tag() else {
            log_line(&format!("{prefix}: lua_tag missing; skipping dump"));
            return;
        };
        let is_string = resolve_lua_isstring();
        let get_string = resolve_lua_getstring();
        log_line(&format!(
            "{prefix}: attempting to capture up to {limit} entries"
        ));
        log_line(&format!("{prefix}: invoking lua_pop()"));
        let obj = pop();
        log_line(&format!(
            "{prefix}: lua_pop returned handle {:#x}",
            obj as usize
        ));
        if obj.is_null() {
            log_line(&format!("{prefix}: stack empty or pop returned nil"));
            return;
        }
        let tag = tag_fn(obj);
        let mut message = format!("{prefix}[0] tag={tag}");
        if let Some(is_string_fn) = is_string {
            if is_string_fn(obj) != 0 {
                if let Some(get_string_fn) = get_string {
                    let ptr = get_string_fn(obj);
                    if !ptr.is_null() {
                        let value = CStr::from_ptr(ptr).to_string_lossy();
                        let sanitized = sanitize_lua_string_fragment(&value);
                        message.push_str(&format!(" string=\"{sanitized}\""));
                    } else {
                        message.push_str(" string=<null>");
                    }
                } else {
                    message.push_str(" string=<getstring-missing>");
                }
            }
        }
        log_line(&message);
        push_object(obj);
    }
}
