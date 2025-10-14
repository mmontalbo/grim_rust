use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
pub(super) struct InventoryState {
    items: BTreeSet<String>,
    rooms: BTreeSet<String>,
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
