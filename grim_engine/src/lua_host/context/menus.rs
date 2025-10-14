use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use anyhow::Result;
use mlua::{Error as LuaError, Function, Lua, Table, Value, Variadic};

use super::{
    describe_value, install_render_helpers, split_self, strip_self, value_to_bool, value_to_string,
    EngineContext,
};

#[derive(Debug, Default, Clone)]
pub(super) struct MenuState {
    pub(super) visible: bool,
    pub(super) auto_freeze: bool,
    pub(super) last_run_mode: Option<String>,
    pub(super) last_action: Option<String>,
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

    #[cfg(test)]
    pub(super) fn get(&self, name: &str) -> Option<&Rc<RefCell<MenuState>>> {
        self.states.get(name)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &Rc<RefCell<MenuState>>)> {
        self.states.iter()
    }
}

pub(super) fn install_menu_infrastructure(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
    install_menu_constants(lua)?;
    install_render_helpers(lua, context.clone())?;
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

    let loading_state = {
        let mut ctx = context.borrow_mut();
        ctx.ensure_menu_state("loading")
    };

    let run_context = context.clone();
    let run_state = loading_state.clone();
    let run = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, values) = split_self(args);
        if let Some(table) = self_table {
            let auto_freeze = values.get(0).map(value_to_bool).unwrap_or(false);
            table.set("autoFreeze", auto_freeze)?;

            if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
                if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                    pause_fn.call::<_, ()>((game_pauser.clone(), true))?;
                }
            }

            if let Ok(show_fn) = table.get::<_, Function>("show") {
                show_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", true)?;
            }

            if auto_freeze {
                if let Ok(single_start) =
                    lua_ctx.globals().get::<_, Function>("single_start_script")
                {
                    let freeze_fn: Function = table.get("freeze")?;
                    single_start.call::<_, u32>((freeze_fn, table.clone()))?;
                }
            }

            {
                let mut state = run_state.borrow_mut();
                state.auto_freeze = auto_freeze;
                state.last_run_mode = Some(if auto_freeze {
                    "auto".to_string()
                } else {
                    "manual".to_string()
                });
                state.visible = true;
                state.last_action = Some("run".to_string());
            }

            run_context.borrow_mut().log_event(format!(
                "loading_menu.run {}",
                if auto_freeze { "auto" } else { "manual" }
            ));
        }
        Ok(())
    })?;
    menu.set("run", run)?;

    let freeze_context = context.clone();
    let freeze_state = loading_state.clone();
    let freeze = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            if let Ok(hide_fn) = table.get::<_, Function>("hide") {
                hide_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", false)?;
            }
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
            }
        }

        if let Ok(set_mode) = lua_ctx.globals().get::<_, Function>("SetGameRenderMode") {
            set_mode.call::<_, ()>(("exit",))?;
        }

        {
            let mut state = freeze_state.borrow_mut();
            state.visible = false;
            state.last_action = Some("freeze".to_string());
        }

        freeze_context.borrow_mut().log_event("loading_menu.freeze");
        Ok(())
    })?;
    menu.set("freeze", freeze)?;

    let close_context = context.clone();
    let close_state = loading_state.clone();
    let close = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            if let Ok(hide_fn) = table.get::<_, Function>("hide") {
                hide_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", false)?;
            }
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
            }
        }

        {
            let mut state = close_state.borrow_mut();
            state.visible = false;
            state.last_action = Some("close".to_string());
        }

        close_context.borrow_mut().log_event("loading_menu.close");
        Ok(())
    })?;
    menu.set("close", close)?;

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

    let boot_state = {
        let mut ctx = context.borrow_mut();
        ctx.ensure_menu_state("boot_warning")
    };

    let run_context = context.clone();
    let run_state = boot_state.clone();
    let run = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            table.set("is_visible", true)?;
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), true))?;
            }
        }

        {
            let mut state = run_state.borrow_mut();
            state.visible = true;
            state.last_action = Some("run".to_string());
        }

        run_context.borrow_mut().log_event("boot_warning_menu.run");
        Ok(())
    })?;
    menu.set("run", run)?;

    let close_context = context.clone();
    let close_state = boot_state.clone();
    let close = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            table.set("is_visible", false)?;
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
            }
        }

        {
            let mut state = close_state.borrow_mut();
            state.visible = false;
            state.last_action = Some("close".to_string());
        }

        close_context
            .borrow_mut()
            .log_event("boot_warning_menu.close");
        Ok(())
    })?;
    menu.set("close", close)?;

    let check_context = context.clone();
    let check_state = boot_state.clone();
    let check = lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            if let Ok(close_fn) = table.get::<_, Function>("close") {
                close_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", false)?;
            }
        }
        {
            let mut state = check_state.borrow_mut();
            state.last_action = Some("check_timeout".to_string());
        }
        check_context
            .borrow_mut()
            .log_event("boot_warning_menu.check_timeout");
        Ok(())
    })?;
    menu.set("check_timeout", check)?;

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

    let menu_table = lua.create_table()?;
    menu_table.set("name", state_name)?;
    menu_table.set("is_visible", false)?;
    menu_table.set("autoFreeze", false)?;

    let menu_state = {
        let mut ctx = context.borrow_mut();
        let handle = ctx.ensure_menu_state(state_name);
        {
            let mut guard = handle.borrow_mut();
            guard.visible = false;
            guard.auto_freeze = false;
            guard.last_action = Some("create".to_string());
        }
        ctx.log_event(format!("{global_name}.create"));
        handle
    };

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;

    let show_state = menu_state.clone();
    let show_context = context.clone();
    let show_label = global_name.to_string();
    let show = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            table.set("is_visible", true)?;
        }
        let should_pause = {
            let mut guard = show_state.borrow_mut();
            guard.visible = true;
            guard.last_action = Some("show".to_string());
            guard.auto_freeze
        };
        if should_pause {
            if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
                if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                    pause_fn.call::<_, ()>((game_pauser.clone(), true))?;
                }
            }
        }
        show_context
            .borrow_mut()
            .log_event(format!("{show_label}.show"));
        Ok(())
    })?;
    menu_table.set("show", show.clone())?;

    let hide_state = menu_state.clone();
    let hide_context = context.clone();
    let hide_label = global_name.to_string();
    let hide = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            table.set("is_visible", false)?;
        }
        let should_unpause = {
            let mut guard = hide_state.borrow_mut();
            guard.visible = false;
            guard.last_action = Some("hide".to_string());
            guard.auto_freeze
        };
        if should_unpause {
            if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
                if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                    pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
                }
            }
        }
        hide_context
            .borrow_mut()
            .log_event(format!("{hide_label}.hide"));
        Ok(())
    })?;
    menu_table.set("hide", hide.clone())?;

    let auto_state = menu_state.clone();
    let auto_context = context.clone();
    let auto_label = global_name.to_string();
    let auto_freeze = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, values) = split_self(args);
        let desired = values.get(0).map(value_to_bool).unwrap_or(false);
        if let Some(table) = self_table {
            table.set("autoFreeze", desired)?;
        }

        let (was_visible, previous_auto) = {
            let guard = auto_state.borrow();
            (guard.visible, guard.auto_freeze)
        };

        {
            let mut guard = auto_state.borrow_mut();
            guard.auto_freeze = desired;
            guard.last_action = Some("auto_freeze".to_string());
        }

        if was_visible && previous_auto != desired {
            if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
                if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                    pause_fn.call::<_, ()>((game_pauser.clone(), desired))?;
                }
            }
        }

        auto_context.borrow_mut().log_event(format!(
            "{auto_label}.auto_freeze {}",
            if desired { "on" } else { "off" }
        ));
        Ok(())
    })?;
    menu_table.set("auto_freeze", auto_freeze.clone())?;
    menu_table.set("set_auto_freeze", auto_freeze.clone())?;
    menu_table.set("setAutoFreeze", auto_freeze)?;

    menu_table.set("show_menu", show.clone())?;
    menu_table.set("open", show)?;

    menu_table.set("close", hide)?;
    menu_table.set("cleanup", noop.clone())?;
    menu_table.set("destroy", noop.clone())?;
    menu_table.set("refresh", noop.clone())?;
    menu_table.set("add_image", noop.clone())?;
    menu_table.set("add_line", noop.clone())?;
    menu_table.set("add_button", noop.clone())?;
    menu_table.set("add_slider", noop.clone())?;
    menu_table.set("add_toggle", noop.clone())?;
    menu_table.set("setup", noop.clone())?;

    let fallback_context = context.clone();
    let fallback_label = global_name.to_string();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("{fallback_label}.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;

    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    menu_table.set_metatable(Some(metatable));

    globals.set(global_name, menu_table)?;
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
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("dialog.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    dialog.set_metatable(Some(metatable));

    globals.set("dialog", dialog.clone())?;

    if matches!(globals.get::<_, Value>("Sentence"), Ok(Value::Nil) | Err(_)) {
        let sentence_context = context.clone();
        let noop = lua.create_function(move |_, _: Variadic<Value>| {
            sentence_context
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

pub(super) fn install_game_pauser(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let game_pauser = lua.create_table()?;

    let pause_context = context.clone();
    game_pauser.set(
        "pause",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let active = values.get(0).map(value_to_bool).unwrap_or(false);
            pause_context.borrow_mut().log_event(format!(
                "game_pauser.pause {}",
                if active { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;

    let resume_context = context.clone();
    game_pauser.set(
        "resume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let active = values.get(0).map(value_to_bool).unwrap_or(false);
            resume_context.borrow_mut().log_event(format!(
                "game_pauser.resume {}",
                if active { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;

    globals.set("game_pauser", game_pauser)?;
    Ok(())
}

fn install_game_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let game_menu = lua.create_table()?;
    let menu_context = context.clone();
    game_menu.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let values = strip_self(args);
            let name = values
                .get(0)
                .and_then(value_to_string)
                .or_else(|| Some("menu".to_string()));
            build_menu_instance(lua_ctx, menu_context.clone(), name).map_err(LuaError::external)
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

    let menu = lua.create_table()?;
    menu.set("items", lua.create_table()?)?;
    saveload.set("menu", menu)?;

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;

    let run_context = context.clone();
    saveload.set(
        "run",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut iter = args.into_iter();
            let _self = iter.next();
            let mode = iter
                .next()
                .as_ref()
                .map(describe_value)
                .unwrap_or_else(|| "<nil>".to_string());
            run_context
                .borrow_mut()
                .log_event(format!("saveload_menu.run {mode}"));
            Ok(())
        })?,
    )?;

    let build_context = context.clone();
    saveload.set(
        "build_menu",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let mut iter = args.into_iter();
            let self_table = match iter.next() {
                Some(Value::Table(table)) => table,
                _ => return Ok(()),
            };

            let exit_index: i64 = self_table.get("exit_index").unwrap_or(1);
            let menu: Table = match self_table.get("menu") {
                Ok(table) => table,
                Err(_) => {
                    let table = lua_ctx.create_table()?;
                    table.set("items", lua_ctx.create_table()?)?;
                    self_table.set("menu", table.clone())?;
                    table
                }
            };

            let items: Table = match menu.get("items") {
                Ok(table) => table,
                Err(_) => {
                    let table = lua_ctx.create_table()?;
                    menu.set("items", table.clone())?;
                    table
                }
            };

            let item_table: Table = match items.get(exit_index) {
                Ok(Value::Table(table)) => table,
                _ => {
                    let table = lua_ctx.create_table()?;
                    items.set(exit_index, table.clone())?;
                    table
                }
            };

            let mut mode = None;
            if let Some(value) = iter.next() {
                if let Value::Table(settings) = value {
                    if let Ok(Value::String(text)) = settings.get("mode") {
                        mode.replace(text.to_str()?.to_string());
                    }
                }
            }
            if let Some(setting) = mode {
                item_table.set("mode", setting.clone())?;
                build_context
                    .borrow_mut()
                    .log_event(format!("saveload_menu.build_menu {setting}"));
            } else {
                build_context
                    .borrow_mut()
                    .log_event("saveload_menu.build_menu".to_string());
            }
            Ok(())
        })?,
    )?;

    saveload.set("cleanup", noop.clone())?;
    saveload.set("destroy", noop.clone())?;
    saveload.set("add_item", noop.clone())?;
    saveload.set("add_button", noop)?;

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

    let state = {
        let mut ctx = context.borrow_mut();
        ctx.ensure_menu_state(&label)
    };

    let show_state = state.clone();
    let show_context = context.clone();
    let show_label = label.clone();
    menu.set(
        "show",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", true)?;
            }
            {
                let mut menu_state = show_state.borrow_mut();
                menu_state.visible = true;
                menu_state.last_action = Some("show".to_string());
            }
            show_context
                .borrow_mut()
                .log_event(format!("menu.show {show_label}"));
            Ok(())
        })?,
    )?;

    let hide_state = state.clone();
    let hide_context = context.clone();
    let hide_label = label.clone();
    menu.set(
        "hide",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", false)?;
            }
            {
                let mut menu_state = hide_state.borrow_mut();
                menu_state.visible = false;
                menu_state.last_action = Some("hide".to_string());
            }
            hide_context
                .borrow_mut()
                .log_event(format!("menu.hide {hide_label}"));
            Ok(())
        })?,
    )?;

    let freeze_state = state.clone();
    let freeze_context = context.clone();
    let freeze_label = label.clone();
    menu.set(
        "freeze",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, _values) = split_self(args);
            {
                let mut menu_state = freeze_state.borrow_mut();
                menu_state.last_action = Some("freeze".to_string());
            }
            freeze_context
                .borrow_mut()
                .log_event(format!("menu.freeze {freeze_label}"));
            Ok(())
        })?,
    )?;

    let close_state = state.clone();
    let close_context = context.clone();
    let close_label = label.clone();
    menu.set(
        "close",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", false)?;
            }
            {
                let mut menu_state = close_state.borrow_mut();
                menu_state.visible = false;
                menu_state.last_action = Some("close".to_string());
            }
            close_context
                .borrow_mut()
                .log_event(format!("menu.close {close_label}"));
            Ok(())
        })?,
    )?;

    let cleanup_state = state.clone();
    let cleanup_context = context.clone();
    let cleanup_label = label.clone();
    menu.set(
        "cleanup",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, _values) = split_self(args);
            {
                let mut menu_state = cleanup_state.borrow_mut();
                menu_state.last_action = Some("cleanup".to_string());
            }
            cleanup_context
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
    menu.set("autoFreeze", noop.clone())?;

    let fallback = {
        let fallback_context = context.clone();
        let fallback_label = label.clone();
        lua_ctx.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
            if let Value::String(method) = key {
                let method_name = method.to_str()?.to_string();
                fallback_context
                    .borrow_mut()
                    .log_event(format!("menu.stub {fallback_label}.{method_name}"));
            }
            let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
            Ok(Value::Function(noop))
        })?
    };

    let metatable = lua_ctx.create_table()?;
    metatable.set("__index", fallback)?;
    menu.set_metatable(Some(metatable));

    Ok(menu)
}
