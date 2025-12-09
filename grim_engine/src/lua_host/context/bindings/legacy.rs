use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use grim_telemetry_common::EventBuilder;
use grim_telemetry_common::OriginFields;
use mlua::{
    Error as LuaError, Function, Lua, RegistryKey, Result as LuaResult, Table, Value, Variadic,
};

use crate::lua_host::telemetry::{
    log_event, log_fetch_ref, log_set_fallback, log_set_tagmethod, log_unref,
    next_fabricated_handle, normalize_handle, origin_fields_for_ptr, ptr_to_handle,
    register_table_label, table_label,
};

use super::util::{
    describe_value, handle_from_value, set_global_silent, value_fields_from_lua, TaggedHandle,
};
use super::{store_registry_value, RegistryRef};
use crate::lua_host::context::EngineContext;

pub(super) fn install_legacy_compat<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let fallbacks = Rc::new(RefCell::new(LegacyFallbacks::new(lua)?));
    install_fallback_globals(lua, globals, fallbacks.clone(), context.clone())?;
    let verbose = context.borrow().verbose();
    install_index_hook(lua, globals, fallbacks.clone(), verbose)?;
    install_error_wrapper(lua, globals, fallbacks)?;

    Ok(())
}

// Lua 3.1/MSVCRT LCG to mirror retail math.random/randomseed output and telemetry.
const LUA31_RAND_MAX: u32 = 0x7fff;

fn lua31_rand(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(214013).wrapping_add(2531011);
    (*state >> 16) & LUA31_RAND_MAX
}

fn coerce_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        Value::String(text) => text.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
}

pub(super) fn install_legacy_math<'lua>(lua: &'lua Lua, globals: &Table<'lua>) -> LuaResult<()> {
    let state = Rc::new(RefCell::new(1u32));

    let random_state = state.clone();
    let random = lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<Value> {
        let mut seed = random_state.borrow_mut();
        let raw = lua31_rand(&mut seed);
        let bounded = raw % LUA31_RAND_MAX;
        let unit = (bounded as f32) / (LUA31_RAND_MAX as f32);
        match args.first() {
            None | Some(Value::Nil) => Ok(Value::Number(unit as f64)),
            Some(arg) => {
                let upper = coerce_number(arg).ok_or_else(|| {
                    LuaError::RuntimeError(
                        "bad argument #1 to 'random' (number expected)".to_string(),
                    )
                })?;
                let value = (unit as f64 * upper).trunc() + 1.0;
                Ok(Value::Number(value))
            }
        }
    })?;

    let seed_state = state.clone();
    let randomseed = lua.create_function(move |_, args: Variadic<Value>| {
        let seed_arg = args.first().ok_or_else(|| {
            LuaError::RuntimeError("bad argument #1 to 'randomseed' (number expected)".to_string())
        })?;
        let seed = coerce_number(seed_arg).ok_or_else(|| {
            LuaError::RuntimeError("bad argument #1 to 'randomseed' (number expected)".to_string())
        })?;
        *seed_state.borrow_mut() = seed as u32;
        Ok(())
    })?;

    set_global_silent(lua, globals, "random", random.clone())?;
    set_global_silent(lua, globals, "randomseed", randomseed.clone())?;
    if let Ok(math) = globals.get::<_, Table>("math") {
        math.set("random", random)?;
        math.set("randomseed", randomseed)?;
    }

    Ok(())
}

