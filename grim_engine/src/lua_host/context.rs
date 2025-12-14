mod bindings;
mod scripts;

pub(super) use bindings::{
    call_boot, drive_active_scripts, dump_runtime_summary, ensure_intro_cutscene,
    install_globals_post_system, install_globals_pre_system, install_package_path,
    load_system_script, override_boot_stubs,
};

use bindings::RefRegistry;
use grim_telemetry_schema::OriginFields;
use mlua::{FromLua, Lua, Result as LuaResult, Value};
use scripts::{ScriptRuntime, ScriptRuntimeAdapter};

pub(super) struct EngineContext {
    verbose: bool,
    headless: bool,
    scripts: ScriptRuntime,
    events: Vec<String>,
    active_movie: Option<ActiveMovie>,
    ref_registry: RefRegistry,
}

struct ActiveMovie {
    name: String,
    remaining_polls: u32,
}

pub(super) enum MovieStep {
    Idle,
    Active(String),
    Finished(String),
}

impl EngineContext {
    pub(super) fn new(verbose: bool, headless: bool) -> Self {
        Self {
            verbose,
            headless,
            scripts: ScriptRuntime::new(),
            events: Vec::new(),
            active_movie: None,
            ref_registry: RefRegistry::new(),
        }
    }

    pub(super) fn verbose(&self) -> bool {
        self.verbose
    }

    pub(super) fn log_event(&mut self, event: impl Into<String>) {
        let message = event.into();
        self.events.push(message.clone());
        if self.headless {
            eprintln!("[grim_engine] {message}");
        }
    }

    fn script_runtime(&mut self) -> ScriptRuntimeAdapter<'_> {
        ScriptRuntimeAdapter::new(&mut self.scripts, &mut self.events)
    }

    pub(super) fn start_script(&mut self, label: String) -> u32 {
        self.script_runtime().start_script(label)
    }

    pub(super) fn complete_script(&mut self, handle: u32) {
        self.script_runtime().complete_script(handle);
    }

    pub(super) fn events(&self) -> &[String] {
        &self.events
    }

    pub(super) fn alloc_ref(
        &mut self,
        lua: &Lua,
        value: Value,
        preferred_ref: Option<i32>,
        label: Option<String>,
        preferred_handle: Option<String>,
        handle_label: Option<String>,
    ) -> LuaResult<i32> {
        self.ref_registry.alloc_ref(
            lua,
            value,
            preferred_ref,
            label,
            preferred_handle,
            handle_label,
        )
    }

    pub(super) fn fetch_ref<'lua, T: FromLua<'lua>>(
        &self,
        lua: &'lua Lua,
        reference: i32,
        origin: OriginFields,
        note: Option<String>,
    ) -> LuaResult<Option<T>> {
        self.ref_registry.fetch_ref(lua, reference, origin, note)
    }

    pub(super) fn remove_ref(&mut self, reference: i32) -> bool {
        self.ref_registry.remove(reference)
    }

    pub(super) fn start_fullscreen_movie(&mut self, movie: String, yields: Option<u32>) -> bool {
        let remaining = yields.unwrap_or(3).max(1);
        self.active_movie = Some(ActiveMovie {
            name: movie.clone(),
            remaining_polls: remaining,
        });
        self.log_event(format!("movie.start {movie}"));
        true
    }

    pub(super) fn poll_fullscreen_movie(&mut self) -> bool {
        matches!(
            self.step_fullscreen_movie(),
            MovieStep::Active(_) | MovieStep::Finished(_)
        )
    }

    pub(super) fn request_cutscene_skip(&mut self) {
        self.log_event("movie.skip_requested");
    }

    pub(super) fn stop_fullscreen_movie(&mut self) {
        if let Some(active) = self.active_movie.take() {
            self.log_event(format!("movie.stop {}", active.name));
        }
    }

    pub(super) fn active_fullscreen_movie(&self) -> Option<String> {
        self.active_movie.as_ref().map(|movie| movie.name.clone())
    }

    pub(super) fn step_fullscreen_movie(&mut self) -> MovieStep {
        match self.active_movie.take() {
            None => MovieStep::Idle,
            Some(mut movie) => {
                if movie.remaining_polls > 0 {
                    movie.remaining_polls = movie.remaining_polls.saturating_sub(1);
                }
                if movie.remaining_polls == 0 {
                    let name = movie.name;
                    MovieStep::Finished(name)
                } else {
                    let name = movie.name.clone();
                    self.active_movie = Some(movie);
                    MovieStep::Active(name)
                }
            }
        }
    }
}
