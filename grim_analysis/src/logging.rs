//! Minimal logging facade that forwards events to `grim_telemetry_common`.
//!
//! The shim uses this to emit both line-based diagnostics and structured Lua
//! events with consistent engine/VM identifiers.
pub(crate) use grim_telemetry_common::{
    EventBuilder, LuaEvent, LuaSemanticEvent, OriginFields, UpvaluePreview, ValueFields, ValueType,
};
use grim_telemetry_common::{TelemetryConfig, TelemetryLogger};

pub(crate) const ENGINE_ID: &str = "retail";
pub(crate) const VM_ID: &str = "lua32";

static LOGGER: TelemetryLogger = TelemetryLogger::new(TelemetryConfig {
    engine_id: ENGINE_ID,
    vm_id: VM_ID,
    log_env_vars: &["GRIM_SHIM_LOG"],
    line_prefix: "grim-rust-shim",
    run_id_env: None,
});

/// Emits a single line-based diagnostic to the configured telemetry sink.
pub(crate) fn log_line(message: &str) {
    LOGGER.log_line(message);
}

/// Sends a structured telemetry event without exposing the logger.
pub(crate) fn log_event(event: impl Into<EventBuilder>) {
    LOGGER.log_event(event);
}

/// Sends a structured telemetry event and returns its sequence number for correlation.
pub(crate) fn log_event_with_seq(event: impl Into<EventBuilder>) -> u64 {
    LOGGER.log_event_with_seq(event)
}
