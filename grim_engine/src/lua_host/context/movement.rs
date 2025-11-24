use super::actors::{runtime::ActorRuntime, ActorSnapshot, ActorStore};
use super::cutscenes::CommentaryRecord;
use super::geometry::SectorHit;
use super::{normalize_tube_event, TubePoseAliasCache};
use crate::lua_host::types::{Vec3, MANNY_OFFICE_SEED_POS};

/// Minimal movement adapter for intro-only flow. Applies position/rotation
/// updates via the actor runtime and logs events; geometry and sector queries
/// are stubbed out to keep state surface small.
pub(super) struct MovementRuntimeAdapter<'a> {
    actors: &'a mut ActorStore,
    events: &'a mut Vec<String>,
    tube_pose_aliases: TubePoseAliasCache,
}

pub(super) struct MovementRuntimeView<'a> {
    actors: &'a ActorStore,
}

impl<'a> MovementRuntimeAdapter<'a> {
    pub(super) fn new(
        actors: &'a mut ActorStore,
        events: &'a mut Vec<String>,
        tube_pose_aliases: TubePoseAliasCache,
    ) -> Self {
        Self {
            actors,
            events,
            tube_pose_aliases,
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
        let (label, current_position) = {
            let snapshot = self.actors.get(&actor_id).cloned().unwrap_or_else(|| {
                let mut actor = ActorSnapshot::default();
                actor.name = actor_id.clone();
                actor
            });
            (
                snapshot.name,
                snapshot.position.unwrap_or(MANNY_OFFICE_SEED_POS),
            )
        };

        let mut next = Vec3 {
            x: current_position.x + delta.x,
            y: current_position.y + delta.y,
            z: current_position.z + delta.z,
        };
        if let Some(offset) = adjust_y {
            next.y += offset;
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
        let mut runtime = self.actor_runtime();
        runtime.set_actor_position(id, label, position);
    }

    pub(super) fn set_actor_rotation(&mut self, id: &str, label: &str, rotation: Vec3) {
        let mut runtime = self.actor_runtime();
        runtime.set_actor_rotation(id, label, rotation);
    }

    pub(super) fn refresh_commentary_visibility(&mut self) {
        // Commentary objects are not tracked in the intro slice.
    }

    fn actor_runtime(&mut self) -> ActorRuntime<'_> {
        ActorRuntime::new(self.actors, self.events, self.tube_pose_aliases.clone())
    }

    fn view(&self) -> MovementRuntimeView<'_> {
        MovementRuntimeView::new(self.actors)
    }

    fn log(&mut self, message: impl Into<String>) {
        let mut message = message.into();
        if let Some(updated) = {
            let cache = self.tube_pose_aliases.borrow();
            cache
                .as_ref()
                .and_then(|map| normalize_tube_event(map, &message))
        } {
            message = updated;
        }
        self.events.push(message);
    }
}

impl<'a> MovementRuntimeView<'a> {
    pub(super) fn new(actors: &'a ActorStore) -> Self {
        Self { actors }
    }

    pub(super) fn actor_position_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actors.actor_position_by_handle(handle)
    }

    pub(super) fn geometry_sector_hit(&self, _actor_id: &str, _raw_kind: &str) -> Option<SectorHit> {
        None
    }

    pub(super) fn resolve_sector_hit(&self, actor_id: &str, kind: &str) -> Option<SectorHit> {
        let normalized_kind = if kind.is_empty() { "walk" } else { kind };
        let request = match normalized_kind {
            "0" => "walk",
            "1" => "hot",
            "2" => "camera",
            other => other,
        };

        self.actors
            .actor_snapshot(actor_id)
            .and_then(|actor| actor.sectors.get(&request.to_ascii_uppercase()))
            .cloned()
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

    pub(super) fn commentary_object_visible(&self, _record: &CommentaryRecord) -> bool {
        false
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
