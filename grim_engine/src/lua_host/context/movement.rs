use grim_formats::SectorKind as SetSectorKind;

use super::actors::{runtime::ActorRuntime, ActorSnapshot, ActorStore};
use super::cutscenes::{CommentaryRecord, CutsceneRuntime, CutsceneRuntimeAdapter};
use super::geometry::SectorHit;
use super::objects::{ObjectRuntime, ObjectRuntimeAdapter};
use super::sets::SetRuntime;
use crate::lua_host::types::{Vec3, MANNY_OFFICE_SEED_POS};

/// Couples movement-oriented mutations with logging and commentary refreshes.
pub(super) struct MovementRuntimeAdapter<'a> {
    actors: &'a mut ActorStore,
    sets: &'a mut SetRuntime,
    objects: &'a mut ObjectRuntime,
    cutscenes: &'a mut CutsceneRuntime,
    events: &'a mut Vec<String>,
}

/// Provides read-only helpers for sector resolution and spatial queries.
pub(super) struct MovementRuntimeView<'a> {
    actors: &'a ActorStore,
    sets: &'a SetRuntime,
    objects: &'a ObjectRuntime,
}

impl<'a> MovementRuntimeAdapter<'a> {
    pub(super) fn new(
        actors: &'a mut ActorStore,
        sets: &'a mut SetRuntime,
        objects: &'a mut ObjectRuntime,
        cutscenes: &'a mut CutsceneRuntime,
        events: &'a mut Vec<String>,
    ) -> Self {
        Self {
            actors,
            sets,
            objects,
            cutscenes,
            events,
        }
    }

    pub(super) fn walk_actor_vector(
        &mut self,
        handle: u32,
        delta: Vec3,
        adjust_y: Option<f32>,
        heading_offset: Option<f32>,
    ) -> bool {
        let Some(actor_id) = self.actors.actor_id_for_handle(handle).cloned() else {
            self.log(format!("walk.delta unknown_handle #{handle}"));
            return false;
        };
        let (label, current_set, current_position) = {
            let snapshot = self.actors.get(&actor_id).cloned().unwrap_or_else(|| {
                let mut actor = ActorSnapshot::default();
                actor.name = actor_id.clone();
                actor
            });
            (
                snapshot.name,
                snapshot
                    .current_set
                    .or_else(|| self.sets.current_set().map(|set| set.set_file.clone())),
                snapshot.position.unwrap_or(MANNY_OFFICE_SEED_POS),
            )
        };

        self.log(format!(
            "walk.vector {} {:.4},{:.4}",
            label, delta.x, delta.y
        ));

        let mut next = Vec3 {
            x: current_position.x + delta.x,
            y: current_position.y + delta.y,
            z: current_position.z + delta.z,
        };
        if let Some(offset) = adjust_y {
            next.y += offset;
        }

        if let Some(ref set_file) = current_set {
            if self.sets.set_geometry().contains_key(set_file)
                && !self.sets.point_in_active_walk(set_file, (next.x, next.y))
            {
                self.log(format!(
                    "walk.delta blocked {} {:.3},{:.3}",
                    label, next.x, next.y
                ));
                return false;
            }
        }

        self.set_actor_position(&actor_id, &label, next);

        if delta.x.abs() + delta.y.abs() > f32::EPSILON {
            let yaw = compute_walk_yaw(delta, heading_offset);
            self.set_actor_rotation(
                &actor_id,
                &label,
                Vec3 {
                    x: 0.0,
                    y: yaw,
                    z: 0.0,
                },
            );
        }

        if let Some(hit) = {
            let view = self.view();
            view.geometry_sector_hit(&actor_id, "walk")
        } {
            self.record_sector_hit(&actor_id, &label, hit);
        }

        true
    }

    pub(super) fn walk_actor_to_handle(&mut self, handle: u32, target: Vec3) -> bool {
        let current = if let Some(position) = self.view().actor_position_by_handle(handle) {
            position
        } else {
            self.log(format!("walk.to unknown_handle #{handle}"));
            return false;
        };

        let delta = Vec3 {
            x: target.x - current.x,
            y: target.y - current.y,
            z: target.z - current.z,
        };

        if delta.x.abs() + delta.y.abs() + delta.z.abs() <= f32::EPSILON {
            return true;
        }

        self.actors.set_actor_moving(handle, true);
        let moved = self.walk_actor_vector(handle, delta, None, None);
        self.actors.set_actor_moving(handle, false);
        moved
    }

