mod logging;
mod lua_api;
mod symbol_map;
mod telemetry;
mod trace;

use libc::c_int;
use lua_api::LuaCFunction;
use trace::{
    trace_lua_call, trace_lua_callfunction, trace_lua_collectgarbage, trace_lua_dobuffer,
    trace_lua_dofile, trace_lua_dostring, trace_lua_error, trace_lua_getglobal, trace_lua_getref,
    trace_lua_push_closure, trace_lua_ref, trace_lua_setglobal, trace_lua_settagmethod,
};

#[no_mangle]
pub unsafe extern "C" fn lua_pushCclosure(func: LuaCFunction, upvalues: c_int) {
    trace_lua_push_closure("lua_pushCclosure", func, upvalues);
}

#[no_mangle]
pub unsafe extern "C" fn lua_dofile(path: *const libc::c_char) -> c_int {
    trace_lua_dofile(path)
}

#[no_mangle]
pub unsafe extern "C" fn lua_dostring(chunk: *const libc::c_char) -> c_int {
    trace_lua_dostring(chunk)
}

#[no_mangle]
pub unsafe extern "C" fn lua_setglobal(name: *const libc::c_char) {
    trace_lua_setglobal(name)
}

#[no_mangle]
pub unsafe extern "C" fn lua_getglobal(name: *const libc::c_char) -> lua_api::LuaObject {
    trace_lua_getglobal(name)
}

#[no_mangle]
pub unsafe extern "C" fn lua_dobuffer(
    buffer: *const libc::c_char,
    size: libc::size_t,
    name: *const libc::c_char,
) -> c_int {
    trace_lua_dobuffer(buffer, size, name)
}

#[no_mangle]
pub unsafe extern "C" fn lua_call(name: *const libc::c_char) -> c_int {
    trace_lua_call(name)
}

#[no_mangle]
pub unsafe extern "C" fn lua_callfunction(func: *mut libc::c_void) -> c_int {
    trace_lua_callfunction(func)
}

#[no_mangle]
pub unsafe extern "C" fn lua_ref(lock: c_int) -> c_int {
    trace_lua_ref(lock)
}

#[no_mangle]
pub unsafe extern "C" fn lua_getref(reference: c_int) -> lua_api::LuaObject {
    trace_lua_getref(reference)
}

#[no_mangle]
pub unsafe extern "C" fn lua_settagmethod(tag: c_int, event: *const libc::c_char) {
    trace_lua_settagmethod(tag, event)
}

#[no_mangle]
pub unsafe extern "C" fn lua_collectgarbage() {
    trace_lua_collectgarbage();
}

#[no_mangle]
pub unsafe extern "C" fn lua_error(message: *const libc::c_char) {
    trace_lua_error(message);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushnumber(value: libc::c_double) {
    telemetry::record_pushed_number(value);
    lua_api::call_real_lua_pushnumber(value);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushnil() {
    telemetry::record_pushed_nil();
    lua_api::call_real_lua_pushnil();
}

// Retail liblua only exports the capital-C variant; keep a note to avoid re-adding lua_pushcclosure.
