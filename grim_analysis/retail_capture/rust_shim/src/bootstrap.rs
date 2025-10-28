use crate::{
    logging::{log_line, sanitize_lua_string_fragment},
    lua_api::{
        call_real_lua_dofile, log_bootstrap_error_global, log_lua_stack_snapshot,
        resolve_lua_beginblock, resolve_lua_callfunction, resolve_lua_endblock,
        resolve_lua_getglobal, resolve_lua_getstring, resolve_lua_isfunction, resolve_lua_isstring,
        resolve_lua_pop, resolve_lua_pushobject, resolve_lua_strlibopen, resolve_lua_tag,
        BootstrapGlobalSnapshot, LuaObject,
    },
    native::{register_native_file_helpers, telemetry_native_write},
};
use libc::c_char;
use std::{
    ffi::{CStr, CString},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

static TELEMETRY_INJECTED: AtomicBool = AtomicBool::new(false);

pub(crate) struct BootstrapConfig<'a> {
    pub script_path: &'a str,
    pub bootstrap_log: &'a str,
    pub bootstrap_global: &'a [u8],
    pub native_name: &'a [u8],
    pub stack_snapshot_limit: usize,
}

pub(crate) fn inject_telemetry_script(config: &BootstrapConfig<'_>) {
    if TELEMETRY_INJECTED.swap(true, Ordering::SeqCst) {
        return;
    }

    unsafe {
        register_native_file_helpers(config.native_name, telemetry_native_write);
    }

    log_line(&format!(
        "injecting telemetry script via lua_dofile({})",
        config.script_path
    ));

    let script = match CString::new(config.script_path) {
        Ok(value) => value,
        Err(err) => {
            log_line(&format!(
                "failed to build CString for telemetry path {}: {err}",
                config.script_path
            ));
            return;
        }
    };

    let script_path = config.script_path;
    unsafe {
        if let Some(strlibopen) = resolve_lua_strlibopen() {
            strlibopen();
            log_line("lua_strlibopen invoked before telemetry load");
        } else {
            log_line("lua_strlibopen unavailable prior to telemetry load");
        }
    }
    let result = call_real_lua_dofile(script.as_ptr() as *mut c_char);
    if result == 0 {
        log_line(&format!("{script_path} executed successfully"));
    } else {
        let post_context = format!("post-{script_path}-dofile");
        let post_error_context = format!("post-{script_path}-dofile-error");
        let stack_context = format!("{script_path}-dofile-error");

        log_bootstrap_error_global(&post_context, config.bootstrap_global);
        let note = config.bootstrap_log;
        log_line(&format!(
            "{script_path} returned error code {result}; check {note}"
        ));
        log_line(&format!("telemetry capture starting ({post_context})"));
        let stack_snapshot = capture_lua_error_snapshot();
        log_line(&format!(
            "telemetry capture snapshot present={}",
            if stack_snapshot.is_some() {
                "true"
            } else {
                "false"
            }
        ));
        if let Some(entry) = &stack_snapshot {
            match entry {
                BootstrapGlobalSnapshot::Message(text) => {
                    let sanitized = sanitize_lua_string_fragment(text);
                    log_line(&format!("telemetry bootstrap stack message: {sanitized}"));
                }
                BootstrapGlobalSnapshot::Detail(detail) => {
                    log_line(&format!("telemetry bootstrap stack detail: {detail}"));
                }
            }
        }
        let snapshot = log_bootstrap_error_global(&post_error_context, config.bootstrap_global);
        if let Some((path, contents)) = read_bootstrap_log(note) {
            let trimmed = contents.trim();
            log_line(&format!(
                "telemetry bootstrap log ({}): {}",
                path.display(),
                trimmed
            ));
        } else {
            log_line(&format!(
                "telemetry bootstrap log missing; checked {:?}",
                candidate_bootstrap_paths(note)
            ));
        }
        let fallback_entry = stack_snapshot.as_ref().unwrap_or(&snapshot);
        write_bootstrap_log_fallback(note, fallback_entry, &post_error_context);
        log_lua_stack_snapshot(&stack_context, config.stack_snapshot_limit);
    }
}

pub(crate) fn candidate_bootstrap_paths(note: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(PathBuf::from(note));
    paths.push(PathBuf::from("dev-install").join(note));
    if let Ok(dev_install) = std::env::var("GRIM_DEV_INSTALL") {
        paths.push(PathBuf::from(dev_install).join(note));
    }
    paths
}

pub(crate) fn read_bootstrap_log(note: &str) -> Option<(PathBuf, String)> {
    for path in candidate_bootstrap_paths(note) {
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                if !contents.trim().is_empty() {
                    return Some((path, contents));
                }
            }
            Err(err) => {
                log_line(&format!(
                    "telemetry bootstrap log not readable at {}: {}",
                    path.display(),
                    err
                ));
            }
        }
    }
    None
}

pub(crate) fn write_bootstrap_log_fallback(
    note: &str,
    snapshot: &BootstrapGlobalSnapshot,
    context: &str,
) {
    let message = match snapshot {
        BootstrapGlobalSnapshot::Message(text) if !text.is_empty() => text.clone(),
        BootstrapGlobalSnapshot::Message(_) => "<empty>".to_string(),
        BootstrapGlobalSnapshot::Detail(detail) => {
            format!("[grim-rust-shim] bootstrap detail: {detail}")
        }
    };
    let entry = format!("{message} [context={context}]");
    for path in candidate_bootstrap_paths(note) {
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                log_line(&format!(
                    "fallback bootstrap log failed to create dir {}: {}",
                    parent.display(),
                    err
                ));
                continue;
            }
        }
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut file) => {
                if let Err(err) = writeln!(file, "{entry}") {
                    log_line(&format!(
                        "fallback bootstrap log write failed at {}: {}",
                        path.display(),
                        err
                    ));
                    continue;
                }
                log_line(&format!(
                    "fallback bootstrap log appended to {}",
                    path.display()
                ));
                break;
            }
            Err(err) => {
                log_line(&format!(
                    "fallback bootstrap log could not open {}: {}",
                    path.display(),
                    err
                ));
            }
        }
    }
}

