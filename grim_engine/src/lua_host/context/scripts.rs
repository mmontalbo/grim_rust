//! Minimal script scheduler bookkeeping used by stubbed boot functions.
//!
//! The real engine tracks script handles and lifecycle; we mirror just enough to
//! log start/complete events and hand back stable handles when boot scripts call
//! `start_script`/`single_start_script`.

use std::collections::BTreeMap;

#[derive(Debug)]
pub(super) struct ScriptRuntime {
    next_handle: u32,
    labels: BTreeMap<u32, String>,
}

impl ScriptRuntime {
    pub(super) fn new() -> Self {
        Self {
            next_handle: 1,
            labels: BTreeMap::new(),
        }
    }

    pub(super) fn start_script(&mut self, label: String) -> (u32, String) {
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1);
        self.labels.insert(handle, label.clone());
        (handle, format!("script.start {label} (#{handle})"))
    }

    pub(super) fn complete_script(&mut self, handle: u32) -> Option<String> {
        let label = self.labels.remove(&handle);
        label.map(|label| format!("script.complete {label} (#{handle})"))
    }
}

pub(super) struct ScriptRuntimeAdapter<'a> {
    runtime: &'a mut ScriptRuntime,
    events: &'a mut Vec<String>,
}

impl<'a> ScriptRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut ScriptRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn start_script(&mut self, label: String) -> u32 {
        let (handle, event) = self.runtime.start_script(label);
        self.events.push(event);
        handle
    }

    pub(super) fn complete_script(&mut self, handle: u32) {
        if let Some(message) = self.runtime.complete_script(handle) {
            self.events.push(message);
        }
    }
}
