use crate::logging::log_line;
use libc::{c_char, c_int, c_void};
use std::sync::OnceLock;

/// Opaque handle matching Lua's lua_State.
#[repr(C)]
pub struct lua_State {
    _private: [u8; 0],
}

pub(crate) type LuaCFunction = unsafe extern "C" fn(*mut lua_State) -> c_int;
type LuaPushCClosureFn = unsafe extern "C" fn(LuaCFunction, c_int);

static LUA_PUSH_CCLOSURE: OnceLock<Option<LuaPushCClosureFn>> = OnceLock::new();

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

fn lua_push_c_closure_symbol() -> Option<LuaPushCClosureFn> {
    *LUA_PUSH_CCLOSURE.get_or_init(|| unsafe {
        resolve_symbol_with_variants(&[SymbolVariant {
            symbol: b"lua_pushCclosure\0",
            label: "lua_pushCclosure",
        }])
        .map(|ptr| std::mem::transmute::<*mut c_void, LuaPushCClosureFn>(ptr))
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
