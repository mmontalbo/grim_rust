use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use anyhow::Result;
use mlua::{Error as LuaError, Lua, Table, Value, Variadic};

use super::pause::install_game_pauser;
use super::{describe_value, split_self, value_to_bool, value_to_string, EngineContext};

#[derive(Debug, Default, Clone)]
pub(super) struct MenuState {
    pub(super) visible: bool,
    pub(super) auto_freeze: bool,
    pub(super) last_action: Option<String>,
    pub(super) last_run_mode: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct MenuRegistry {
    states: BTreeMap<String, Rc<RefCell<MenuState>>>,
}

impl MenuRegistry {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn ensure(&mut self, name: &str) -> Rc<RefCell<MenuState>> {
        self.states
            .entry(name.to_string())
            .or_insert_with(|| Rc::new(RefCell::new(MenuState::default())))
            .clone()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &Rc<RefCell<MenuState>>)> {
        self.states.iter()
    }
}

pub(super) struct MenuRegistryView<'a> {
    registry: &'a MenuRegistry,
}

impl<'a> MenuRegistryView<'a> {
    pub(super) fn new(registry: &'a MenuRegistry) -> Self {
        Self { registry }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    pub(super) fn iter(
        &self,
    ) -> impl Iterator<Item = (&'a String, &'a Rc<RefCell<MenuState>>)> + 'a {
        self.registry.iter()
    }
}

pub(super) fn install_menu_infrastructure(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    install_menu_constants(lua)?;
    install_game_pauser(lua, context.clone())?;
    install_game_menu(lua, context.clone())?;
    install_saveload_menu(lua, context)?;
    Ok(())
}

pub(super) fn install_loading_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("loading_menu"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let menu = build_menu_instance(lua, context.clone(), Some("loading".to_string()))?;
    menu.set("autoFreeze", false)?;

    let run_ctx = context.clone();
    menu.set(
        "run",
        lua.create_function(move |_, args: Variadic<Value>| {
            let auto = args.get(0).map(value_to_bool).unwrap_or(false);
            let mut guard = run_ctx.borrow_mut();
            let handle = guard.ensure_menu_state("loading");
            {
                let mut state = handle.borrow_mut();
                state.auto_freeze = auto;
                state.visible = true;
                state.last_run_mode = Some(if auto { "auto".into() } else { "manual".into() });
                state.last_action = Some("run".to_string());
            }
            guard.log_event(format!(
                "loading_menu.run {}",
                if auto { "auto" } else { "manual" }
            ));
            Ok(())
        })?,
    )?;

    let freeze_ctx = context.clone();
    menu.set(
        "freeze",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut guard = freeze_ctx.borrow_mut();
            let handle = guard.ensure_menu_state("loading");
            {
                let mut state = handle.borrow_mut();
                state.visible = false;
                state.last_action = Some("freeze".to_string());
            }
            guard.log_event("loading_menu.freeze");
            Ok(())
        })?,
    )?;

    let close_ctx = context.clone();
    menu.set(
        "close",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut guard = close_ctx.borrow_mut();
            let handle = guard.ensure_menu_state("loading");
            {
                let mut state = handle.borrow_mut();
                state.visible = false;
                state.last_action = Some("close".to_string());
            }
            guard.log_event("loading_menu.close");
            Ok(())
        })?,
    )?;

    globals.set("loading_menu", menu)?;
    Ok(())
}

