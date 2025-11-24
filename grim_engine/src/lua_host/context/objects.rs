use std::collections::BTreeMap;

use crate::lua_host::types::Vec3;

use super::actors::ActorStore;
use super::{distance_between, heading_between};

#[derive(Debug, Clone)]
pub(super) struct ObjectSnapshot {
    pub(super) handle: i64,
    pub(super) name: String,
    pub(super) string_name: Option<String>,
    pub(super) set_file: Option<String>,
    pub(super) position: Option<Vec3>,
    pub(super) range: f32,
    pub(super) touchable: bool,
    pub(super) visible: bool,
    pub(super) interest_actor: Option<u32>,
}

#[derive(Debug, Clone)]
pub(super) struct VisibleObjectInfo {
    pub(super) handle: i64,
    pub(super) name: String,
    pub(super) string_name: Option<String>,
    pub(super) range: f32,
    pub(super) distance: Option<f32>,
    pub(super) angle: Option<f32>,
    pub(super) within_range: Option<bool>,
}

impl VisibleObjectInfo {
    pub(super) fn display_name(&self) -> &str {
        self.string_name.as_deref().unwrap_or(self.name.as_str())
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct ObjectRuntime {
    records: BTreeMap<i64, ObjectSnapshot>,
    by_name: BTreeMap<String, i64>,
    by_actor: BTreeMap<u32, i64>,
    visible_infos: Vec<VisibleObjectInfo>,
}

impl ObjectRuntime {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn register(&mut self, snapshot: ObjectSnapshot) -> bool {
        let handle = snapshot.handle;
        if let Some(existing) = self.records.get(&handle) {
            if let Some(actor_handle) = existing.interest_actor {
                self.by_actor.remove(&actor_handle);
            }
        }

        let name = snapshot.name.clone();
        let interest_actor = snapshot.interest_actor;
        let existed = self.records.insert(handle, snapshot).is_some();
        self.by_name.insert(name, handle);
        if let Some(actor_handle) = interest_actor {
            self.by_actor.insert(actor_handle, handle);
        }
        existed
    }

    pub(super) fn unregister(&mut self, handle: i64) -> Option<ObjectSnapshot> {
        let snapshot = self.records.remove(&handle)?;
        if let Some(actor_handle) = snapshot.interest_actor {
            self.by_actor.remove(&actor_handle);
        }
        self.by_name.retain(|_, value| *value != handle);
        Some(snapshot)
    }

    pub(super) fn object_mut(&mut self, handle: i64) -> Option<&mut ObjectSnapshot> {
        self.records.get_mut(&handle)
    }

    pub(super) fn object_position_by_actor(&self, actor_handle: u32) -> Option<Vec3> {
        self.by_actor
            .get(&actor_handle)
            .and_then(|object_handle| self.records.get(object_handle))
            .and_then(|object| object.position)
    }

    pub(super) fn handle_for_actor(&self, actor_handle: u32) -> Option<i64> {
        self.by_actor.get(&actor_handle).copied()
    }

    pub(super) fn lookup_by_name(&self, label: &str) -> Option<i64> {
        self.by_name.get(label).copied()
    }

    pub(super) fn visible_handles(&self, current_set: Option<&str>) -> Vec<i64> {
        let Some(current_file) = current_set else {
            return Vec::new();
        };

        let mut handles = Vec::new();
        for object in self.records.values() {
            if !object.touchable || !object.visible {
                continue;
            }
            let Some(set_file) = object.set_file.as_deref() else {
                continue;
            };
            if !set_file.eq_ignore_ascii_case(current_file) {
                continue;
            }
            handles.push(object.handle);
        }
        handles
    }

    pub(super) fn record_visible_objects<F>(
        &mut self,
        handles: &[i64],
        actors: &ActorStore,
        actor_position: Option<Vec3>,
        actor_handle: Option<u32>,
        mut log_event: F,
    ) where
        F: FnMut(String),
    {
        self.visible_infos.clear();
        if handles.is_empty() {
            log_event("scene.visible <none>".to_string());
            return;
        }

        let mut names = Vec::new();
        let mut visible_infos: Vec<VisibleObjectInfo> = Vec::new();

        for handle in handles {
            if let Some(object) = self.records.get(handle).cloned() {
                let display = object
                    .string_name
                    .clone()
                    .unwrap_or_else(|| object.name.clone());
                names.push(display.clone());

                let mut info = VisibleObjectInfo {
                    handle: *handle,
                    name: object.name.clone(),
                    string_name: object.string_name.clone(),
                    range: object.range,
                    distance: None,
                    angle: None,
                    within_range: None,
                };

                let object_position = object.position.or_else(|| {
                    object.interest_actor.and_then(|h| {
                        actors
                            .actor_position_by_handle(h)
                            .or_else(|| self.object_position_by_actor(h))
                    })
                });
                if let (Some(actor_pos), Some(obj_pos)) = (actor_position, object_position) {
                    let distance = distance_between(actor_pos, obj_pos);
                    info.distance = Some(distance);
                    info.within_range = Some(distance <= object.range + f32::EPSILON);
                }

                if let (Some(focus_handle), Some(target_handle)) =
                    (actor_handle, object.interest_actor)
                {
                    if let (Some(actor_pos), Some(target_pos)) = (
                        actors
                            .actor_position_by_handle(focus_handle)
                            .or_else(|| self.object_position_by_actor(focus_handle)),
                        actors
                            .actor_position_by_handle(target_handle)
                            .or_else(|| self.object_position_by_actor(target_handle)),
                    ) {
                        info.angle = Some(heading_between(actor_pos, target_pos) as f32);
                    }
                }

                visible_infos.push(info);
            }
        }

        if names.is_empty() {
            log_event("scene.visible <unknown>".to_string());
        } else {
            log_event(format!("scene.visible {}", names.join(", ")));
        }

        self.visible_infos = visible_infos;
    }

    pub(super) fn commentary_candidate_handle(&self) -> Option<i64> {
        self.visible_infos.first().map(|info| info.handle)
    }

    pub(super) fn visible_objects(&self) -> &[VisibleObjectInfo] {
        &self.visible_infos
    }
}

/// Provides high-level object runtime operations coupled with engine event logging.
pub(super) struct ObjectRuntimeAdapter<'a> {
    runtime: &'a mut ObjectRuntime,
    events: &'a mut Vec<String>,
    actors: &'a ActorStore,
}

impl<'a> ObjectRuntimeAdapter<'a> {
    pub(super) fn new(
        runtime: &'a mut ObjectRuntime,
        events: &'a mut Vec<String>,
        actors: &'a ActorStore,
    ) -> Self {
        Self {
            runtime,
            events,
            actors,
        }
    }