    pub(super) fn set_actor_position(&mut self, id: &str, label: &str, position: Vec3) {
        let handle = {
            let mut runtime = self.actor_runtime();
            runtime.set_actor_position(id, label, position)
        };
        if handle != 0 {
            self.update_object_position_for_actor(handle, position);
        }
    }

    pub(super) fn update_object_position_for_actor(&mut self, actor_handle: u32, position: Vec3) {
        {
            let mut runtime = self.object_runtime();
            runtime.update_object_position_for_actor(actor_handle, position);
        }
        self.refresh_commentary_visibility();
    }

    pub(super) fn refresh_commentary_visibility(&mut self) {
        let Some(record) = self.cutscenes.commentary().cloned() else {
            return;
        };
        let visible = {
            let view = self.view();
            view.commentary_object_visible(&record)
        };
        let mut runtime = self.cutscene_runtime();
        runtime.update_commentary_visibility(visible, "not_visible");
    }

    fn set_actor_rotation(&mut self, id: &str, label: &str, rotation: Vec3) {
        let mut runtime = self.actor_runtime();
        runtime.set_actor_rotation(id, label, rotation);
    }

    fn record_sector_hit(&mut self, id: &str, label: &str, hit: SectorHit) {
        let mut runtime = self.actor_runtime();
        runtime.record_sector_hit(id, label, hit);
    }

    fn actor_runtime(&mut self) -> ActorRuntime<'_> {
        ActorRuntime::new(self.actors, self.events)
    }

    fn object_runtime(&mut self) -> ObjectRuntimeAdapter<'_> {
        ObjectRuntimeAdapter::new(self.objects, self.events, &*self.actors, self.sets)
    }

    fn cutscene_runtime(&mut self) -> CutsceneRuntimeAdapter<'_> {
        CutsceneRuntimeAdapter::new(self.cutscenes, self.events)
    }

    fn view(&self) -> MovementRuntimeView<'_> {
        MovementRuntimeView::new(&*self.actors, &*self.sets, &*self.objects)
    }

    fn log(&mut self, message: impl Into<String>) {
        self.events.push(message.into());
    }
}

impl<'a> MovementRuntimeView<'a> {
    pub(super) fn new(
        actors: &'a ActorStore,
        sets: &'a SetRuntime,
        objects: &'a ObjectRuntime,
    ) -> Self {
        Self {
            actors,
            sets,
            objects,
        }
    }

