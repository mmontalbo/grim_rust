use std::collections::{BTreeMap, BTreeSet};

use super::geometry::SectorHit;
use crate::lua_host::types::Vec3;

#[derive(Debug, Default, Clone)]
pub(crate) struct ActorSnapshot {
    pub(super) name: String,
    pub(super) costume: Option<String>,
    pub(super) base_costume: Option<String>,
    pub(super) current_set: Option<String>,
    pub(super) at_interest: bool,
    pub(super) position: Option<Vec3>,
    pub(super) rotation: Option<Vec3>,
    pub(super) scale: Option<f32>,
    pub(super) collision_scale: Option<f32>,
    pub(super) is_selected: bool,
    pub(super) is_visible: bool,
    pub(super) handle: u32,
    pub(super) sectors: BTreeMap<String, SectorHit>,
    pub(super) costume_stack: Vec<String>,
    pub(super) current_chore: Option<String>,
    pub(super) walk_chore: Option<String>,
    pub(super) talk_chore: Option<String>,
    pub(super) talk_drop_chore: Option<String>,
    pub(super) mumble_chore: Option<String>,
    pub(super) talk_color: Option<String>,
    pub(super) head_target: Option<String>,
    pub(super) head_look_rate: Option<f32>,
    pub(super) collision_mode: Option<String>,
    pub(super) ignoring_boxes: bool,
    pub(super) last_chore_costume: Option<String>,
    pub(super) speaking: bool,
    pub(super) last_line: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct ActorStore {
    actors: BTreeMap<String, ActorSnapshot>,
    labels: BTreeMap<String, String>,
    handles: BTreeMap<u32, String>,
    selected_actor: Option<String>,
    next_handle: u32,
    actors_installed: bool,
    moving_actors: BTreeSet<u32>,
}

impl ActorStore {
    pub(super) fn new(starting_handle: u32) -> Self {
        Self {
            next_handle: starting_handle,
            ..Self::default()
        }
    }

    pub(super) fn ensure_actor_mut(&mut self, id: &str, label: &str) -> &mut ActorSnapshot {
        let entry = self.actors.entry(id.to_string()).or_insert_with(|| {
            let mut actor = ActorSnapshot::default();
            actor.name = label.to_string();
            actor.is_visible = true;
            actor
        });
        entry.name = label.to_string();
        self.labels
            .entry(label.to_string())
            .or_insert_with(|| id.to_string());
        entry
    }

    pub(super) fn select_actor(&mut self, id: &str, label: &str) {
        if let Some(previous) = self.selected_actor.take() {
            if previous != id {
                if let Some(actor) = self.actors.get_mut(&previous) {
                    actor.is_selected = false;
                }
            } else {
                self.selected_actor = Some(previous);
            }
        }
        let actor = self.ensure_actor_mut(id, label);
        actor.is_selected = true;
        self.selected_actor = Some(id.to_string());
    }

    pub(super) fn selected_actor_id(&self) -> Option<&String> {
        self.selected_actor.as_ref()
    }

    pub(super) fn selected_actor_snapshot(&self) -> Option<&ActorSnapshot> {
        self.selected_actor
            .as_ref()
            .and_then(|id| self.actors.get(id))
    }

    pub(super) fn get(&self, id: &str) -> Option<&ActorSnapshot> {
        self.actors.get(id)
    }

    pub(super) fn get_mut(&mut self, id: &str) -> Option<&mut ActorSnapshot> {
        self.actors.get_mut(id)
    }

