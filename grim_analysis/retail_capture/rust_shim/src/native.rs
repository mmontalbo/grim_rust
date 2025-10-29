use crate::{
    env::telemetry_debug_enabled,
    logging::{log_line, sanitize_lua_string_fragment, TelemetryLogger},
    lua_api::{
        capture_lua_params, resolve_lua_getglobal, resolve_lua_getparam, resolve_lua_getstring,
        resolve_lua_isfunction, resolve_lua_isstring, resolve_lua_pushcclosure,
        resolve_lua_pushnumber, resolve_lua_setglobal, LuaCFunction,
    },
};
use libc::c_char;
use std::{
    ffi::CStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
    str,
    sync::atomic::{AtomicBool, Ordering},
};

static TELEMETRY_NATIVE_REGISTERED: AtomicBool = AtomicBool::new(false);
static TELEMETRY_NATIVE_WRITE_SEEN: AtomicBool = AtomicBool::new(false);
static TELEMETRY_NATIVE_MARK_REGISTERED: AtomicBool = AtomicBool::new(false);
static TELEMETRY_NATIVE_MARK_SEEN: AtomicBool = AtomicBool::new(false);

unsafe fn register_lua_native(
    flag: &AtomicBool,
    name: &[u8],
    handler: LuaCFunction,
    description: &str,
) -> bool {
    if flag.load(Ordering::SeqCst) {
        log_line(&format!(
            "skipping {description} registration; flag already set"
        ));
        return false;
    }
    let Some(push_closure) = resolve_lua_pushcclosure() else {
        log_line(&format!(
            "lua_pushcclosure unavailable; cannot register {description}"
        ));
        return false;
    };
    let Some(set_global) = resolve_lua_setglobal() else {
        log_line(&format!(
            "lua_setglobal unavailable; cannot register {description}"
        ));
        return false;
    };
    push_closure(handler, 0);
    set_global(name.as_ptr() as *mut c_char);
    flag.store(true, Ordering::SeqCst);
    log_line(&format!("registered {description} helper"));
    true
}

pub(crate) unsafe fn register_native_file_helpers(native_name: &[u8], handler: LuaCFunction) {
    if !register_lua_native(
        &TELEMETRY_NATIVE_REGISTERED,
        native_name,
        handler,
        "telemetry_native_write",
    ) {
        return;
    }
    if let (Some(get_global), Some(is_function)) =
        (resolve_lua_getglobal(), resolve_lua_isfunction())
    {
        let obj = get_global(native_name.as_ptr() as *const c_char);
        let state = if obj.is_null() {
            "missing"
        } else if is_function(obj) != 0 {
            "function"
        } else {
            "non-function"
        };
        log_line(&format!(
            "telemetry_native_write global state: {state} (ptr={:?})",
            obj
        ));
    }
}

pub(crate) unsafe fn register_native_mark(native_name: &[u8]) {
    log_line("attempting telemetry_native_mark registration");
    register_lua_native(
        &TELEMETRY_NATIVE_MARK_REGISTERED,
        native_name,
        telemetry_native_mark,
        "telemetry_native_mark",
    );
}

