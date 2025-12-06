//! Instrumented shim for the retail Lua 3.1 VM that mirrors the engine’s C ABI while
//! emitting telemetry and semantic events for analysis.
//!
//! The exported functions forward to the real Lua VM symbols (resolved via `dlsym`)
//! and capture activity such as pushes, global binds, table writes, and cutscene
//! state changes. They are meant to be injected into the game process and are not
//! general-purpose bindings.
//!
//! # Examples
//! Using the shim just forwards to the retail VM while recording telemetry:
//! ```no_run
//! use grim_analysis::lua_dostring;
//! use std::ffi::CString;
//!
//! unsafe {
//!     let script = CString::new("return 1 + 1").unwrap();
//!     let _ = lua_dostring(script.as_ptr());
//! }
//! ```
//!
//! # Safety
//! Every exported function assumes the retail Lua 3.1 runtime is loaded in-process,
//! that any `*const c_char` arguments are valid, NUL-terminated strings, and that
//! Lua handles point to live VM objects. Calling these from another context is
//! undefined behavior.
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
/// Instrumented shim for Lua 3.1's `lua_open`, forwarding to the traced VM.
///
/// # Safety
/// Only call when the retail Lua runtime is present; the returned pointer must be
/// used with that VM's lifecycle.
pub unsafe extern "C" fn lua_open() -> *mut c_void {
    trace::trace_lua_open()
}

#[no_mangle]
/// Instrumented shim for `lua_newstate`, creating a new Lua state in the traced VM.
///
/// # Safety
/// Must be called inside the retail process; returns a raw VM pointer that must not
/// outlive the underlying runtime.
pub unsafe extern "C" fn lua_newstate() -> *mut c_void {
    trace::trace_lua_newstate()
}

#[no_mangle]
/// Pushes a C closure while recording telemetry about the function pointer and its
/// upvalues.
///
/// # Safety
/// `func` must be a valid C function address and `upvalues` must match the current
/// Lua stack expectations.
pub unsafe extern "C" fn lua_pushCclosure(func: LuaCFunction, upvalues: c_int) {
    trace_lua_push_closure("lua_pushCclosure", func, upvalues);
}

#[no_mangle]
/// Executes a Lua file and logs the path.
///
/// # Safety
/// `path` must point to a valid NUL-terminated string; the VM must be initialized.
pub unsafe extern "C" fn lua_dofile(path: *const libc::c_char) -> c_int {
    trace_lua_dofile(path)
}

#[no_mangle]
/// Executes a Lua chunk provided as a string.
///
/// # Safety
/// `chunk` must be a valid NUL-terminated string; invokes the retail VM.
pub unsafe extern "C" fn lua_dostring(chunk: *const libc::c_char) -> c_int {
    trace_lua_dostring(chunk)
}

#[no_mangle]
/// Sets a global name in the VM and records binding metadata.
///
/// # Safety
/// `name` must be a valid NUL-terminated string; requires a live Lua state.
pub unsafe extern "C" fn lua_setglobal(name: *const libc::c_char) {
    trace_lua_setglobal(name)
}

#[no_mangle]
/// Reads a global value from the VM while tracking access frequency.
///
/// # Safety
/// `name` must be a valid NUL-terminated string; returns a raw Lua handle.
pub unsafe extern "C" fn lua_getglobal(name: *const libc::c_char) -> lua_api::LuaObject {
    trace_lua_getglobal(name)
}

#[no_mangle]
/// Executes a Lua buffer with an optional chunk name.
///
/// # Safety
/// `buffer`/`name` must be valid strings of length `size`; VM must be initialized.
pub unsafe extern "C" fn lua_dobuffer(
    buffer: *const libc::c_char,
    size: libc::size_t,
    name: *const libc::c_char,
) -> c_int {
    trace_lua_dobuffer(buffer, size, name)
}

#[no_mangle]
/// Calls a Lua function by name, recording the invocation.
///
/// # Safety
/// `name` must be a valid NUL-terminated string; assumes the function exists in the VM.
pub unsafe extern "C" fn lua_call(name: *const libc::c_char) -> c_int {
    trace_lua_call(name)
}

