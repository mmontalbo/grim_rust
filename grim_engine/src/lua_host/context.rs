mod bindings;
mod scripts;

pub(super) use bindings::{
    call_boot, install_globals_post_system, install_globals_pre_system, install_package_path,
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
    ref_registry: RefRegistry,
}

impl EngineContext {
    pub(super) fn new(verbose: bool, headless: bool) -> Self {
        Self {
            verbose,
            headless,
            scripts: ScriptRuntime::new(),
            events: Vec::new(),
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
}
