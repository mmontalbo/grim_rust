//! Manny-office specific behaviour: pruning the scene allowlist and reconciling
//! geometry snapshots so the viewer focuses on the core desk/tube experience.

use super::{GeometryPose, LuaGeometrySnapshot, SceneEntity};

pub(super) fn is_manny_office(
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) -> bool {
    set_variable_name
        .map(|value| value.eq_ignore_ascii_case("mo"))
        .unwrap_or(false)
        || set_display_name
            .map(|value| value.eq_ignore_ascii_case("Manny's Office"))
            .unwrap_or(false)
}

pub(super) fn prune_entities_for_set(
    entities: Vec<SceneEntity>,
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) -> Vec<SceneEntity> {
    if !is_manny_office(set_variable_name, set_display_name) {
        return entities;
    }

    let set_prefix = set_variable_name.unwrap_or("mo");
    let allowlist = manny_office_entity_names(set_prefix);

    entities
        .into_iter()
        .filter(|entity| {
            allowlist
                .iter()
                .any(|allowed| entity.name.eq_ignore_ascii_case(allowed))
        })
        .collect()
}

pub(super) fn apply_geometry_overrides(
    entities: &mut [SceneEntity],
    geometry: &LuaGeometrySnapshot,
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) {
    if !is_manny_office(set_variable_name, set_display_name) {
        return;
    }

    let prefix = set_variable_name.unwrap_or("mo");
    let prefix_lower = prefix.to_ascii_lowercase();

    let mut overrides: Vec<(String, GeometryPose)> = Vec::new();

    if let Some(pose) = geometry_actor_pose(geometry, "manny") {
        overrides.push(("manny".to_string(), pose));
    }

    if let Some(pose) = geometry_object_pose(geometry, "computer") {
        overrides.push((format!("{prefix_lower}.computer"), pose));
    }

    if let Some(pose) = geometry_object_pose(geometry, "tube") {
        overrides.push((format!("{prefix_lower}.tube"), pose));
    }

    if let Some(pose) = geometry_actor_pose(geometry, "motx083tube") {
        overrides.push((format!("{prefix_lower}.tube.interest_actor"), pose));
    }

    if let Some(pose) = geometry_object_pose(geometry, "deck of playing cards") {
        overrides.push((format!("{prefix_lower}.cards"), pose));
    }

    if let Some(pose) = geometry_actor_pose(geometry, "motx094deck_of_playing_cards") {
        overrides.push((format!("{prefix_lower}.cards.interest_actor"), pose));
    }

    for (name, pose) in overrides {
        if let Some(entity) = entities
            .iter_mut()
            .find(|entity| entity.name.eq_ignore_ascii_case(&name))
        {
            entity.position = Some(pose.position);
            if let Some(rotation) = pose.rotation {
                entity.rotation = Some(rotation);
            }
        }
    }
}

fn manny_office_entity_names(set_prefix: &str) -> Vec<String> {
    let prefix = if set_prefix.is_empty() {
        "mo"
    } else {
        set_prefix
    };
    let mut names = vec!["manny".to_string()];
    for suffix in [
        "cards",
        "cards.interest_actor",
        "computer",
        "tube",
        "tube.interest_actor",
    ] {
        names.push(format!("{prefix}.{suffix}"));
    }
    names
}

fn geometry_actor_pose(snapshot: &LuaGeometrySnapshot, key: &str) -> Option<GeometryPose> {
    snapshot
        .actors
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .and_then(|(_, actor)| actor.pose())
}

fn geometry_object_pose(snapshot: &LuaGeometrySnapshot, name: &str) -> Option<GeometryPose> {
    snapshot
        .objects
        .iter()
        .find(|object| {
            object
                .string_name
                .as_deref()
                .map(|value| value.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        })
        .and_then(|object| object.pose())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{LuaActorSnapshot, LuaObjectSnapshot, SceneEntity, SceneEntityKind};
    use std::collections::BTreeMap;

    fn make_entity(kind: SceneEntityKind, name: &str) -> SceneEntity {
        SceneEntity {
            kind,
            name: name.to_string(),
            created_by: None,
            timeline_hook_index: None,
            timeline_stage_index: None,
            timeline_stage_label: None,
            timeline_hook_name: None,
            methods: Vec::new(),
            position: None,
            rotation: None,
            facing_target: None,
            head_control: None,
            head_look_rate: None,
            last_played: None,
            last_looping: None,
            last_completed: None,
        }
    }

    #[test]
    fn prune_entities_retains_manny_focus() {
        let entities = vec![
            make_entity(SceneEntityKind::Actor, "Manny"),
            make_entity(SceneEntityKind::Object, "mo.CARDS"),
            make_entity(SceneEntityKind::Object, "mo.coffee_mug"),
        ];
        let pruned = prune_entities_for_set(entities, Some("mo"), Some("Manny's Office"));
        let names: Vec<String> = pruned.into_iter().map(|entity| entity.name).collect();
        assert_eq!(names, vec!["Manny", "mo.CARDS"]);
    }

    #[test]
    fn prune_entities_skips_other_sets() {
        let entities = vec![
            make_entity(SceneEntityKind::Actor, "Glottis"),
            make_entity(SceneEntityKind::Object, "gl.car"),
        ];
        let pruned = prune_entities_for_set(entities, Some("gl"), Some("Glottis' Garage"));
        assert_eq!(pruned.len(), 2);
    }

    #[test]
    fn geometry_overrides_apply_when_manny_office() {
        let mut entities = vec![
            make_entity(SceneEntityKind::Actor, "Manny"),
            make_entity(SceneEntityKind::Object, "mo.computer"),
        ];

        let mut snapshot = LuaGeometrySnapshot {
            actors: BTreeMap::new(),
            objects: Vec::new(),
        };
        snapshot.actors.insert(
            "manny".to_string(),
            LuaActorSnapshot {
                name: Some("Manny".to_string()),
                position: Some([1.0, 2.0, 3.0]),
                rotation: Some([0.0, 90.0, 0.0]),
            },
        );
        snapshot.objects.push(LuaObjectSnapshot {
            name: Some("Computer".to_string()),
            string_name: Some("computer".to_string()),
            position: Some([4.0, 5.0, 6.0]),
            interest_actor: None,
        });

        apply_geometry_overrides(&mut entities, &snapshot, Some("mo"), Some("Manny's Office"));

        assert_eq!(entities[0].position, Some([1.0, 2.0, 3.0]));
        assert_eq!(entities[0].rotation, Some([0.0, 90.0, 0.0]));
        assert_eq!(entities[1].position, Some([4.0, 5.0, 6.0]));
    }
}
