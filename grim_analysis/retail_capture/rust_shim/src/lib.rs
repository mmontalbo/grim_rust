use std::{ffi::CStr, sync::OnceLock};

use libc::{c_char, c_int, c_void};

type LuaDofileFn = unsafe extern "C" fn(*mut c_char) -> c_int;

static LUA_DOFILE: OnceLock<Option<LuaDofileFn>> = OnceLock::new();

fn log_line(message: &str) {
    eprintln!("[grim-rust-shim] {message}");
}

unsafe fn resolve_lua_dofile() -> Option<LuaDofileFn> {
    LUA_DOFILE
        .get_or_init(|| {
            let symbol = b"lua_dofile\0";
            let ptr = libc::dlsym(libc::RTLD_NEXT, symbol.as_ptr() as *const c_char);
            if ptr.is_null() {
                log_line("failed to resolve lua_dofile via dlsym");
                None
            } else {
                log_line("resolved lua_dofile symbol");
                let func: LuaDofileFn = std::mem::transmute::<*mut c_void, LuaDofileFn>(ptr);
                Some(func)
            }
        })
        .clone()
}

fn filename_from_ptr(filename: *mut c_char) -> Option<String> {
    if filename.is_null() {
        return None;
    }

    unsafe { Some(CStr::from_ptr(filename).to_string_lossy().into_owned()) }
}

unsafe fn call_real_lua_dofile(filename: *mut c_char) -> c_int {
    match resolve_lua_dofile() {
        Some(real) => real(filename),
        None => {
            log_line("lua_dofile unavailable; returning error");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_dofile(filename: *mut c_char) -> c_int {
    match filename_from_ptr(filename) {
        Some(path) => {
            if path.ends_with("_system.lua") {
                log_line(&format!("observed lua_dofile call for {path}"));
            }
        }
        None => log_line("lua_dofile invoked with null filename"),
    }

    call_real_lua_dofile(filename)
}