    pub(super) fn register_object(&mut self, mut snapshot: ObjectSnapshot) {
        let handle = snapshot.handle;
        if snapshot.set_file.is_none() {
            if let Some(actor_handle) = snapshot.interest_actor {
                if let Some(actor_id) = self.actors.actor_id_for_handle(actor_handle) {
                    if let Some(actor) = self.actors.get(actor_id) {
                        if let Some(set_file) = actor.current_set.clone() {
                            snapshot.set_file = Some(set_file);
                        }
                    }
                }
            }
        }
        let interest_actor = snapshot.interest_actor;
        let name = snapshot.name.clone();
        let set_label = snapshot
            .set_file
            .clone()
            .unwrap_or_else(|| "<unknown>".to_string());
        let existed = self.runtime.register(snapshot);
        if let Some(actor_handle) = interest_actor {
            self.events
                .push(format!("object.link actor#{} -> {}", actor_handle, name));
        }
        let verb = if existed {
            "object.update"
        } else {
            "object.register"
        };
        self.events
            .push(format!("{verb} {name} (#{handle}) @ {set_label}"));
    }

    pub(super) fn unregister_object(&mut self, handle: i64) -> bool {
        if let Some(snapshot) = self.runtime.unregister(handle) {
            self.events
                .push(format!("object.remove {} (#{handle})", snapshot.name));
            true
        } else {
            false
        }
    }

    pub(super) fn record_visible_objects(&mut self, handles: &[i64]) {
        let actor_snapshot = self
            .actors
            .selected_actor_snapshot()
            .cloned()
            .or_else(|| self.actors.get("manny").cloned());
        let actor_position = actor_snapshot.as_ref().and_then(|actor| actor.position);
        let actor_handle = actor_snapshot
            .as_ref()
            .map(|actor| actor.handle)
            .filter(|handle| *handle != 0);

        let mut log_messages: Vec<String> = Vec::new();
        self.runtime.record_visible_objects(
            handles,
            self.actors,
            actor_position,
            actor_handle,
            |message| log_messages.push(message),
        );
        for message in log_messages {
            self.events.push(message);
        }
    }

    pub(super) fn set_object_touchable(&mut self, handle: i64, touchable: bool) {
        if let Some(object) = self.runtime.object_mut(handle) {
            object.touchable = touchable;
        }
        let state = if touchable {
            "touchable"
        } else {
            "untouchable"
        };
        self.events
            .push(format!("object.touchable #{handle} {state}"));
    }

    pub(super) fn set_object_visibility(&mut self, handle: i64, visible: bool) {
        if let Some(object) = self.runtime.object_mut(handle) {
            if object.visible != visible {
                object.visible = visible;
                let state = if visible { "visible" } else { "hidden" };
                self.events
                    .push(format!("object.visible #{handle} {state}"));
            } else {
                object.visible = visible;
            }
        }
    }
}
