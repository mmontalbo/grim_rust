use crate::logging::log_line;
use libc::{c_char, c_double, c_int, c_void, size_t};
use std::{ffi::CStr, sync::OnceLock};

/// Retail Lua 3.2 uses a `void (*)(void)` callback type for C functions.
pub(crate) type LuaCFunction = unsafe extern "C" fn();
pub(crate) type LuaObject = u32;
type LuaPushCClosureFn = unsafe extern "C" fn(LuaCFunction, c_int);
type LuaDoFileFn = unsafe extern "C" fn(*const c_char) -> c_int;
type LuaDoStringFn = unsafe extern "C" fn(*const c_char) -> c_int;
type LuaDoBufferFn = unsafe extern "C" fn(*const c_char, size_t, *const c_char) -> c_int;
type LuaCallFn = unsafe extern "C" fn(*const c_char) -> c_int;
type LuaCallFunctionFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaGetObjNameFn = unsafe extern "C" fn(LuaObject, *mut *mut c_char) -> *mut c_char;
type LuaSetGlobalFn = unsafe extern "C" fn(*const c_char);
type LuaGetGlobalFn = unsafe extern "C" fn(*const c_char) -> LuaObject;
type LuaGetCFunctionFn = unsafe extern "C" fn(LuaObject) -> Option<LuaCFunction>;
type LuaLua2CFn = unsafe extern "C" fn(c_int) -> LuaObject;
type LuaIsStringFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaGetStringFn = unsafe extern "C" fn(LuaObject) -> *const c_char;
type LuaIsNilFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsNumberFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaGetNumberFn = unsafe extern "C" fn(LuaObject) -> c_double;
type LuaRefFn = unsafe extern "C" fn(c_int) -> c_int;
type LuaGetRefFn = unsafe extern "C" fn(c_int) -> LuaObject;
type LuaSetTagMethodFn = unsafe extern "C" fn(c_int, *const c_char);
type LuaCollectGarbageFn = unsafe extern "C" fn();
type LuaErrorFn = unsafe extern "C" fn(*const c_char);

static LUA_PUSH_CCLOSURE: OnceLock<Option<LuaPushCClosureFn>> = OnceLock::new();
static LUA_DOSTRING: OnceLock<Option<LuaDoStringFn>> = OnceLock::new();
static LUA_DOFILE: OnceLock<Option<LuaDoFileFn>> = OnceLock::new();
static LUA_DOBUFFER: OnceLock<Option<LuaDoBufferFn>> = OnceLock::new();
static LUA_CALL: OnceLock<Option<LuaCallFn>> = OnceLock::new();
static LUA_CALLFUNCTION: OnceLock<Option<LuaCallFunctionFn>> = OnceLock::new();
static LUA_GETOBJNAME: OnceLock<Option<LuaGetObjNameFn>> = OnceLock::new();
static LUA_SETGLOBAL: OnceLock<Option<LuaSetGlobalFn>> = OnceLock::new();
static LUA_GETGLOBAL: OnceLock<Option<LuaGetGlobalFn>> = OnceLock::new();
static LUA_GETCFUNCTION: OnceLock<Option<LuaGetCFunctionFn>> = OnceLock::new();
static LUA_LUA2C: OnceLock<Option<LuaLua2CFn>> = OnceLock::new();
static LUA_ISSTRING: OnceLock<Option<LuaIsStringFn>> = OnceLock::new();
static LUA_GETSTRING: OnceLock<Option<LuaGetStringFn>> = OnceLock::new();
static LUA_ISNIL: OnceLock<Option<LuaIsNilFn>> = OnceLock::new();
static LUA_ISNUMBER: OnceLock<Option<LuaIsNumberFn>> = OnceLock::new();
static LUA_GETNUMBER: OnceLock<Option<LuaGetNumberFn>> = OnceLock::new();
static LUA_REF: OnceLock<Option<LuaRefFn>> = OnceLock::new();
static LUA_GETREF: OnceLock<Option<LuaGetRefFn>> = OnceLock::new();
static LUA_SETTAGMETHOD: OnceLock<Option<LuaSetTagMethodFn>> = OnceLock::new();
static LUA_COLLECTGARBAGE: OnceLock<Option<LuaCollectGarbageFn>> = OnceLock::new();
static LUA_ERROR: OnceLock<Option<LuaErrorFn>> = OnceLock::new();

