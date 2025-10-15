use std::collections::BTreeMap;

use mlua::RegistryKey;

#[derive(Debug)]
pub(super) struct ScriptRecord {
    label: String,
    thread: Option<RegistryKey>,
    yields: u32,
    callable: Option<RegistryKey>,
}

#[derive(Debug, Default)]
pub(super) struct ScriptCleanup {
    pub(super) thread: Option<RegistryKey>,
    pub(super) callable: Option<RegistryKey>,
}

#[derive(Debug)]
pub(super) struct ScriptRuntime {
    next_handle: u32,
    records: BTreeMap<u32, ScriptRecord>,
}

impl ScriptRuntime {
    pub(super) fn new() -> Self {
        ScriptRuntime {
            next_handle: 1,
            records: BTreeMap::new(),
        }
    }

    pub(super) fn start_script(
        &mut self,
        label: String,
        callable: Option<RegistryKey>,
    ) -> (u32, String) {
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1);
        self.records.insert(
            handle,
            ScriptRecord {
                label: label.clone(),
                thread: None,
                yields: 0,
                callable,
            },
        );
        (handle, format!("script.start {label} (#{handle})"))
    }

    pub(super) fn has_label(&self, label: &str) -> bool {
        self.records.values().any(|record| record.label == label)
    }

    pub(super) fn attach_thread(&mut self, handle: u32, key: RegistryKey) {
        if let Some(record) = self.records.get_mut(&handle) {
            record.thread = Some(key);
        }
    }

    pub(super) fn thread_key(&self, handle: u32) -> Option<&RegistryKey> {
        self.records
            .get(&handle)
            .and_then(|record| record.thread.as_ref())
    }

    pub(super) fn increment_yield(&mut self, handle: u32) {
        if let Some(record) = self.records.get_mut(&handle) {
            record.yields = record.yields.saturating_add(1);
        }
    }

    pub(super) fn yield_count(&self, handle: u32) -> Option<u32> {
        self.records.get(&handle).map(|record| record.yields)
    }

    pub(super) fn label(&self, handle: u32) -> Option<&str> {
        self.records
            .get(&handle)
            .map(|record| record.label.as_str())
    }

    pub(super) fn active_handles(&self) -> Vec<u32> {
        self.records.keys().copied().collect()
    }

    pub(super) fn is_running(&self, handle: u32) -> bool {
        self.records.contains_key(&handle)
    }

    pub(super) fn complete_script(&mut self, handle: u32) -> (ScriptCleanup, Option<String>) {
        if let Some(record) = self.records.remove(&handle) {
            let message = format!("script.complete {} (#{handle})", record.label);
            (
                ScriptCleanup {
                    thread: record.thread,
                    callable: record.callable,
                },
                Some(message),
            )
        } else {
            (ScriptCleanup::default(), None)
        }
    }

    pub(super) fn find_handle(&self, label: &str) -> Option<u32> {
        self.records
            .iter()
            .find_map(|(handle, record)| (record.label == label).then_some(*handle))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&u32, &ScriptRecord)> {
        self.records.iter()
    }
}

impl ScriptRecord {
    pub(super) fn label(&self) -> &str {
        self.label.as_str()
    }

    pub(super) fn yields(&self) -> u32 {
        self.yields
    }
}

/// Couples script lifecycle operations with engine event logging.
pub(super) struct ScriptRuntimeAdapter<'a> {
    runtime: &'a mut ScriptRuntime,
    events: &'a mut Vec<String>,
}

/// Provides read-only accessors for script runtime state.
pub(super) struct ScriptRuntimeView<'a> {
    runtime: &'a ScriptRuntime,
}

impl<'a> ScriptRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut ScriptRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn start_script(
        &mut self,
        label: String,
        callable: Option<RegistryKey>,
    ) -> u32 {
        let (handle, event) = self.runtime.start_script(label, callable);
        self.events.push(event);
        handle
    }

    pub(super) fn attach_thread(&mut self, handle: u32, key: RegistryKey) {
        self.runtime.attach_thread(handle, key);
    }

    pub(super) fn increment_yield(&mut self, handle: u32) {
        self.runtime.increment_yield(handle);
    }

    pub(super) fn complete_script(&mut self, handle: u32) -> ScriptCleanup {
        let (cleanup, event) = self.runtime.complete_script(handle);
        if let Some(message) = event {
            self.events.push(message);
        }
        cleanup
    }
}

impl<'a> ScriptRuntimeView<'a> {
    pub(super) fn new(runtime: &'a ScriptRuntime) -> Self {
        Self { runtime }
    }

    pub(super) fn has_label(&self, label: &str) -> bool {
        self.runtime.has_label(label)
    }

    pub(super) fn yield_count(&self, handle: u32) -> Option<u32> {
        self.runtime.yield_count(handle)
    }

    pub(super) fn active_handles(&self) -> Vec<u32> {
        self.runtime.active_handles()
    }

    pub(super) fn is_running(&self, handle: u32) -> bool {
        self.runtime.is_running(handle)
    }

    pub(super) fn find_handle(&self, label: &str) -> Option<u32> {
        self.runtime.find_handle(label)
    }

    pub(super) fn thread_key(&self, handle: u32) -> Option<&RegistryKey> {
        self.runtime.thread_key(handle)
    }

    pub(super) fn label(&self, handle: u32) -> Option<String> {
        self.runtime.label(handle).map(|label| label.to_string())
    }
}
