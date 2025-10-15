use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use mlua::{Lua, Value, Variadic};

use super::{strip_self, value_to_bool, EngineContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PauseLabel {
    Pause,
    Resume,
}

impl PauseLabel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PauseLabel::Pause => "pause",
            PauseLabel::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PauseEvent {
    pub(super) label: PauseLabel,
    pub(super) active: bool,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PauseState {
    pub(super) active: bool,
    pub(super) history: Vec<PauseEvent>,
}

impl PauseState {
    pub(crate) fn record(&mut self, label: PauseLabel, active: bool) {
        self.history.push(PauseEvent { label, active });
        self.active = active;
    }
}

/// Read-only access to pause state for callers that only need inspection.
pub(super) struct PauseRuntimeView<'a> {
    state: &'a PauseState,
}

impl<'a> PauseRuntimeView<'a> {
    pub(super) fn new(state: &'a PauseState) -> Self {
        Self { state }
    }

    pub(super) fn active(&self) -> bool {
        self.state.active
    }

    #[cfg(test)]
    pub(super) fn history(&self) -> &'a [PauseEvent] {
        &self.state.history
    }
}

pub(crate) fn install_game_pauser(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let game_pauser = lua.create_table()?;

    let pause_context = context.clone();
    game_pauser.set(
        "pause",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let active = values.get(0).map(value_to_bool).unwrap_or(false);
            pause_context
                .borrow_mut()
                .handle_pause_request(PauseLabel::Pause, active);
            Ok(())
        })?,
    )?;

    let resume_context = context;
    game_pauser.set(
        "resume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let active = values.get(0).map(value_to_bool).unwrap_or(false);
            resume_context
                .borrow_mut()
                .handle_pause_request(PauseLabel::Resume, active);
            Ok(())
        })?,
    )?;

    globals.set("game_pauser", game_pauser)?;
    Ok(())
}
