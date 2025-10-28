use crate::env::telemetry_debug_enabled;
use std::path::Path;

pub(crate) fn log_line(message: &str) {
    eprintln!("[grim-rust-shim] {message}");
}

pub(crate) fn sanitize_lua_string_fragment(input: &str) -> String {
    const LIMIT: usize = 200;
    let mut text = input
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    if text.len() > LIMIT {
        text.truncate(LIMIT);
        text.push_str("...");
    }
    text
}

pub(crate) struct TelemetryLogger {
    debug: bool,
}

impl TelemetryLogger {
    pub(crate) fn new() -> Self {
        Self {
            debug: telemetry_debug_enabled(),
        }
    }

    pub(crate) fn log_call(&self, path: &Path, mode: &str, bytes: usize) {
        if !self.debug {
            return;
        }
        let (parent_desc, parent_exists) = match path.parent() {
            Some(parent) => (parent.display().to_string(), parent.exists()),
            None => ("<none>".to_string(), true),
        };
        log_line(&format!(
            "telemetry_native_write call path={} mode={} bytes={} parent={} (exists={})",
            path.display(),
            mode,
            bytes,
            parent_desc,
            parent_exists
        ));
    }

    pub(crate) fn log_success(&self, path: &Path, mode: &str, bytes: usize) {
        if !self.debug {
            return;
        }
        log_line(&format!(
            "telemetry_native_write wrote {} bytes to {} (mode={})",
            bytes,
            path.display(),
            mode
        ));
    }

    pub(crate) fn log_open_error(&self, path: &Path, err: &std::io::Error) {
        log_line(&format!(
            "telemetry_native_write failed to open {}: {} (errno={})",
            path.display(),
            err,
            Self::errno(err)
        ));
    }

    pub(crate) fn log_write_error(&self, path: &Path, err: &std::io::Error) {
        log_line(&format!(
            "telemetry_native_write failed to write {}: {} (errno={})",
            path.display(),
            err,
            Self::errno(err)
        ));
    }

    pub(crate) fn errno(err: &std::io::Error) -> String {
        err.raw_os_error()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}
