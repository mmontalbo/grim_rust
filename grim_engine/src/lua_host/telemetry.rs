use std::ffi::c_void;

pub(crate) use grim_telemetry_common::EventBuilder;
use grim_telemetry_common::{TelemetryConfig, TelemetryLogger};

const ENGINE_ID: &str = "grim_engine";
const VM_ID: &str = "lua";

static LOGGER: TelemetryLogger = TelemetryLogger::new(TelemetryConfig {
    engine_id: ENGINE_ID,
    vm_id: VM_ID,
    log_env_vars: &["GRIM_ENGINE_LOG"],
    line_prefix: "grim_engine",
    run_id_env: None,
});

pub(crate) fn log_event(event: EventBuilder) {
    LOGGER.log_event(event);
}

pub(crate) fn log_push_cclosure(label: &str, func: *const c_void) {
    log_event(
        EventBuilder::new("push_cclosure")
            .kv("name", label)
            .kv("func", format!("{func:p}")),
    );
}

pub(crate) fn log_bind_global(name: &str, func: *const c_void) {
    log_event(
        EventBuilder::new("bind_global")
            .kv("name", name)
            .kv("handle", format!("{func:p}")),
    );
}

pub(crate) fn log_store_ref(lock: i32, reference: i32, label: Option<String>) {
    let mut event = EventBuilder::new("store_ref")
        .kv("lock", lock)
        .kv("ref", reference);
    if let Some(label) = label {
        event = event.kv("label", label);
    }
    log_event(event);
}

pub(crate) fn log_set_tagmethod(tag: i64, event: &str) {
    log_event(
        EventBuilder::new("set_tagmethod")
            .kv("tag", tag)
            .kv("event_name", event),
    );
}
