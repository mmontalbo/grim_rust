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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LuaLReg {
    pub name: *const c_char,
    pub func: Option<LuaCFunction>,
}

type LuaOpenLibFn = unsafe extern "C" fn(LuaState, *const c_char, *const LuaLReg, c_int);
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
static LUA_OPENLIB: OnceLock<Option<LuaOpenLibFn>> = OnceLock::new();
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

/// Calls the resolved `lua_pushCclosure` and returns whether the symbol existed.
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

/// Creates a Lua state via the retail `lua_open`, if available.
pub(crate) fn call_real_lua_open() -> Option<LuaState> {
    unsafe { lua_open_symbol().map(|symbol| symbol()) }
}

/// Creates a Lua state via the retail `lua_newstate`, if available.
pub(crate) fn call_real_lua_newstate() -> Option<LuaState> {
    unsafe { lua_newstate_symbol().map(|symbol| symbol()) }
}

/// Spawns a new Lua thread within the retail VM when the symbol is present.
pub(crate) fn call_real_lua_newthread(state: LuaState) -> Option<LuaState> {
    unsafe { lua_newthread_symbol().map(|symbol| symbol(state)) }
}

/// Registers a library of native functions with the retail VM, returning whether the symbol existed.
pub(crate) fn call_real_lua_openlib(
    state: LuaState,
    libname: *const c_char,
    l: *const LuaLReg,
    nup: c_int,
) -> bool {
    unsafe {
        match lua_openlib_symbol() {
            Some(symbol) => {
                symbol(state, libname, l, nup);
                true
            }
            None => {
                log_line("lua_openlib symbol missing; skipping trace");
                false
            }
        }
    }
}

/// Forwards `lua_dofile` into the retail VM, returning None if the symbol is missing.
pub(crate) fn call_real_lua_dofile(filename: *const c_char) -> Option<c_int> {
    unsafe { lua_dofile_symbol().map(|symbol| symbol(filename)) }
}

/// Forwards `lua_dostring` into the retail VM, returning None if the symbol is missing.
pub(crate) fn call_real_lua_dostring(chunk: *const c_char) -> Option<c_int> {
    unsafe { lua_dostring_symbol().map(|symbol| symbol(chunk)) }
}

/// Runs a Lua buffer in the retail VM if the symbol resolves successfully.
pub(crate) fn call_real_lua_dobuffer(
    buffer: *const c_char,
    size: size_t,
    name: *const c_char,
) -> Option<c_int> {
    unsafe { lua_dobuffer_symbol().map(|symbol| symbol(buffer, size, name)) }
}

/// Calls a Lua function by name in the retail VM, if available.
pub(crate) fn call_real_lua_call(func_name: *const c_char) -> Option<c_int> {
    unsafe { lua_call_symbol().map(|symbol| symbol(func_name)) }
}

/// Calls a Lua function by handle in the retail VM, if available.
pub(crate) fn call_real_lua_callfunction(func: LuaObject) -> Option<c_int> {
    unsafe { lua_callfunction_symbol().map(|symbol| symbol(func)) }
}

/// Retrieves the name and kind for a Lua function handle when the symbol exists.
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

/// Sets a global in the retail VM and reports whether the symbol was available.
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

/// Reads a global from the retail VM, returning None if the symbol is missing.
pub(crate) fn call_real_lua_getglobal(name: *const c_char) -> Option<LuaObject> {
    unsafe { lua_getglobal_symbol().map(|symbol| symbol(name)) }
}

/// Resolves the C function pointer behind a Lua handle.
pub(crate) fn call_real_lua_getcfunction(handle: LuaObject) -> Option<LuaCFunction> {
    unsafe { lua_getcfunction_symbol().and_then(|symbol| symbol(handle)) }
}

/// Fetches the Lua stack parameter at `index`, returning None for null/invalid entries.
pub(crate) fn call_real_lua_getparam(index: c_int) -> Option<LuaObject> {
    unsafe {
        lua_lua2c_symbol()
            .map(|symbol| symbol(index))
            .filter(|obj| *obj != 0)
    }
}

