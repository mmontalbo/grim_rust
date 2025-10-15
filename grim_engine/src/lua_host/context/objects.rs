use std::collections::BTreeMap;

use grim_formats::SectorKind as SetSectorKind;

use crate::lua_host::types::Vec3;

use super::actors::ActorStore;
use super::cutscenes::CommentaryRecord;
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
