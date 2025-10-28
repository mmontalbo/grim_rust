use std::sync::OnceLock;

static TELEMETRY_DEBUG: OnceLock<bool> = OnceLock::new();
static TELEMETRY_STACK_DUMP: OnceLock<bool> = OnceLock::new();

pub(crate) fn telemetry_debug_enabled() -> bool {
    *TELEMETRY_DEBUG.get_or_init(|| match std::env::var("GRIM_TELEMETRY_DEBUG") {
        Ok(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    })
}

pub(crate) fn telemetry_stack_dump_enabled() -> bool {
    *TELEMETRY_STACK_DUMP.get_or_init(|| match std::env::var("GRIM_TELEMETRY_STACK_DUMP") {
        Ok(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    })
}
