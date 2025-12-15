//! Retail-specific telemetry hooks are disabled for boot-parity captures.
//! The shim still surfaces the same call tracing, but cutscene/menu wrappers
//! and JSONL fan-out are intentionally removed to keep the boot window minimal.

/// No-op placeholder; kept for call-site compatibility.
pub(crate) fn observe_lua_activity() {}

/// No-op push tracking; retained so trace code builds without the cutscene pipeline.
pub(crate) fn record_pushed_number(_value: f64) {}

/// No-op push tracking; retained so trace code builds without the cutscene pipeline.
pub(crate) fn record_pushed_nil() {}
