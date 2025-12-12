use crate::{
    logging::{log_event, log_line, LuaEvent},
    lua_api::{call_real_lua_getglobal, call_real_lua_openlib, LuaLReg, LuaObject, LuaState},
};
use libc::{c_char, c_int};
use std::{
    ffi::{c_void, CStr},
    mem::MaybeUninit,
};

use super::{
    caller_origin_fields, handle_hex, origin_fields, record_non_push_event, ClosureOrigin,
};

const MAX_OPENLIB_ENTRIES: usize = 256;
const MAX_NAME_BYTES: usize = 256;

/// Traces library registration via `lua_openlib`, emitting an event for each native entry.
pub(crate) unsafe fn trace_lua_openlib(
    state: LuaState,
    libname: *const c_char,
    l: *const LuaLReg,
    nup: c_int,
) {
    record_non_push_event();
    let caller = caller_origin_fields();
    let forwarded = call_real_lua_openlib(state, libname, l, nup);
    if !forwarded {
        return;
    }
    let library = safe_cstr(libname);
    let entries = snapshot_entries(l);

    for entry in entries {
        let func_addr = entry.func_ptr as usize;
        let handle = resolve_registered_handle(libname, &entry);
        let origin = origin_fields(Some(&entry.origin));
        log_event(LuaEvent::RegisterNative {
            name: entry.name,
            handle: handle.unwrap_or_else(|| handle_hex(func_addr)),
            func: handle_hex(func_addr),
            upvalues: nup,
            origin,
            caller: caller.clone(),
            library: library.clone(),
        });
    }
}

#[derive(Clone)]
struct OpenlibEntry {
    name: String,
    name_ptr: *const c_char,
    func_ptr: *const c_void,
    origin: ClosureOrigin,
}

/// Copies the luaL_reg entries into owned structs for logging after the forward.
unsafe fn snapshot_entries(l: *const LuaLReg) -> Vec<OpenlibEntry> {
    let mut entries = Vec::new();
    if l.is_null() {
        return entries;
    }
    if !is_mapped_pointer(l as *const c_void) {
        log_line("lua_openlib luaL_reg pointer not in a mapped module; skipping entries");
        return entries;
    }

    for idx in 0..MAX_OPENLIB_ENTRIES {
        let entry_ptr = l.add(idx);
        if !is_mapped_pointer(entry_ptr as *const c_void) {
            log_line("lua_openlib entry pointer not in a mapped module; stopping iteration");
            break;
        }
        let entry = *entry_ptr;
        if entry.name.is_null() || entry.func.is_none() {
            break;
        }
        let func_ptr = entry.func.unwrap() as *const c_void;
        let Some(name) = safe_cstr(entry.name) else {
            log_line("lua_openlib entry name unreadable; stopping iteration");
            break;
        };
        entries.push(OpenlibEntry {
            name,
            name_ptr: entry.name,
            func_ptr,
            origin: ClosureOrigin::new(func_ptr),
        });
    }

    if entries.len() == MAX_OPENLIB_ENTRIES {
        log_line("lua_openlib entries hit limit; truncating");
    }

    entries
}

/// Attempts to resolve the Lua handle for a registered native when globals are targeted.
unsafe fn resolve_registered_handle(
    libname: *const c_char,
    entry: &OpenlibEntry,
) -> Option<String> {
    if !libname.is_null() || entry.name_ptr.is_null() {
        return None;
    }
    call_real_lua_getglobal(entry.name_ptr).map(|handle: LuaObject| handle_hex(handle as usize))
}

/// Returns true when the pointer appears to live inside a mapped module.
fn is_mapped_pointer(ptr: *const c_void) -> bool {
    unsafe {
        let mut info = MaybeUninit::<libc::Dl_info>::zeroed();
        libc::dladdr(ptr, info.as_mut_ptr()) != 0
    }
}

/// Safely converts a C string pointer into an owned string, guarding against invalid pointers.
unsafe fn safe_cstr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() || !is_mapped_pointer(ptr as *const c_void) {
        return None;
    }
    let len = unsafe { libc::strnlen(ptr, MAX_NAME_BYTES) };
    if len == 0 || len == MAX_NAME_BYTES {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len + 1) };
    CStr::from_bytes_with_nul(bytes)
        .ok()
        .and_then(|s| s.to_str().ok())
        .map(|s| s.to_string())
}