#[no_mangle]
/// Calls a Lua function by handle, capturing call counts and origins.
///
/// # Safety
/// `func` must be a valid Lua function handle for the active VM.
pub unsafe extern "C" fn lua_callfunction(func: *mut libc::c_void) -> c_int {
    trace_lua_callfunction(func)
}

#[no_mangle]
/// Creates a new Lua thread in the traced VM.
///
/// # Safety
/// `state` must be a valid Lua state pointer from the retail runtime.
pub unsafe extern "C" fn lua_newthread(state: *mut c_void) -> *mut c_void {
    trace::trace_lua_newthread(state)
}

#[no_mangle]
/// Stores the value on top of the stack in the reference table.
///
/// # Safety
/// The Lua stack must contain a value and `lock` must follow Lua 3.1 expectations.
pub unsafe extern "C" fn lua_ref(lock: c_int) -> c_int {
    trace_lua_ref(lock)
}

#[no_mangle]
/// Fetches a value from the reference table.
///
/// # Safety
/// `reference` must be a valid reference ID for the current VM.
pub unsafe extern "C" fn lua_getref(reference: c_int) -> lua_api::LuaObject {
    trace_lua_getref(reference)
}

#[no_mangle]
/// Sets a tag method on an existing tag.
///
/// # Safety
/// `event` must be a valid NUL-terminated string and the stack must hold the handler.
pub unsafe extern "C" fn lua_settagmethod(tag: c_int, event: *const libc::c_char) {
    trace_lua_settagmethod(tag, event)
}

#[no_mangle]
/// Collects garbage in the VM and logs the event.
///
/// # Safety
/// Requires a live Lua state; may run finalizers in the retail VM context.
pub unsafe extern "C" fn lua_collectgarbage() {
    trace_lua_collectgarbage();
}

#[no_mangle]
/// Raises a Lua error with a message, truncating it for telemetry.
///
/// # Safety
/// `message` must be a valid NUL-terminated string; longjmp/error semantics follow the VM.
pub unsafe extern "C" fn lua_error(message: *const libc::c_char) {
    trace_lua_error(message);
}

#[no_mangle]
/// Pushes a number onto the Lua stack while recording the value.
///
/// # Safety
/// Requires a valid Lua state and stack space for the push.
pub unsafe extern "C" fn lua_pushnumber(value: libc::c_float) {
    trace_lua_pushnumber(value);
}

#[no_mangle]
/// Pushes `nil` onto the Lua stack and notes the push for telemetry.
///
/// # Safety
/// Requires a valid Lua state and stack space for the push.
pub unsafe extern "C" fn lua_pushnil() {
    trace_lua_pushnil();
}

#[no_mangle]
/// Pushes a string onto the Lua stack, capturing length and preview.
///
/// # Safety
/// `value` must be a valid NUL-terminated string; requires stack space.
pub unsafe extern "C" fn lua_pushstring(value: *const libc::c_char) {
    trace_lua_pushstring(value);
}

#[no_mangle]
/// Pushes a sized string onto the Lua stack, capturing length and preview.
///
/// # Safety
/// `value` must point to `len` bytes of valid UTF-8/bytes; requires stack space.
pub unsafe extern "C" fn lua_pushlstring(value: *const libc::c_char, len: libc::size_t) {
    trace_lua_pushlstring(value, len);
}

#[no_mangle]
/// Pushes a userdata tagged value, recording the tag and caller.
///
/// # Safety
/// `id` and `tag` must match the VM's expectations; requires stack space.
pub unsafe extern "C" fn lua_pushusertag(id: libc::c_int, tag: libc::c_int) {
    trace_lua_pushusertag(id, tag);
}

#[no_mangle]
/// Pushes an existing Lua object handle onto the stack, emitting value metadata.
///
/// # Safety
/// `object` must be a valid Lua handle for the current VM.
pub unsafe extern "C" fn lua_pushobject(object: lua_api::LuaObject) {
    trace_lua_pushobject(object);
}

