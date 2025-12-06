use crate::{
    logging::{log_event, log_line, LuaEvent},
    lua_api::{
        call_real_lua_createtable, call_real_lua_getparam, call_real_lua_gettable,
        call_real_lua_rawgettable, call_real_lua_rawsettable, call_real_lua_settable, LuaObject,
    },
};

use super::{
    caller_origin_fields, describe_lua_value, emit_set_table_entry, record_non_push_event,
    take_recent_pushes, value_fields_from_details,
};

/// Traces table creation and records the resulting handle metadata.
pub(crate) unsafe fn trace_lua_createtable() -> LuaObject {
    record_non_push_event();
    match call_real_lua_createtable() {
        Some(handle) => {
            let caller = caller_origin_fields();
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::CreateTable {
                handle: format!("0x{handle:08x}"),
                values,
                caller,
            });
            handle
        }
        None => {
            log_line("lua_createtable symbol missing; returning null handle");
            0
        }
    }
}

/// Traces `lua_settable`, emitting events about the table mutation when possible.
pub(crate) unsafe fn trace_lua_settable() {
    let pushes = take_recent_pushes(3);
    let table_handle = call_real_lua_getparam(-3);
    let caller = caller_origin_fields();
    let succeeded = call_real_lua_settable();
    let note = if succeeded {
        None
    } else {
        Some("lua_settable_missing".to_string())
    };
    log_event(LuaEvent::SetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    if succeeded {
        emit_set_table_entry(table_handle, pushes, caller, None);
    }
}

/// Traces `lua_rawsettable`, emitting events about the table mutation when possible.
pub(crate) unsafe fn trace_lua_rawsettable() {
    let pushes = take_recent_pushes(3);
    let table_handle = call_real_lua_getparam(-3);
    let caller = caller_origin_fields();
    let succeeded = call_real_lua_rawsettable();
    let note = if succeeded {
        None
    } else {
        Some("lua_rawsettable_missing".to_string())
    };
    log_event(LuaEvent::RawsetTable {
        note: note.clone(),
        caller: caller.clone(),
    });
    if succeeded {
        emit_set_table_entry(
            table_handle,
            pushes,
            caller,
            Some("via_rawsettable".to_string()),
        );
    }
}

/// Traces a table lookup with metamethods, logging the returned value.
pub(crate) unsafe fn trace_lua_gettable() -> LuaObject {
    record_non_push_event();
    match call_real_lua_gettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::GetTable {
                handle: format!("0x{handle:08x}"),
                values,
            });
            handle
        }
        None => {
            log_line("lua_gettable symbol missing; returning null handle");
            0
        }
    }
}

/// Traces a table lookup without metamethods, logging the returned value.
pub(crate) unsafe fn trace_lua_rawgettable() -> LuaObject {
    record_non_push_event();
    match call_real_lua_rawgettable() {
        Some(handle) => {
            let values = describe_lua_value(handle)
                .map(|value| value_fields_from_details(&value))
                .unwrap_or_default();
            log_event(LuaEvent::RawgetTable {
                handle: format!("0x{handle:08x}"),
                values,
            });
            handle
        }
        None => {
            log_line("lua_rawgettable symbol missing; returning null handle");
            0
        }
    }
}
