use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct LuaMusicSnapshot {
    pub current: Option<LuaMusicCueSnapshot>,
    pub queued: Vec<LuaMusicCueSnapshot>,
    pub current_state: Option<String>,
    pub state_stack: Vec<String>,
    pub paused: bool,
    pub muted_groups: Vec<String>,
    pub volume: Option<f32>,
    pub history: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LuaMusicCueSnapshot {
    pub name: String,
    pub parameters: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct LuaSfxSnapshot {
    pub active: Vec<LuaSfxInstanceSnapshot>,
    pub history: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LuaSfxInstanceSnapshot {
    pub handle: String,
    pub cue: String,
    pub parameters: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaGeometrySnapshot {
    pub current_set: Option<LuaCurrentSetSnapshot>,
    pub selected_actor: Option<String>,
    pub voice_effect: Option<String>,
    pub loaded_sets: Vec<String>,
    pub current_setups: BTreeMap<String, LuaSetSelectionSnapshot>,
    pub sets: Vec<LuaSetSnapshot>,
    pub actors: BTreeMap<String, LuaActorSnapshot>,
    pub objects: Vec<LuaObjectSnapshot>,
    pub visible_objects: Vec<LuaVisibleObjectSnapshot>,
    pub hotlist_handles: Vec<i64>,
    pub inventory: Vec<String>,
    pub inventory_rooms: Vec<String>,
    pub commentary: Option<LuaCommentarySnapshot>,
    pub cut_scenes: Vec<LuaCutSceneSnapshot>,
    #[serde(default)]
    pub music: LuaMusicSnapshot,
    #[serde(default)]
    pub sfx: LuaSfxSnapshot,
    pub events: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaCurrentSetSnapshot {
    pub set_file: String,
    pub variable_name: String,
    pub display_name: Option<String>,
    pub selection: Option<LuaSetSelectionSnapshot>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaSetSelectionSnapshot {
    pub index: i32,
    pub label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaSetSnapshot {
    pub set_file: String,
    pub variable_name: Option<String>,
    pub display_name: Option<String>,
    pub has_geometry: bool,
    pub current_setup: Option<LuaSetSelectionSnapshot>,
    pub setups: Vec<LuaSetupSnapshot>,
    pub sectors: Vec<LuaSectorSnapshot>,
    pub active_sectors: BTreeMap<String, bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaSetupSnapshot {
    pub name: String,
    pub interest: Option<[f32; 2]>,
    pub position: Option<[f32; 2]>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaSectorSnapshot {
    pub id: i32,
    pub name: String,
    pub kind: String,
    pub default_active: bool,
    pub active: bool,
    pub vertices: Vec<[f32; 2]>,
    pub centroid: [f32; 2],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaActorSnapshot {
    pub name: String,
    pub costume: Option<String>,
    pub base_costume: Option<String>,
    pub current_set: Option<String>,
    pub at_interest: bool,
    pub position: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub is_selected: bool,
    pub is_visible: bool,
    pub handle: u32,
    pub sectors: BTreeMap<String, LuaActorSectorSnapshot>,
    pub costume_stack: Vec<String>,
    pub current_chore: Option<String>,
    pub walk_chore: Option<String>,
    pub talk_chore: Option<String>,
    pub talk_drop_chore: Option<String>,
    pub mumble_chore: Option<String>,
    pub talk_color: Option<String>,
    pub head_target: Option<String>,
    pub head_look_rate: Option<f32>,
    pub collision_mode: Option<String>,
    pub ignoring_boxes: bool,
    pub last_chore_costume: Option<String>,
    pub speaking: bool,
    pub last_line: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaActorSectorSnapshot {
    pub id: i32,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaObjectSnapshot {
    pub handle: i64,
    pub name: String,
    pub string_name: Option<String>,
    pub set_file: Option<String>,
    pub position: Option<[f32; 3]>,
    pub range: f32,
    pub touchable: bool,
    pub visible: bool,
    pub interest_actor: Option<LuaObjectActorLink>,
    pub sectors: Vec<LuaObjectSectorSnapshot>,
    pub in_active_sector: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaObjectActorLink {
    pub handle: u32,
    pub actor_id: Option<String>,
    pub actor_label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaObjectSectorSnapshot {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaVisibleObjectSnapshot {
    pub handle: i64,
    pub name: String,
    pub string_name: Option<String>,
    pub display_name: String,
    pub range: f32,
    pub distance: Option<f32>,
    pub angle: Option<f32>,
    pub within_range: Option<bool>,
    pub in_hotlist: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaCommentarySnapshot {
    pub label: Option<String>,
    pub object_handle: Option<i64>,
    pub active: bool,
    pub suppressed_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LuaCutSceneSnapshot {
    pub label: Option<String>,
    pub set_file: Option<String>,
    pub sector: Option<String>,
    pub suppressed: bool,
}
