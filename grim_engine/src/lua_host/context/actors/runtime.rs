use super::super::geometry::SectorHit;
use super::{ActorSnapshot, ActorStore};
use crate::lua_host::types::Vec3;

/// Bundles actor state mutations with logging for runtime consumers.
pub(crate) struct ActorRuntime<'a> {
    store: &'a mut ActorStore,
    events: &'a mut Vec<String>,
}

impl<'a> ActorRuntime<'a> {
    pub(crate) fn new(store: &'a mut ActorStore, events: &'a mut Vec<String>) -> Self {
        Self { store, events }
    }

    fn log(&mut self, message: String) {
        self.events.push(message);
    }

    fn ensure_actor_mut(&mut self, id: &str, label: &str) -> &mut ActorSnapshot {
        self.store.ensure_actor_mut(id, label)
    }

    pub(crate) fn select_actor(&mut self, id: &str, label: &str) {
        self.store.select_actor(id, label);
        self.log(format!("actor.select {id}"));
    }

    pub(crate) fn set_actor_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        let actor = self.ensure_actor_mut(id, label);
        actor.costume = costume.clone();
        match costume {
            Some(ref name) => {
                if let Some(slot) = actor.costume_stack.last_mut() {
                    *slot = name.clone();
                } else {
                    actor.costume_stack.push(name.clone());
                }
                self.log(format!("actor.{id}.costume {name}"));
            }
            None => {
                actor.costume_stack.clear();
                self.log(format!("actor.{id}.costume <nil>"));
            }
        }
    }

    pub(crate) fn set_actor_base_costume(
        &mut self,
        id: &str,
        label: &str,
        costume: Option<String>,
    ) {
        let actor = self.ensure_actor_mut(id, label);
        actor.base_costume = costume.clone();
        actor.costume_stack.clear();
        match costume {
            Some(ref name) => {
                actor.costume_stack.push(name.clone());
                self.log(format!("actor.{id}.base_costume {name}"));
            }
            None => self.log(format!("actor.{id}.base_costume <nil>")),
        }
    }

    pub(crate) fn push_actor_costume(&mut self, id: &str, label: &str, costume: String) -> usize {
        let depth;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.costume_stack.push(costume.clone());
            actor.costume = Some(costume.clone());
            depth = actor.costume_stack.len();
        }
        self.log(format!("actor.{id}.push_costume {costume} depth {depth}"));
        depth
    }

    pub(crate) fn pop_actor_costume(&mut self, id: &str, label: &str) -> Option<String> {
        let mut removed: Option<String> = None;
        let mut next: Option<String> = None;
        let blocked;
        {
            let actor = self.ensure_actor_mut(id, label);
            if actor.costume_stack.len() <= 1 {
                blocked = true;
            } else {
                blocked = false;
                removed = actor.costume_stack.pop();
                next = actor.costume_stack.last().cloned();
                actor.costume = next.clone();
            }
        }
        if blocked {
            self.log(format!("actor.{id}.pop_costume blocked"));
            None
        } else {
            let name = removed.as_deref().unwrap_or("<nil>").to_string();
            self.log(format!("actor.{id}.pop_costume {name}"));
            next
        }
    }

    pub(crate) fn set_actor_current_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.current_chore = chore.clone();
            actor.last_chore_costume = costume.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!("actor.{id}.chore {chore_label} {costume_label}"));
    }

    pub(crate) fn set_actor_walk_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.walk_chore = chore.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!(
            "actor.{id}.walk_chore {chore_label} {costume_label}"
        ));
    }

    pub(crate) fn set_actor_talk_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        drop: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, drop_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.talk_chore = chore.clone();
            actor.talk_drop_chore = drop.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            drop_label = drop.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!(
            "actor.{id}.talk_chore {chore_label} drop {drop_label} costume {costume_label}"
        ));
    }

    pub(crate) fn set_actor_mumble_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.mumble_chore = chore.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!(
            "actor.{id}.mumble_chore {chore_label} costume {costume_label}"
        ));
    }

    pub(crate) fn set_actor_talk_color(&mut self, id: &str, label: &str, color: Option<String>) {
        let display;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.talk_color = color.clone();
            display = color.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!("actor.{id}.talk_color {display}"));
    }

    pub(crate) fn set_actor_head_target(&mut self, id: &str, label: &str, target: Option<String>) {
        let display;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.head_target = target.clone();
            display = target.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!("actor.{id}.head_target {display}"));
    }

    pub(crate) fn set_actor_head_look_rate(&mut self, id: &str, label: &str, rate: Option<f32>) {
        let snapshot;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.head_look_rate = rate;
            snapshot = actor.head_look_rate;
        }
        match snapshot {
            Some(value) => self.log(format!("actor.{id}.head_rate {value:.3}")),
            None => self.log(format!("actor.{id}.head_rate <nil>")),
        }
    }

    pub(crate) fn set_actor_collision_mode(&mut self, id: &str, label: &str, mode: Option<String>) {
        let display;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.collision_mode = mode.clone();
            display = mode.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log(format!("actor.{id}.collision_mode {display}"));
    }

    pub(crate) fn set_actor_ignore_boxes(&mut self, id: &str, label: &str, ignore: bool) {
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.ignoring_boxes = ignore;
        }
        self.log(format!("actor.{id}.ignore_boxes {}", ignore));
    }

    pub(crate) fn put_actor_in_set(&mut self, id: &str, label: &str, set_file: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.current_set = Some(set_file.to_string());
        self.log(format!("actor.{id}.enter {set_file}"));
    }

    pub(crate) fn actor_at_interest(&mut self, id: &str, label: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.at_interest = true;
        self.log(format!("actor.{id}.at_interest"));
    }

    pub(crate) fn set_actor_position(&mut self, id: &str, label: &str, position: Vec3) -> u32 {
        let handle = {
            let actor = self.ensure_actor_mut(id, label);
            actor.position = Some(position);
            actor.handle
        };
        self.log(format!(
            "actor.{id}.pos {:.3},{:.3},{:.3}",
            position.x, position.y, position.z
        ));
        handle
    }

    pub(crate) fn set_actor_rotation(&mut self, id: &str, label: &str, rotation: Vec3) {
        self.ensure_actor_mut(id, label).rotation = Some(rotation);
        self.log(format!(
            "actor.{id}.rot {:.3},{:.3},{:.3}",
            rotation.x, rotation.y, rotation.z
        ));
    }

    pub(crate) fn set_actor_scale(&mut self, id: &str, label: &str, scale: Option<f32>) {
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.scale = scale;
        }
        let display = scale
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "<nil>".to_string());
        self.log(format!("actor.{id}.scale {display}"));
    }

    pub(crate) fn set_actor_collision_scale(&mut self, id: &str, label: &str, scale: Option<f32>) {
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.collision_scale = scale;
        }
        let display = scale
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "<nil>".to_string());
        self.log(format!("actor.{id}.collision_scale {display}"));
    }

    pub(crate) fn record_sector_hit(&mut self, id: &str, label: &str, hit: SectorHit) {
        let actor = self.ensure_actor_mut(id, label);
        actor.sectors.insert(hit.kind.clone(), hit);
    }
}