/// Returns whether the handle represents a number according to the retail VM.
pub(crate) fn call_real_lua_isnumber(object: LuaObject) -> bool {
    unsafe {
        lua_isnumber_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

/// Returns whether the handle represents a string according to the retail VM.
pub(crate) fn call_real_lua_isstring(object: LuaObject) -> bool {
    unsafe {
        lua_isstring_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

/// Returns whether the handle represents a table according to the retail VM.
pub(crate) fn call_real_lua_istable(object: LuaObject) -> bool {
    unsafe {
        lua_istable_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

/// Returns whether the handle represents a function according to the retail VM.
pub(crate) fn call_real_lua_isfunction(object: LuaObject) -> bool {
    unsafe {
        lua_isfunction_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

/// Returns whether the handle represents a C function according to the retail VM.
pub(crate) fn call_real_lua_iscfunction(object: LuaObject) -> bool {
    unsafe {
        lua_iscfunction_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

/// Returns whether the handle represents userdata according to the retail VM.
pub(crate) fn call_real_lua_isuserdata(object: LuaObject) -> bool {
    unsafe {
        lua_isuserdata_symbol()
            .map(|symbol| symbol(object) != 0)
            .unwrap_or(false)
    }
}

/// Fetches the numeric value behind a Lua handle if the symbol is present.
pub(crate) fn call_real_lua_getnumber(object: LuaObject) -> Option<c_double> {
    unsafe { lua_getnumber_symbol().map(|symbol| symbol(object)) }
}

/// Converts a Lua string handle into a Rust `String`, if possible.
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

/// Returns the tag associated with a Lua handle via the retail API.
pub(crate) fn call_real_lua_tag(object: LuaObject) -> Option<c_int> {
    unsafe { lua_tag_symbol().map(|symbol| symbol(object)) }
}

/// Pushes a number via the retail API, returning whether the symbol existed.
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

/// Pushes a NUL-terminated string via the retail API, returning success.
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

/// Pushes a sized string via the retail API, returning success.
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

/// Pushes `nil` via the retail API, returning whether the symbol existed.
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

/// Duplicates a stack value by index via the retail API, returning success.
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

/// Pushes a tagged userdata via the retail API, returning success.
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

/// Pushes an existing Lua handle via the retail API, returning success.
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

/// Creates a table in the retail VM if the symbol resolves.
pub(crate) fn call_real_lua_createtable() -> Option<LuaObject> {
    unsafe { lua_createtable_symbol().map(|symbol| symbol()) }
}

/// Sets a table entry via the retail API, returning whether the symbol existed.
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

/// Sets a table entry without metamethods via the retail API, returning success.
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

/// Reads a table entry via the retail API if the symbol is present.
pub(crate) fn call_real_lua_gettable() -> Option<LuaObject> {
    unsafe { lua_gettable_symbol().map(|symbol| symbol()) }
}

/// Reads a table entry without metamethods via the retail API if available.
pub(crate) fn call_real_lua_rawgettable() -> Option<LuaObject> {
    unsafe { lua_rawgettable_symbol().map(|symbol| symbol()) }
}

/// Reads a global without metamethods via the retail API if available.
pub(crate) fn call_real_lua_rawgetglobal(name: *const c_char) -> Option<LuaObject> {
    unsafe { lua_rawgetglobal_symbol().map(|symbol| symbol(name)) }
}

/// Writes a global without metamethods via the retail API, returning success.
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

/// Releases a Lua reference if the retail symbol exists.
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

/// Sets a fallback handler in the retail VM if the symbol resolves.
pub(crate) fn call_real_lua_setfallback(
    event: *const c_char,
    func: LuaCFunction,
) -> Option<LuaObject> {
    unsafe { lua_setfallback_symbol().map(|symbol| symbol(event, func)) }
}

/// Allocates a new tag in the retail VM if available.
pub(crate) fn call_real_lua_newtag() -> Option<c_int> {
    unsafe { lua_newtag_symbol().map(|symbol| symbol()) }
}

/// Copies tag methods between tags in the retail VM, when supported.
pub(crate) fn call_real_lua_copytagmethods(tagto: c_int, tagfrom: c_int) -> Option<c_int> {
    unsafe { lua_copytagmethods_symbol().map(|symbol| symbol(tagto, tagfrom)) }
}

/// Sets the active tag for the top-of-stack value, returning success.
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

/// Stores the top-of-stack value in the reference table if the symbol resolves.
pub(crate) fn call_real_lua_ref(lock: c_int) -> Option<c_int> {
    unsafe { lua_ref_symbol().map(|symbol| symbol(lock)) }
}

/// Retrieves a value from the reference table if the symbol resolves.
pub(crate) fn call_real_lua_getref(reference: c_int) -> Option<LuaObject> {
    unsafe { lua_getref_symbol().map(|symbol| symbol(reference)) }
}

/// Installs a tag method for an event if the symbol resolves.
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

/// Runs Lua's garbage collector if the symbol resolves.
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

/// Raises a Lua error through the retail VM, returning success.
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

// Helper to define symbol resolvers without repeating the OnceLock/dlsym boilerplate.
macro_rules! define_symbol_resolver {
    ($fn_name:ident, $once:ident, $type:ty, [$($symbol:expr => $label:expr),+ $(,)?]) => {
        fn $fn_name() -> Option<$type> {
            *$once.get_or_init(|| unsafe {
                resolve_symbol_with_variants(&[
                    $(SymbolVariant {
                        symbol: $symbol,
                        label: $label,
                    },)+
                ])
                .map(|ptr| std::mem::transmute::<*mut c_void, $type>(ptr))
            })
        }
    };
}

define_symbol_resolver!(
    lua_push_c_closure_symbol,
    LUA_PUSH_CCLOSURE,
    LuaPushCClosureFn,
    [b"lua_pushCclosure\0" => "lua_pushCclosure"]
);

define_symbol_resolver!(
    lua_dostring_symbol,
    LUA_DOSTRING,
    LuaDoStringFn,
    [b"lua_dostring\0" => "lua_dostring"]
);

define_symbol_resolver!(
    lua_dofile_symbol,
    LUA_DOFILE,
    LuaDoFileFn,
    [b"lua_dofile\0" => "lua_dofile"]
);

define_symbol_resolver!(
    lua_dobuffer_symbol,
    LUA_DOBUFFER,
    LuaDoBufferFn,
    [b"lua_dobuffer\0" => "lua_dobuffer"]
);

define_symbol_resolver!(lua_call_symbol, LUA_CALL, LuaCallFn, [b"lua_call\0" => "lua_call"]);

define_symbol_resolver!(
    lua_callfunction_symbol,
    LUA_CALLFUNCTION,
    LuaCallFunctionFn,
    [b"lua_callfunction\0" => "lua_callfunction"]
);

define_symbol_resolver!(lua_open_symbol, LUA_OPEN, LuaOpenFn, [b"lua_open\0" => "lua_open"]);

define_symbol_resolver!(
    lua_newstate_symbol,
    LUA_NEWSTATE,
    LuaNewStateFn,
    [b"lua_newstate\0" => "lua_newstate"]
);

define_symbol_resolver!(
    lua_newthread_symbol,
    LUA_NEWTHREAD,
    LuaNewThreadFn,
    [b"lua_newthread\0" => "lua_newthread"]
);

define_symbol_resolver!(
    lua_openlib_symbol,
    LUA_OPENLIB,
    LuaOpenLibFn,
    [
        b"lua_openlib\0" => "lua_openlib",
        b"luaL_openlib\0" => "luaL_openlib",
        b"luaI_openlib\0" => "luaI_openlib",
    ]
);

define_symbol_resolver!(
    lua_getobjname_symbol,
    LUA_GETOBJNAME,
    LuaGetObjNameFn,
    [b"lua_getobjname\0" => "lua_getobjname"]
);

define_symbol_resolver!(
    lua_setglobal_symbol,
    LUA_SETGLOBAL,
    LuaSetGlobalFn,
    [b"lua_setglobal\0" => "lua_setglobal"]
);

define_symbol_resolver!(
    lua_getglobal_symbol,
    LUA_GETGLOBAL,
    LuaGetGlobalFn,
    [b"lua_getglobal\0" => "lua_getglobal"]
);

define_symbol_resolver!(
    lua_getcfunction_symbol,
    LUA_GETCFUNCTION,
    LuaGetCFunctionFn,
    [b"lua_getcfunction\0" => "lua_getcfunction"]
);

define_symbol_resolver!(
    lua_lua2c_symbol,
    LUA_LUA2C,
    LuaLua2CFn,
    [b"lua_lua2C\0" => "lua_lua2C"]
);

define_symbol_resolver!(
    lua_isnumber_symbol,
    LUA_ISNUMBER,
    LuaIsNumberFn,
    [b"lua_isnumber\0" => "lua_isnumber"]
);

define_symbol_resolver!(
    lua_isstring_symbol,
    LUA_ISSTRING,
    LuaIsStringFn,
    [b"lua_isstring\0" => "lua_isstring"]
);

define_symbol_resolver!(
    lua_istable_symbol,
    LUA_ISTABLE,
    LuaIsTableFn,
    [b"lua_istable\0" => "lua_istable"]
);

define_symbol_resolver!(
    lua_isfunction_symbol,
    LUA_ISFUNCTION,
    LuaIsFunctionFn,
    [b"lua_isfunction\0" => "lua_isfunction"]
);

define_symbol_resolver!(
    lua_iscfunction_symbol,
    LUA_ISCFUNCTION,
    LuaIsCFunctionFn,
    [b"lua_iscfunction\0" => "lua_iscfunction"]
);

define_symbol_resolver!(
    lua_isuserdata_symbol,
    LUA_ISUSERDATA,
    LuaIsUserdataFn,
    [b"lua_isuserdata\0" => "lua_isuserdata"]
);

define_symbol_resolver!(
    lua_getnumber_symbol,
    LUA_GETNUMBER,
    LuaGetNumberFn,
    [b"lua_getnumber\0" => "lua_getnumber"]
);

define_symbol_resolver!(
    lua_getstring_symbol,
    LUA_GETSTRING,
    LuaGetStringFn,
    [b"lua_getstring\0" => "lua_getstring"]
);

define_symbol_resolver!(lua_tag_symbol, LUA_TAG, LuaTagFn, [b"lua_tag\0" => "lua_tag"]);

define_symbol_resolver!(
    lua_pushnumber_symbol,
    LUA_PUSHNUMBER,
    LuaPushNumberFn,
    [b"lua_pushnumber\0" => "lua_pushnumber"]
);

define_symbol_resolver!(
    lua_pushstring_symbol,
    LUA_PUSHSTRING,
    LuaPushStringFn,
    [b"lua_pushstring\0" => "lua_pushstring"]
);

define_symbol_resolver!(
    lua_pushlstring_symbol,
    LUA_PUSHLSTRING,
    LuaPushLStringFn,
    [b"lua_pushlstring\0" => "lua_pushlstring"]
);

define_symbol_resolver!(
    lua_pushusertag_symbol,
    LUA_PUSHUSERTAG,
    LuaPushUsertagFn,
    [b"lua_pushusertag\0" => "lua_pushusertag"]
);

define_symbol_resolver!(
    lua_pushobject_symbol,
    LUA_PUSHOBJECT,
    LuaPushObjectFn,
    [b"lua_pushobject\0" => "lua_pushobject"]
);

define_symbol_resolver!(
    lua_pushnil_symbol,
    LUA_PUSHNIL,
    LuaPushNilFn,
    [b"lua_pushnil\0" => "lua_pushnil"]
);

define_symbol_resolver!(
    lua_pushvalue_symbol,
    LUA_PUSHVALUE,
    LuaPushValueFn,
    [b"lua_pushvalue\0" => "lua_pushvalue"]
);

define_symbol_resolver!(lua_ref_symbol, LUA_REF, LuaRefFn, [b"lua_ref\0" => "lua_ref"]);

define_symbol_resolver!(
    lua_getref_symbol,
    LUA_GETREF,
    LuaGetRefFn,
    [b"lua_getref\0" => "lua_getref"]
);

define_symbol_resolver!(
    lua_unref_symbol,
    LUA_UNREF,
    LuaUnrefFn,
    [b"lua_unref\0" => "lua_unref"]
);

define_symbol_resolver!(
    lua_settagmethod_symbol,
    LUA_SETTAGMETHOD,
    LuaSetTagMethodFn,
    [b"lua_settagmethod\0" => "lua_settagmethod"]
);

define_symbol_resolver!(
    lua_collectgarbage_symbol,
    LUA_COLLECTGARBAGE,
    LuaCollectGarbageFn,
    [b"lua_collectgarbage\0" => "lua_collectgarbage"]
);

define_symbol_resolver!(
    lua_error_symbol,
    LUA_ERROR,
    LuaErrorFn,
    [b"lua_error\0" => "lua_error"]
);

define_symbol_resolver!(
    lua_createtable_symbol,
    LUA_CREATETABLE,
    LuaCreateTableFn,
    [b"lua_createtable\0" => "lua_createtable"]
);

define_symbol_resolver!(
    lua_settable_symbol,
    LUA_SETTABLE,
    LuaSetTableFn,
    [b"lua_settable\0" => "lua_settable"]
);

define_symbol_resolver!(
    lua_rawsettable_symbol,
    LUA_RAWSETTABLE,
    LuaRawSetTableFn,
    [b"lua_rawsettable\0" => "lua_rawsettable"]
);

define_symbol_resolver!(
    lua_gettable_symbol,
    LUA_GETTABLE,
    LuaGetTableFn,
    [b"lua_gettable\0" => "lua_gettable"]
);

define_symbol_resolver!(
    lua_rawgettable_symbol,
    LUA_RAWGETTABLE,
    LuaRawGetTableFn,
    [b"lua_rawgettable\0" => "lua_rawgettable"]
);

define_symbol_resolver!(
    lua_rawgetglobal_symbol,
    LUA_RAWGETGLOBAL,
    LuaRawGetGlobalFn,
    [b"lua_rawgetglobal\0" => "lua_rawgetglobal"]
);

define_symbol_resolver!(
    lua_rawsetglobal_symbol,
    LUA_RAWSETGLOBAL,
    LuaRawSetGlobalFn,
    [b"lua_rawsetglobal\0" => "lua_rawsetglobal"]
);

define_symbol_resolver!(
    lua_setfallback_symbol,
    LUA_SETFALLBACK,
    LuaSetFallbackFn,
    [b"lua_setfallback\0" => "lua_setfallback"]
);

define_symbol_resolver!(
    lua_newtag_symbol,
    LUA_NEWTAG,
    LuaNewTagFn,
    [b"lua_newtag\0" => "lua_newtag"]
);

define_symbol_resolver!(
    lua_copytagmethods_symbol,
    LUA_COPYTAGMETHODS,
    LuaCopyTagMethodsFn,
    [b"lua_copytagmethods\0" => "lua_copytagmethods"]
);

define_symbol_resolver!(
    lua_settag_symbol,
    LUA_SETTAG,
    LuaSetTagFn,
    [b"lua_settag\0" => "lua_settag"]
);

#[derive(Clone, Copy)]
struct SymbolVariant {
    symbol: &'static [u8],
    label: &'static str,
}

/// Attempts each symbol spelling until one resolves via `dlsym`.
unsafe fn resolve_symbol_with_variants(variants: &[SymbolVariant]) -> Option<*mut c_void> {
    for variant in variants {
        if let Some(ptr) = resolve_symbol_ptr(variant.symbol, variant.label) {
            return Some(ptr);
        }
    }
    None
}

/// Resolves a single symbol name using `RTLD_NEXT` and falls back to `RTLD_DEFAULT`.
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

/// Converts an optional C string pointer into an owned `String`.
unsafe fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}