fn capture_lua_error_snapshot() -> Option<BootstrapGlobalSnapshot> {
    unsafe {
        let Some(pop_fn) = resolve_lua_pop() else {
            log_line("lua_pop unavailable; cannot capture telemetry error");
            return None;
        };
        let begin_block = resolve_lua_beginblock();
        let end_block = resolve_lua_endblock();
        if begin_block.is_none() || end_block.is_none() {
            log_line("lua_beginblock or lua_endblock unavailable; cannot capture telemetry error");
            return None;
        }
        let begin_fn = begin_block.unwrap();
        let end_fn = end_block.unwrap();
        begin_fn();
        let obj = pop_fn();
        end_fn();
        log_line(&format!("telemetry capture pop returned object={:?}", obj));
        if obj.is_null() {
            return Some(BootstrapGlobalSnapshot::Detail(
                "lua error stack empty".to_string(),
            ));
        }
        if let Some(tag_fn) = resolve_lua_tag() {
            let tag = tag_fn(obj);
            log_line(&format!(
                "telemetry bootstrap raw error object={:?} tag={}",
                obj, tag
            ));
        } else {
            log_line("telemetry bootstrap raw error missing lua_tag");
        }

        let snapshot = if let Some(message) = extract_lua_error_message(obj) {
            BootstrapGlobalSnapshot::Message(message)
        } else {
            let detail = if let Some(tag_fn) = resolve_lua_tag() {
                let tag = tag_fn(obj);
                format!("non-string object (tag={tag})")
            } else {
                "non-string object".to_string()
            };
            BootstrapGlobalSnapshot::Detail(detail)
        };

        if let Some(push_object_fn) = resolve_lua_pushobject() {
            begin_fn();
            push_object_fn(obj);
            end_fn();
        } else {
            log_line("lua_pushobject unavailable; telemetry error object dropped");
        }

        Some(snapshot)
    }
}

unsafe fn extract_lua_error_message(obj: LuaObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    if let (Some(is_string_fn), Some(get_string_fn)) =
        (resolve_lua_isstring(), resolve_lua_getstring())
    {
        let is_string = is_string_fn(obj) != 0;
        log_line(&format!(
            "telemetry bootstrap error is_string={}",
            if is_string { "true" } else { "false" }
        ));
        if is_string {
            let ptr = get_string_fn(obj);
            if !ptr.is_null() {
                return Some(CStr::from_ptr(ptr).to_string_lossy().into_owned());
            }
            log_line("telemetry bootstrap error string pointer null");
        } else {
            log_line("telemetry bootstrap error not a string; attempting tostring");
        }
    } else {
        log_line("telemetry bootstrap missing lua_isstring or lua_getstring");
    }

    stringify_lua_object(obj)
}

pub(crate) unsafe fn stringify_lua_object(obj: LuaObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }

    let Some(push_object_fn) = resolve_lua_pushobject() else {
        log_line("lua_pushobject unavailable during error stringify");
        return None;
    };
    let Some(pop_fn) = resolve_lua_pop() else {
        log_line("lua_pop unavailable during error stringify");
        return None;
    };
    let Some(get_global_fn) = resolve_lua_getglobal() else {
        log_line("lua_getglobal unavailable during error stringify");
        return None;
    };
    let Some(is_function_fn) = resolve_lua_isfunction() else {
        log_line("lua_isfunction unavailable during error stringify");
        return None;
    };
    let Some(call_function_fn) = resolve_lua_callfunction() else {
        log_line("lua_callfunction unavailable during error stringify");
        return None;
    };
    let Some(begin_block_fn) = resolve_lua_beginblock() else {
        log_line("lua_beginblock unavailable during error stringify");
        return None;
    };
    let Some(end_block_fn) = resolve_lua_endblock() else {
        log_line("lua_endblock unavailable during error stringify");
        return None;
    };
    let Some(is_string_fn) = resolve_lua_isstring() else {
        log_line("lua_isstring unavailable during string result check");
        return None;
    };
    let Some(get_string_fn) = resolve_lua_getstring() else {
        log_line("lua_getstring unavailable during string result fetch");
        return None;
    };

    let tostring_obj = get_global_fn(b"tostring\0".as_ptr() as *const c_char);
    if tostring_obj.is_null() || is_function_fn(tostring_obj) == 0 {
        log_line("telemetry stringify could not resolve tostring");
        return None;
    }

    log_line("telemetry stringify invoking tostring()");
    begin_block_fn();
    push_object_fn(obj);
    let call_result = call_function_fn(tostring_obj);
    let result_obj = pop_fn();
    end_block_fn();

    if call_result != 0 {
        log_line(&format!(
            "tostring() failed for telemetry error object (code={call_result})"
        ));
        return None;
    }
    if result_obj.is_null() {
        return None;
    }
    if is_string_fn(result_obj) == 0 {
        return None;
    }
    let ptr = get_string_fn(result_obj);
    if ptr.is_null() {
        log_line("telemetry stringify result pointer null");
        return None;
    }
    log_line("telemetry stringify succeeded");

    Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
}