pub(crate) fn call_real_lua_push_c_closure(func: LuaCFunction, upvalues: c_int) -> bool {
    unsafe {
        match lua_push_c_closure_symbol() {
            Some(symbol) => {
                symbol(func, upvalues);
                true
            }
            None => {
                log_line("lua_pushCclosure symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_dofile(filename: *const c_char) -> Option<c_int> {
    unsafe { lua_dofile_symbol().map(|symbol| symbol(filename)) }
}

pub(crate) fn call_real_lua_dostring(chunk: *const c_char) -> Option<c_int> {
    unsafe { lua_dostring_symbol().map(|symbol| symbol(chunk)) }
}

pub(crate) fn call_real_lua_dobuffer(
    buffer: *const c_char,
    size: size_t,
    name: *const c_char,
) -> Option<c_int> {
    unsafe { lua_dobuffer_symbol().map(|symbol| symbol(buffer, size, name)) }
}

pub(crate) fn call_real_lua_call(func_name: *const c_char) -> Option<c_int> {
    unsafe { lua_call_symbol().map(|symbol| symbol(func_name)) }
}

pub(crate) fn call_real_lua_callfunction(func: LuaObject) -> Option<c_int> {
    unsafe { lua_callfunction_symbol().map(|symbol| symbol(func)) }
}

pub(crate) fn call_real_lua_getobjname(
    handle: LuaObject,
) -> Option<(Option<String>, Option<String>)> {
    unsafe {
        lua_getobjname_symbol().map(|symbol| {
            let mut name_ptr: *mut c_char = std::ptr::null_mut();
            let kind_ptr = symbol(handle, &mut name_ptr);
            (cstr_opt(kind_ptr), cstr_opt(name_ptr))
        })
    }
}

pub(crate) fn call_real_lua_setglobal(name: *const c_char) -> bool {
    unsafe {
        match lua_setglobal_symbol() {
            Some(symbol) => {
                symbol(name);
                true
            }
            None => {
                log_line("lua_setglobal symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_getglobal(name: *const c_char) -> Option<LuaObject> {
    unsafe { lua_getglobal_symbol().map(|symbol| symbol(name)) }
}

pub(crate) fn call_real_lua_getcfunction(handle: LuaObject) -> Option<LuaCFunction> {
    unsafe { lua_getcfunction_symbol().and_then(|symbol| symbol(handle)) }
}

pub(crate) fn call_real_lua_getparam(index: c_int) -> Option<LuaObject> {
    unsafe {
        lua_lua2c_symbol()
            .map(|symbol| symbol(index))
            .filter(|obj| *obj != 0)
    }
}

pub(crate) fn call_real_lua_isstring(object: LuaObject) -> bool {
    unsafe {
        lua_isstring_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_getstring(object: LuaObject) -> Option<String> {
    unsafe {
        lua_getstring_symbol().and_then(|symbol| {
            let ptr = symbol(object);
            if ptr.is_null() {
                None
            } else {
                Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
            }
        })
    }
}

pub(crate) fn call_real_lua_isnil(object: LuaObject) -> bool {
    unsafe {
        lua_isnil_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_isnumber(object: LuaObject) -> bool {
    unsafe {
        lua_isnumber_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_getnumber(object: LuaObject) -> Option<f64> {
    unsafe { lua_getnumber_symbol().map(|symbol| symbol(object)) }
}

pub(crate) fn call_real_lua_ref(lock: c_int) -> Option<c_int> {
    unsafe { lua_ref_symbol().map(|symbol| symbol(lock)) }
}

pub(crate) fn call_real_lua_getref(reference: c_int) -> Option<LuaObject> {
    unsafe { lua_getref_symbol().map(|symbol| symbol(reference)) }
}

pub(crate) fn call_real_lua_settagmethod(tag: c_int, event: *const c_char) -> bool {
    unsafe {
        match lua_settagmethod_symbol() {
            Some(symbol) => {
                symbol(tag, event);
                true
            }
            None => {
                log_line("lua_settagmethod symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_collectgarbage() -> bool {
    unsafe {
        match lua_collectgarbage_symbol() {
            Some(symbol) => {
                symbol();
                true
            }
            None => {
                log_line("lua_collectgarbage symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_error(message: *const c_char) -> bool {
    unsafe {
        match lua_error_symbol() {
            Some(symbol) => {
                symbol(message);
                true
            }
            None => {
                log_line("lua_error symbol missing; skipping trace");
                false
            }
        }
    }
}

fn lua_push_c_closure_symbol() -> Option<LuaPushCClosureFn> {
    *LUA_PUSH_CCLOSURE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushCclosure\0",
            label: "lua_pushCclosure",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushCClosureFn>(ptr))
    })
}

fn lua_dostring_symbol() -> Option<LuaDoStringFn> {
    *LUA_DOSTRING.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_dostring\0",
            label: "lua_dostring",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaDoStringFn>(ptr))
    })
}

fn lua_dofile_symbol() -> Option<LuaDoFileFn> {
    *LUA_DOFILE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_dofile\0",
            label: "lua_dofile",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaDoFileFn>(ptr))
    })
}

fn lua_dobuffer_symbol() -> Option<LuaDoBufferFn> {
    *LUA_DOBUFFER.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_dobuffer\0",
            label: "lua_dobuffer",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaDoBufferFn>(ptr))
    })
}

fn lua_call_symbol() -> Option<LuaCallFn> {
    *LUA_CALL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_call\0",
            label: "lua_call",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaCallFn>(ptr))
    })
}

fn lua_callfunction_symbol() -> Option<LuaCallFunctionFn> {
    *LUA_CALLFUNCTION.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_callfunction\0",
            label: "lua_callfunction",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaCallFunctionFn>(ptr))
    })
}

fn lua_getobjname_symbol() -> Option<LuaGetObjNameFn> {
    *LUA_GETOBJNAME.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getobjname\0",
            label: "lua_getobjname",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetObjNameFn>(ptr))
    })
}

fn lua_setglobal_symbol() -> Option<LuaSetGlobalFn> {
    *LUA_SETGLOBAL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_setglobal\0",
            label: "lua_setglobal",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaSetGlobalFn>(ptr))
    })
}

