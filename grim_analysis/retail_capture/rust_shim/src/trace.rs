use crate::{
    logging::log_line,
    lua_api::{call_real_lua_push_c_closure, LuaCFunction},
};
use libc::{c_char, c_int, Dl_info};
use std::{
    ffi::{c_void, CStr},
    mem::MaybeUninit,
    sync::atomic::{AtomicU64, Ordering},
};

static CLOSURE_PUSH_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) unsafe fn trace_lua_push_closure(label: &str, func: LuaCFunction, upvalues: c_int) {
    let sequence = CLOSURE_PUSH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let func_addr = func as *const c_void as usize;
    let symbol_details = describe_closure_target(func as *const c_void);

    log_line(&format!(
        "#{sequence:06} {label} func=0x{func_addr:08x} upvalues={upvalues}{symbol_details}"
    ));

    if !call_real_lua_push_c_closure(func, upvalues) {
        log_line("unable to forward lua_pushCclosure call; retail VM may misbehave");
    }
}

fn describe_closure_target(ptr: *const c_void) -> String {
    unsafe {
        let mut info = MaybeUninit::<Dl_info>::zeroed();
        if libc::dladdr(ptr, info.as_mut_ptr()) == 0 {
            return String::new();
        }
        let info = info.assume_init();
        let module = cstr_opt(info.dli_fname);
        let symbol = cstr_opt(info.dli_sname);
        let mut fragments = Vec::new();
        if let Some(module) = module {
            fragments.push(format!("module={module}"));
        }
        if let Some(symbol) = symbol {
            fragments.push(format!("symbol={symbol}"));
        }
        if fragments.is_empty() {
            String::new()
        } else {
            format!(" {}", fragments.join(" "))
        }
    }
}

unsafe fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}
