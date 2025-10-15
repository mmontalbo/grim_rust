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

/// Provides read-only helpers for inventory queries.
pub(super) struct InventoryRuntimeView<'a> {
    state: &'a InventoryState,
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

    pub(super) fn clone_items(&self) -> BTreeSet<String> {
        self.items.clone()
    }

    pub(super) fn clone_rooms(&self) -> BTreeSet<String> {
        self.rooms.clone()
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

impl<'a> InventoryRuntimeView<'a> {
    pub(super) fn new(state: &'a InventoryState) -> Self {
        Self { state }
    }

    pub(super) fn clone_items(&self) -> BTreeSet<String> {
        self.state.clone_items()
    }

    pub(super) fn clone_rooms(&self) -> BTreeSet<String> {
        self.state.clone_rooms()
    }
}