fn install_fallback_globals<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    fallbacks: Rc<RefCell<LegacyFallbacks>>,
    context: Rc<RefCell<EngineContext>>,
) -> LuaResult<()> {
    let setfallback_state = fallbacks.clone();
    let setfallback_ctx = context.clone();
    let setfallback =
        lua.create_function(move |lua_ctx, (event, handler): (String, Function)| {
            let event = event.to_ascii_lowercase();
            if !setfallback_state.borrow().is_known_event(&event)
                && setfallback_ctx.borrow().verbose()
            {
                eprintln!("[lua][setfallback] installing stubbed handler for {event}");
            }
            let previous = setfallback_state.borrow_mut().set_fallback_for_all(
                lua_ctx,
                &event,
                handler.clone(),
            )?;
            let values = value_fields_from_lua(&Value::Function(handler.clone()));
            let handle = ptr_to_handle(handler.to_pointer());
            log_set_fallback(&event, handle, values, Some(handler.to_pointer()));
            Ok(previous.map(Value::Function).unwrap_or(Value::Nil))
        })?;
    set_global_silent(lua, globals, "setfallback", setfallback)?;

    let gettag_state = fallbacks.clone();
    let gettagmethod = lua.create_function(
        move |lua_ctx, (tag, event): (Value, String)| -> LuaResult<Value> {
            let tag = LegacyFallbacks::parse_tag(tag);
            let event = event.to_ascii_lowercase();
            let method = gettag_state.borrow().get_tag_method(lua_ctx, tag, &event)?;
            Ok(method.map(Value::Function).unwrap_or(Value::Nil))
        },
    )?;
    set_global_silent(lua, globals, "gettagmethod", gettagmethod)?;

    let settag_state = fallbacks.clone();
    let settagmethod = lua.create_function(
        move |lua_ctx, (tag, event, handler): (Value, String, Function)| -> LuaResult<Value> {
            let tag = LegacyFallbacks::parse_tag(tag);
            let event = event.to_ascii_lowercase();
            let previous =
                settag_state
                    .borrow_mut()
                    .set_tag_method(lua_ctx, tag, &event, handler.clone())?;
            if tag == LegacyFallbacks::TAG_NIL {
                settag_state
                    .borrow_mut()
                    .set_fallback_for_tag(lua_ctx, &event, handler)?;
            }
            Ok(previous.map(Value::Function).unwrap_or(Value::Nil))
        },
    )?;
    set_global_silent(lua, globals, "settagmethod", settagmethod)?;

    let seterror_state = fallbacks.clone();
    let seterrormethod =
        lua.create_function(move |lua_ctx, handler: Function| -> LuaResult<Value> {
            let previous = seterror_state.borrow_mut().set_fallback_for_all(
                lua_ctx,
                "error",
                handler.clone(),
            )?;
            let values = value_fields_from_lua(&Value::Function(handler.clone()));
            let handle = ptr_to_handle(handler.to_pointer());
            log_set_fallback("error", handle, values, Some(handler.to_pointer()));
            Ok(previous.map(Value::Function).unwrap_or(Value::Nil))
        })?;
    set_global_silent(lua, globals, "seterrormethod", seterrormethod)?;

    let tag =
        lua.create_function(|_, value: Value| Ok(LegacyFallbacks::tag_id_for_value(&value)))?;
    set_global_silent(lua, globals, "tag", tag)?;

    let refs = RegistryRefs::new();
    let refs_state = refs.clone();
    let lua_ref = lua.create_function(move |lua_ctx, value: Value| -> LuaResult<i32> {
        let reference = refs_state.next_handle();
        let preferred_handle = handle_from_value(&value).filter(|h| h != "0x00000000");
        let fabricated = normalize_handle(
            "handle",
            Some(
                preferred_handle
                    .clone()
                    .unwrap_or_else(|| format!("0x{:08x}", next_fabricated_handle().raw as u32)),
            ),
        );
        let handle = preferred_handle.unwrap_or(fabricated);
        let label = format!("handle={handle}");
        let entry = store_registry_value(
            lua_ctx,
            value,
            1,
            Some(reference),
            Some(label.clone()),
            Some(handle.clone()),
            Some(handle),
        )?;
        refs_state.store(entry);
        Ok(reference)
    })?;
    set_global_silent(lua, globals, "lua_ref", lua_ref)?;

    let refs_state = refs.clone();
    let lua_unref = lua.create_function(move |_, handle: i32| {
        refs_state.remove(handle);
        log_unref(handle, None);
        Ok(())
    })?;
    set_global_silent(lua, globals, "lua_unref", lua_unref)?;

    let refs_state = refs.clone();
    let lua_getref = lua.create_function(move |lua_ctx, handle: i32| -> LuaResult<Value> {
        let value = refs_state.resolve_value(lua_ctx, handle, |value| match value {
            Value::Function(func) => origin_fields_for_ptr(func.to_pointer()),
            _ => OriginFields::default(),
        })?;
        if let Some(value) = value {
            Ok(value)
        } else {
            log_fetch_ref(
                handle,
                None,
                None,
                Some("missing_ref".to_string()),
                OriginFields::default(),
            );
            Ok(Value::Nil)
        }
    })?;
    set_global_silent(lua, globals, "lua_getref", lua_getref)?;

    Ok(())
}