#[no_mangle]
/// Pushes a value already on the stack by index, capturing its metadata.
///
/// # Safety
/// `index` must refer to a valid stack slot for the current VM.
pub unsafe extern "C" fn lua_pushvalue(index: libc::c_int) {
    trace_lua_pushvalue(index);
}

#[no_mangle]
/// Creates a new table and logs the resulting handle.
///
/// # Safety
/// Requires a valid Lua state; returns a handle tied to that VM.
pub unsafe extern "C" fn lua_createtable() -> lua_api::LuaObject {
    trace_lua_createtable()
}

#[no_mangle]
/// Sets a table entry using the top three stack values (table, key, value).
///
/// # Safety
/// Stack must contain the correct operands; manipulates the current VM stack.
pub unsafe extern "C" fn lua_settable() {
    trace_lua_settable();
}

#[no_mangle]
/// Sets a table entry without metamethods, using the top three stack values.
///
/// # Safety
/// Stack must contain the correct operands; manipulates the current VM stack.
pub unsafe extern "C" fn lua_rawsettable() {
    trace_lua_rawsettable();
}

#[no_mangle]
/// Retrieves a table entry using the key on top of the stack.
///
/// # Safety
/// Stack must contain a key and target table; returns a raw Lua handle.
pub unsafe extern "C" fn lua_gettable() -> lua_api::LuaObject {
    trace_lua_gettable()
}

#[no_mangle]
/// Retrieves a table entry without metamethods, using the key on top of the stack.
///
/// # Safety
/// Stack must contain a key and target table; returns a raw Lua handle.
pub unsafe extern "C" fn lua_rawgettable() -> lua_api::LuaObject {
    trace_lua_rawgettable()
}

#[no_mangle]
/// Reads a global without invoking metamethods, logging handle metadata.
///
/// # Safety
/// `name` must be a valid NUL-terminated string; returns a raw Lua handle.
pub unsafe extern "C" fn lua_rawgetglobal(name: *const libc::c_char) -> lua_api::LuaObject {
    trace_lua_rawgetglobal(name)
}

#[no_mangle]
/// Writes a global without invoking metamethods, recording the resulting value.
///
/// # Safety
/// `name` must be a valid NUL-terminated string; expects a value on the stack.
pub unsafe extern "C" fn lua_rawsetglobal(name: *const libc::c_char) {
    trace_lua_rawsetglobal(name);
}

#[no_mangle]
/// Releases a reference from the reference table.
///
/// # Safety
/// `reference` must have been returned by `lua_ref` for the active VM.
pub unsafe extern "C" fn lua_unref(reference: libc::c_int) {
    trace_lua_unref(reference);
}

#[no_mangle]
/// Sets a fallback handler (metamethod) and logs its origin.
///
/// # Safety
/// `event` must be a valid NUL-terminated string; `func` must be a callable C function.
pub unsafe extern "C" fn lua_setfallback(
    event: *const c_char,
    func: LuaCFunction,
) -> lua_api::LuaObject {
    trace_lua_setfallback(event, func)
}

#[no_mangle]
/// Allocates a new tag in the VM and logs creation.
///
/// # Safety
/// Requires a valid Lua state.
pub unsafe extern "C" fn lua_newtag() -> libc::c_int {
    trace_lua_newtag()
}

#[no_mangle]
/// Copies tag methods from one tag to another, recording the result.
///
/// # Safety
/// `tagto`/`tagfrom` must be valid tags in the current VM.
pub unsafe extern "C" fn lua_copytagmethods(
    tagto: libc::c_int,
    tagfrom: libc::c_int,
) -> libc::c_int {
    trace_lua_copytagmethods(tagto, tagfrom)
}

#[no_mangle]
/// Sets the active tag for the value on top of the stack.
///
/// # Safety
/// `tag` must be valid in the current VM; stack must contain a value.
pub unsafe extern "C" fn lua_settag(tag: libc::c_int) {
    trace_lua_settag(tag);
}

// Retail liblua only exports the capital-C variant; keep a note to avoid re-adding lua_pushcclosure.
