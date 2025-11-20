use libc::c_int;

mod logging;
mod lua_api;
mod trace;

use lua_api::LuaCFunction;
use trace::trace_lua_push_closure;

#[no_mangle]
pub unsafe extern "C" fn lua_pushCclosure(func: LuaCFunction, upvalues: c_int) {
    trace_lua_push_closure("lua_pushCclosure", func, upvalues);
}

// Retail liblua only exports the capital-C variant; keep a note to avoid re-adding lua_pushcclosure.