fn install_index_hook(
    lua: &Lua,
    globals: &Table,
    fallbacks: Rc<RefCell<LegacyFallbacks>>,
    verbose: bool,
) -> LuaResult<()> {
    let globals_ptr = globals.to_pointer();
    register_table_label(globals_ptr, "global:_G");
    // Prevent the global reentrancy flag from triggering the index fallback when unset.
    globals.set("indexFB_disabled", false)?;
    let index_state = fallbacks.clone();
    let guard_state = if verbose {
        Some(Rc::new(RefCell::new(IndexGuard::default())))
    } else {
        None
    };
    let index_fb = lua.create_function(move |lua_ctx, (table, key): (Value, Value)| {
        let key_label = describe_value(&key);
        let mut inserted_entry: Option<(usize, String)> = None;
        if let Some(state) = guard_state.as_ref() {
            let mut guard = state.borrow_mut();
            guard.depth += 1;
            let depth = guard.depth;
            if let Value::Table(t) = &table {
                let ptr = t.to_pointer() as usize;
                let entry = (ptr, key_label.clone());
                if guard.seen.contains(&entry) {
                    let mut event = EventBuilder::new("lua_index_recursion")
                        .kv("table", ptr_to_handle(t.to_pointer()))
                        .kv("key", key_label.clone())
                        .kv("depth", depth as i64)
                        .kv("reason", "cycle");
                    if let Some(label) = table_label(t.to_pointer()) {
                        event = event.kv("label", label);
                    }
                    log_event(event);
                    guard.depth -= 1;
                    return Err(LuaError::RuntimeError(
                        "index fallback parent cycle detected".to_string(),
                    ));
                }
                guard.seen.insert(entry.clone());
                inserted_entry = Some(entry);
            }
            if depth > INDEX_RECURSION_LIMIT {
                let (table_handle, label) = match &table {
                    Value::Table(t) => (ptr_to_handle(t.to_pointer()), table_label(t.to_pointer())),
                    other => (describe_value(other), None),
                };
                let mut event = EventBuilder::new("lua_index_recursion")
                    .kv("table", table_handle)
                    .kv("key", key_label.clone())
                    .kv("depth", depth as i64)
                    .kv("reason", "depth_limit");
                if let Some(label) = label {
                    event = event.kv("label", label);
                }
                log_event(event);
                guard.depth -= 1;
                if let Some(entry) = inserted_entry {
                    guard.seen.remove(&entry);
                }
                return Err(LuaError::RuntimeError(format!(
                    "index fallback recursion depth exceeded ({INDEX_RECURSION_LIMIT})"
                )));
            }
        }

        // Avoid routing globals through the index fallback to prevent recursion
        // when scripts probe table.parent, but fall back to the index handler
        // when no getglobal override is present.
        let use_getglobal = matches!(&table, Value::Table(t) if t.to_pointer() == globals_ptr);
        let handler = {
            let state = index_state.borrow();
            let event = if use_getglobal && state.fallbacks.contains_key("getglobal") {
                "getglobal"
            } else {
                "index"
            };
            state.handler_for_event(lua_ctx, event)?
        };
        let result = if let Some(func) = handler {
            func.call::<_, Value>((table, key))
        } else {
            Ok(Value::Nil)
        };

        if let Some(state) = guard_state.as_ref() {
            let mut guard = state.borrow_mut();
            if let Some(entry) = inserted_entry {
                guard.seen.remove(&entry);
            }
            if guard.depth > 0 {
                guard.depth -= 1;
            }
        }
        result
    })?;

    let metatable = match globals.get_metatable() {
        Some(table) => table,
        None => lua.create_table()?,
    };
    metatable.set("__index", index_fb)?;
    globals.set_metatable(Some(metatable));
    Ok(())
}