fn lua_getglobal_symbol() -> Option<LuaGetGlobalFn> {
    *LUA_GETGLOBAL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getglobal\0",
            label: "lua_getglobal",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetGlobalFn>(ptr))
    })
}

fn lua_getcfunction_symbol() -> Option<LuaGetCFunctionFn> {
    *LUA_GETCFUNCTION.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getcfunction\0",
            label: "lua_getcfunction",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetCFunctionFn>(ptr))
    })
}

fn lua_lua2c_symbol() -> Option<LuaLua2CFn> {
    *LUA_LUA2C.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_lua2C\0",
            label: "lua_lua2C",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaLua2CFn>(ptr))
    })
}

fn lua_isstring_symbol() -> Option<LuaIsStringFn> {
    *LUA_ISSTRING.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_isstring\0",
            label: "lua_isstring",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsStringFn>(ptr))
    })
}

fn lua_getstring_symbol() -> Option<LuaGetStringFn> {
    *LUA_GETSTRING.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getstring\0",
            label: "lua_getstring",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetStringFn>(ptr))
    })
}

fn lua_isnil_symbol() -> Option<LuaIsNilFn> {
    *LUA_ISNIL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_isnil\0",
            label: "lua_isnil",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsNilFn>(ptr))
    })
}

fn lua_isnumber_symbol() -> Option<LuaIsNumberFn> {
    *LUA_ISNUMBER.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_isnumber\0",
            label: "lua_isnumber",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsNumberFn>(ptr))
    })
}

fn lua_getnumber_symbol() -> Option<LuaGetNumberFn> {
    *LUA_GETNUMBER.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getnumber\0",
            label: "lua_getnumber",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetNumberFn>(ptr))
    })
}

fn lua_ref_symbol() -> Option<LuaRefFn> {
    *LUA_REF.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_ref\0",
            label: "lua_ref",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaRefFn>(ptr))
    })
}

fn lua_getref_symbol() -> Option<LuaGetRefFn> {
    *LUA_GETREF.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getref\0",
            label: "lua_getref",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetRefFn>(ptr))
    })
}

fn lua_settagmethod_symbol() -> Option<LuaSetTagMethodFn> {
    *LUA_SETTAGMETHOD.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_settagmethod\0",
            label: "lua_settagmethod",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaSetTagMethodFn>(ptr))
    })
}

fn lua_collectgarbage_symbol() -> Option<LuaCollectGarbageFn> {
    *LUA_COLLECTGARBAGE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_collectgarbage\0",
            label: "lua_collectgarbage",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaCollectGarbageFn>(ptr))
    })
}

fn lua_error_symbol() -> Option<LuaErrorFn> {
    *LUA_ERROR.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_error\0",
            label: "lua_error",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaErrorFn>(ptr))
    })
}

#[derive(Clone, Copy)]
struct SymbolVariant {
    symbol: &'static [u8],
    label: &'static str,
}

unsafe fn resolve_symbol_with_variants(variants: &[SymbolVariant]) -> Option<*mut c_void> {
    for variant in variants {
        if let Some(ptr) = resolve_symbol_ptr(variant.symbol, variant.label) {
            return Some(ptr);
        }
    }
    None
}

unsafe fn resolve_symbol_ptr(symbol: &[u8], label: &str) -> Option<*mut c_void> {
    let mut ptr = libc::dlsym(libc::RTLD_NEXT, symbol.as_ptr() as *const c_char);
    if ptr.is_null() {
        ptr = libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr() as *const c_char);
    }
    if ptr.is_null() {
        log_line(&format!("failed to resolve {label} via dlsym"));
        None
    } else {
        Some(ptr)
    }
}

unsafe fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}
