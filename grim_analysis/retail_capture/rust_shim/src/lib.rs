use libc::{c_char, c_int};

mod bootstrap;
mod env;
mod logging;
mod lua_api;
mod native;

use bootstrap::{inject_telemetry_script, BootstrapConfig};
use logging::log_line;
use lua_api::{call_real_lua_dofile, filename_from_ptr, is_system_script};

const TELEMETRY_SCRIPT: &str = "mods/telemetry_simple.lua";
const TELEMETRY_BOOTSTRAP_ERROR_LOG: &str = "mods/telemetry_bootstrap_error.log";
const TELEMETRY_BOOTSTRAP_ERROR_GLOBAL: &[u8] = b"__telemetry_bootstrap_error\0";
const TELEMETRY_NATIVE_WRITE_NAME_CSTR: &[u8] = b"telemetry_native_write\0";
const TELEMETRY_NATIVE_MARK_NAME_CSTR: &[u8] = b"telemetry_native_mark\0";
const LUA_STACK_SNAPSHOT_LIMIT: usize = 4;

fn telemetry_bootstrap_config() -> BootstrapConfig<'static> {
    BootstrapConfig {
        script_path: TELEMETRY_SCRIPT,
        bootstrap_log: TELEMETRY_BOOTSTRAP_ERROR_LOG,
        bootstrap_global: TELEMETRY_BOOTSTRAP_ERROR_GLOBAL,
        native_write_name: TELEMETRY_NATIVE_WRITE_NAME_CSTR,
        native_mark_name: TELEMETRY_NATIVE_MARK_NAME_CSTR,
        stack_snapshot_limit: LUA_STACK_SNAPSHOT_LIMIT,
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
            log_line(&format!(
                "observed lua_dofile call for {text} (result={result})"
            ));
            if result == 0 {
                inject_telemetry_script(&telemetry_bootstrap_config());
            } else {
                log_line(&format!(
                    "skipping telemetry injection because _system.lua returned error code {result}"
                ));
            }
        }
    }

    result
}