fn install_error_wrapper<'lua>(
    lua: &'lua Lua,
    globals: &Table<'lua>,
    fallbacks: Rc<RefCell<LegacyFallbacks>>,
) -> LuaResult<()> {
    let original_error: Function = globals.get("error")?;
    let original_error_key = lua.create_registry_value(original_error)?;
    let error_state = fallbacks.clone();
    let wrapped_error = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        if let Some(handler) = error_state.borrow().handler_for_event(lua_ctx, "error")? {
            let _ = handler.call::<_, Value>(args.clone());
        }
        let call_error: Function = lua_ctx.registry_value(&original_error_key)?;
        call_error.call::<_, Value>(args)
    })?;
    set_global_silent(lua, globals, "error", wrapped_error)?;
    Ok(())
}

struct LegacyFallbacks {
    defaults: HashMap<String, RegistryKey>,
    fallbacks: HashMap<String, RegistryKey>,
    tag_methods: HashMap<i64, HashMap<String, RegistryKey>>,
}

#[derive(Default)]
struct IndexGuard {
    depth: usize,
    seen: HashSet<(usize, String)>,
}

const INDEX_RECURSION_LIMIT: usize = 64;

impl LegacyFallbacks {
    const TAG_NIL: i64 = -1;
    const TAG_BOOLEAN: i64 = -2;
    const TAG_NUMBER: i64 = 0;
    const TAG_STRING: i64 = 1;
    const TAG_TABLE: i64 = 2;
    const TAG_FUNCTION: i64 = 3;
    const TAG_THREAD: i64 = 4;
    const TAG_USERDATA: i64 = 5;
    const TAG_LIGHTUSERDATA: i64 = 6;
    const TAG_ERROR: i64 = 7;

    fn new(lua: &Lua) -> LuaResult<Self> {
        let mut state = Self {
            defaults: HashMap::new(),
            fallbacks: HashMap::new(),
            tag_methods: HashMap::new(),
        };

        state.install_default(
            lua,
            "gettable",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "indexed expression not a table".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "settable",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "indexed expression not a table".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "index",
            lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
        )?;
        state.install_default(
            lua,
            "getglobal",
            lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
        )?;
        state.install_default(
            lua,
            "arith",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "number expected in arithmetic operation".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "order",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "incompatible types in comparison".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "concat",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "string expected in concatenation".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "gc",
            lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?,
        )?;
        state.install_default(
            lua,
            "function",
            lua.create_function(|_, _: Variadic<Value>| -> LuaResult<Value> {
                Err(LuaError::RuntimeError(
                    "called expression not a function".to_string(),
                ))
            })?,
        )?;
        state.install_default(
            lua,
            "error",
            lua.create_function(|_, args: Variadic<Value>| {
                if let Some(Value::String(message)) = args.first() {
                    eprintln!("[lua][error] {}", message.to_str()?);
                }
                Ok(Value::Nil)
            })?,
        )?;

        Ok(state)
    }

    fn install_default(&mut self, lua: &Lua, event: &str, func: Function) -> LuaResult<()> {
        let key = lua.create_registry_value(func)?;
        self.defaults.insert(event.to_string(), key);
        Ok(())
    }

    fn is_known_event(&self, event: &str) -> bool {
        self.defaults.contains_key(event)
    }