    pub(super) fn register_actor_with_handle(
        &mut self,
        label: &str,
        preferred_handle: Option<u32>,
    ) -> (String, u32, bool) {
        let id = self
            .labels
            .get(label)
            .cloned()
            .unwrap_or_else(|| canonicalize_actor_label(label));

        let entry = self.actors.entry(id.clone()).or_insert_with(|| {
            let mut actor = ActorSnapshot::default();
            actor.name = label.to_string();
            actor.is_visible = true;
            actor
        });
        entry.name = label.to_string();

        if let Some(existing) = self.labels.get(label) {
            if existing != &id {
                self.labels.insert(label.to_string(), id.clone());
            }
        } else {
            self.labels.insert(label.to_string(), id.clone());
        }

        let mut newly_assigned = None;
        if entry.handle == 0 {
            let handle = preferred_handle.unwrap_or_else(|| {
                let handle = self.next_handle;
                self.next_handle += 1;
                handle
            });
            entry.handle = handle;
            self.handles.insert(handle, id.clone());
            newly_assigned = Some(handle);
        }

        let assigned = newly_assigned.is_some();
        (id, entry.handle, assigned)
    }

    pub(super) fn mark_actors_installed(&mut self) {
        self.actors_installed = true;
    }

    pub(super) fn actors_installed(&self) -> bool {
        self.actors_installed
    }

    pub(super) fn resolve_actor_handle(&self, candidates: &[&str]) -> Option<(u32, String)> {
        for candidate in candidates {
            if let Some(actor) = self.actors.get(*candidate) {
                if actor.handle == 0 {
                    return None;
                }
                let id = self
                    .handles
                    .get(&actor.handle)
                    .cloned()
                    .unwrap_or_else(|| actor.name.to_ascii_lowercase());
                return Some((actor.handle, id));
            }
        }
        None
    }

    pub(super) fn actor_identity_by_handle(&self, handle: u32) -> Option<(String, String)> {
        let id = self.handles.get(&handle)?.clone();
        let label = self
            .actors
            .get(&id)
            .map(|actor| actor.name.clone())
            .unwrap_or_else(|| id.clone());
        Some((id, label))
    }

    pub(super) fn actor_id_for_handle(&self, handle: u32) -> Option<&String> {
        self.handles.get(&handle)
    }

    pub(super) fn actor_position_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.handles
            .get(&handle)
            .and_then(|id| self.actors.get(id))
            .and_then(|actor| actor.position)
    }

    pub(super) fn actor_rotation_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.handles
            .get(&handle)
            .and_then(|id| self.actors.get(id))
            .and_then(|actor| actor.rotation)
    }

    pub(super) fn actor_snapshot(&self, actor_id: &str) -> Option<&ActorSnapshot> {
        self.actors
            .get(actor_id)
            .or_else(|| self.actors.get(&actor_id.to_ascii_lowercase()))
    }

    pub(super) fn actor_position_xy(&self, actor_id: &str) -> Option<(f32, f32)> {
        if let Some(actor) = self.actors.get(actor_id) {
            return actor.position.map(|pos| (pos.x, pos.y));
        }
        let lowercase = actor_id.to_ascii_lowercase();
        self.actors
            .get(&lowercase)
            .and_then(|actor| actor.position)
            .map(|pos| (pos.x, pos.y))
    }

    pub(super) fn set_actor_moving(&mut self, handle: u32, moving: bool) {
        if moving {
            self.moving_actors.insert(handle);
        } else {
            self.moving_actors.remove(&handle);
        }
    }

    pub(super) fn is_actor_moving(&self, handle: u32) -> bool {
        self.moving_actors.contains(&handle)
    }

    pub(super) fn clone_map(&self) -> BTreeMap<String, ActorSnapshot> {
        self.actors.clone()
    }

    pub(super) fn clone_handles(&self) -> BTreeMap<u32, String> {
        self.handles.clone()
    }
}

fn canonicalize_actor_label(label: &str) -> String {
    let mut id = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_whitespace() || matches!(ch, '.' | '-' | '_' | ':') {
            if !id.ends_with('_') {
                id.push('_');
            }
        }
    }
    if id.is_empty() {
        id.push_str("actor");
    }
    while id.ends_with('_') {
        id.pop();
    }
    if id.is_empty() {
        id.push_str("actor");
    }
    id
}