    pub(super) fn actor_position_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actors
            .actor_position_by_handle(handle)
            .or_else(|| self.objects.object_position_by_actor(handle))
    }

    pub(super) fn geometry_sector_hit(&self, actor_id: &str, raw_kind: &str) -> Option<SectorHit> {
        self.sets.current_set()?;
        let point = self.actor_position_xy(actor_id)?;
        self.sets.geometry_sector_hit(raw_kind, point)
    }

    pub(super) fn geometry_sector_name(&self, actor_id: &str, raw_kind: &str) -> Option<String> {
        self.geometry_sector_hit(actor_id, raw_kind)
            .map(|hit| hit.name)
    }

    pub(super) fn resolve_sector_hit(&self, actor_id: &str, kind: &str) -> Option<SectorHit> {
        let normalized_kind = if kind.is_empty() { "walk" } else { kind };
        let request = match normalized_kind {
            "0" => "walk",
            "1" => "hot",
            "2" => "camera",
            other => other,
        };

        let lookup_key = match request {
            "walk" => "WALK".to_string(),
            "hot" => "HOT".to_string(),
            "camera" => "CAMERA".to_string(),
            other => other.to_ascii_uppercase(),
        };

        if let Some(hit) = self
            .actors
            .actor_snapshot(actor_id)
            .and_then(|actor| actor.sectors.get(&lookup_key))
        {
            return Some(hit.clone());
        }

        if let Some(hit) = self.geometry_sector_hit(actor_id, request) {
            return Some(hit);
        }

        if let Some(hit) = self.visible_sector_hit(actor_id, request) {
            return Some(hit);
        }

        if let Some(current) = self.sets.current_set() {
            if let Some(descriptor) = self.sets.available_sets().get(&current.set_file) {
                match request {
                    "camera" => {
                        if let Some(current_setup) = self.sets.current_setup_for(&current.set_file)
                        {
                            if let Some(label) = descriptor.setup_label_for_index(current_setup) {
                                return Some(SectorHit::new(
                                    current_setup,
                                    label.to_string(),
                                    "CAMERA",
                                ));
                            }
                        }
                        if let Some(info) = descriptor.first_setup() {
                            return Some(SectorHit::new(info.index, info.label.clone(), "CAMERA"));
                        }
                    }
                    "hot" => {
                        if let Some(info) = descriptor.first_setup() {
                            return Some(SectorHit::new(info.index, info.label.clone(), "HOT"));
                        }
                    }
                    _ => {}
                }
            }
        }

        None
    }

    pub(super) fn default_sector_hit(
        &self,
        actor_id: &str,
        requested_kind: Option<&str>,
    ) -> SectorHit {
        let normalized = requested_kind
            .map(|kind| kind.trim().to_ascii_lowercase())
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| "walk".to_string());

        let request = match normalized.as_str() {
            "0" => "walk".to_string(),
            "1" => "hot".to_string(),
            "2" => "camera".to_string(),
            other => other.to_string(),
        };

        if let Some(hit) = self.resolve_sector_hit(actor_id, &request) {
            return hit;
        }

        let kind = match request.as_str() {
            "walk" => "WALK".to_string(),
            "hot" => "HOT".to_string(),
            "camera" => "CAMERA".to_string(),
            other => other.to_ascii_uppercase(),
        };
        SectorHit::new(1000, format!("{}_sector", actor_id), kind)
    }

    pub(super) fn commentary_object_visible(&self, record: &CommentaryRecord) -> bool {
        let current = self.sets.current_set().map(|set| set.set_file.as_str());
        self.objects
            .commentary_object_visible(record, current, |set, sector| {
                self.sets.is_sector_active(set, sector)
            })
    }

    fn actor_position_xy(&self, actor_id: &str) -> Option<(f32, f32)> {
        self.actors.actor_position_xy(actor_id)
    }

    pub(super) fn visible_sector_hit(&self, _actor_id: &str, request: &str) -> Option<SectorHit> {
        let current = self.sets.current_set()?;
        let geometry = self.sets.set_geometry().get(&current.set_file)?;

        let mut handles: Vec<i64> = self.objects.hotlist_handles().to_vec();
        for info in self.objects.visible_objects() {
            if !handles.contains(&info.handle) {
                handles.push(info.handle);
            }
        }

        if handles.is_empty() {
            return None;
        }

        for handle in handles {
            let object = self.objects.object(handle)?;
            if !object.visible || !object.touchable {
                continue;
            }
            if let Some(set_file) = object.set_file.as_ref() {
                if !set_file.eq_ignore_ascii_case(&current.set_file) {
                    continue;
                }
            } else {
                continue;
            }

            let point = if let Some(position) = object.position.as_ref() {
                Some((position.x, position.y))
            } else if let Some(actor_handle) = object.interest_actor {
                self.actor_position_by_handle(actor_handle)
                    .map(|vec| (vec.x, vec.y))
            } else {
                None
            };

            let point = match point {
                Some(value) => value,
                None => continue,
            };

            match request {
                "camera" | "hot" => {
                    if let Some(setup) = geometry.best_setup_for_point(point) {
                        if let Some(hit) =
                            self.sets
                                .sector_hit_from_setup(&current.set_file, &setup.name, request)
                        {
                            return Some(hit);
                        }
                    }
                }
                "walk" => {
                    if let Some(polygon) = geometry.find_polygon(SetSectorKind::Walk, point) {
                        if self.sets.is_sector_active(&current.set_file, &polygon.name) {
                            return Some(SectorHit::new(polygon.id, polygon.name.clone(), "WALK"));
                        }
                    }
                }
                other => {
                    let sector_kind = match other {
                        "camera" => Some(SetSectorKind::Camera),
                        "walk" => Some(SetSectorKind::Walk),
                        _ => None,
                    };
                    if let Some(kind) = sector_kind {
                        if let Some(polygon) = geometry.find_polygon(kind, point) {
                            if self.sets.is_sector_active(&current.set_file, &polygon.name) {
                                return Some(SectorHit::new(
                                    polygon.id,
                                    polygon.name.clone(),
                                    other.to_ascii_uppercase(),
                                ));
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

fn compute_walk_yaw(delta: Vec3, heading_offset: Option<f32>) -> f32 {
    let mut yaw = (-delta.x).atan2(delta.y).to_degrees();
    if let Some(offset) = heading_offset {
        yaw += offset;
    }
    yaw = yaw.rem_euclid(360.0);
    if yaw < 0.0 {
        yaw + 360.0
    } else {
        yaw
    }
}
