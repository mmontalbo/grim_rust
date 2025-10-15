//! Manny-office specific behaviour: prune the scene allowlist and verify that
//! captured transforms match the geometry recorded from the real game.

use anyhow::{Result, anyhow, ensure};

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

pub(super) fn validate_geometry_alignment(
    entities: &[SceneEntity],
    geometry: &LuaGeometrySnapshot,
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) -> Result<()> {
    if !is_manny_office(set_variable_name, set_display_name) {
        return Ok(());
    }

    const POSITION_EPSILON: f32 = 1e-3;
    const SCALE_EPSILON: f32 = 1e-3;
    const ROTATION_EPSILON: f32 = 1e-2;

    let prefix = set_variable_name.unwrap_or("mo");
    let prefix_lower = prefix.to_ascii_lowercase();

    let mut expected: Vec<(String, GeometryPose)> = Vec::new();

    if let Some(pose) = geometry_actor_pose(geometry, "manny") {
        expected.push(("manny".to_string(), pose));
    }
    if let Some(pose) = geometry_object_pose(geometry, "computer") {
        expected.push((format!("{prefix_lower}.computer"), pose));
    }
    if let Some(pose) = geometry_object_pose(geometry, "tube") {
        expected.push((format!("{prefix_lower}.tube"), pose));
    }
    if let Some(pose) = geometry_actor_pose(geometry, "motx083tube") {
        expected.push((format!("{prefix_lower}.tube.interest_actor"), pose));
    }
    if let Some(pose) = geometry_object_pose(geometry, "deck of playing cards") {
        expected.push((format!("{prefix_lower}.cards"), pose));
    }
    if let Some(pose) = geometry_actor_pose(geometry, "motx094deck_of_playing_cards") {
        expected.push((format!("{prefix_lower}.cards.interest_actor"), pose));
    }

    for (name, pose) in expected {
        let entity = entities
            .iter()
            .find(|entity| entity.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| {
                anyhow!("geometry snapshot contains {name} but timeline manifest did not record it")
            })?;

        let position = entity
            .position
            .ok_or_else(|| anyhow!("entity {name} missing position in timeline data"))?;
        ensure!(
            nearly_equal_vec3(position, pose.position, POSITION_EPSILON),
            "entity {name} position mismatch (timeline {:?} vs geometry {:?})",
            position,
            pose.position
        );

        if let Some(expected_rotation) = pose.rotation {
            if let Some(actual_rotation) = entity.rotation {
                ensure!(
                    nearly_equal_vec3(actual_rotation, expected_rotation, ROTATION_EPSILON),
                    "entity {name} rotation mismatch (timeline {:?} vs geometry {:?})",
                    actual_rotation,
                    expected_rotation
                );
            }
        }

        if let Some(expected_scale) = pose.scale {
            let actual_scale = entity.scale_multiplier().ok_or_else(|| {
                anyhow!("entity {name} missing scale multiplier in timeline data")
            })?;
            ensure!(
                (actual_scale - expected_scale).abs() <= SCALE_EPSILON,
                "entity {name} scale mismatch (timeline {} vs geometry {})",
                actual_scale,
                expected_scale
            );
        }

        if let Some(expected_collision) = pose.collision_scale {
            if let Some(actual_collision) = entity.collision_scale {
                ensure!(
                    (actual_collision - expected_collision).abs() <= SCALE_EPSILON,
                    "entity {name} collision scale mismatch (timeline {} vs geometry {})",
                    actual_collision,
                    expected_collision
                );
            }
        }
    }

    Ok(())
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

fn nearly_equal_vec3(a: [f32; 3], b: [f32; 3], epsilon: f32) -> bool {
    (0..3).all(|idx| (a[idx] - b[idx]).abs() <= epsilon)
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
            orientation: None,
            facing_target: None,
            head_control: None,
            head_look_rate: None,
            last_played: None,
            last_looping: None,
            last_completed: None,
            actor_scale: None,
            collision_scale: None,
            transform_stream: Vec::new(),
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
    fn geometry_alignment_passes_when_positions_match() {
        let mut actors = BTreeMap::new();
        actors.insert(
            "manny".to_string(),
            LuaActorSnapshot {
                name: Some("Manny".to_string()),
                position: Some([1.0, 2.0, 3.0]),
                rotation: Some([0.0, 90.0, 0.0]),
                scale: Some(1.0),
                collision_scale: Some(0.8),
            },
        );
        let snapshot = LuaGeometrySnapshot {
            actors,
            objects: vec![LuaObjectSnapshot {
                name: Some("tube".to_string()),
                string_name: Some("tube".to_string()),
                position: Some([0.0, 0.0, 0.0]),
                interest_actor: None,
            }],
        };

        let entities = vec![
            SceneEntity {
                kind: SceneEntityKind::Actor,
                name: "Manny".to_string(),
                position: Some([1.0, 2.0, 3.0]),
                rotation: Some([0.0, 90.0, 0.0]),
                actor_scale: Some(1.0),
                collision_scale: Some(0.8),
                ..make_entity(SceneEntityKind::Actor, "Manny")
            },
            SceneEntity {
                kind: SceneEntityKind::Object,
                name: "mo.tube".to_string(),
                position: Some([0.0, 0.0, 0.0]),
                ..make_entity(SceneEntityKind::Object, "mo.tube")
            },
        ];

        assert!(
            validate_geometry_alignment(&entities, &snapshot, Some("mo"), Some("Manny's Office"))
                .is_ok()
        );
    }

    #[test]
    fn geometry_alignment_fails_on_divergent_positions() {
        let mut actors = BTreeMap::new();
        actors.insert(
            "manny".to_string(),
            LuaActorSnapshot {
                name: Some("Manny".to_string()),
                position: Some([1.0, 2.0, 3.0]),
                rotation: None,
                scale: None,
                collision_scale: None,
            },
        );
        let snapshot = LuaGeometrySnapshot {
            actors,
            objects: Vec::new(),
        };

        let entities = vec![SceneEntity {
            kind: SceneEntityKind::Actor,
            name: "Manny".to_string(),
            position: Some([9.0, 2.0, 3.0]),
            actor_scale: Some(1.0),
            collision_scale: None,
            ..make_entity(SceneEntityKind::Actor, "Manny")
        }];

        let result =
            validate_geometry_alignment(&entities, &snapshot, Some("mo"), Some("Manny's Office"));
        assert!(result.is_err());
    }
}
