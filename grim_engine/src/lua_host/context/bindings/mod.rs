mod boot;
mod bootstrap;
mod dofile;
mod legacy;
mod util;

pub(crate) use boot::{
    call_boot, drive_active_scripts, dump_runtime_summary, ensure_intro_cutscene,
    load_system_script, override_boot_stubs,
};
pub(crate) use bootstrap::{install_globals, install_package_path};

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, path::Path, rc::Rc};

    use mlua::{Function, Lua, LuaOptions, Result as LuaResult, StdLib, Value, Variadic};

    use super::util::describe_value;
    use crate::lua_host::context::EngineContext;

    use super::*;

    fn setup_lua() -> Lua {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        let context = Rc::new(RefCell::new(EngineContext::new(true, true)));
        install_globals(&lua, Path::new("."), context).unwrap();
        lua
    }

    #[test]
    fn setfallback_returns_previous_handler() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();

        let handler_one = lua.create_function(|_, ()| Ok("first")).unwrap();
        let previous = setfallback
            .call::<_, Value>(("index", handler_one.clone()))
            .unwrap();
        if let Value::Function(default_fb) = previous {
            let default_result: Value = default_fb.call((Value::Nil, Value::Nil)).unwrap();
            assert!(matches!(default_result, Value::Nil));
        } else {
            panic!("expected function from default index fallback");
        }

        let handler_two = lua.create_function(|_, ()| Ok("second")).unwrap();
        let returned = setfallback
            .call::<_, Value>(("index", handler_two.clone()))
            .unwrap();
        let previous_fn = match returned {
            Value::Function(func) => func,
            other => panic!("expected function from previous handler, got {other:?}"),
        };
        assert_eq!(previous_fn.to_pointer(), handler_one.to_pointer());
    }

    #[test]
    fn setfallback_rejects_non_function() {
        let lua = setup_lua();
        let result = lua.load("return setfallback('index', 42)").eval::<Value>();
        assert!(result.is_err());
    }

    #[test]
    fn error_fallback_runs_before_error() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let flag = Rc::new(RefCell::new(false));
        let flag_ref = flag.clone();
        let handler = lua
            .create_function(move |_, _: Variadic<Value>| {
                *flag_ref.borrow_mut() = true;
                Ok(Value::Nil)
            })
            .unwrap();
        setfallback.call::<_, Value>(("error", handler)).unwrap();
        let result: LuaResult<()> = lua.load("error('boom')").exec();
        assert!(result.is_err());
        assert!(*flag.borrow());
    }

    #[test]
    fn index_fallback_applies_to_missing_globals() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua
            .create_function(
                |lua_ctx, (_table, key): (Value, Value)| -> LuaResult<Value> {
                    let key_name = match key {
                        Value::String(text) => text.to_str().unwrap_or("<key>").to_string(),
                        other => describe_value(&other),
                    };
                    Ok(Value::String(
                        lua_ctx.create_string(&format!("fb::{key_name}"))?,
                    ))
                },
            )
            .unwrap();
        setfallback.call::<_, Value>(("index", handler)).unwrap();
        let value: String = lua.load("return missing_global_name").eval().unwrap();
        assert_eq!(value, "fb::missing_global_name");
    }

    #[test]
    fn gettable_fallback_available_via_tag_lookup() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua
            .create_function(|_, (_table, _key): (Value, Value)| Ok("handled"))
            .unwrap();
        setfallback.call::<_, Value>(("gettable", handler)).unwrap();
        let value: String = lua
            .load("local fb = gettagmethod(tag(nil), 'gettable'); return fb(nil, 'field')")
            .eval()
            .unwrap();
        assert_eq!(value, "handled");
    }

    #[test]
    fn unknown_fallbacks_can_be_installed() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua.create_function(|_, ()| Ok(123)).unwrap();
        let previous: Value = setfallback.call(("mystery", handler)).unwrap();
        assert!(matches!(previous, Value::Nil));
        let value: i32 = lua
            .load("local fb = gettagmethod(tag(nil), 'mystery'); return fb()")
            .eval()
            .unwrap();
        assert_eq!(value, 123);
    }
}
