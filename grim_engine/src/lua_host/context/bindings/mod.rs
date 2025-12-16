mod boot;
mod bootstrap;
mod dofile;
mod legacy;
mod registry;
mod util;

pub(crate) use boot::{call_boot, load_system_script, override_boot_stubs, wrap_boot};
pub(crate) use bootstrap::{
    install_globals_post_system, install_globals_pre_system, install_package_path,
};
pub(crate) use registry::{store_registry_value, PinnedRegistryKeys, RefRegistry};

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, path::Path, rc::Rc};

    use mlua::{Function, Lua, LuaOptions, Result as LuaResult, StdLib, Value, Variadic};

    use crate::lua_host::context::EngineContext;

    use super::*;

    fn setup_lua() -> Lua {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        let context = Rc::new(RefCell::new(EngineContext::new(true, true)));
        install_package_path(&lua, Path::new(".")).unwrap();
        install_globals_pre_system(&lua, Path::new("."), context.clone()).unwrap();
        install_globals_post_system(&lua, context).unwrap();
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
    fn missing_globals_use_getglobal_fallback() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let index_called = Rc::new(RefCell::new(false));
        let index_flag = index_called.clone();
        let index_handler = lua
            .create_function(
                move |_, (_table, _key): (Value, Value)| -> LuaResult<Value> {
                    *index_flag.borrow_mut() = true;
                    Ok(Value::Nil)
                },
            )
            .unwrap();
        setfallback
            .call::<_, Value>(("index", index_handler))
            .unwrap();

        let getglobal_called = Rc::new(RefCell::new(false));
        let getglobal_flag = getglobal_called.clone();
        let getglobal_handler = lua
            .create_function(
                move |lua_ctx, (_table, _key): (Value, Value)| -> LuaResult<Value> {
                    *getglobal_flag.borrow_mut() = true;
                    Ok(Value::String(lua_ctx.create_string("from_getglobal")?))
                },
            )
            .unwrap();
        setfallback
            .call::<_, Value>(("getglobal", getglobal_handler))
            .unwrap();

        let value: String = lua.load("return missing_global_name").eval().unwrap();
        assert_eq!(value, "from_getglobal");
        assert!(*getglobal_called.borrow());
        assert!(!*index_called.borrow());
    }

    #[test]
    fn index_fallback_applies_to_tables() {
        let lua = setup_lua();
        let globals = lua.globals();
        let setfallback: Function = globals.get("setfallback").unwrap();
        let handler = lua
            .create_function(|_, (table, field): (Value, Value)| {
                if let Value::Table(child) = table {
                    if let Ok(Value::Table(parent)) = child.get::<_, Value>("parent") {
                        return parent.get(field);
                    }
                }
                Ok(Value::Nil)
            })
            .unwrap();
        setfallback.call::<_, Value>(("index", handler)).unwrap();
        let value: String = lua
            .load(
                r#"
local Actor = { greet = function(self) return "hi" end }
function Actor:create()
    return { parent = self }
end
child = Actor:create()
return child:greet()
"#,
            )
            .eval()
            .unwrap();
        assert_eq!(value, "hi");
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
