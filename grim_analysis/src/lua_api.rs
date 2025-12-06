//! Low-level bindings that resolve the retail Lua 3.1 symbols at runtime.
//!
//! Functions here perform lazy `dlsym` lookups for the real VM entry points and
//! provide thin wrappers that return `Option`/`bool` for easier error handling in
//! the tracing layer.
use crate::logging::log_line;
use libc::{c_char, c_double, c_float, c_int, c_void, size_t};
use std::{ffi::CStr, sync::OnceLock};

/// Retail Lua 3.1 uses a `void (*)(void)` callback type for C functions.
pub(crate) type LuaCFunction = unsafe extern "C" fn();
pub(crate) type LuaObject = u32;
pub(crate) type LuaState = *mut c_void;
type LuaPushCClosureFn = unsafe extern "C" fn(LuaCFunction, c_int);
type LuaDoFileFn = unsafe extern "C" fn(*const c_char) -> c_int;
type LuaDoStringFn = unsafe extern "C" fn(*const c_char) -> c_int;
type LuaDoBufferFn = unsafe extern "C" fn(*const c_char, size_t, *const c_char) -> c_int;
type LuaCallFn = unsafe extern "C" fn(*const c_char) -> c_int;
type LuaCallFunctionFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaOpenFn = unsafe extern "C" fn() -> LuaState;
type LuaNewStateFn = unsafe extern "C" fn() -> LuaState;
type LuaNewThreadFn = unsafe extern "C" fn(LuaState) -> LuaState;
type LuaGetObjNameFn = unsafe extern "C" fn(LuaObject, *mut *mut c_char) -> *mut c_char;
type LuaSetGlobalFn = unsafe extern "C" fn(*const c_char);
type LuaGetGlobalFn = unsafe extern "C" fn(*const c_char) -> LuaObject;
type LuaGetCFunctionFn = unsafe extern "C" fn(LuaObject) -> Option<LuaCFunction>;
type LuaLua2CFn = unsafe extern "C" fn(c_int) -> LuaObject;
type LuaIsNumberFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsStringFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsTableFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsFunctionFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsCFunctionFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaIsUserdataFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaGetNumberFn = unsafe extern "C" fn(LuaObject) -> c_double;
type LuaGetStringFn = unsafe extern "C" fn(LuaObject) -> *const c_char;
type LuaTagFn = unsafe extern "C" fn(LuaObject) -> c_int;
type LuaPushNumberFn = unsafe extern "C" fn(c_float);
type LuaPushStringFn = unsafe extern "C" fn(*const c_char);
type LuaPushLStringFn = unsafe extern "C" fn(*const c_char, size_t);
type LuaPushNilFn = unsafe extern "C" fn();
type LuaPushValueFn = unsafe extern "C" fn(c_int);
type LuaPushUsertagFn = unsafe extern "C" fn(c_int, c_int);
type LuaPushObjectFn = unsafe extern "C" fn(LuaObject);
type LuaCreateTableFn = unsafe extern "C" fn() -> LuaObject;
type LuaSetTableFn = unsafe extern "C" fn();
type LuaRawSetTableFn = unsafe extern "C" fn();
type LuaGetTableFn = unsafe extern "C" fn() -> LuaObject;
type LuaRawGetTableFn = unsafe extern "C" fn() -> LuaObject;
type LuaRawGetGlobalFn = unsafe extern "C" fn(*const c_char) -> LuaObject;
type LuaRawSetGlobalFn = unsafe extern "C" fn(*const c_char);
type LuaUnrefFn = unsafe extern "C" fn(c_int);
type LuaSetFallbackFn = unsafe extern "C" fn(*const c_char, LuaCFunction) -> LuaObject;
type LuaNewTagFn = unsafe extern "C" fn() -> c_int;
type LuaCopyTagMethodsFn = unsafe extern "C" fn(c_int, c_int) -> c_int;
type LuaSetTagFn = unsafe extern "C" fn(c_int);
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
static LUA_OPEN: OnceLock<Option<LuaOpenFn>> = OnceLock::new();
static LUA_NEWSTATE: OnceLock<Option<LuaNewStateFn>> = OnceLock::new();
static LUA_NEWTHREAD: OnceLock<Option<LuaNewThreadFn>> = OnceLock::new();
static LUA_GETOBJNAME: OnceLock<Option<LuaGetObjNameFn>> = OnceLock::new();
static LUA_SETGLOBAL: OnceLock<Option<LuaSetGlobalFn>> = OnceLock::new();
static LUA_GETGLOBAL: OnceLock<Option<LuaGetGlobalFn>> = OnceLock::new();
static LUA_GETCFUNCTION: OnceLock<Option<LuaGetCFunctionFn>> = OnceLock::new();
static LUA_LUA2C: OnceLock<Option<LuaLua2CFn>> = OnceLock::new();
static LUA_ISNUMBER: OnceLock<Option<LuaIsNumberFn>> = OnceLock::new();
static LUA_ISSTRING: OnceLock<Option<LuaIsStringFn>> = OnceLock::new();
static LUA_ISTABLE: OnceLock<Option<LuaIsTableFn>> = OnceLock::new();
static LUA_ISFUNCTION: OnceLock<Option<LuaIsFunctionFn>> = OnceLock::new();
static LUA_ISCFUNCTION: OnceLock<Option<LuaIsCFunctionFn>> = OnceLock::new();
static LUA_ISUSERDATA: OnceLock<Option<LuaIsUserdataFn>> = OnceLock::new();
static LUA_GETNUMBER: OnceLock<Option<LuaGetNumberFn>> = OnceLock::new();
static LUA_GETSTRING: OnceLock<Option<LuaGetStringFn>> = OnceLock::new();
static LUA_TAG: OnceLock<Option<LuaTagFn>> = OnceLock::new();
static LUA_PUSHNUMBER: OnceLock<Option<LuaPushNumberFn>> = OnceLock::new();
static LUA_PUSHSTRING: OnceLock<Option<LuaPushStringFn>> = OnceLock::new();
static LUA_PUSHLSTRING: OnceLock<Option<LuaPushLStringFn>> = OnceLock::new();
static LUA_PUSHNIL: OnceLock<Option<LuaPushNilFn>> = OnceLock::new();
static LUA_PUSHVALUE: OnceLock<Option<LuaPushValueFn>> = OnceLock::new();
static LUA_PUSHUSERTAG: OnceLock<Option<LuaPushUsertagFn>> = OnceLock::new();
static LUA_PUSHOBJECT: OnceLock<Option<LuaPushObjectFn>> = OnceLock::new();
static LUA_CREATETABLE: OnceLock<Option<LuaCreateTableFn>> = OnceLock::new();
static LUA_SETTABLE: OnceLock<Option<LuaSetTableFn>> = OnceLock::new();
static LUA_RAWSETTABLE: OnceLock<Option<LuaRawSetTableFn>> = OnceLock::new();
static LUA_GETTABLE: OnceLock<Option<LuaGetTableFn>> = OnceLock::new();
static LUA_RAWGETTABLE: OnceLock<Option<LuaRawGetTableFn>> = OnceLock::new();
static LUA_RAWGETGLOBAL: OnceLock<Option<LuaRawGetGlobalFn>> = OnceLock::new();
static LUA_RAWSETGLOBAL: OnceLock<Option<LuaRawSetGlobalFn>> = OnceLock::new();
static LUA_UNREF: OnceLock<Option<LuaUnrefFn>> = OnceLock::new();
static LUA_SETFALLBACK: OnceLock<Option<LuaSetFallbackFn>> = OnceLock::new();
static LUA_NEWTAG: OnceLock<Option<LuaNewTagFn>> = OnceLock::new();
static LUA_COPYTAGMETHODS: OnceLock<Option<LuaCopyTagMethodsFn>> = OnceLock::new();
static LUA_SETTAG: OnceLock<Option<LuaSetTagFn>> = OnceLock::new();
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

