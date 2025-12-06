#![allow(clippy::missing_safety_doc, clippy::missing_const_for_thread_local)]

mod logging;
mod lua_api;
mod symbol_map;
mod telemetry;
mod trace;

use libc::{c_char, c_int, c_void};
use lua_api::LuaCFunction;
use trace::{
    trace_lua_call, trace_lua_callfunction, trace_lua_collectgarbage, trace_lua_copytagmethods,
    trace_lua_createtable, trace_lua_dobuffer, trace_lua_dofile, trace_lua_dostring,
    trace_lua_error, trace_lua_getglobal, trace_lua_getref, trace_lua_gettable, trace_lua_newtag,
    trace_lua_push_closure, trace_lua_pushlstring, trace_lua_pushnil, trace_lua_pushnumber,
    trace_lua_pushobject, trace_lua_pushstring, trace_lua_pushusertag, trace_lua_pushvalue,
    trace_lua_rawgetglobal, trace_lua_rawgettable, trace_lua_rawsetglobal, trace_lua_rawsettable,
    trace_lua_ref, trace_lua_setfallback, trace_lua_setglobal, trace_lua_settable,
    trace_lua_settag, trace_lua_settagmethod, trace_lua_unref,
};

#[no_mangle]
pub unsafe extern "C" fn lua_open() -> *mut c_void {
    trace::trace_lua_open()
}

#[no_mangle]
pub unsafe extern "C" fn lua_newstate() -> *mut c_void {
    trace::trace_lua_newstate()
}

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
pub unsafe extern "C" fn lua_newthread(state: *mut c_void) -> *mut c_void {
    trace::trace_lua_newthread(state)
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
pub unsafe extern "C" fn lua_pushnumber(value: libc::c_float) {
    trace_lua_pushnumber(value);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushnil() {
    trace_lua_pushnil();
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushstring(value: *const libc::c_char) {
    trace_lua_pushstring(value);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushlstring(value: *const libc::c_char, len: libc::size_t) {
    trace_lua_pushlstring(value, len);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushusertag(id: libc::c_int, tag: libc::c_int) {
    trace_lua_pushusertag(id, tag);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushobject(object: lua_api::LuaObject) {
    trace_lua_pushobject(object);
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushvalue(index: libc::c_int) {
    trace_lua_pushvalue(index);
}

#[no_mangle]
pub unsafe extern "C" fn lua_createtable() -> lua_api::LuaObject {
    trace_lua_createtable()
}

#[no_mangle]
pub unsafe extern "C" fn lua_settable() {
    trace_lua_settable();
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawsettable() {
    trace_lua_rawsettable();
}

#[no_mangle]
pub unsafe extern "C" fn lua_gettable() -> lua_api::LuaObject {
    trace_lua_gettable()
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawgettable() -> lua_api::LuaObject {
    trace_lua_rawgettable()
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawgetglobal(name: *const libc::c_char) -> lua_api::LuaObject {
    trace_lua_rawgetglobal(name)
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawsetglobal(name: *const libc::c_char) {
    trace_lua_rawsetglobal(name);
}

#[no_mangle]
pub unsafe extern "C" fn lua_unref(reference: libc::c_int) {
    trace_lua_unref(reference);
}

#[no_mangle]
pub unsafe extern "C" fn lua_setfallback(
    event: *const c_char,
    func: LuaCFunction,
) -> lua_api::LuaObject {
    trace_lua_setfallback(event, func)
}

#[no_mangle]
pub unsafe extern "C" fn lua_newtag() -> libc::c_int {
    trace_lua_newtag()
}

#[no_mangle]
pub unsafe extern "C" fn lua_copytagmethods(
    tagto: libc::c_int,
    tagfrom: libc::c_int,
) -> libc::c_int {
    trace_lua_copytagmethods(tagto, tagfrom)
}

#[no_mangle]
pub unsafe extern "C" fn lua_settag(tag: libc::c_int) {
    trace_lua_settag(tag);
}

// Retail liblua only exports the capital-C variant; keep a note to avoid re-adding lua_pushcclosure.
