use crate::{
    logging::{log_boot_sequence_start, log_line},
    lua_api::{call_real_lua_newstate, call_real_lua_newthread, call_real_lua_open, LuaState},
};
use std::ptr;

/// Opens a new Lua state via the retail VM, returning null if the symbol is missing.
pub(crate) unsafe fn trace_lua_open() -> LuaState {
    log_boot_sequence_start();
    match call_real_lua_open() {
        Some(state) => state,
        None => {
            log_line("lua_open symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

/// Creates a new Lua state via the retail VM, returning null if the symbol is missing.
pub(crate) unsafe fn trace_lua_newstate() -> LuaState {
    log_boot_sequence_start();
    match call_real_lua_newstate() {
        Some(state) => state,
        None => {
            log_line("lua_newstate symbol missing; returning null");
            ptr::null_mut()
        }
    }
}

/// Creates a new Lua thread via the retail VM, returning null if the symbol is missing.
pub(crate) unsafe fn trace_lua_newthread(state: LuaState) -> LuaState {
    match call_real_lua_newthread(state) {
        Some(thread) => thread,
        None => {
            log_line("lua_newthread symbol missing; returning null");
            ptr::null_mut()
        }
    }
}
