//! Minimal logging facade that forwards events to `grim_telemetry_schema`.
//!
//! The shim uses this to emit structured Lua events with consistent engine/VM
//! identifiers while printing line-based diagnostics to stderr.
use grim_telemetry_schema::{BootSequenceTracker, JsonlWriter, TelemetryConfig, TelemetryLogger};
pub(crate) use grim_telemetry_schema::{
    LuaEvent, LuaSemanticEvent, OriginFields, UpvaluePreview, ValueFields, ValueType,
};

pub(crate) const ENGINE_ID: &str = "retail";
pub(crate) const VM_ID: &str = "lua32";

static BOOT_SEQUENCE: BootSequenceTracker = BootSequenceTracker::new();
static LOGGER: TelemetryLogger = TelemetryLogger::new(TelemetryConfig {
    engine_id: ENGINE_ID,
    vm_id: VM_ID,
    log_env_vars: &["GRIM_SHIM_LOG"],
    line_prefix: "grim-rust-shim",
    run_id_env: None,
    raw_stream_env: Some("GRIM_RAW_TELEMETRY"),
});

/// Emits a single line-based diagnostic to the configured telemetry sink.
pub(crate) fn log_line(message: &str) {
    LOGGER.log_line(message);
}

/// Sends a structured telemetry event without exposing the logger.
pub(crate) fn log_event(event: impl grim_telemetry_schema::TelemetryEventPayload) {
    LOGGER.log_event(event);
}

/// Sends a structured telemetry event and returns its sequence number for correlation.
pub(crate) fn log_event_with_seq(event: impl grim_telemetry_schema::TelemetryEventPayload) -> u64 {
    LOGGER.log_event_with_seq(event)
}

#[allow(dead_code)]
/// Serializes and writes a structured telemetry event to an external JSONL sink.
pub(crate) fn log_event_to_writer(
    event: impl grim_telemetry_schema::TelemetryEventPayload,
    writer: &mut JsonlWriter,
) -> std::io::Result<u64> {
    LOGGER.log_event_to_writer(event, writer)
}

pub(crate) fn log_boot_sequence_start() {
    if let Some(event) = BOOT_SEQUENCE.boot_started() {
        LOGGER.log_event(event);
    }
}

pub(crate) fn log_boot_sequence_complete(note: Option<&str>) {
    if let Some(event) = BOOT_SEQUENCE.boot_complete(note.map(str::to_string)) {
        LOGGER.log_event(event);
    }
}
