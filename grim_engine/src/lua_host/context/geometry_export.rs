use std::collections::{BTreeMap, BTreeSet};

use crate::geometry_snapshot::{
    LuaActorSectorSnapshot, LuaActorSnapshot, LuaCommentarySnapshot, LuaCurrentSetSnapshot,
    LuaCutSceneSnapshot, LuaGeometrySnapshot, LuaObjectActorLink, LuaObjectSectorSnapshot,
    LuaObjectSnapshot, LuaSectorSnapshot, LuaSetSelectionSnapshot, LuaSetSnapshot,
    LuaSetupSnapshot, LuaVisibleObjectSnapshot,
};

use super::actors::ActorSnapshot;
use super::geometry::{sector_kind_label, ParsedSetGeometry, SetDescriptor, SetSnapshot};
use super::{
    vec3_to_array, CommentaryRecord, CutSceneRecord, MusicState, ObjectSnapshot, SfxState,
    VisibleObjectInfo,
};

#[derive(Clone)]
pub(super) struct SnapshotState {
    pub(super) current_set: Option<SetSnapshot>,
    pub(super) selected_actor: Option<String>,
    pub(super) voice_effect: Option<String>,
    pub(super) loaded_sets: BTreeSet<String>,
    pub(super) current_setups: BTreeMap<String, i32>,
    pub(super) available_sets: BTreeMap<String, SetDescriptor>,
    pub(super) set_geometry: BTreeMap<String, ParsedSetGeometry>,
    pub(super) sector_states: BTreeMap<String, BTreeMap<String, bool>>,
    pub(super) actors: BTreeMap<String, ActorSnapshot>,
    pub(super) objects: BTreeMap<i64, ObjectSnapshot>,
    pub(super) actor_handles: BTreeMap<u32, String>,
    pub(super) visible_objects: Vec<VisibleObjectInfo>,
    pub(super) hotlist_handles: Vec<i64>,
    pub(super) inventory: BTreeSet<String>,
    pub(super) inventory_rooms: BTreeSet<String>,
    pub(super) commentary: Option<CommentaryRecord>,
    pub(super) cut_scene_stack: Vec<CutSceneRecord>,
    pub(super) music: MusicState,
    pub(super) sfx: SfxState,
    pub(super) events: Vec<String>,
}

