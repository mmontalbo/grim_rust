use crate::{logging::log_line, lua_api::LuaState};
use std::sync::OnceLock;

static MAIN_STATE: OnceLock<usize> = OnceLock::new();

pub(crate) fn record_main_state(state: LuaState) {
    if state.is_null() {
        log_line("lua_open returned null; cannot record lua_State");
        return;
    }
    let addr = state as usize;
    match MAIN_STATE.set(addr) {
        Ok(()) => {
            log_line(&format!(
                "recorded lua main state at 0x{addr:08x} (first observation)"
            ));
        }
        Err(existing) => {
            if let Some(current) = MAIN_STATE.get().copied() {
                if current != addr {
                    log_line(&format!(
                        "lua_open returned a different lua_State (current=0x{current:08x}, new=0x{new:08x}); keeping the first",
                        current = current,
                        new = addr
                    ));
                }
            } else {
                log_line(&format!(
                    "lua_open state set raced; existing pointer=0x{existing:08x}"
                ));
            }
        }
    }
}
