use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
pub(super) struct InventoryState {
    items: BTreeSet<String>,
    rooms: BTreeSet<String>,
}

/// Couples inventory mutations with the engine event log.
pub(super) struct InventoryRuntimeAdapter<'a> {
    state: &'a mut InventoryState,
    events: &'a mut Vec<String>,
}

impl InventoryState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn add_item(&mut self, name: &str) -> bool {
        self.items.insert(name.to_string())
    }

    pub(super) fn register_room(&mut self, name: &str) -> bool {
        self.rooms.insert(name.to_string())
    }

    pub(super) fn items(&self) -> &BTreeSet<String> {
        &self.items
    }

    pub(super) fn rooms(&self) -> &BTreeSet<String> {
        &self.rooms
    }
}

impl<'a> InventoryRuntimeAdapter<'a> {
    pub(super) fn new(state: &'a mut InventoryState, events: &'a mut Vec<String>) -> Self {
        Self { state, events }
    }

    pub(super) fn add_item(&mut self, name: &str) -> bool {
        let added = self.state.add_item(name);
        if added {
            self.events.push(format!("inventory.add {name}"));
        }
        added
    }

    pub(super) fn register_room(&mut self, name: &str) -> bool {
        let registered = self.state.register_room(name);
        if registered {
            self.events.push(format!("inventory.room {name}"));
        }
        registered
    }
}
