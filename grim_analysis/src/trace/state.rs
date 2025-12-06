use crate::{
    logging::log_line,
    lua_api::{call_real_lua_newstate, call_real_lua_newthread, call_real_lua_open, LuaState},
};
use std::ptr;

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