    fn handler_for_event<'lua>(
        &self,
        lua: &'lua Lua,
        event: &str,
    ) -> LuaResult<Option<Function<'lua>>> {
        if let Some(key) = self.fallbacks.get(event) {
            return lua.registry_value(key).map(Some);
        }
        if let Some(key) = self.defaults.get(event) {
            return lua.registry_value(key).map(Some);
        }
        Ok(None)
    }

    fn set_fallback_for_all<'lua>(
        &mut self,
        lua: &'lua Lua,
        event: &str,
        handler: Function<'lua>,
    ) -> LuaResult<Option<Function<'lua>>> {
        let previous = self.get_tag_method(lua, Self::TAG_NIL, event)?;
        let key = lua.create_registry_value(handler.clone())?;
        self.fallbacks.insert(event.to_string(), key);

        for tag in Self::default_tags() {
            let func = handler.clone();
            self.set_tag_method(lua, tag, event, func)?;
        }

        Ok(previous)
    }

    fn set_fallback_for_tag<'lua>(
        &mut self,
        lua: &'lua Lua,
        event: &str,
        handler: Function<'lua>,
    ) -> LuaResult<()> {
        let key = lua.create_registry_value(handler)?;
        self.fallbacks.insert(event.to_string(), key);
        Ok(())
    }

    fn set_tag_method<'lua>(
        &mut self,
        lua: &'lua Lua,
        tag: i64,
        event: &str,
        handler: Function<'lua>,
    ) -> LuaResult<Option<Function<'lua>>> {
        let previous = self.get_tag_method(lua, tag, event)?;
        let handle = ptr_to_handle(handler.to_pointer());
        let mut values = value_fields_from_lua(&Value::Function(handler.clone()));
        values.tag = Some(tag as i32);
        let origin = origin_fields_for_ptr(handler.to_pointer());
        let key = lua.create_registry_value(handler)?;
        self.tag_methods
            .entry(tag)
            .or_default()
            .insert(event.to_string(), key);
        log_set_tagmethod(tag, event, Some(handle), values, origin);
        Ok(previous)
    }

    fn get_tag_method<'lua>(
        &self,
        lua: &'lua Lua,
        tag: i64,
        event: &str,
    ) -> LuaResult<Option<Function<'lua>>> {
        if let Some(methods) = self.tag_methods.get(&tag) {
            if let Some(key) = methods.get(event) {
                return lua.registry_value(key).map(Some);
            }
        }
        self.handler_for_event(lua, event)
    }

    fn parse_tag(value: Value) -> i64 {
        match value {
            Value::Integer(id) => id,
            Value::Number(id) => id.trunc() as i64,
            other => Self::tag_id_for_value(&other),
        }
    }

    fn tag_id_for_value(value: &Value) -> i64 {
        match value {
            Value::Nil => Self::TAG_NIL,
            Value::Boolean(_) => Self::TAG_BOOLEAN,
            Value::Integer(_) | Value::Number(_) => Self::TAG_NUMBER,
            Value::String(_) => Self::TAG_STRING,
            Value::Table(_) => Self::TAG_TABLE,
            Value::Function(_) => Self::TAG_FUNCTION,
            Value::Thread(_) => Self::TAG_THREAD,
            Value::UserData(data) => data
                .borrow::<TaggedHandle>()
                .map(|handle| handle.tag as i64)
                .unwrap_or(Self::TAG_USERDATA),
            Value::LightUserData(_) => Self::TAG_LIGHTUSERDATA,
            Value::Error(_) => Self::TAG_ERROR,
        }
    }

    fn default_tags() -> Vec<i64> {
        // Mirror retail's setfallback.lua ordering: 0, tag(0), tag(""), tag({}),
        // tag(function() end), tag(settagmethod), tag(nil).
        vec![0, -1, -2, -3, -4, -5, -7]
    }
}

#[derive(Clone)]
struct RegistryRefs {
    entries: Rc<RefCell<HashMap<i32, RegistryRef>>>,
    next: Rc<RefCell<i32>>,
}

impl RegistryRefs {
    fn new() -> Self {
        Self {
            entries: Rc::new(RefCell::new(HashMap::new())),
            next: Rc::new(RefCell::new(2)),
        }
    }

    fn next_handle(&self) -> i32 {
        let mut counter = self.next.borrow_mut();
        let handle = *counter;
        *counter = counter.wrapping_add(1).max(1);
        handle
    }

    fn store(&self, entry: RegistryRef) {
        self.entries.borrow_mut().insert(entry.reference, entry);
    }

    fn remove(&self, handle: i32) {
        self.entries.borrow_mut().remove(&handle);
    }

    fn resolve_value<'lua>(
        &self,
        lua: &'lua Lua,
        handle: i32,
        origin_fn: impl FnOnce(&Value) -> OriginFields,
    ) -> LuaResult<Option<Value<'lua>>> {
        let entries = self.entries.borrow();
        if let Some(entry) = entries.get(&handle) {
            let value: Value = lua.registry_value(&entry.key)?;
            let origin = origin_fn(&value);
            entry.log_fetch(origin, None);
            return Ok(Some(value));
        }
        Ok(None)
    }
}