pub(crate) fn call_real_lua_open() -> Option<LuaState> {
    unsafe { lua_open_symbol().map(|symbol| symbol()) }
}

pub(crate) fn call_real_lua_newstate() -> Option<LuaState> {
    unsafe { lua_newstate_symbol().map(|symbol| symbol()) }
}

pub(crate) fn call_real_lua_newthread(state: LuaState) -> Option<LuaState> {
    unsafe { lua_newthread_symbol().map(|symbol| symbol(state)) }
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

pub(crate) fn call_real_lua_isnumber(object: LuaObject) -> bool {
    unsafe {
        lua_isnumber_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_isstring(object: LuaObject) -> bool {
    unsafe {
        lua_isstring_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_istable(object: LuaObject) -> bool {
    unsafe {
        lua_istable_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_isfunction(object: LuaObject) -> bool {
    unsafe {
        lua_isfunction_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_iscfunction(object: LuaObject) -> bool {
    unsafe {
        lua_iscfunction_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_isuserdata(object: LuaObject) -> bool {
    unsafe {
        lua_isuserdata_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

pub(crate) fn call_real_lua_getnumber(object: LuaObject) -> Option<c_double> {
    unsafe { lua_getnumber_symbol().map(|symbol| symbol(object)) }
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

pub(crate) fn call_real_lua_tag(object: LuaObject) -> Option<c_int> {
    unsafe { lua_tag_symbol().map(|symbol| symbol(object)) }
}

pub(crate) fn call_real_lua_pushnumber(value: c_float) -> bool {
    unsafe {
        match lua_pushnumber_symbol() {
            Some(symbol) => {
                symbol(value);
                true
            }
            None => {
                log_line("lua_pushnumber symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_pushstring(value: *const c_char) -> bool {
    unsafe {
        match lua_pushstring_symbol() {
            Some(symbol) => {
                symbol(value);
                true
            }
            None => {
                log_line("lua_pushstring symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_pushlstring(value: *const c_char, len: size_t) -> bool {
    unsafe {
        match lua_pushlstring_symbol() {
            Some(symbol) => {
                symbol(value, len);
                true
            }
            None => {
                log_line("lua_pushlstring symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_pushnil() -> bool {
    unsafe {
        match lua_pushnil_symbol() {
            Some(symbol) => {
                symbol();
                true
            }
            None => {
                log_line("lua_pushnil symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_pushvalue(index: c_int) -> bool {
    unsafe {
        match lua_pushvalue_symbol() {
            Some(symbol) => {
                symbol(index);
                true
            }
            None => {
                log_line("lua_pushvalue symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_pushusertag(id: c_int, tag: c_int) -> bool {
    unsafe {
        match lua_pushusertag_symbol() {
            Some(symbol) => {
                symbol(id, tag);
                true
            }
            None => {
                log_line("lua_pushusertag symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_pushobject(object: LuaObject) -> bool {
    unsafe {
        match lua_pushobject_symbol() {
            Some(symbol) => {
                symbol(object);
                true
            }
            None => {
                log_line("lua_pushobject symbol missing; skipping push");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_createtable() -> Option<LuaObject> {
    unsafe { lua_createtable_symbol().map(|symbol| symbol()) }
}

pub(crate) fn call_real_lua_settable() -> bool {
    unsafe {
        match lua_settable_symbol() {
            Some(symbol) => {
                symbol();
                true
            }
            None => {
                log_line("lua_settable symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_rawsettable() -> bool {
    unsafe {
        match lua_rawsettable_symbol() {
            Some(symbol) => {
                symbol();
                true
            }
            None => {
                log_line("lua_rawsettable symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_gettable() -> Option<LuaObject> {
    unsafe { lua_gettable_symbol().map(|symbol| symbol()) }
}

pub(crate) fn call_real_lua_rawgettable() -> Option<LuaObject> {
    unsafe { lua_rawgettable_symbol().map(|symbol| symbol()) }
}

pub(crate) fn call_real_lua_rawgetglobal(name: *const c_char) -> Option<LuaObject> {
    unsafe { lua_rawgetglobal_symbol().map(|symbol| symbol(name)) }
}

pub(crate) fn call_real_lua_rawsetglobal(name: *const c_char) -> bool {
    unsafe {
        match lua_rawsetglobal_symbol() {
            Some(symbol) => {
                symbol(name);
                true
            }
            None => {
                log_line("lua_rawsetglobal symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_unref(reference: c_int) -> bool {
    unsafe {
        match lua_unref_symbol() {
            Some(symbol) => {
                symbol(reference);
                true
            }
            None => {
                log_line("lua_unref symbol missing; skipping trace");
                false
            }
        }
    }
}

pub(crate) fn call_real_lua_setfallback(
    event: *const c_char,
    func: LuaCFunction,
) -> Option<LuaObject> {
    unsafe { lua_setfallback_symbol().map(|symbol| symbol(event, func)) }
}

pub(crate) fn call_real_lua_newtag() -> Option<c_int> {
    unsafe { lua_newtag_symbol().map(|symbol| symbol()) }
}

pub(crate) fn call_real_lua_copytagmethods(tagto: c_int, tagfrom: c_int) -> Option<c_int> {
    unsafe { lua_copytagmethods_symbol().map(|symbol| symbol(tagto, tagfrom)) }
}

pub(crate) fn call_real_lua_settag(tag: c_int) -> bool {
    unsafe {
        match lua_settag_symbol() {
            Some(symbol) => {
                symbol(tag);
                true
            }
            None => {
                log_line("lua_settag symbol missing; skipping trace");
                false
            }
        }
    }
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

fn lua_open_symbol() -> Option<LuaOpenFn> {
    *LUA_OPEN.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_open\0",
            label: "lua_open",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaOpenFn>(ptr))
    })
}

fn lua_newstate_symbol() -> Option<LuaNewStateFn> {
    *LUA_NEWSTATE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_newstate\0",
            label: "lua_newstate",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaNewStateFn>(ptr))
    })
}

fn lua_newthread_symbol() -> Option<LuaNewThreadFn> {
    *LUA_NEWTHREAD.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_newthread\0",
            label: "lua_newthread",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaNewThreadFn>(ptr))
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

fn lua_isnumber_symbol() -> Option<LuaIsNumberFn> {
    *LUA_ISNUMBER.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_isnumber\0",
            label: "lua_isnumber",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsNumberFn>(ptr))
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

fn lua_istable_symbol() -> Option<LuaIsTableFn> {
    *LUA_ISTABLE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_istable\0",
            label: "lua_istable",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsTableFn>(ptr))
    })
}

fn lua_isfunction_symbol() -> Option<LuaIsFunctionFn> {
    *LUA_ISFUNCTION.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_isfunction\0",
            label: "lua_isfunction",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsFunctionFn>(ptr))
    })
}

fn lua_iscfunction_symbol() -> Option<LuaIsCFunctionFn> {
    *LUA_ISCFUNCTION.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_iscfunction\0",
            label: "lua_iscfunction",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsCFunctionFn>(ptr))
    })
}

fn lua_isuserdata_symbol() -> Option<LuaIsUserdataFn> {
    *LUA_ISUSERDATA.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_isuserdata\0",
            label: "lua_isuserdata",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaIsUserdataFn>(ptr))
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

fn lua_getstring_symbol() -> Option<LuaGetStringFn> {
    *LUA_GETSTRING.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_getstring\0",
            label: "lua_getstring",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetStringFn>(ptr))
    })
}

fn lua_tag_symbol() -> Option<LuaTagFn> {
    *LUA_TAG.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_tag\0",
            label: "lua_tag",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaTagFn>(ptr))
    })
}

fn lua_pushnumber_symbol() -> Option<LuaPushNumberFn> {
    *LUA_PUSHNUMBER.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushnumber\0",
            label: "lua_pushnumber",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushNumberFn>(ptr))
    })
}

fn lua_pushstring_symbol() -> Option<LuaPushStringFn> {
    *LUA_PUSHSTRING.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushstring\0",
            label: "lua_pushstring",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushStringFn>(ptr))
    })
}

fn lua_pushlstring_symbol() -> Option<LuaPushLStringFn> {
    *LUA_PUSHLSTRING.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushlstring\0",
            label: "lua_pushlstring",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushLStringFn>(ptr))
    })
}

fn lua_pushusertag_symbol() -> Option<LuaPushUsertagFn> {
    *LUA_PUSHUSERTAG.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushusertag\0",
            label: "lua_pushusertag",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushUsertagFn>(ptr))
    })
}

fn lua_pushobject_symbol() -> Option<LuaPushObjectFn> {
    *LUA_PUSHOBJECT.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushobject\0",
            label: "lua_pushobject",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushObjectFn>(ptr))
    })
}

fn lua_pushnil_symbol() -> Option<LuaPushNilFn> {
    *LUA_PUSHNIL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushnil\0",
            label: "lua_pushnil",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushNilFn>(ptr))
    })
}

fn lua_pushvalue_symbol() -> Option<LuaPushValueFn> {
    *LUA_PUSHVALUE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushvalue\0",
            label: "lua_pushvalue",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushValueFn>(ptr))
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

fn lua_unref_symbol() -> Option<LuaUnrefFn> {
    *LUA_UNREF.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_unref\0",
            label: "lua_unref",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaUnrefFn>(ptr))
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

fn lua_createtable_symbol() -> Option<LuaCreateTableFn> {
    *LUA_CREATETABLE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_createtable\0",
            label: "lua_createtable",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaCreateTableFn>(ptr))
    })
}

fn lua_settable_symbol() -> Option<LuaSetTableFn> {
    *LUA_SETTABLE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_settable\0",
            label: "lua_settable",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaSetTableFn>(ptr))
    })
}

fn lua_rawsettable_symbol() -> Option<LuaRawSetTableFn> {
    *LUA_RAWSETTABLE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_rawsettable\0",
            label: "lua_rawsettable",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaRawSetTableFn>(ptr))
    })
}

fn lua_gettable_symbol() -> Option<LuaGetTableFn> {
    *LUA_GETTABLE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_gettable\0",
            label: "lua_gettable",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaGetTableFn>(ptr))
    })
}

fn lua_rawgettable_symbol() -> Option<LuaRawGetTableFn> {
    *LUA_RAWGETTABLE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_rawgettable\0",
            label: "lua_rawgettable",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaRawGetTableFn>(ptr))
    })
}

fn lua_rawgetglobal_symbol() -> Option<LuaRawGetGlobalFn> {
    *LUA_RAWGETGLOBAL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_rawgetglobal\0",
            label: "lua_rawgetglobal",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaRawGetGlobalFn>(ptr))
    })
}

fn lua_rawsetglobal_symbol() -> Option<LuaRawSetGlobalFn> {
    *LUA_RAWSETGLOBAL.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_rawsetglobal\0",
            label: "lua_rawsetglobal",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaRawSetGlobalFn>(ptr))
    })
}

fn lua_setfallback_symbol() -> Option<LuaSetFallbackFn> {
    *LUA_SETFALLBACK.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_setfallback\0",
            label: "lua_setfallback",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaSetFallbackFn>(ptr))
    })
}

fn lua_newtag_symbol() -> Option<LuaNewTagFn> {
    *LUA_NEWTAG.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_newtag\0",
            label: "lua_newtag",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaNewTagFn>(ptr))
    })
}

fn lua_copytagmethods_symbol() -> Option<LuaCopyTagMethodsFn> {
    *LUA_COPYTAGMETHODS.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_copytagmethods\0",
            label: "lua_copytagmethods",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaCopyTagMethodsFn>(ptr))
    })
}

fn lua_settag_symbol() -> Option<LuaSetTagFn> {
    *LUA_SETTAG.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_settag\0",
            label: "lua_settag",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaSetTagFn>(ptr))
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