pub(super) fn install_boot_warning_menu(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    let globals = lua.globals();
    if matches!(
        globals.get::<_, Value>("boot_warning_menu"),
        Ok(Value::Table(_))
    ) {
        return Ok(());
    }

    let menu = build_menu_instance(lua, context.clone(), Some("boot_warning".to_string()))?;

    let run_ctx = context.clone();
    menu.set(
        "run",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut guard = run_ctx.borrow_mut();
            let handle = guard.ensure_menu_state("boot_warning");
            {
                let mut state = handle.borrow_mut();
                state.visible = true;
                state.last_action = Some("run".to_string());
                state.last_run_mode = Some("manual".to_string());
            }
            guard.log_event("boot_warning_menu.run");
            Ok(())
        })?,
    )?;

    let close_ctx = context.clone();
    menu.set(
        "close",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut guard = close_ctx.borrow_mut();
            let handle = guard.ensure_menu_state("boot_warning");
            {
                let mut state = handle.borrow_mut();
                state.visible = false;
                state.last_action = Some("close".to_string());
            }
            guard.log_event("boot_warning_menu.close");
            Ok(())
        })?,
    )?;

    let check_ctx = context.clone();
    menu.set(
        "check_timeout",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut guard = check_ctx.borrow_mut();
            let handle = guard.ensure_menu_state("boot_warning");
            {
                let mut state = handle.borrow_mut();
                state.last_action = Some("check_timeout".to_string());
            }
            guard.log_event("boot_warning_menu.check_timeout");
            Ok(())
        })?,
    )?;

    globals.set("boot_warning_menu", menu)?;
    Ok(())
}

pub(super) fn install_stateful_menu(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    global_name: &str,
    state_name: &str,
) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>(global_name), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = build_menu_instance(lua, context.clone(), Some(state_name.to_string()))?;
    globals.set(global_name, table)?;
    Ok(())
}

pub(super) fn install_menu_dialog(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    install_stateful_menu(lua, context, "menu_dialog", "menu_dialog")
}

pub(super) fn install_menu_common(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    install_stateful_menu(lua, context, "menu_common", "menu_common")
}

pub(super) fn install_menu_remap(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    install_stateful_menu(lua, context, "menu_remap_keys", "menu_remap_keys")
}

pub(super) fn install_menu_prefs(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    install_stateful_menu(lua, context, "menu_prefs", "menu_prefs")
}

pub(super) fn install_dialog_scaffold(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("dialog"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let dialog = lua.create_table()?;
    let fallback_ctx = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_ctx
                .borrow_mut()
                .log_event(format!("dialog.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    dialog.set_metatable(Some(metatable));

    globals.set("dialog", dialog)?;

    if matches!(globals.get::<_, Value>("Sentence"), Ok(Value::Nil) | Err(_)) {
        let sentence_ctx = context.clone();
        let noop = lua.create_function(move |_, _: Variadic<Value>| {
            sentence_ctx
                .borrow_mut()
                .log_event("dialog.sentence".to_string());
            Ok(())
        })?;
        globals.set("Sentence", noop)?;
    }

    Ok(())
}

fn install_menu_constants(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("menu_ctor"), Ok(Value::Function(_))) {
        return Ok(());
    }

    let ctor = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;
    globals.set("menu_ctor", ctor)?;
    globals.set("createMenuWidget", lua.create_table()?)?;
    globals.set("createMenuLayout", lua.create_table()?)?;
    globals.set("LoadingMenuAllocator", lua.create_table()?)?;
    globals.set("MenuCommon", lua.create_table()?)?;
    Ok(())
}

fn install_game_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let game_menu = lua.create_table()?;
    let menu_ctx = context.clone();
    game_menu.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (_self, values) = split_self(args);
            let name = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "menu".to_string());
            build_menu_instance(lua_ctx, menu_ctx.clone(), Some(name))
                .map_err(LuaError::external)
        })?,
    )?;
    globals.set("game_menu", game_menu)?;
    Ok(())
}

fn install_saveload_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let saveload = lua.create_table()?;
    saveload.set("name", "SaveLoad")?;
    saveload.set("exit_index", 1)?;

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;

    let run_ctx = context.clone();
    saveload.set(
        "run",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mode = args
                .get(1)
                .map(describe_value)
                .unwrap_or_else(|| "<nil>".to_string());
            run_ctx
                .borrow_mut()
                .log_event(format!("saveload_menu.run {mode}"));
            Ok(())
        })?,
    )?;

    let build_ctx = context.clone();
    saveload.set(
        "build_menu",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mode = args
                .get(1)
                .map(describe_value)
                .unwrap_or_else(|| "<nil>".to_string());
            build_ctx
                .borrow_mut()
                .log_event(format!("saveload_menu.build_menu {mode}"));
            Ok(())
        })?,
    )?;

    saveload.set("cleanup", noop.clone())?;
    saveload.set("destroy", noop)?;
    globals.set("saveload_menu", saveload)?;
    Ok(())
}