pub(super) fn build_snapshot(state: SnapshotState) -> LuaGeometrySnapshot {
    let current_set = state.current_set.as_ref().map(|current| {
        let selection =
            state
                .current_setups
                .get(&current.set_file)
                .map(|index| LuaSetSelectionSnapshot {
                    index: *index,
                    label: state
                        .available_sets
                        .get(&current.set_file)
                        .and_then(|descriptor| descriptor.setup_label_for_index(*index))
                        .map(|label| label.to_string()),
                });
        LuaCurrentSetSnapshot {
            set_file: current.set_file.clone(),
            variable_name: current.variable_name.clone(),
            display_name: current.display_name.clone(),
            selection,
        }
    });

    let mut set_keys = BTreeSet::new();
    set_keys.extend(state.set_geometry.keys().cloned());
    set_keys.extend(state.sector_states.keys().cloned());

    let mut sets = Vec::new();
    for set_file in set_keys {
        let descriptor = state.available_sets.get(&set_file);
        let geometry = state.set_geometry.get(&set_file);
        let states = state.sector_states.get(&set_file);

        let current_setup =
            state
                .current_setups
                .get(&set_file)
                .map(|index| LuaSetSelectionSnapshot {
                    index: *index,
                    label: descriptor
                        .and_then(|desc| desc.setup_label_for_index(*index))
                        .map(|label| label.to_string()),
                });

        let setups = geometry
            .map(|geometry| {
                geometry
                    .setups
                    .iter()
                    .map(|setup| LuaSetupSnapshot {
                        name: setup.name.clone(),
                        interest: setup.interest.map(|(x, y)| [x, y]),
                        position: setup.position.map(|(x, y)| [x, y]),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(Vec::new);

        let sectors = geometry
            .map(|geometry| {
                geometry
                    .sectors
                    .iter()
                    .map(|sector| LuaSectorSnapshot {
                        id: sector.id,
                        name: sector.name.clone(),
                        kind: sector_kind_label(sector.kind).to_string(),
                        default_active: sector.default_active,
                        active: states
                            .and_then(|map| map.get(&sector.name).copied())
                            .unwrap_or(sector.default_active),
                        vertices: sector
                            .vertices
                            .iter()
                            .map(|(x, y)| [*x, *y])
                            .collect::<Vec<_>>(),
                        centroid: [sector.centroid.0, sector.centroid.1],
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(Vec::new);

        let active_sectors = states
            .map(|map| {
                map.iter()
                    .map(|(name, active)| (name.clone(), *active))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_else(BTreeMap::new);

        sets.push(LuaSetSnapshot {
            set_file: set_file.clone(),
            variable_name: descriptor.map(|desc| desc.variable_name.clone()),
            display_name: descriptor.and_then(|desc| desc.display_name.clone()),
            has_geometry: geometry.is_some(),
            current_setup,
            setups,
            sectors,
            active_sectors,
        });
    }

    let actors = state
        .actors
        .iter()
        .map(|(id, actor)| {
            let sectors = actor
                .sectors
                .iter()
                .map(|(kind, hit)| {
                    (
                        kind.clone(),
                        LuaActorSectorSnapshot {
                            id: hit.id,
                            name: hit.name.clone(),
                            kind: hit.kind.clone(),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            (
                id.clone(),
                LuaActorSnapshot {
                    name: actor.name.clone(),
                    costume: actor.costume.clone(),
                    base_costume: actor.base_costume.clone(),
                    current_set: actor.current_set.clone(),
                    at_interest: actor.at_interest,
                    position: actor.position.map(vec3_to_array),
                    rotation: actor.rotation.map(vec3_to_array),
                    scale: actor.scale,
                    collision_scale: actor.collision_scale,
                    is_selected: actor.is_selected,
                    is_visible: actor.is_visible,
                    handle: actor.handle,
                    sectors,
                    costume_stack: actor.costume_stack.clone(),
                    current_chore: actor.current_chore.clone(),
                    walk_chore: actor.walk_chore.clone(),
                    talk_chore: actor.talk_chore.clone(),
                    talk_drop_chore: actor.talk_drop_chore.clone(),
                    mumble_chore: actor.mumble_chore.clone(),
                    talk_color: actor.talk_color.clone(),
                    head_target: actor.head_target.clone(),
                    head_look_rate: actor.head_look_rate,
                    collision_mode: actor.collision_mode.clone(),
                    ignoring_boxes: actor.ignoring_boxes,
                    last_chore_costume: actor.last_chore_costume.clone(),
                    speaking: actor.speaking,
                    last_line: actor.last_line.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut objects: Vec<LuaObjectSnapshot> = state
        .objects
        .values()
        .map(|object| {
            let interest_actor = object.interest_actor.map(|handle| {
                let actor_id = state.actor_handles.get(&handle).cloned();
                let actor_label = actor_id
                    .as_ref()
                    .and_then(|id| state.actors.get(id))
                    .map(|actor| actor.name.clone());
                LuaObjectActorLink {
                    handle,
                    actor_id,
                    actor_label,
                }
            });
            let sectors = object
                .sectors
                .iter()
                .map(|sector| LuaObjectSectorSnapshot {
                    name: sector.name.clone(),
                    kind: sector_kind_label(sector.kind).to_string(),
                })
                .collect::<Vec<_>>();
            LuaObjectSnapshot {
                handle: object.handle,
                name: object.name.clone(),
                string_name: object.string_name.clone(),
                set_file: object.set_file.clone(),
                position: object.position.map(vec3_to_array),
                range: object.range,
                touchable: object.touchable,
                visible: object.visible,
                interest_actor,
                sectors,
                in_active_sector: None,
            }
        })
        .collect();
    objects.sort_by_key(|object| object.handle);

    let mut visible_objects: Vec<LuaVisibleObjectSnapshot> = state
        .visible_objects
        .iter()
        .map(|object| LuaVisibleObjectSnapshot {
            handle: object.handle,
            name: object.name.clone(),
            string_name: object.string_name.clone(),
            display_name: object.display_name().to_string(),
            range: object.range,
            distance: object.distance,
            angle: object.angle,
            within_range: object.within_range,
            in_hotlist: object.in_hotlist,
        })
        .collect();
    visible_objects.sort_by_key(|object| object.handle);

    let hotlist_handles = state.hotlist_handles.clone();
    let inventory = state.inventory.iter().cloned().collect::<Vec<_>>();
    let inventory_rooms = state.inventory_rooms.iter().cloned().collect::<Vec<_>>();

    let commentary = state
        .commentary
        .as_ref()
        .map(|record| LuaCommentarySnapshot {
            label: record.label.clone(),
            object_handle: record.object_handle,
            active: record.active,
            suppressed_reason: record.suppressed_reason.clone(),
        });

    let cut_scenes = state
        .cut_scene_stack
        .iter()
        .map(|record| LuaCutSceneSnapshot {
            label: record.label.clone(),
            set_file: record.set_file.clone(),
            sector: record.sector.clone(),
            suppressed: record.suppressed,
        })
        .collect::<Vec<_>>();

    let music = state.music.to_snapshot();
    let sfx = state.sfx.to_snapshot();

    let current_setups = state
        .current_setups
        .iter()
        .map(|(set_file, index)| {
            let label = state
                .available_sets
                .get(set_file)
                .and_then(|descriptor| descriptor.setup_label_for_index(*index))
                .map(|label| label.to_string());
            (
                set_file.clone(),
                LuaSetSelectionSnapshot {
                    index: *index,
                    label,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    LuaGeometrySnapshot {
        current_set,
        selected_actor: state.selected_actor.clone(),
        voice_effect: state.voice_effect.clone(),
        loaded_sets: state.loaded_sets.iter().cloned().collect::<Vec<_>>(),
        current_setups,
        sets,
        actors,
        objects,
        visible_objects,
        hotlist_handles,
        inventory,
        inventory_rooms,
        commentary,
        cut_scenes,
        music,
        sfx,
        events: state.events,
    }
}
