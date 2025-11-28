use std::{
    ffi::c_void,
    sync::atomic::{AtomicU64, Ordering},
};

use grim_telemetry_common::{
    EventBuilder, LuaEvent, OriginFields, TelemetryConfig, TelemetryLogger, ValueFields,
};

const ENGINE_ID: &str = "grim_engine";
const VM_ID: &str = "lua";

static PUSH_SEQ: AtomicU64 = AtomicU64::new(0);

static LOGGER: TelemetryLogger = TelemetryLogger::new(TelemetryConfig {
    engine_id: ENGINE_ID,
    vm_id: VM_ID,
    log_env_vars: &["GRIM_ENGINE_LOG"],
    line_prefix: "grim_engine",
    run_id_env: None,
});

pub(crate) fn log_event(event: impl Into<EventBuilder>) {
    LOGGER.log_event(event);
}

pub(crate) fn log_push_cclosure(label: &str, func: *const c_void) {
    let seq = PUSH_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    log_event(LuaEvent::PushCclosure {
        name: label.to_string(),
        func: format!("0x{:08x}", func as usize),
        push_seq: seq,
        upvalues: 0,
        origin: OriginFields::default(),
    });
}

pub(crate) fn log_lua_setglobal(name: &str, func: *const c_void) {
    log_event(LuaEvent::BindGlobal {
        name: name.to_string(),
        handle: format!("0x{:08x}", func as usize),
        handle_label: None,
        label: None,
        values: ValueFields::default(),
        origin: OriginFields::default(),
    });
}

pub(crate) fn log_store_ref(lock: i32, reference: i32, label: Option<String>) {
    log_event(LuaEvent::StoreRef {
        lock,
        reference,
        handle: None,
        handle_label: None,
        label,
        note: None,
        origin: OriginFields::default(),
    });
}

pub(crate) fn log_set_tagmethod(tag: i64, event: &str) {
    log_event(LuaEvent::SetTagmethod {
        tag: tag as i32,
        event_name: event.to_string(),
        tag_label: None,
    });
}
