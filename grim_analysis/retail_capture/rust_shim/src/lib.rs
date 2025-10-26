use std::{
    ffi::{CStr, CString},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

use libc::{c_char, c_int, c_void};

type LuaDofileFn = unsafe extern "C" fn(*mut c_char) -> c_int;

static LUA_DOFILE: OnceLock<Option<LuaDofileFn>> = OnceLock::new();
static TELEMETRY_INJECTED: AtomicBool = AtomicBool::new(false);

const TELEMETRY_SCRIPT: &str = "mods/telemetry_simple.lua";
const TELEMETRY_BOOTSTRAP_ERROR_LOG: &str = "mods/telemetry_bootstrap_error.log";

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

fn is_system_script(path: &str) -> bool {
    let trimmed = path.trim();
    let filename = trimmed
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(trimmed);
    filename.eq_ignore_ascii_case("_system.lua")
}

unsafe fn inject_telemetry_script() {
    if TELEMETRY_INJECTED.swap(true, Ordering::SeqCst) {
        return;
    }

    log_line(&format!(
        "injecting telemetry script via lua_dofile({TELEMETRY_SCRIPT})"
    ));

    let script = match CString::new(TELEMETRY_SCRIPT) {
        Ok(value) => value,
        Err(err) => {
            log_line(&format!(
                "failed to build CString for telemetry path {TELEMETRY_SCRIPT}: {err}"
            ));
            return;
        }
    };

    let result = call_real_lua_dofile(script.as_ptr() as *mut c_char);
    if result == 0 {
        log_line("telemetry.lua executed successfully");
    } else {
        let note = TELEMETRY_BOOTSTRAP_ERROR_LOG;
        log_line(&format!(
            "telemetry.lua returned error code {result}; check {note}"
        ));
        if let Ok(contents) = std::fs::read_to_string(format!("dev-install/{note}")) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                log_line(&format!("telemetry bootstrap log: {trimmed}"));
            }
        }
    }
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
    let path = filename_from_ptr(filename);
    if path.is_none() {
        log_line("lua_dofile invoked with null filename");
    }

    let result = call_real_lua_dofile(filename);

    if let Some(ref text) = path {
        if is_system_script(text) {
            log_line(&format!("observed lua_dofile call for {text} (result={result})"));
            if result == 0 {
                inject_telemetry_script();
            } else {
                log_line(&format!(
                    "skipping telemetry injection because _system.lua returned error code {result}"
                ));
            }
        }
    }

    result
}