pub(super) fn build_menu_instance<'lua>(
    lua_ctx: &'lua Lua,
    context: Rc<RefCell<EngineContext>>,
    name: Option<String>,
) -> Result<Table<'lua>> {
    let label = name.unwrap_or_else(|| "menu".to_string());
    let menu = lua_ctx.create_table()?;
    menu.set("name", label.clone())?;
    menu.set("is_visible", false)?;

    {
        let mut ctx = context.borrow_mut();
        ctx.log_event(format!("menu.create {label}"));
        let handle = ctx.ensure_menu_state(&label);
        let mut state = handle.borrow_mut();
        state.visible = false;
        state.last_action = Some("create".to_string());
    }

    let show_ctx = context.clone();
    let show_label = label.clone();
    menu.set(
        "show",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", true)?;
            }
            {
                let mut ctx = show_ctx.borrow_mut();
                let handle = ctx.ensure_menu_state(&show_label);
                let mut state = handle.borrow_mut();
                state.visible = true;
                state.last_action = Some("show".to_string());
                ctx.log_event(format!("menu.show {show_label}"));
            }
            Ok(())
        })?,
    )?;

    let hide_ctx = context.clone();
    let hide_label = label.clone();
    menu.set(
        "hide",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", false)?;
            }
            {
                let mut ctx = hide_ctx.borrow_mut();
                let handle = ctx.ensure_menu_state(&hide_label);
                let mut state = handle.borrow_mut();
                state.visible = false;
                state.last_action = Some("hide".to_string());
                ctx.log_event(format!("menu.hide {hide_label}"));
            }
            Ok(())
        })?,
    )?;

    let freeze_ctx = context.clone();
    let freeze_label = label.clone();
    menu.set(
        "freeze",
        lua_ctx.create_function(move |_, _: Variadic<Value>| {
            let mut ctx = freeze_ctx.borrow_mut();
            let handle = ctx.ensure_menu_state(&freeze_label);
            handle.borrow_mut().last_action = Some("freeze".to_string());
            ctx.log_event(format!("menu.freeze {freeze_label}"));
            Ok(())
        })?,
    )?;

    let close_ctx = context.clone();
    let close_label = label.clone();
    menu.set(
        "close",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", false)?;
            }
            {
                let mut ctx = close_ctx.borrow_mut();
                let handle = ctx.ensure_menu_state(&close_label);
                let mut state = handle.borrow_mut();
                state.visible = false;
                state.last_action = Some("close".to_string());
                ctx.log_event(format!("menu.close {close_label}"));
            }
            Ok(())
        })?,
    )?;

    let cleanup_ctx = context.clone();
    let cleanup_label = label.clone();
    menu.set(
        "cleanup",
        lua_ctx.create_function(move |_, _: Variadic<Value>| {
            cleanup_ctx
                .borrow_mut()
                .log_event(format!("menu.cleanup {cleanup_label}"));
            Ok(())
        })?,
    )?;

    let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
    menu.set("add_image", noop.clone())?;
    menu.set("add_line", noop.clone())?;
    menu.set("setup", noop.clone())?;
    menu.set("destroy", noop.clone())?;
    menu.set("cancel", noop.clone())?;
    menu.set("refresh", noop.clone())?;
    menu.set("add_button", noop.clone())?;
    menu.set("add_slider", noop.clone())?;
    menu.set("add_toggle", noop.clone())?;
    menu.set("autoFreeze", false)?;

    let fallback_ctx = context.clone();
    let fallback_label = label.clone();
    let fallback = lua_ctx.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_ctx
                .borrow_mut()
                .log_event(format!("menu.stub {fallback_label}.{}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;

    let metatable = lua_ctx.create_table()?;
    metatable.set("__index", fallback)?;
    menu.set_metatable(Some(metatable));

    Ok(menu)
}