unsafe fn telemetry_native_write_impl() -> bool {
    let Some(get_param) = resolve_lua_getparam() else {
        log_line("lua_getparam unavailable; telemetry_native_write aborted");
        return false;
    };
    let Some(is_string) = resolve_lua_isstring() else {
        log_line("lua_isstring unavailable; telemetry_native_write aborted");
        return false;
    };
    let Some(get_string) = resolve_lua_getstring() else {
        log_line("lua_getstring unavailable; telemetry_native_write aborted");
        return false;
    };

    let path_obj = get_param(1);
    let contents_obj = get_param(2);
    if path_obj.is_null() || contents_obj.is_null() {
        return false;
    }
    if is_string(path_obj) == 0 || is_string(contents_obj) == 0 {
        return false;
    }
    let path_ptr = get_string(path_obj);
    let contents_ptr = get_string(contents_obj);
    if path_ptr.is_null() || contents_ptr.is_null() {
        return false;
    }

    let mode_obj = get_param(3);
    let mut mode_bytes: &[u8] = b"a";
    if !mode_obj.is_null() && is_string(mode_obj) != 0 {
        let mode_ptr = get_string(mode_obj);
        if !mode_ptr.is_null() {
            let raw = CStr::from_ptr(mode_ptr).to_bytes();
            if !raw.is_empty() {
                mode_bytes = raw;
            }
        }
    }

    let path_bytes = CStr::from_ptr(path_ptr).to_bytes();
    if path_bytes.is_empty() {
        return false;
    }
    let contents_bytes = CStr::from_ptr(contents_ptr).to_bytes();

    let path_buf = PathBuf::from(std::ffi::OsString::from_vec(path_bytes.to_vec()));
    if !TELEMETRY_NATIVE_WRITE_SEEN.swap(true, Ordering::SeqCst) {
        log_line(&format!(
            "telemetry_native_write invoked for {}",
            path_buf.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.create(true);
    let mode = str::from_utf8(mode_bytes).unwrap_or("a");
    let logger = TelemetryLogger::new();
    logger.log_call(&path_buf, mode, contents_bytes.len());
    if mode.contains('w') {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }

    if let Some(parent) = path_buf.parent() {
        if !parent.exists() {
            if let Err(err) = fs::create_dir_all(parent) {
                log_line(&format!(
                    "telemetry_native_write failed to create parent dir {}: {} (errno={})",
                    parent.display(),
                    err,
                    TelemetryLogger::errno(&err)
                ));
                return false;
            }
        }
    }

    match options.open(&path_buf) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(contents_bytes) {
                logger.log_write_error(&path_buf, &err);
                false
            } else {
                logger.log_success(&path_buf, mode, contents_bytes.len());
                true
            }
        }
        Err(err) => {
            logger.log_open_error(&path_buf, &err);
            false
        }
    }
}

pub(crate) unsafe extern "C" fn telemetry_native_write() {
    let success = telemetry_native_write_impl();
    if let Some(push_number) = resolve_lua_pushnumber() {
        push_number(if success { 1.0 } else { 0.0 });
    } else {
        log_line("lua_pushnumber unavailable; telemetry_native_write skipping return value");
    }
}

unsafe fn telemetry_native_mark_impl() -> bool {
    let Some(arguments) = capture_lua_params(1) else {
        log_line("telemetry_native_mark missing argument");
        return false;
    };
    let object = arguments[0];
    let Some(is_string) = resolve_lua_isstring() else {
        log_line("lua_isstring unavailable; telemetry_native_mark aborted");
        return false;
    };
    if is_string(object) == 0 {
        log_line("telemetry_native_mark received non-string argument");
        return false;
    }
    let Some(get_string) = resolve_lua_getstring() else {
        log_line("lua_getstring unavailable; telemetry_native_mark aborted");
        return false;
    };
    let ptr = get_string(object);
    if ptr.is_null() {
        log_line("telemetry_native_mark argument string pointer null");
        return false;
    }
    let key = CStr::from_ptr(ptr).to_string_lossy();
    let sanitized = sanitize_lua_string_fragment(&key);
    let debug = telemetry_debug_enabled();
    if !TELEMETRY_NATIVE_MARK_SEEN.swap(true, Ordering::SeqCst) {
        log_line(&format!(
            "telemetry_native_mark first invocation key=\"{sanitized}\""
        ));
    } else if debug {
        log_line(&format!("telemetry_native_mark key=\"{sanitized}\""));
    }
    true
}

#[no_mangle]
pub unsafe extern "C" fn telemetry_native_mark() {
    let success = telemetry_native_mark_impl();
    if let Some(push_number) = resolve_lua_pushnumber() {
        push_number(if success { 1.0 } else { 0.0 });
    } else {
        log_line("lua_pushnumber unavailable; telemetry_native_mark skipping return value");
    }
}
