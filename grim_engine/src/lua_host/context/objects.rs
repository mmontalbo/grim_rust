use std::collections::BTreeMap;

use grim_formats::SectorKind as SetSectorKind;

use crate::lua_host::types::Vec3;

use super::actors::ActorStore;
use super::cutscenes::CommentaryRecord;
use super::sets::SetRuntime;
use super::{distance_between, heading_between};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct ObjectSectorRef {
    pub(super) name: String,
    pub(super) kind: SetSectorKind,
}

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
    pub(super) sectors: Vec<ObjectSectorRef>,
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
    pub(super) in_hotlist: bool,
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
    hotlist_handles: Vec<i64>,
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

    pub(super) fn object(&self, handle: i64) -> Option<&ObjectSnapshot> {
        self.records.get(&handle)
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

    pub(super) fn visible_handles<F>(
        &self,
        current_set: Option<&str>,
        mut is_sector_active: F,
    ) -> Vec<i64>
    where
        F: FnMut(&str, &str) -> bool,
    {
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
            if !object_is_in_active_sector(object, set_file, &mut is_sector_active) {
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
        self.hotlist_handles.clear();
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
                    in_hotlist: false,
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

        let mut best_angle: Option<f32> = None;
        for info in &visible_infos {
            if let Some(angle) = info.angle {
                if best_angle.map(|best| angle < best).unwrap_or(true) {
                    best_angle = Some(angle);
                }
            }
        }

        if let Some(best) = best_angle {
            for info in &mut visible_infos {
                if let Some(angle) = info.angle {
                    if (angle - best).abs() < 10.0 {
                        info.in_hotlist = true;
                    }
                }
            }
        }

        let hot_names: Vec<String> = visible_infos
            .iter()
            .filter(|info| info.in_hotlist)
            .map(|info| info.display_name().to_string())
            .collect();

        if !hot_names.is_empty() {
            log_event(format!("scene.hotlist {}", hot_names.join(", ")));
        }

        self.hotlist_handles = visible_infos
            .iter()
            .filter(|info| info.in_hotlist)
            .map(|info| info.handle)
            .collect();
        self.visible_infos = visible_infos;
    }

    pub(super) fn commentary_candidate_handle(&self) -> Option<i64> {
        self.hotlist_handles
            .first()
            .copied()
            .or_else(|| self.visible_infos.first().map(|info| info.handle))
    }

    pub(super) fn commentary_object_visible<F>(
        &self,
        record: &CommentaryRecord,
        current_set: Option<&str>,
        mut is_sector_active: F,
    ) -> bool
    where
        F: FnMut(&str, &str) -> bool,
    {
        if let Some(handle) = record.object_handle {
            let Some(object) = self.records.get(&handle) else {
                return false;
            };
            if !object.visible || !object.touchable {
                return false;
            }
            let Some(object_set) = object.set_file.as_deref() else {
                return false;
            };
            let Some(current_set) = current_set else {
                return false;
            };
            if !current_set.eq_ignore_ascii_case(object_set) {
                return false;
            }
            return object_is_in_active_sector(object, object_set, &mut is_sector_active);
        }
        !self.hotlist_handles.is_empty() || !self.visible_infos.is_empty()
    }

    pub(super) fn visible_objects(&self) -> &[VisibleObjectInfo] {
        &self.visible_infos
    }

    pub(super) fn hotlist_handles(&self) -> &[i64] {
        &self.hotlist_handles
    }

    pub(super) fn clone_records(&self) -> BTreeMap<i64, ObjectSnapshot> {
        self.records.clone()
    }
}

/// Provides high-level object runtime operations coupled with engine event logging.
pub(super) struct ObjectRuntimeAdapter<'a> {
    runtime: &'a mut ObjectRuntime,
    events: &'a mut Vec<String>,
    actors: &'a ActorStore,
    sets: &'a mut SetRuntime,
}

impl<'a> ObjectRuntimeAdapter<'a> {
    pub(super) fn new(
        runtime: &'a mut ObjectRuntime,
        events: &'a mut Vec<String>,
        actors: &'a ActorStore,
        sets: &'a mut SetRuntime,
    ) -> Self {
        Self {
            runtime,
            events,
            actors,
            sets,
        }
    }

    fn ensure_sector_state_map(&mut self, set_file: &str) -> bool {
        let (has_geometry, message) = self.sets.ensure_sector_state_map(set_file);
        if let Some(message) = message {
            self.events.push(message);
        }
        has_geometry
    }

    fn compute_object_sectors(&mut self, set_file: &str, position: Vec3) -> Vec<ObjectSectorRef> {
        if !self.ensure_sector_state_map(set_file) {
            return Vec::new();
        }
        let Some(geometry) = self.sets.set_geometry().get(set_file) else {
            return Vec::new();
        };
        let point = (position.x, position.y);
        geometry
            .sectors
            .iter()
            .filter(|sector| sector.contains(point))
            .map(|sector| ObjectSectorRef {
                name: sector.name.clone(),
                kind: sector.kind,
            })
            .collect()
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
            if snapshot.set_file.is_none() {
                if let Some(current) = self.sets.current_set() {
                    snapshot.set_file = Some(current.set_file.clone());
                }
            }
        }
        let sectors = if let (Some(set_file), Some(position)) =
            (snapshot.set_file.as_ref(), snapshot.position)
        {
            self.compute_object_sectors(set_file, position)
        } else {
            Vec::new()
        };
        snapshot.sectors = sectors;
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

    pub(super) fn update_object_position_for_actor(&mut self, actor_handle: u32, position: Vec3) {
        if let Some(object_handle) = self.runtime.handle_for_actor(actor_handle) {
            let actor_set = self
                .actors
                .actor_id_for_handle(actor_handle)
                .and_then(|id| self.actors.get(id))
                .and_then(|actor| actor.current_set.clone())
                .or_else(|| {
                    self.sets
                        .current_set()
                        .map(|snapshot| snapshot.set_file.clone())
                });
            let mut object_name = None;
            let mut set_for_recalc: Option<(String, Vec3)> = None;
            {
                if let Some(object) = self.runtime.object_mut(object_handle) {
                    object.position = Some(position);
                    object_name = Some(object.name.clone());
                    if object.set_file.is_none() {
                        if let Some(set_file) = actor_set.clone() {
                            object.set_file = Some(set_file);
                        }
                    }
                    if let Some(ref set_file) = object.set_file {
                        set_for_recalc = Some((set_file.clone(), position));
                    } else {
                        object.sectors.clear();
                    }
                }
            }
            if let Some((set_file, pos)) = set_for_recalc {
                let sectors = self.compute_object_sectors(&set_file, pos);
                if let Some(object) = self.runtime.object_mut(object_handle) {
                    object.sectors = sectors;
                }
            }
            if let Some(name) = object_name {
                self.events.push(format!(
                    "object.actor#{}.pos {} {:.3},{:.3},{:.3}",
                    actor_handle, name, position.x, position.y, position.z
                ));
            }
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

fn object_is_in_active_sector<F>(
    snapshot: &ObjectSnapshot,
    set_file: &str,
    is_sector_active: &mut F,
) -> bool
where
    F: FnMut(&str, &str) -> bool,
{
    if snapshot.sectors.is_empty() {
        return true;
    }
    let mut considered = false;
    for sector in &snapshot.sectors {
        if matches!(
            sector.kind,
            SetSectorKind::Walk | SetSectorKind::Special | SetSectorKind::Other
        ) {
            considered = true;
            if is_sector_active(set_file, &sector.name) {
                return true;
            }
        }
    }
    if considered {
        false
    } else {
        true
    }
}
