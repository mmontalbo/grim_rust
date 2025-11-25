pub(crate) use grim_telemetry_common::EventBuilder;
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

pub(crate) fn log_line(message: &str) {
    LOGGER.log_line(message);
}

pub(crate) fn log_event(event: EventBuilder) {
    LOGGER.log_event(event);
}
