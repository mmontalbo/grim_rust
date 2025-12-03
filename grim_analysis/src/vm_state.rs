use crate::{logging::log_line, lua_api::LuaState};
use std::sync::{Mutex, OnceLock};

static MAIN_STATE: OnceLock<usize> = OnceLock::new();
static THREAD_STATES: OnceLock<Mutex<Vec<usize>>> = OnceLock::new();

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

pub(crate) fn main_state_addr() -> Option<usize> {
    MAIN_STATE.get().copied()
}

pub(crate) fn record_thread_state(state: LuaState) {
    if state.is_null() {
        log_line("lua_newthread returned null; skipping thread registry update");
        return;
    }
    let addr = state as usize;
    let registry = THREAD_STATES.get_or_init(|| Mutex::new(Vec::new()));
    match registry.lock() {
        Ok(mut states) => {
            if !states.contains(&addr) {
                states.push(addr);
                log_line(&format!(
                    "recorded lua thread state at 0x{addr:08x} ({} tracked total)",
                    states.len()
                ));
            }
        }
        Err(_) => log_line("lua_newthread registry mutex poisoned; skipping update"),
    }
}
