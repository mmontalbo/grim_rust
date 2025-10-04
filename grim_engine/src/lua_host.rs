use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{anyhow, Context, Result};
use grim_analysis::resources::{normalize_legacy_lua, ResourceGraph};
use mlua::{
    Error as LuaError, Function, Lua, LuaOptions, MultiValue, RegistryKey, Result as LuaResult,
    StdLib, Table, Thread, ThreadStatus, Value, Variadic,
};

#[derive(Debug)]
struct ScriptRecord {
    label: String,
    thread: Option<RegistryKey>,
    yields: u32,
    callable: Option<RegistryKey>,
}

#[derive(Debug, Default)]
struct ScriptCleanup {
    thread: Option<RegistryKey>,
    callable: Option<RegistryKey>,
}

#[derive(Debug, Clone)]
struct SetupInfo {
    label: String,
    index: i32,
}

#[derive(Debug, Clone)]
struct SetDescriptor {
    variable_name: String,
    display_name: Option<String>,
    setups: Vec<SetupInfo>,
}

impl SetDescriptor {
    fn setup_index(&self, label: &str) -> Option<i32> {
        self.setups.iter().find_map(|slot| {
            if slot.label.eq_ignore_ascii_case(label) {
                Some(slot.index)
            } else {
                None
            }
        })
    }

    fn setup_label_for_index(&self, index: i32) -> Option<&str> {
        self.setups
            .iter()
            .find(|slot| slot.index == index)
            .map(|slot| slot.label.as_str())
    }

    fn first_setup(&self) -> Option<&SetupInfo> {
        self.setups.first()
    }
}

#[derive(Debug, Clone)]
struct SetSnapshot {
    set_file: String,
    variable_name: String,
    display_name: Option<String>,
}

#[derive(Debug, Copy, Clone)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

const MANNY_OFFICE_SEED_POS: Vec3 = Vec3 {
    x: 0.606_999_993,
    y: 2.040_999_89,
    z: 0.0,
};

const MANNY_OFFICE_SEED_ROT: Vec3 = Vec3 {
    x: 0.0,
    y: 222.210_007,
    z: 0.0,
};

#[derive(Debug, Clone)]
struct SectorHit {
    id: i32,
    name: String,
    kind: String,
}

impl SectorHit {
    fn new(id: i32, name: impl Into<String>, kind: impl Into<String>) -> Self {
        SectorHit {
            id,
            name: name.into(),
            kind: kind.into(),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct ActorSnapshot {
    name: String,
    costume: Option<String>,
    base_costume: Option<String>,
    current_set: Option<String>,
    at_interest: bool,
    position: Option<Vec3>,
    rotation: Option<Vec3>,
    is_selected: bool,
    is_visible: bool,
    handle: u32,
    sectors: BTreeMap<String, SectorHit>,
}

#[derive(Debug, Default, Clone)]
struct MenuState {
    visible: bool,
    auto_freeze: bool,
    last_run_mode: Option<String>,
    last_action: Option<String>,
}

#[derive(Clone, Copy)]
struct FootstepProfile {
    key: &'static str,
    prefix: &'static str,
    left_walk: u8,
    right_walk: u8,
    left_run: Option<u8>,
    right_run: Option<u8>,
}

const FOOTSTEP_PROFILES: &[FootstepProfile] = &[
    FootstepProfile {
        key: "concrete",
        prefix: "fscon",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "dirt",
        prefix: "fsdrt",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "gravel",
        prefix: "fsgrv",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "creak",
        prefix: "fscrk",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "marble",
        prefix: "fsmar",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "metal",
        prefix: "fsmet",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "pavement",
        prefix: "fspav",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "rug",
        prefix: "fsrug",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "sand",
        prefix: "fssnd",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "snow",
        prefix: "fssno",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "trapdoor",
        prefix: "fstrp",
        left_walk: 1,
        right_walk: 1,
        left_run: Some(1),
        right_run: Some(1),
    },
    FootstepProfile {
        key: "echo",
        prefix: "fseko",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "reverb",
        prefix: "fsrvb",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "metal2",
        prefix: "fs3mt",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "wet",
        prefix: "fswet",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "flowers",
        prefix: "fsflw",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "glottis",
        prefix: "fsglt",
        left_walk: 2,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
    FootstepProfile {
        key: "jello",
        prefix: "fsjll",
        left_walk: 2,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
    FootstepProfile {
        key: "nick_virago",
        prefix: "fsnic",
        left_walk: 2,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
    FootstepProfile {
        key: "underwater",
        prefix: "fswtr",
        left_walk: 3,
        right_walk: 3,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "velasco",
        prefix: "fsbcn",
        left_walk: 3,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
];

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ObjectSnapshot {
    handle: i64,
    name: String,
    string_name: Option<String>,
    set_file: Option<String>,
    position: Option<Vec3>,
    range: f32,
    touchable: bool,
    visible: bool,
    interest_actor: Option<u32>,
}

#[derive(Debug)]
struct EngineContext {
    verbose: bool,
    _resources: Rc<ResourceGraph>,
    next_script_handle: u32,
    scripts: BTreeMap<u32, ScriptRecord>,
    events: Vec<String>,
    current_set: Option<SetSnapshot>,
    selected_actor: Option<String>,
    actors: BTreeMap<String, ActorSnapshot>,
    available_sets: BTreeMap<String, SetDescriptor>,
    loaded_sets: BTreeSet<String>,
    current_setups: BTreeMap<String, i32>,
    inventory: BTreeSet<String>,
    inventory_rooms: BTreeSet<String>,
    menus: BTreeMap<String, Rc<RefCell<MenuState>>>,
    actor_labels: BTreeMap<String, String>,
    actor_handles: BTreeMap<u32, String>,
    next_actor_handle: u32,
    actors_installed: bool,
    voice_effect: Option<String>,
    objects: BTreeMap<i64, ObjectSnapshot>,
    objects_by_name: BTreeMap<String, i64>,
    objects_by_actor: BTreeMap<u32, i64>,
}

impl EngineContext {
    fn new(resources: Rc<ResourceGraph>, verbose: bool) -> Self {
        let mut available_sets = BTreeMap::new();
        for meta in &resources.sets {
            let setups = meta
                .setup_slots
                .iter()
                .map(|slot| SetupInfo {
                    label: slot.label.clone(),
                    index: slot.index as i32,
                })
                .collect();
            available_sets.insert(
                meta.set_file.clone(),
                SetDescriptor {
                    variable_name: meta.variable_name.clone(),
                    display_name: meta.display_name.clone(),
                    setups,
                },
            );
        }

        EngineContext {
            verbose,
            _resources: resources,
            next_script_handle: 1,
            scripts: BTreeMap::new(),
            events: Vec::new(),
            current_set: None,
            selected_actor: None,
            actors: BTreeMap::new(),
            available_sets,
            loaded_sets: BTreeSet::new(),
            current_setups: BTreeMap::new(),
            inventory: BTreeSet::new(),
            inventory_rooms: BTreeSet::new(),
            menus: BTreeMap::new(),
            actor_labels: BTreeMap::new(),
            actor_handles: BTreeMap::new(),
            next_actor_handle: 1100,
            actors_installed: false,
            voice_effect: None,
            objects: BTreeMap::new(),
            objects_by_name: BTreeMap::new(),
            objects_by_actor: BTreeMap::new(),
        }
    }

    fn log_event(&mut self, event: impl Into<String>) {
        self.events.push(event.into());
    }

    fn ensure_menu_state(&mut self, name: &str) -> Rc<RefCell<MenuState>> {
        self.menus
            .entry(name.to_string())
            .or_insert_with(|| Rc::new(RefCell::new(MenuState::default())))
            .clone()
    }

    fn start_script(&mut self, label: String, callable: Option<RegistryKey>) -> u32 {
        let handle = self.next_script_handle;
        self.next_script_handle += 1;
        self.scripts.insert(
            handle,
            ScriptRecord {
                label: label.clone(),
                thread: None,
                yields: 0,
                callable,
            },
        );
        self.log_event(format!("script.start {label} (#{handle})"));
        handle
    }

    fn has_script_with_label(&self, label: &str) -> bool {
        self.scripts.values().any(|record| record.label == label)
    }

    fn attach_script_thread(&mut self, handle: u32, key: RegistryKey) {
        if let Some(record) = self.scripts.get_mut(&handle) {
            record.thread = Some(key);
        }
    }

    fn script_thread_key(&self, handle: u32) -> Option<&RegistryKey> {
        self.scripts
            .get(&handle)
            .and_then(|record| record.thread.as_ref())
    }

    fn increment_script_yield(&mut self, handle: u32) {
        if let Some(record) = self.scripts.get_mut(&handle) {
            record.yields = record.yields.saturating_add(1);
        }
    }

    fn script_yield_count(&self, handle: u32) -> Option<u32> {
        self.scripts.get(&handle).map(|record| record.yields)
    }

    fn script_label(&self, handle: u32) -> Option<&str> {
        self.scripts
            .get(&handle)
            .map(|record| record.label.as_str())
    }

    fn active_script_handles(&self) -> Vec<u32> {
        self.scripts.keys().copied().collect()
    }

    fn is_script_running(&self, handle: u32) -> bool {
        self.scripts.contains_key(&handle)
    }

    fn complete_script(&mut self, handle: u32) -> ScriptCleanup {
        if let Some(record) = self.scripts.remove(&handle) {
            self.log_event(format!("script.complete {} (#{handle})", record.label));
            return ScriptCleanup {
                thread: record.thread,
                callable: record.callable,
            };
        }
        ScriptCleanup::default()
    }

    fn ensure_actor_mut(&mut self, id: &str, label: &str) -> &mut ActorSnapshot {
        let entry = self.actors.entry(id.to_string()).or_insert_with(|| {
            let mut actor = ActorSnapshot::default();
            actor.name = label.to_string();
            actor.is_visible = true;
            actor
        });
        entry.name = label.to_string();
        self.actor_labels
            .entry(label.to_string())
            .or_insert_with(|| id.to_string());
        entry
    }

    fn select_actor(&mut self, id: &str, label: &str) {
        if let Some(previous) = self.selected_actor.take() {
            if let Some(actor) = self.actors.get_mut(&previous) {
                actor.is_selected = false;
            }
        }
        let actor = self.ensure_actor_mut(id, label);
        actor.is_selected = true;
        self.selected_actor = Some(id.to_string());
        self.log_event(format!("actor.select {id}"));
    }

    fn switch_to_set(&mut self, set_file: &str) {
        let set_key = set_file.to_string();
        let (variable_name, display_name) = match self.available_sets.get(&set_key) {
            Some(descriptor) => (
                descriptor.variable_name.clone(),
                descriptor.display_name.clone(),
            ),
            None => (set_key.clone(), None),
        };
        self.current_set = Some(SetSnapshot {
            set_file: set_key.clone(),
            variable_name,
            display_name,
        });
        self.current_setups.entry(set_key.clone()).or_insert(0);
        self.log_event(format!("set.switch {set_file}"));
        if set_file.eq_ignore_ascii_case("mo.set") {
            let needs_pos = self
                .actors
                .get("manny")
                .map(|actor| actor.position.is_none())
                .unwrap_or(true);
            if needs_pos {
                self.set_actor_position("manny", "Manny", MANNY_OFFICE_SEED_POS);
            }
            let needs_rot = self
                .actors
                .get("manny")
                .map(|actor| actor.rotation.is_none())
                .unwrap_or(true);
            if needs_rot {
                self.set_actor_rotation("manny", "Manny", MANNY_OFFICE_SEED_ROT);
            }
        }
    }

    fn mark_set_loaded(&mut self, set_file: &str) {
        if self.loaded_sets.insert(set_file.to_string()) {
            self.log_event(format!("set.load {set_file}"));
        }
    }

    fn record_current_setup(&mut self, set_file: &str, setup: i32) {
        self.current_setups.insert(set_file.to_string(), setup);
    }

    fn current_setup_for(&self, set_file: &str) -> Option<i32> {
        self.current_setups.get(set_file).copied()
    }

    fn set_actor_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        let actor = self.ensure_actor_mut(id, label);
        actor.costume = costume.clone();
        match costume {
            Some(name) => self.log_event(format!("actor.{id}.costume {name}")),
            None => self.log_event(format!("actor.{id}.costume <nil>")),
        }
    }

    fn set_actor_base_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        let actor = self.ensure_actor_mut(id, label);
        actor.base_costume = costume.clone();
        match costume {
            Some(name) => self.log_event(format!("actor.{id}.base_costume {name}")),
            None => self.log_event(format!("actor.{id}.base_costume <nil>")),
        }
    }

    fn actor_costume(&self, id: &str) -> Option<&str> {
        self.actors
            .get(id)
            .and_then(|actor| actor.costume.as_deref())
    }

    fn actor_base_costume(&self, id: &str) -> Option<&str> {
        self.actors
            .get(id)
            .and_then(|actor| actor.base_costume.as_deref())
    }

    fn put_actor_in_set(&mut self, id: &str, label: &str, set_file: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.current_set = Some(set_file.to_string());
        self.log_event(format!("actor.{id}.enter {set_file}"));
    }

    fn actor_at_interest(&mut self, id: &str, label: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.at_interest = true;
        self.log_event(format!("actor.{id}.at_interest"));
    }

    fn set_actor_position(&mut self, id: &str, label: &str, position: Vec3) {
        let handle = {
            let actor = self.ensure_actor_mut(id, label);
            actor.position = Some(position);
            actor.handle
        };
        self.log_event(format!(
            "actor.{id}.pos {:.3},{:.3},{:.3}",
            position.x, position.y, position.z
        ));
        if handle != 0 {
            self.update_object_position_for_actor(handle, position);
        }
    }

    fn set_actor_rotation(&mut self, id: &str, label: &str, rotation: Vec3) {
        let _handle = {
            let actor = self.ensure_actor_mut(id, label);
            actor.rotation = Some(rotation);
            actor.handle
        };
        self.log_event(format!(
            "actor.{id}.rot {:.3},{:.3},{:.3}",
            rotation.x, rotation.y, rotation.z
        ));
    }

    fn set_voice_effect(&mut self, effect: &str) {
        self.voice_effect = Some(effect.to_string());
        self.log_event(format!("prefs.voice_effect {}", effect));
    }

    fn add_inventory_item(&mut self, name: &str) {
        if self.inventory.insert(name.to_string()) {
            self.log_event(format!("inventory.add {name}"));
        }
    }

    fn register_inventory_room(&mut self, name: &str) {
        if self.inventory_rooms.insert(name.to_string()) {
            self.log_event(format!("inventory.room {name}"));
        }
    }

    fn record_sector_hit(&mut self, id: &str, label: &str, hit: SectorHit) {
        let actor = self.ensure_actor_mut(id, label);
        actor.sectors.insert(hit.kind.clone(), hit);
    }

    fn default_sector_hit(&self, actor_id: &str, requested_kind: Option<&str>) -> SectorHit {
        let normalized = requested_kind
            .map(|kind| kind.trim().to_ascii_lowercase())
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| "walk".to_string());

        if let Some(hit) = self.resolve_sector_hit(actor_id, &normalized) {
            return hit;
        }

        if actor_id.eq_ignore_ascii_case("manny") {
            match normalized.as_str() {
                "camera" | "2" => SectorHit::new(6000, "mo_mcecu", "CAMERA"),
                "hot" | "1" => SectorHit::new(6001, "mo_ddtws", "HOT"),
                "walk" | "0" => SectorHit::new(6002, "mo_walk_default", "WALK"),
                _ => SectorHit::new(
                    6003,
                    format!("manny_sector_{}", normalized),
                    normalized.to_ascii_uppercase(),
                ),
            }
        } else {
            let kind = normalized.to_ascii_uppercase();
            SectorHit::new(1000, format!("{}_sector", actor_id), kind)
        }
    }

    fn resolve_sector_hit(&self, actor_id: &str, kind: &str) -> Option<SectorHit> {
        let normalized_kind = if kind.is_empty() { "walk" } else { kind };

        if actor_id.eq_ignore_ascii_case("manny") {
            if let Some(hit) = self.manny_office_sector(normalized_kind) {
                return Some(hit);
            }
        }

        if let Some(current) = &self.current_set {
            if let Some(descriptor) = self.available_sets.get(&current.set_file) {
                if normalized_kind == "camera" || normalized_kind == "2" {
                    if let Some(current_setup) = self.current_setup_for(&current.set_file) {
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
            }
        }

        None
    }

    fn sector_hit_from_setup(&self, set_file: &str, label: &str, kind: &str) -> Option<SectorHit> {
        let descriptor = self.available_sets.get(set_file)?;
        let index = descriptor.setup_index(label)?;
        let kind_upper = match kind {
            "2" => "CAMERA".to_string(),
            "1" => "HOT".to_string(),
            "0" => "WALK".to_string(),
            other => other.to_ascii_uppercase(),
        };
        Some(SectorHit::new(index, label.to_string(), kind_upper))
    }

    fn manny_office_sector(&self, raw_kind: &str) -> Option<SectorHit> {
        let current_set = self.current_set.as_ref()?;
        if !current_set.set_file.eq_ignore_ascii_case("mo.set") {
            return None;
        }
        let normalized_kind = match raw_kind {
            "2" => "camera",
            "1" => "hot",
            "0" => "walk",
            other => other,
        };

        let manny = self.actors.get("manny")?;
        let position = manny.position.unwrap_or(MANNY_OFFICE_SEED_POS);

        enum MannyZone {
            Desk,
            Window,
            Door,
            Closet,
        }

        let zone = if position.y < 0.6 {
            MannyZone::Door
        } else if position.x > 1.15 {
            MannyZone::Closet
        } else if position.x < 0.35 {
            MannyZone::Window
        } else {
            MannyZone::Desk
        };

        let label = match (zone, normalized_kind) {
            (MannyZone::Desk, "camera") => "mo_mcecu",
            (MannyZone::Desk, "hot") => "mo_ddtws",
            (MannyZone::Desk, "walk") => "mo_ddtws",
            (MannyZone::Window, "camera") => "mo_winws",
            (MannyZone::Window, "hot") => "mo_winws",
            (MannyZone::Window, "walk") => "mo_winws",
            (MannyZone::Closet, "camera") => "mo_cornr",
            (MannyZone::Closet, "hot") => "mo_cornr",
            (MannyZone::Closet, "walk") => "mo_cornr",
            (MannyZone::Door, "camera") => "mo_mnycu",
            (MannyZone::Door, "hot") => "mo_comin",
            (MannyZone::Door, "walk") => "mo_comin",
            (_, _) => "mo_mcecu",
        };

        self.sector_hit_from_setup(&current_set.set_file, label, normalized_kind)
    }

    fn evaluate_sector_name(&self, actor_id: &str, query: &str) -> bool {
        if actor_id.eq_ignore_ascii_case("manny") {
            matches!(query, "manny" | "office" | "desk")
        } else {
            false
        }
    }

    fn find_script_handle(&self, label: &str) -> Option<u32> {
        self.scripts
            .iter()
            .find_map(|(handle, record)| (record.label == label).then_some(*handle))
    }

    fn canonicalize_actor_label(label: &str) -> String {
        let mut id = String::new();
        for ch in label.chars() {
            if ch.is_ascii_alphanumeric() {
                id.push(ch.to_ascii_lowercase());
            } else if ch.is_ascii_whitespace() || matches!(ch, '.' | '-' | '_' | ':') {
                if !id.ends_with('_') {
                    id.push('_');
                }
            }
        }
        if id.is_empty() {
            id.push_str("actor");
        }
        while id.ends_with('_') {
            id.pop();
        }
        if id.is_empty() {
            id.push_str("actor");
        }
        id
    }

    fn register_actor_with_handle(
        &mut self,
        label: &str,
        preferred_handle: Option<u32>,
    ) -> (String, u32) {
        let id = self
            .actor_labels
            .get(label)
            .cloned()
            .unwrap_or_else(|| Self::canonicalize_actor_label(label));

        let entry = self.actors.entry(id.clone()).or_insert_with(|| {
            let mut actor = ActorSnapshot::default();
            actor.name = label.to_string();
            actor.is_visible = true;
            actor
        });
        entry.name = label.to_string();

        if let Some(existing) = self.actor_labels.get(label) {
            if existing != &id {
                self.actor_labels.insert(label.to_string(), id.clone());
            }
        } else {
            self.actor_labels.insert(label.to_string(), id.clone());
        }

        let mut newly_assigned = None;
        if entry.handle == 0 {
            let handle = preferred_handle.unwrap_or_else(|| {
                let handle = self.next_actor_handle;
                self.next_actor_handle += 1;
                handle
            });
            entry.handle = handle;
            self.actor_handles.insert(handle, id.clone());
            newly_assigned = Some(handle);
        }

        let handle = entry.handle;

        if let Some(handle) = newly_assigned {
            self.log_event(format!("actor.register {} (#{handle})", label));
        }

        (id, handle)
    }

    fn mark_actors_installed(&mut self) {
        self.actors_installed = true;
    }

    fn actors_installed(&self) -> bool {
        self.actors_installed
    }

    fn register_object(&mut self, snapshot: ObjectSnapshot) {
        let handle = snapshot.handle;
        if let Some(existing) = self.objects.get(&handle) {
            if let Some(actor_handle) = existing.interest_actor {
                self.objects_by_actor.remove(&actor_handle);
            }
        }
        let interest_actor = snapshot.interest_actor;
        let name = snapshot.name.clone();
        let set_label = snapshot
            .set_file
            .clone()
            .unwrap_or_else(|| "<unknown>".to_string());
        let existed = self.objects.insert(handle, snapshot).is_some();
        self.objects_by_name.insert(name.clone(), handle);
        if let Some(actor_handle) = interest_actor {
            self.objects_by_actor.insert(actor_handle, handle);
            self.log_event(format!("object.link actor#{} -> {}", actor_handle, name));
        }
        let verb = if existed {
            "object.update"
        } else {
            "object.register"
        };
        self.log_event(format!("{verb} {name} (#{handle}) @ {set_label}"));
    }

    fn unregister_object(&mut self, handle: i64) {
        if let Some(snapshot) = self.objects.remove(&handle) {
            if let Some(actor_handle) = snapshot.interest_actor {
                self.objects_by_actor.remove(&actor_handle);
            }
            self.objects_by_name.retain(|_, value| *value != handle);
            self.log_event(format!("object.remove {} (#{handle})", snapshot.name));
        }
    }

    fn visible_object_handles(&self) -> Vec<i64> {
        if let Some(current) = &self.current_set {
            let current_file = current.set_file.as_str();
            self.objects
                .values()
                .filter(|object| {
                    object.touchable
                        && object.visible
                        && object
                            .set_file
                            .as_deref()
                            .map(|file| file.eq_ignore_ascii_case(current_file))
                            .unwrap_or(false)
                })
                .map(|object| object.handle)
                .collect()
        } else {
            Vec::new()
        }
    }

    fn record_visible_objects(&mut self, handles: &[i64]) {
        if handles.is_empty() {
            self.log_event("scene.visible <none>".to_string());
            return;
        }
        let mut names = Vec::new();
        for handle in handles {
            if let Some(object) = self.objects.get(handle) {
                if let Some(label) = &object.string_name {
                    names.push(label.clone());
                } else {
                    names.push(object.name.clone());
                }
            }
        }
        if names.is_empty() {
            self.log_event("scene.visible <unknown>".to_string());
        } else {
            self.log_event(format!("scene.visible {}", names.join(", ")));
        }
    }

    fn object_position_by_actor(&self, actor_handle: u32) -> Option<Vec3> {
        self.objects_by_actor
            .get(&actor_handle)
            .and_then(|object_handle| self.objects.get(object_handle))
            .and_then(|object| object.position)
    }

    fn update_object_position_for_actor(&mut self, actor_handle: u32, position: Vec3) {
        if let Some(object_handle) = self.objects_by_actor.get(&actor_handle).copied() {
            let mut object_name = None;
            if let Some(object) = self.objects.get_mut(&object_handle) {
                object.position = Some(position);
                object_name = Some(object.name.clone());
            }
            if let Some(name) = object_name {
                self.log_event(format!(
                    "object.actor#{}.pos {} {:.3},{:.3},{:.3}",
                    actor_handle, name, position.x, position.y, position.z
                ));
            }
        }
    }

    fn set_object_touchable(&mut self, handle: i64, touchable: bool) {
        if let Some(object) = self.objects.get_mut(&handle) {
            object.touchable = touchable;
        }
        let state = if touchable {
            "touchable"
        } else {
            "untouchable"
        };
        self.log_event(format!("object.touchable #{handle} {state}"));
    }

    fn set_object_visibility(&mut self, handle: i64, visible: bool) {
        if let Some(object) = self.objects.get_mut(&handle) {
            if object.visible != visible {
                object.visible = visible;
                let state = if visible { "visible" } else { "hidden" };
                self.log_event(format!("object.visible #{handle} {state}"));
            } else {
                object.visible = visible;
            }
        }
    }

    fn actor_position_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actor_handles
            .get(&handle)
            .and_then(|id| self.actors.get(id))
            .and_then(|actor| actor.position)
            .or_else(|| self.object_position_by_actor(handle))
    }
    fn actor_rotation_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actor_handles
            .get(&handle)
            .and_then(|id| self.actors.get(id))
            .and_then(|actor| actor.rotation)
    }

    fn set_actor_visibility(&mut self, actor_id: &str, label: &str, visible: bool) {
        let state = if visible { "visible" } else { "hidden" };
        self.log_event(format!("actor.visibility {} {state}", label));
        if let Some(actor) = self.actors.get_mut(actor_id) {
            actor.is_visible = visible;
            if let Some(object_handle) = self.objects_by_actor.get(&actor.handle).copied() {
                self.set_object_visibility(object_handle, visible);
            }
        }
    }

    fn put_actor_handle_in_set(&mut self, handle: u32, set_file: &str) {
        if let Some(id) = self.actor_handles.get(&handle).cloned() {
            if let Some(actor) = self.actors.get(&id) {
                let label = actor.name.clone();
                self.put_actor_in_set(&id, &label, set_file);
            }
        }
    }
}

pub fn run_boot_sequence(data_root: &Path, verbose: bool) -> Result<()> {
    let resources = Rc::new(
        ResourceGraph::from_data_root(data_root)
            .with_context(|| format!("loading resource graph from {}", data_root.display()))?,
    );

    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(EngineContext::new(resources, verbose)));

    install_package_path(&lua, data_root)?;
    install_globals(&lua, data_root, context.clone())?;
    load_system_script(&lua, data_root)?;
    override_boot_stubs(&lua, context.clone())?;
    call_boot(&lua, context.clone())?;
    drive_active_scripts(&lua, context.clone(), 8, 32)?;

    let snapshot = context.borrow();
    dump_runtime_summary(&snapshot);
    Ok(())
}

fn install_package_path(lua: &Lua, data_root: &Path) -> Result<()> {
    let globals = lua.globals();
    let package: Table = globals
        .get("package")
        .context("package table missing from Lua state")?;
    let current_path: String = package.get("path")?;
    let mut paths = vec![format!("{}/?.lua", data_root.display())];
    paths.push(format!("{}/?.decompiled.lua", data_root.display()));
    paths.push(current_path);
    let new_path = paths.join(";");
    package.set("path", new_path)?;
    Ok(())
}

fn install_globals(lua: &Lua, data_root: &Path, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let root = data_root.to_path_buf();
    let verbose_context = context.clone();
    let system_key = Rc::new(install_runtime_tables(lua, context.clone())?);
    install_actor_scaffold(lua, context.clone(), system_key.clone()).map_err(|err| anyhow!(err))?;
    let dofile_context = context.clone();
    let wrapped_dofile = lua.create_function(move |lua_ctx, path: String| -> LuaResult<Value> {
        if let Some(value) =
            handle_special_dofile(lua_ctx, &path, dofile_context.clone(), system_key.clone())?
        {
            if verbose_context.borrow().verbose {
                println!("[lua][dofile] handled {} via host", path);
            }
            return Ok(value);
        }
        let mut tried = Vec::new();
        let candidates = candidate_paths(&path);
        for candidate in candidates {
            let absolute = if candidate.is_absolute() {
                candidate.clone()
            } else {
                root.join(&candidate)
            };
            tried.push(absolute.clone());
            if let Some(value) = execute_script(lua_ctx, &absolute)? {
                if verbose_context.borrow().verbose {
                    println!("[lua][dofile] loaded {}", absolute.display());
                }
                return Ok(value);
            }
        }
        if verbose_context.borrow().verbose {
            println!("[lua][dofile] skipped {}", path);
            for attempt in tried {
                println!("  tried {}", attempt.display());
            }
        }
        Ok(Value::Nil)
    })?;
    globals.set("dofile", wrapped_dofile)?;

    install_logging_functions(lua, context.clone())?;
    install_engine_bindings(lua, context.clone())?;

    Ok(())
}

fn handle_special_dofile<'lua>(
    lua: &'lua Lua,
    path: &str,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<Option<Value<'lua>>> {
    if let Some(filename) = Path::new(path).file_name().and_then(|name| name.to_str()) {
        let lower = filename.to_ascii_lowercase();
        match lower.as_str() {
            "setfallback.lua" => return Ok(Some(Value::Nil)),
            "_colors.lua" | "_colors.decompiled.lua" => {
                install_color_constants(lua)?;
                return Ok(Some(Value::Nil));
            }
            "_sfx.lua" | "_sfx.decompiled.lua" => {
                install_sfx_scaffold(lua, context.clone())?;
                return Ok(Some(Value::Nil));
            }
            "_controls.lua" | "_controls.decompiled.lua" => {
                install_controls_scaffold(lua, context, system_key.clone())?;
                return Ok(Some(Value::Nil));
            }
            "_dialog.lua" | "_dialog.decompiled.lua" => {
                install_dialog_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_music.lua" | "_music.decompiled.lua" => {
                install_music_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_mouse.lua" | "_mouse.decompiled.lua" => {
                install_mouse_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_ui.lua" | "_ui.decompiled.lua" => {
                install_ui_scaffold(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "_actors.lua" | "_actors.decompiled.lua" => {
                install_actor_scaffold(lua, context, system_key.clone())?;
                return Ok(Some(Value::Nil));
            }
            "menu_loading.lua" | "menu_loading.decompiled.lua" => {
                install_loading_menu(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_boot_warning.lua" | "menu_boot_warning.decompiled.lua" => {
                install_boot_warning_menu(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_dialog.lua" | "menu_dialog.decompiled.lua" => {
                install_menu_dialog_stub(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_common.lua" | "menu_common.decompiled.lua" => {
                install_menu_common_stub(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_remap_keys.lua" | "menu_remap_keys.decompiled.lua" => {
                install_menu_remap_stub(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_prefs.lua" | "menu_prefs.decompiled.lua" => {
                install_menu_prefs_stub(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            _ => {}
        }

        if let Some(base) = lower
            .strip_suffix(".decompiled.lua")
            .or_else(|| lower.strip_suffix(".lua"))
        {
            if base.ends_with("_inv") {
                install_inventory_variant_stub(lua, context.clone(), base)
                    .map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }

            if base == "mn_scythe" {
                install_manny_scythe_stub(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
        }
    }
    Ok(None)
}

fn install_footsteps_table(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("footsteps"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    for profile in FOOTSTEP_PROFILES {
        let entry = lua.create_table()?;
        entry.set("prefix", profile.prefix)?;
        entry.set("left_walk", profile.left_walk)?;
        entry.set("right_walk", profile.right_walk)?;
        if let Some(count) = profile.left_run {
            entry.set("left_run", count)?;
        }
        if let Some(count) = profile.right_run {
            entry.set("right_run", count)?;
        }
        table.set(profile.key, entry)?;
    }

    globals.set("footsteps", table)?;
    Ok(())
}

fn install_color_constants(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    let make_color = |r: f32, g: f32, b: f32| -> LuaResult<Value> {
        let table = lua.create_table()?;
        table.set("r", r)?;
        table.set("g", g)?;
        table.set("b", b)?;
        Ok(Value::Table(table))
    };

    globals.set("White", make_color(1.0, 1.0, 1.0)?)?;
    globals.set("Yellow", make_color(1.0, 0.9, 0.2)?)?;
    globals.set("Magenta", make_color(0.9, 0.1, 0.9)?)?;
    globals.set("Aqua", make_color(0.1, 0.7, 0.9)?)?;

    Ok(())
}

fn install_sfx_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> LuaResult<()> {
    let globals = lua.globals();

    globals.set("IM_GROUP_SFX", 1)?;

    if matches!(globals.get::<_, Value>("sfx"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let sfx = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            if let Ok(name) = method.to_str() {
                fallback_context
                    .borrow_mut()
                    .log_event(format!("sfx.stub {name}"));
            }
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    sfx.set_metatable(Some(metatable));
    globals.set("sfx", sfx)?;

    Ok(())
}

fn install_controls_scaffold(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<()> {
    let globals = lua.globals();
    let system: Table = lua.registry_value(system_key.as_ref())?;

    if system
        .get::<_, Value>("controls")
        .map(|value| !matches!(value, Value::Nil))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let controls = lua.create_table()?;
    let entries = [
        ("AXIS_JOY1_X", 0),
        ("AXIS_JOY1_Y", 1),
        ("AXIS_MOUSE_X", 2),
        ("AXIS_MOUSE_Y", 3),
        ("AXIS_SENSITIVITY", 4),
        ("KEY1", 10),
        ("KEY2", 11),
        ("KEY3", 12),
        ("KEY4", 13),
        ("KEY5", 14),
        ("KEY6", 15),
        ("KEY7", 16),
        ("KEY8", 17),
        ("KEY9", 18),
        ("LCONTROLKEY", 30),
        ("RCONTROLKEY", 31),
    ];
    for (name, value) in entries {
        controls.set(name, value)?;
    }
    system.set("controls", controls)?;

    globals.set("MODE_NORMAL", 0)?;
    globals.set("MODE_MOUSE", 1)?;
    globals.set("MODE_KEYS", 2)?;
    globals.set("MODE_BACKGROUND", 3)?;
    globals.set("CONTROL_MODE", 0)?;

    globals.set("WALK", 0)?;
    globals.set("HOT", 1)?;
    globals.set("CAMERA", 2)?;

    let system_controls = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            if let Ok(name) = method.to_str() {
                fallback_context
                    .borrow_mut()
                    .log_event(format!("system_controls.stub {name}"));
            }
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    system_controls.set_metatable(Some(metatable));
    globals.set("system_controls", system_controls)?;

    system.set("axisHandler", Value::Nil)?;

    Ok(())
}

fn candidate_paths(path: &str) -> Vec<PathBuf> {
    let mut base_candidates = Vec::new();
    base_candidates.push(path.to_string());

    if path.ends_with(".lua") {
        let mut alt = path.to_string();
        alt.truncate(alt.len().saturating_sub(4));
        alt.push_str(".decompiled.lua");
        base_candidates.push(alt);
    } else if path.ends_with(".decompiled.lua") {
        let mut alt = path.to_string();
        alt.truncate(alt.len().saturating_sub(".decompiled.lua".len()));
        alt.push_str(".lua");
        base_candidates.push(alt);
    } else {
        base_candidates.push(format!("{path}.lua"));
        base_candidates.push(format!("{path}.decompiled.lua"));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|existing| existing == &candidate) {
            candidates.push(candidate);
        }
    };

    for candidate in base_candidates {
        let direct = PathBuf::from(&candidate);
        push_unique(direct.clone());
        push_unique(PathBuf::from("Scripts").join(&direct));
    }

    candidates
}

fn execute_script<'lua>(lua: &'lua Lua, path: &Path) -> LuaResult<Option<Value<'lua>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(LuaError::external)?;
    let chunk_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("script");
    let eval_result = if path.to_string_lossy().ends_with(".decompiled.lua") {
        let source = String::from_utf8_lossy(&bytes);
        let script = normalize_legacy_lua(&source);
        lua.load(&script).set_name(chunk_name).eval::<MultiValue>()
    } else if is_precompiled_chunk(&bytes) {
        lua.load(&bytes).set_name(chunk_name).eval::<MultiValue>()
    } else {
        let source = String::from_utf8_lossy(&bytes).into_owned();
        lua.load(&source).set_name(chunk_name).eval::<MultiValue>()
    };

    match eval_result {
        Ok(results) => {
            let mut iter = results.into_iter();
            if let Some(value) = iter.next() {
                Ok(Some(value))
            } else {
                Ok(Some(Value::Nil))
            }
        }
        Err(LuaError::SyntaxError { message, .. })
            if message.contains("bad header in precompiled chunk") =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn is_precompiled_chunk(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == 0x1B && bytes[1] == b'L' && bytes[2] == b'u' && bytes[3] == b'a'
}

fn install_logging_functions(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let debug_state = context.clone();
    let print_debug = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if debug_state.borrow().verbose {
                println!("[lua][PrintDebug] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    globals.set("PrintDebug", print_debug)?;

    let logf_state = context.clone();
    let logf = lua.create_function(move |_, args: Variadic<Value>| {
        if let Some(Value::String(text)) = args.get(0) {
            if logf_state.borrow().verbose {
                println!("[lua][logf] {}", text.to_str()?);
            }
        }
        Ok(())
    })?;
    globals.set("logf", logf)?;

    Ok(())
}

fn install_engine_bindings(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    if let Ok(string_table) = globals.get::<_, Table>("string") {
        if let Ok(sub) = string_table.get::<_, Function>("sub") {
            globals.set("strsub", sub.clone())?;
        }
        if let Ok(find) = string_table.get::<_, Function>("find") {
            globals.set("strfind", find.clone())?;
        }
        if let Ok(lower) = string_table.get::<_, Function>("lower") {
            globals.set("strlower", lower.clone())?;
        }
        if let Ok(upper) = string_table.get::<_, Function>("upper") {
            globals.set("strupper", upper.clone())?;
        }
        if let Ok(len) = string_table.get::<_, Function>("len") {
            globals.set("strlen", len)?;
        }
    }

    if let Ok(math_table) = globals.get::<_, Table>("math") {
        if let Ok(sqrt_fn) = math_table.get::<_, Function>("sqrt") {
            globals.set("sqrt", sqrt_fn.clone())?;
        }
        if let Ok(abs_fn) = math_table.get::<_, Function>("abs") {
            globals.set("abs", abs_fn)?;
        }
    }

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    let nil_return = lua.create_function(|_, _: Variadic<Value>| Ok(Value::Nil))?;

    globals.set(
        "LockFont",
        lua.create_function(|_, name: String| Ok(format!("font::{name}")))?,
    )?;
    globals.set(
        "LockCursor",
        lua.create_function(|_, name: String| Ok(format!("cursor::{name}")))?,
    )?;
    globals.set("TRUE", true)?;
    globals.set("FALSE", false)?;
    globals.set("SetSayLineDefaults", noop.clone())?;
    globals.set("GetPlatform", lua.create_function(|_, ()| Ok(1))?)?; // PLATFORM_PC_WIN
    globals.set("ReadRegistryValue", nil_return.clone())?;
    globals.set("ReadRegistryIntValue", nil_return.clone())?;
    globals.set("WriteRegistryValue", noop.clone())?;
    globals.set("enable_basic_remappable_key_set", noop.clone())?;
    globals.set("enable_joystick_controls", noop.clone())?;
    globals.set("enable_mouse_controls", noop.clone())?;
    globals.set(
        "AreAchievementsInstalled",
        lua.create_function(|_, ()| Ok(1))?,
    )?;
    globals.set("GlobalSaveResolved", lua.create_function(|_, ()| Ok(1))?)?;
    globals.set(
        "CheckForFile",
        lua.create_function(|_, _args: Variadic<Value>| Ok(true))?,
    )?;
    globals.set(
        "CheckForCD",
        lua.create_function(|_, _args: Variadic<Value>| Ok((false, false)))?,
    )?;
    globals.set("NukeResources", noop.clone())?;
    globals.set("GetSystemFonts", noop.clone())?;
    globals.set("PreloadCursors", noop.clone())?;
    let break_here = lua
        .load("return function(...) return coroutine.yield(...) end")
        .eval::<Function>()?;
    globals.set("break_here", break_here)?;
    globals.set("HideVerbSkull", noop.clone())?;
    let make_set_ctx = context.clone();
    globals.set(
        "MakeCurrentSet",
        lua.create_function(move |_, value: Value| {
            if let Some(set_file) = value_to_set_file(&value) {
                make_set_ctx.borrow_mut().switch_to_set(&set_file);
            } else {
                let description = describe_value(&value);
                make_set_ctx
                    .borrow_mut()
                    .log_event(format!("set.switch <unknown> ({description})"));
            }
            Ok(())
        })?,
    )?;
    let make_setup_ctx = context.clone();
    globals.set(
        "MakeCurrentSetup",
        lua.create_function(move |_, value: Value| {
            let description = describe_value(&value);
            if let Some(setup) = value_to_i32(&value) {
                let mut ctx = make_setup_ctx.borrow_mut();
                if let Some(current) = ctx.current_set.clone() {
                    let file = current.set_file.clone();
                    ctx.record_current_setup(&file, setup);
                    ctx.log_event(format!("set.setup.make {file} -> {setup}"));
                } else {
                    ctx.log_event(format!("set.setup.make <none> -> {setup}"));
                }
            } else {
                make_setup_ctx
                    .borrow_mut()
                    .log_event(format!("set.setup.make <invalid> ({description})"));
            }
            Ok(())
        })?,
    )?;
    let get_setup_ctx = context.clone();
    globals.set(
        "GetCurrentSetup",
        lua.create_function(move |_, value: Value| {
            let set_file_opt = value_to_set_file(&value);
            let (label, setup) = {
                let ctx = get_setup_ctx.borrow();
                if let Some(set_file) = set_file_opt.clone() {
                    let setup = ctx.current_setup_for(&set_file).unwrap_or(0);
                    (set_file, setup)
                } else if let Some(current) = ctx.current_set.as_ref() {
                    let file = current.set_file.clone();
                    let setup = ctx.current_setup_for(&file).unwrap_or(0);
                    (file, setup)
                } else {
                    ("<none>".to_string(), 0)
                }
            };
            {
                let mut ctx = get_setup_ctx.borrow_mut();
                ctx.log_event(format!("set.setup.get {label} -> {setup}"));
            }
            Ok(Value::Integer(setup as i64))
        })?,
    )?;
    globals.set("SetAmbientLight", noop.clone())?;
    let commentary_ctx = context.clone();
    globals.set(
        "SetActiveCommentary",
        lua.create_function(move |_, args: Variadic<Value>| {
            let enabled = args.get(0).map(value_to_bool).unwrap_or(false);
            commentary_ctx.borrow_mut().log_event(format!(
                "commentary.active {}",
                if enabled { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;
    globals.set("LightMgrSetChange", noop.clone())?;
    globals.set("HideMouseCursor", noop.clone())?;
    globals.set("ShowCursor", noop.clone())?;
    globals.set("SetShadowColor", noop.clone())?;
    globals.set("SetActiveShadow", noop.clone())?;
    globals.set("SetActorShadowPoint", noop.clone())?;
    globals.set("SetActorShadowPlane", noop.clone())?;
    globals.set("AddShadowPlane", noop.clone())?;
    let new_object_state_ctx = context.clone();
    globals.set(
        "NewObjectState",
        lua.create_function(move |_, args: Variadic<Value>| {
            let setup = args
                .get(0)
                .map(|value| describe_value(value))
                .unwrap_or_else(|| "<nil>".to_string());
            let kind = args
                .get(1)
                .map(|value| describe_value(value))
                .unwrap_or_else(|| "<nil>".to_string());
            let bitmap = args
                .get(2)
                .map(|value| value_to_string(value).unwrap_or_else(|| describe_value(value)))
                .unwrap_or_else(|| "<nil>".to_string());
            let zbitmap = args
                .get(3)
                .map(|value| value_to_string(value).unwrap_or_else(|| describe_value(value)))
                .unwrap_or_else(|| "<nil>".to_string());
            let enabled = args
                .get(4)
                .map(|value| value_to_bool(value))
                .unwrap_or(false);
            new_object_state_ctx.borrow_mut().log_event(format!(
                "object.state.new setup={setup} kind={kind} bm={bitmap} zbm={zbitmap} {}",
                if enabled { "enabled" } else { "disabled" }
            ));
            Ok(())
        })?,
    )?;
    let send_front_ctx = context.clone();
    globals.set(
        "SendObjectToFront",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut label = args
                .get(0)
                .map(|value| describe_value(value))
                .unwrap_or_else(|| "<nil>".to_string());
            let mut handle: Option<i64> = None;
            if let Some(Value::Table(table)) = args.get(0) {
                if let Some(name) = table.get::<_, Option<String>>("name").ok().flatten() {
                    label = name;
                }
                if let Some(string_name) =
                    table.get::<_, Option<String>>("string_name").ok().flatten()
                {
                    label = string_name;
                }
                handle = table
                    .get::<_, Option<i64>>("handle")
                    .ok()
                    .flatten()
                    .or_else(|| table.get::<_, Option<i64>>("object_handle").ok().flatten());
                if handle.is_none() {
                    handle = table.get::<_, Option<i64>>("hObject").ok().flatten();
                }
            }
            if handle.is_none() {
                let lookup = {
                    let ctx = send_front_ctx.borrow();
                    ctx.objects_by_name.get(&label).copied()
                };
                if let Some(found) = lookup {
                    handle = Some(found);
                }
            }
            let description = handle.map(|id| format!("{label} (#{id})")).unwrap_or(label);
            send_front_ctx
                .borrow_mut()
                .log_event(format!("object.front {description}"));
            Ok(())
        })?,
    )?;
    let constrain_ctx = context.clone();
    globals.set(
        "SetActorConstrain",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let actor = values
                .next()
                .map(|value| describe_value(&value))
                .unwrap_or_else(|| "<nil>".to_string());
            let enabled = values
                .next()
                .map(|value| value_to_bool(&value))
                .unwrap_or(false);
            constrain_ctx.borrow_mut().log_event(format!(
                "actor.constrain {actor} {}",
                if enabled { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;
    let next_script_ctx = context.clone();
    globals.set(
        "next_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            let current = args.get(0).and_then(|value| match value {
                Value::Nil => None,
                Value::Integer(i) if *i >= 0 => Some(*i as u32),
                Value::Number(n) if *n >= 0.0 => Some(*n as u32),
                _ => None,
            });
            let handles = {
                let ctx = next_script_ctx.borrow();
                let mut handles = ctx.active_script_handles();
                handles.sort_unstable();
                handles
            };
            let next = if let Some(current) = current {
                handles.into_iter().find(|handle| *handle > current)
            } else {
                handles.into_iter().next()
            };
            {
                let mut ctx = next_script_ctx.borrow_mut();
                let from = current
                    .map(|handle| format!("#{handle}"))
                    .unwrap_or_else(|| "<nil>".to_string());
                let to = next
                    .map(|handle| format!("#{handle}"))
                    .unwrap_or_else(|| "<nil>".to_string());
                ctx.log_event(format!("script.next {from} -> {to}"));
            }
            if let Some(handle) = next {
                Ok(Value::Integer(handle as i64))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;
    let identify_script_ctx = context.clone();
    globals.set(
        "identify_script",
        lua.create_function(move |lua_ctx, value: Value| {
            let handle = match value {
                Value::Nil => None,
                Value::Integer(i) if i >= 0 => Some(i as u32),
                Value::Number(n) if n >= 0.0 => Some(n as u32),
                _ => None,
            };
            if let Some(handle) = handle {
                if let Some(label) = {
                    let ctx = identify_script_ctx.borrow();
                    ctx.script_label(handle).map(|s| s.to_string())
                } {
                    return Ok(Value::String(lua_ctx.create_string(&label)?));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;
    globals.set(
        "FunctionName",
        lua.create_function(move |lua_ctx, value: Value| {
            let name = match &value {
                Value::String(text) => text.to_str()?.to_string(),
                Value::Function(func) => {
                    let pointer = func.to_pointer();
                    format!("function {pointer:p}")
                }
                Value::Thread(thread) => format!("thread {:?}", thread.status()),
                other => describe_value(other),
            };
            Ok(Value::String(lua_ctx.create_string(&name)?))
        })?,
    )?;
    globals.set("LoadCostume", noop.clone())?;
    globals.set(
        "tag",
        lua.create_function(|_, _args: Variadic<Value>| Ok(0))?,
    )?;
    globals.set("settagmethod", noop.clone())?;
    globals.set("setfallback", noop.clone())?;
    globals.set(
        "look_up_correct_costume",
        lua.create_function(|_, _args: Variadic<Value>| Ok(String::from("suit")))?,
    )?;
    globals.set("gettagmethod", nil_return.clone())?;
    globals.set("getglobal", nil_return.clone())?;
    globals.set("setglobal", noop.clone())?;
    globals.set("GlobalShrinkEnabled", false)?;
    globals.set("shrinkBoxesEnabled", false)?;
    globals.set(
        "randomseed",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    globals.set("random", lua.create_function(|_, ()| Ok(0.42))?)?;
    let visible_ctx = context.clone();
    globals.set(
        "GetVisibleThings",
        lua.create_function(move |lua_ctx, ()| {
            {
                let mut ctx = visible_ctx.borrow_mut();
                ctx.log_event("scene.get_visible_things".to_string());
            }
            let handles = {
                let ctx = visible_ctx.borrow();
                ctx.visible_object_handles()
            };
            let table = lua_ctx.create_table()?;
            for handle in &handles {
                table.set(*handle, true)?;
            }
            visible_ctx.borrow_mut().record_visible_objects(&handles);
            Ok(table)
        })?,
    )?;
    globals.set("sleep_for", noop.clone())?;
    globals.set("set_override", noop.clone())?;
    globals.set("kill_override", noop.clone())?;
    globals.set("FadeInChore", noop.clone())?;
    globals.set("START_CUT_SCENE", noop.clone())?;
    globals.set("END_CUT_SCENE", noop.clone())?;
    globals.set("wait_for_message", noop.clone())?;
    globals.set("IsMessageGoing", lua.create_function(|_, ()| Ok(false))?)?;
    globals.set(
        "Load",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;

    let actor_pos_ctx = context.clone();
    globals.set(
        "GetActorPos",
        lua.create_function(move |_, actor: Value| -> LuaResult<(f64, f64, f64)> {
            if let Some(handle) = value_to_actor_handle(&actor) {
                if let Some(pos) = actor_pos_ctx.borrow().actor_position_by_handle(handle) {
                    return Ok((pos.x as f64, pos.y as f64, pos.z as f64));
                }
            }
            Ok((0.0, 0.0, 0.0))
        })?,
    )?;

    let actor_rot_ctx = context.clone();
    globals.set(
        "GetActorRot",
        lua.create_function(move |_, actor: Value| -> LuaResult<(f64, f64, f64)> {
            if let Some(handle) = value_to_actor_handle(&actor) {
                if let Some(rot) = actor_rot_ctx.borrow().actor_rotation_by_handle(handle) {
                    return Ok((rot.x as f64, rot.y as f64, rot.z as f64));
                }
            }
            Ok((0.0, 0.0, 0.0))
        })?,
    )?;

    let angle_between_ctx = context.clone();
    globals.set(
        "GetAngleBetweenActors",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle_a = args.get(0).and_then(value_to_actor_handle);
            let handle_b = args.get(1).and_then(value_to_actor_handle);
            let (mut angle, label) = {
                let ctx = angle_between_ctx.borrow();
                if let (Some(a), Some(b)) = (handle_a, handle_b) {
                    let pos_a = ctx.actor_position_by_handle(a);
                    let pos_b = ctx.actor_position_by_handle(b);
                    if let (Some(a_pos), Some(b_pos)) = (pos_a, pos_b) {
                        let dx = (b_pos.x - a_pos.x) as f64;
                        let dy = (b_pos.y - a_pos.y) as f64;
                        let mut angle = dy.atan2(dx).to_degrees();
                        if angle < 0.0 {
                            angle += 360.0;
                        }
                        (angle, format!("#{a} -> #{b}"))
                    } else {
                        (0.0, format!("#{a} -> #{b} (no pos)"))
                    }
                } else {
                    (0.0, "<invalid>".to_string())
                }
            };
            if angle.is_nan() {
                angle = 0.0;
            }
            {
                let mut ctx = angle_between_ctx.borrow_mut();
                ctx.log_event(format!("actor.angle_between {label} -> {:.2}", angle));
            }
            Ok(angle)
        })?,
    )?;

    let put_actor_set_ctx = context.clone();
    globals.set(
        "PutActorInSet",
        lua.create_function(move |_, (actor_value, set_value): (Value, Value)| {
            if let Some(handle) = value_to_actor_handle(&actor_value) {
                let set_file = match &set_value {
                    Value::Table(table) => {
                        if let Some(value) = table.get::<_, Option<String>>("setFile")? {
                            value
                        } else if let Some(value) = table.get::<_, Option<String>>("name")? {
                            value
                        } else if let Some(value) = table.get::<_, Option<String>>("label")? {
                            value
                        } else {
                            "<unknown>".to_string()
                        }
                    }
                    Value::String(text) => text.to_str()?.to_string(),
                    _ => "<unknown>".to_string(),
                };
                put_actor_set_ctx
                    .borrow_mut()
                    .put_actor_handle_in_set(handle, &set_file);
            }
            Ok(())
        })?,
    )?;

    let prefs = lua.create_table()?;
    prefs.set("init", noop.clone())?;
    prefs.set("write", noop.clone())?;
    prefs.set(
        "read",
        lua.create_function(|_, (_this, _key): (Table, Value)| Ok(0))?,
    )?;
    let voice_context = context.clone();
    prefs.set(
        "set_voice_effect",
        lua.create_function(move |_, (_this, value): (Table, Value)| {
            let effect = match value {
                Value::String(text) => text.to_str()?.to_string(),
                Value::Nil => "OFF".to_string(),
                other => format!("{:?}", other),
            };
            voice_context.borrow_mut().set_voice_effect(&effect);
            Ok(())
        })?,
    )?;
    globals.set("system_prefs", prefs)?;

    let concept_menu = lua.create_table()?;
    concept_menu.set("unlock_concepts", noop.clone())?;
    globals.set("concept_menu", concept_menu)?;

    let inventory_state = context.clone();
    let inventory = lua.create_table()?;
    inventory.set("unordered_inventory_table", lua.create_table()?)?;
    inventory.set(
        "add_item_to_inventory",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::Table(item)) = args.get(0) {
                if let Ok(name) = item.get::<_, String>("name") {
                    inventory_state.borrow_mut().add_inventory_item(&name);
                    return Ok(());
                }
            }
            if let Some(Value::String(name)) = args.get(0) {
                inventory_state
                    .borrow_mut()
                    .add_inventory_item(name.to_str()?);
            }
            Ok(())
        })?,
    )?;
    globals.set("Inventory", inventory)?;

    let cut_scene = lua.create_table()?;
    let runtime_clone = context.clone();
    cut_scene.set(
        "logos",
        lua.create_function(move |_, ()| {
            runtime_clone
                .borrow_mut()
                .log_event("cut_scene.logos scheduled");
            Ok(())
        })?,
    )?;
    let runtime_clone = context.clone();
    cut_scene.set(
        "intro",
        lua.create_function(move |_, ()| {
            runtime_clone
                .borrow_mut()
                .log_event("cut_scene.intro scheduled");
            Ok(())
        })?,
    )?;
    globals.set("cut_scene", cut_scene)?;

    Ok(())
}

fn install_set_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> LuaResult<()> {
    let globals = lua.globals();
    let set_table: Table = globals.get("Set")?;
    let original_create: Function = set_table.get("create")?;
    let create_key = lua.create_registry_value(original_create)?;
    let wrapper_context = context.clone();
    let wrapper = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let original: Function = lua_ctx.registry_value(&create_key)?;
        let result = original.call::<_, Value>(args)?;
        if let Value::Table(set_instance) = &result {
            ensure_set_metatable(lua_ctx, &set_instance)?;
            if let Ok(Some(set_file)) = set_instance.get::<_, Option<String>>("setFile") {
                wrapper_context.borrow_mut().mark_set_loaded(&set_file);
            }
        }
        Ok(result)
    })?;
    set_table.set("create", wrapper)?;
    Ok(())
}

fn install_parent_object_hook(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> LuaResult<()> {
    let globals = lua.globals();
    let existing = match globals.get::<_, Value>("parent_object") {
        Ok(Value::Table(table)) => Some(table),
        _ => None,
    };

    let parent_table = lua.create_table()?;
    if let Some(original) = existing {
        for pair in original.pairs::<Value, Value>() {
            let (key, value) = pair?;
            parent_table.raw_set(key, value)?;
        }
    }

    let parent_context = context.clone();
    let parent_handler =
        lua.create_function(move |lua_ctx, (table, key, value): (Table, Value, Value)| {
            if let Some(handle) = value_to_object_handle(&key) {
                match value.clone() {
                    Value::Nil => {
                        parent_context.borrow_mut().unregister_object(handle);
                    }
                    Value::Table(object_table) => {
                        ensure_object_metatable(lua_ctx, &object_table)?;
                        inject_object_controls(
                            lua_ctx,
                            &object_table,
                            parent_context.clone(),
                            handle,
                        )?;
                        let snapshot = read_object_snapshot(lua_ctx, &object_table, handle)
                            .map_err(LuaError::external)?;
                        parent_context.borrow_mut().register_object(snapshot);
                    }
                    _ => {}
                }
            }
            table.raw_set(key, value)?;
            Ok(())
        })?;
    let parent_meta = lua.create_table()?;
    parent_meta.set("__newindex", parent_handler)?;
    parent_table.set_metatable(Some(parent_meta));
    globals.set("parent_object", parent_table)?;
    Ok(())
}

fn install_runtime_tables(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<RegistryKey> {
    let globals = lua.globals();

    let system = lua.create_table()?;
    system.set("setTable", lua.create_table()?)?;
    system.set("setCount", 0)?;
    globals.set("system", system.clone())?;

    let system_key = lua.create_registry_value(system.clone())?;

    install_menu_infrastructure(lua, context)?;

    Ok(system_key)
}

fn install_actor_scaffold(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<()> {
    let already_installed = {
        let borrow = context.borrow();
        borrow.actors_installed()
    };

    install_footsteps_table(lua)?;

    if already_installed {
        return Ok(());
    }

    ensure_actor_prototype(lua, context.clone(), system_key.clone())?;

    let (manny_id, manny_handle) = {
        let mut ctx = context.borrow_mut();
        ctx.register_actor_with_handle("Manny", Some(1001))
    };

    let manny_table = build_actor_table(
        lua,
        context.clone(),
        system_key.clone(),
        manny_id.clone(),
        "Manny".to_string(),
        manny_handle,
    )?;

    let globals = lua.globals();
    globals.set("manny", manny_table.clone())?;

    {
        let mut ctx = context.borrow_mut();
        ctx.select_actor(&manny_id, "Manny");
        ctx.mark_actors_installed();
    }

    let system: Table = lua.registry_value(system_key.as_ref())?;
    system.set("currentActor", manny_table.clone())?;
    if matches!(system.get::<_, Value>("rootActor"), Ok(Value::Nil)) {
        system.set("rootActor", manny_table.clone())?;
    }

    Ok(())
}

fn ensure_actor_prototype<'lua>(
    lua: &'lua Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<Table<'lua>> {
    let globals = lua.globals();
    if let Ok(actor) = globals.get::<_, Table>("Actor") {
        return Ok(actor);
    }

    let actor = lua.create_table()?;
    install_actor_methods(lua, &actor, context.clone(), system_key.clone())?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("actor.stub Actor.{}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;

    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    actor.set_metatable(Some(metatable));

    globals.set("Actor", actor.clone())?;
    Ok(actor)
}

fn install_actor_methods(
    lua: &Lua,
    actor: &Table,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
) -> LuaResult<()> {
    let create_context = context.clone();
    let create_system_key = system_key.clone();
    actor.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let mut label = None;
            for value in values.iter().rev() {
                if let Value::String(text) = value {
                    label = Some(text.to_str()?.to_string());
                    break;
                }
            }
            let label = label.unwrap_or_else(|| "actor".to_string());
            let (id, handle) = {
                let mut ctx = create_context.borrow_mut();
                ctx.register_actor_with_handle(&label, None)
            };
            build_actor_table(
                lua_ctx,
                create_context.clone(),
                create_system_key.clone(),
                id,
                label,
                handle,
            )
        })?,
    )?;

    let select_context = context.clone();
    let select_system_key = system_key.clone();
    actor.set(
        "set_selected",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                select_context.borrow_mut().select_actor(&id, &name);
                let system: Table = lua_ctx.registry_value(select_system_key.as_ref())?;
                system.set("currentActor", table)?;
            }
            Ok(())
        })?,
    )?;

    let put_context = context.clone();
    let put_system_key = system_key.clone();
    actor.set(
        "put_in_set",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                if let Some(set_value) = values.get(0) {
                    let set_file = if let Value::Table(set_table) = set_value {
                        if let Ok(Some(value)) = set_table.get::<_, Option<String>>("setFile") {
                            value
                        } else if let Ok(Some(value)) = set_table.get::<_, Option<String>>("name") {
                            value
                        } else if let Ok(Some(value)) = set_table.get::<_, Option<String>>("label")
                        {
                            value
                        } else {
                            "<unknown>".to_string()
                        }
                    } else if let Value::String(text) = set_value {
                        text.to_str()?.to_string()
                    } else {
                        "<unknown>".to_string()
                    };
                    put_context
                        .borrow_mut()
                        .put_actor_in_set(&id, &name, &set_file);
                    let system: Table = lua_ctx.registry_value(put_system_key.as_ref())?;
                    if let Ok(Value::Nil) = system.get::<_, Value>("currentActor") {
                        system.set("currentActor", table.clone())?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    let interest_context = context.clone();
    actor.set(
        "put_at_interest",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                interest_context.borrow_mut().actor_at_interest(&id, &name);
            }
            Ok(())
        })?,
    )?;

    let moveto_context = context.clone();
    actor.set(
        "moveto",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                if let Some(position) = value_slice_to_vec3(&values) {
                    let (id, name) = actor_identity(&table)?;
                    moveto_context
                        .borrow_mut()
                        .set_actor_position(&id, &name, position);
                }
            }
            Ok(())
        })?,
    )?;

    let setpos_context = context.clone();
    actor.set(
        "setpos",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                if let Some(position) = value_slice_to_vec3(&values) {
                    let (id, name) = actor_identity(&table)?;
                    setpos_context
                        .borrow_mut()
                        .set_actor_position(&id, &name, position);
                }
            }
            Ok(())
        })?,
    )?;

    let setrot_context = context.clone();
    actor.set(
        "setrot",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                if let Some(rotation) = value_slice_to_vec3(&values) {
                    let (id, name) = actor_identity(&table)?;
                    setrot_context
                        .borrow_mut()
                        .set_actor_rotation(&id, &name, rotation);
                }
            }
            Ok(())
        })?,
    )?;

    let visibility_context = context.clone();
    actor.set(
        "set_visibility",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let visible = values.get(0).map(value_to_bool).unwrap_or(false);
                table.set("is_visible", visible)?;
                let (id, name) = actor_identity(&table)?;
                visibility_context
                    .borrow_mut()
                    .set_actor_visibility(&id, &name, visible);
            }
            Ok(())
        })?,
    )?;

    let getpos_context = context.clone();
    actor.set(
        "getpos",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            let table = lua_ctx.create_table()?;
            if let Some(actor_table) = self_table {
                let (id, _name) = actor_identity(&actor_table)?;
                if let Some(snapshot) = getpos_context.borrow().actors.get(&id) {
                    if let Some(pos) = snapshot.position {
                        table.set("x", pos.x)?;
                        table.set("y", pos.y)?;
                        table.set("z", pos.z)?;
                        return Ok(table);
                    }
                }
            }
            table.set("x", 0.0)?;
            table.set("y", 0.0)?;
            table.set("z", 0.0)?;
            Ok(table)
        })?,
    )?;

    let getrot_context = context.clone();
    actor.set(
        "getrot",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            let table = lua_ctx.create_table()?;
            if let Some(actor_table) = self_table {
                let (id, _name) = actor_identity(&actor_table)?;
                if let Some(snapshot) = getrot_context.borrow().actors.get(&id) {
                    if let Some(rot) = snapshot.rotation {
                        table.set("x", rot.x)?;
                        table.set("y", rot.y)?;
                        table.set("z", rot.z)?;
                        return Ok(table);
                    }
                }
            }
            table.set("x", 0.0)?;
            table.set("y", 0.0)?;
            table.set("z", 0.0)?;
            Ok(table)
        })?,
    )?;

    let sector_type_context = context.clone();
    actor.set(
        "find_sector_type",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let requested = values.get(0).and_then(|value| value_to_string(value));
                let request_label = requested.clone().unwrap_or_else(|| "<nil>".to_string());
                let hit = {
                    let mut ctx = sector_type_context.borrow_mut();
                    let hit = ctx.default_sector_hit(&id, requested.as_deref());
                    ctx.log_event(format!(
                        "actor.sector {} {} (req={}) -> {}",
                        id, hit.kind, request_label, hit.name
                    ));
                    ctx.record_sector_hit(&id, &label, hit.clone());
                    hit
                };
                let values = vec![
                    Value::Integer(hit.id as i64),
                    Value::String(lua_ctx.create_string(&hit.name)?),
                    Value::String(lua_ctx.create_string(&hit.kind)?),
                ];
                return Ok(MultiValue::from_vec(values));
            }
            Ok(MultiValue::new())
        })?,
    )?;

    let sector_name_context = context.clone();
    actor.set(
        "find_sector_name",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _label) = actor_identity(&table)?;
                let query = values
                    .get(0)
                    .and_then(|value| value_to_string(value))
                    .unwrap_or_default();
                let result = {
                    let mut ctx = sector_name_context.borrow_mut();
                    let hit = ctx.evaluate_sector_name(&id, &query);
                    ctx.log_event(format!(
                        "actor.sector_name {} {} -> {}",
                        id,
                        query,
                        if hit { "true" } else { "false" }
                    ));
                    hit
                };
                return Ok(result);
            }
            Ok(false)
        })?,
    )?;

    let costume_context = context.clone();
    actor.set(
        "set_costume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let costume = values.get(0).and_then(|value| match value {
                    Value::String(text) => Some(text.to_str().ok()?.to_string()),
                    Value::Nil => None,
                    _ => None,
                });
                let (id, name) = actor_identity(&table)?;
                {
                    let mut ctx = costume_context.borrow_mut();
                    ctx.set_actor_base_costume(&id, &name, costume.clone());
                    ctx.set_actor_costume(&id, &name, costume.clone());
                }
                match costume {
                    Some(ref value) => {
                        table.set("base_costume", value.clone())?;
                        table.set("current_costume", value.clone())?;
                    }
                    None => {
                        table.set("base_costume", Value::Nil)?;
                        table.set("current_costume", Value::Nil)?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    let default_context = context.clone();
    actor.set(
        "default",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let costume = values.get(0).and_then(|value| match value {
                    Value::String(text) => Some(text.to_str().ok()?.to_string()),
                    Value::Nil => None,
                    _ => None,
                });
                let (id, name) = actor_identity(&table)?;
                {
                    let mut ctx = default_context.borrow_mut();
                    ctx.set_actor_base_costume(&id, &name, costume.clone());
                    ctx.set_actor_costume(&id, &name, costume.clone());
                }
                match costume {
                    Some(ref value) => {
                        table.set("base_costume", value.clone())?;
                        table.set("current_costume", value.clone())?;
                    }
                    None => {
                        table.set("base_costume", Value::Nil)?;
                        table.set("current_costume", Value::Nil)?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    let get_costume_context = context.clone();
    actor.set(
        "get_costume",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _label) = actor_identity(&table)?;
                if let Some(costume) = get_costume_context.borrow().actor_costume(&id) {
                    return Ok(Value::String(lua_ctx.create_string(costume)?));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;

    let say_context = context.clone();
    let say_system_key = system_key.clone();
    actor.set(
        "normal_say_line",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| -> LuaResult<()> {
            let (self_table, values) = split_self(args);
            if let Some(actor_table) = self_table {
                let (id, _label) = actor_identity(&actor_table)?;
                let line = values
                    .get(0)
                    .and_then(|value| value_to_string(value))
                    .unwrap_or_else(|| "<nil>".to_string());
                let options_table = values.get(1).and_then(|value| match value {
                    Value::Table(table) => Some(table.clone()),
                    _ => None,
                });

                let mut background = false;
                let mut skip_log = false;

                if let Ok(Value::Table(defaults)) = actor_table.get::<_, Value>("saylineTable") {
                    if let Ok(value) = defaults.get::<_, Value>("background") {
                        background = value_to_bool(&value);
                    }
                    if let Ok(value) = defaults.get::<_, Value>("skip_log") {
                        skip_log = value_to_bool(&value);
                    }
                }

                if let Some(options) = options_table {
                    if let Ok(value) = options.get::<_, Value>("background") {
                        background = value_to_bool(&value);
                    }
                    if let Ok(value) = options.get::<_, Value>("skip_log") {
                        skip_log = value_to_bool(&value);
                    }
                }

                {
                    let mut ctx = say_context.borrow_mut();
                    ctx.log_event(format!("dialog.say {id} {line}"));
                    if !skip_log {
                        ctx.log_event(format!("dialog.log {id} {line}"));
                    }
                }

                if !background {
                    let system: Table = lua_ctx.registry_value(say_system_key.as_ref())?;
                    system.set("lastActorTalking", actor_table.clone())?;
                }
            }
            Ok(())
        })?,
    )?;

    let complete_chore_context = context.clone();
    actor.set(
        "complete_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _label) = actor_identity(&table)?;
                let (has_costume, base_costume) = {
                    let ctx = complete_chore_context.borrow();
                    (
                        ctx.actor_costume(&id).is_some(),
                        ctx.actor_base_costume(&id).map(str::to_string),
                    )
                };
                if !has_costume {
                    return Ok(());
                }
                let chore = values
                    .get(0)
                    .and_then(|value| value_to_string(value))
                    .unwrap_or_else(|| "<nil>".to_string());
                let costume_override = values.get(1).and_then(|value| value_to_string(value));
                let costume_label = costume_override
                    .or(base_costume)
                    .unwrap_or_else(|| "<nil>".to_string());
                complete_chore_context
                    .borrow_mut()
                    .log_event(format!("actor.{id}.complete_chore {chore} {costume_label}"));
            }
            Ok(())
        })?,
    )?;

    let speak_context = context.clone();
    actor.set(
        "is_speaking",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, _name) = actor_identity(&table)?;
                let speaking = speak_context
                    .borrow()
                    .selected_actor
                    .as_deref()
                    .map(|selected| selected == id)
                    .unwrap_or(false);
                return Ok(speaking);
            }
            Ok(false)
        })?,
    )?;

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    actor.set("play_chore", noop.clone())?;
    actor.set("pop_costume", noop.clone())?;
    actor.set("head_look_at", noop.clone())?;
    actor.set("push_costume", noop.clone())?;
    actor.set("set_walk_chore", noop.clone())?;
    actor.set("set_talk_color", noop.clone())?;
    actor.set("set_mumble_chore", noop.clone())?;
    actor.set("set_talk_chore", noop.clone())?;
    actor.set("set_head", noop.clone())?;
    actor.set("set_look_rate", noop.clone())?;
    actor.set("set_collision_mode", noop.clone())?;
    actor.set("ignore_boxes", noop.clone())?;

    Ok(())
}

fn build_actor_table<'lua>(
    lua_ctx: &'lua Lua,
    context: Rc<RefCell<EngineContext>>,
    system_key: Rc<RegistryKey>,
    id: String,
    label: String,
    handle: u32,
) -> LuaResult<Table<'lua>> {
    let actor_table = lua_ctx.create_table()?;
    actor_table.set("name", label.clone())?;
    actor_table.set("id", id.clone())?;
    actor_table.set("hActor", handle as i64)?;

    actor_table.set("is_running", false)?;
    actor_table.set("is_backward", false)?;
    actor_table.set("no_idle_head", false)?;

    let actor_proto: Table = lua_ctx.globals().get("Actor")?;
    actor_table.set("parent", actor_proto.clone())?;

    let metatable = lua_ctx.create_table()?;
    metatable.set("__index", actor_proto.clone())?;
    actor_table.set_metatable(Some(metatable));

    let system: Table = lua_ctx.registry_value(system_key.as_ref())?;
    let registry: Table = match system.get("actorTable") {
        Ok(table) => table,
        Err(_) => {
            let table = lua_ctx.create_table()?;
            system.set("actorTable", table.clone())?;
            table
        }
    };

    let existing = registry
        .get::<_, Value>(label.clone())
        .unwrap_or(Value::Nil);
    if matches!(existing, Value::Nil) {
        let count: i64 = system.get("actorCount").unwrap_or(0);
        system.set("actorCount", count + 1)?;
    }

    registry.set(label.clone(), actor_table.clone())?;
    registry.set(handle as i64, actor_table.clone())?;

    {
        let mut ctx = context.borrow_mut();
        ctx.ensure_actor_mut(&id, &label);
        ctx.log_event(format!("actor.table {} (#{handle})", label));
    }

    Ok(actor_table)
}

fn split_self<'lua>(args: Variadic<Value<'lua>>) -> (Option<Table<'lua>>, Vec<Value<'lua>>) {
    let mut iter = args.into_iter();
    match iter.next() {
        Some(Value::Table(table)) => (Some(table), iter.collect()),
        Some(first) => {
            let mut values = vec![first];
            values.extend(iter);
            (None, values)
        }
        None => (None, Vec::new()),
    }
}

fn actor_identity<'lua>(table: &Table<'lua>) -> LuaResult<(String, String)> {
    let id: String = table.get("id")?;
    let name: String = table.get("name")?;
    Ok((id, name))
}

fn value_slice_to_vec3(values: &[Value]) -> Option<Vec3> {
    if values.len() >= 3 {
        let x = value_to_f32(&values[0])?;
        let y = value_to_f32(&values[1])?;
        let z = value_to_f32(&values[2])?;
        return Some(Vec3 { x, y, z });
    }
    if let Some(Value::Table(table)) = values.get(0) {
        let x = table.get::<_, f32>("x").ok()?;
        let y = table.get::<_, f32>("y").ok()?;
        let z = table.get::<_, f32>("z").ok()?;
        return Some(Vec3 { x, y, z });
    }
    None
}

fn value_to_f32(value: &Value) -> Option<f32> {
    match value {
        Value::Integer(i) => Some(*i as f32),
        Value::Number(n) => Some(*n as f32),
        _ => None,
    }
}

fn value_to_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Integer(i) => Some(*i as i32),
        Value::Number(n) => Some(*n as i32),
        Value::String(text) => text.to_str().ok()?.parse().ok(),
        _ => None,
    }
}

fn value_to_set_file(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_str().ok()?.to_string()),
        Value::Table(table) => {
            if let Ok(Some(file)) = table.get::<_, Option<String>>("setFile") {
                return Some(file);
            }
            if let Ok(Some(name)) = table.get::<_, Option<String>>("name") {
                return Some(name);
            }
            if let Ok(Some(label)) = table.get::<_, Option<String>>("label") {
                return Some(label);
            }
            None
        }
        _ => None,
    }
}

fn value_to_object_handle(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(handle) => Some(*handle),
        Value::Number(number) => Some(*number as i64),
        Value::String(text) => text.to_str().ok()?.parse().ok(),
        _ => None,
    }
}

fn value_to_actor_handle(value: &Value) -> Option<u32> {
    match value {
        Value::Integer(handle) if *handle >= 0 => Some(*handle as u32),
        Value::Number(number) if *number >= 0.0 => Some(*number as u32),
        Value::Table(table) => {
            if let Ok(Some(id)) = table.get::<_, Option<i64>>("hActor") {
                if id >= 0 {
                    return Some(id as u32);
                }
            }
            if let Ok(Some(id)) = table.get::<_, Option<u32>>("hActor") {
                return Some(id);
            }
            None
        }
        _ => None,
    }
}

fn ensure_object_metatable(lua: &Lua, object: &Table) -> LuaResult<()> {
    if let Ok(parent) = object.get::<_, Table>("parent") {
        let metatable = match object.get_metatable() {
            Some(meta) => meta,
            None => lua.create_table()?,
        };
        metatable.set("__index", parent)?;
        object.set_metatable(Some(metatable));
    }
    Ok(())
}

fn ensure_set_metatable(lua: &Lua, set_instance: &Table) -> LuaResult<()> {
    let globals = lua.globals();
    let prototype: Table = globals.get("Set")?;
    let metatable = match set_instance.get_metatable() {
        Some(meta) => meta,
        None => lua.create_table()?,
    };
    metatable.set("__index", prototype)?;
    set_instance.set_metatable(Some(metatable));
    Ok(())
}

fn inject_object_controls(
    lua: &Lua,
    object: &Table,
    context: Rc<RefCell<EngineContext>>,
    handle: i64,
) -> LuaResult<()> {
    context
        .borrow_mut()
        .log_event(format!("object.prepare #{handle}"));
    let untouchable = object
        .get::<_, Value>("make_untouchable")
        .unwrap_or(Value::Nil);
    if matches!(untouchable, Value::Nil) {
        let ctx = context.clone();
        let func = lua.create_function(move |_, (this,): (Table,)| {
            this.set("touchable", false)?;
            ctx.borrow_mut().set_object_touchable(handle, false);
            Ok(())
        })?;
        object.set("make_untouchable", func)?;
    }

    let touchable = object
        .get::<_, Value>("make_touchable")
        .unwrap_or(Value::Nil);
    if matches!(touchable, Value::Nil) {
        let ctx = context.clone();
        let func = lua.create_function(move |_, (this,): (Table,)| {
            this.set("touchable", true)?;
            ctx.borrow_mut().set_object_touchable(handle, true);
            Ok(())
        })?;
        object.set("make_touchable", func)?;
    }

    Ok(())
}

fn read_object_snapshot(_lua: &Lua, object: &Table, handle: i64) -> LuaResult<ObjectSnapshot> {
    let string_name = object.get::<_, Option<String>>("string_name")?;
    let name = object
        .get::<_, Option<String>>("name")?
        .or_else(|| string_name.clone())
        .unwrap_or_else(|| format!("object#{handle}"));
    let set_file = if let Some(set_table) = object.get::<_, Option<Table>>("obj_set")? {
        set_table.get::<_, Option<String>>("setFile")?
    } else {
        None
    };
    let obj_x = object.get::<_, Option<f32>>("obj_x")?;
    let obj_y = object.get::<_, Option<f32>>("obj_y")?;
    let obj_z = object.get::<_, Option<f32>>("obj_z")?;
    let position = match (obj_x, obj_y, obj_z) {
        (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
        _ => None,
    };
    let range = object.get::<_, Option<f32>>("range")?.unwrap_or(0.0);
    let touchable = object.get::<_, Option<bool>>("touchable")?.unwrap_or(false);
    let visible = if let Some(flag) = object.get::<_, Option<bool>>("is_visible")? {
        flag
    } else if let Some(flag) = object.get::<_, Option<bool>>("visible")? {
        flag
    } else {
        true
    };
    let interest_actor = object
        .get::<_, Value>("interest_actor")
        .ok()
        .and_then(|value| value_to_actor_handle(&value));
    Ok(ObjectSnapshot {
        handle,
        name,
        string_name,
        set_file,
        position,
        range,
        touchable,
        visible,
        interest_actor,
    })
}

fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let system_path = data_root.join("_system.decompiled.lua");
    let source = fs::read_to_string(&system_path)
        .with_context(|| format!("reading {}", system_path.display()))?;
    let normalized = normalize_legacy_lua(&source);
    let chunk = lua.load(&normalized).set_name("_system.decompiled.lua");
    chunk.exec().context("executing _system.decompiled.lua")?;
    Ok(())
}

fn override_boot_stubs(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    install_parent_object_hook(lua, context.clone()).map_err(|err| anyhow!(err))?;
    install_set_scaffold(lua, context.clone()).map_err(|err| anyhow!(err))?;
    let globals = lua.globals();
    let source_context = context.clone();
    let source_stub = lua.create_function(move |lua_ctx, ()| {
        let globals = lua_ctx.globals();
        if let Ok(load_room_code) = globals.get::<_, Function>("load_room_code") {
            let _: Value = load_room_code.call("mo.lua")?;
        } else if let Ok(dofile) = globals.get::<_, Function>("dofile") {
            let _: Value = dofile.call("mo.lua")?;
        }
        source_context.borrow_mut().mark_set_loaded("mo.set");
        Ok(())
    })?;
    globals.set("source_all_set_files", source_stub)?;

    globals.set("start_script", create_start_script(lua, context.clone())?)?;
    globals.set(
        "single_start_script",
        create_single_start_script(lua, context.clone())?,
    )?;

    let wait_context = context.clone();
    globals.set(
        "wait_for_script",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            for value in args.into_iter() {
                match value {
                    Value::Integer(handle) => {
                        wait_for_handle(lua_ctx, wait_context.clone(), handle as u32)?;
                    }
                    Value::Number(handle) => {
                        wait_for_handle(lua_ctx, wait_context.clone(), handle as u32)?;
                    }
                    Value::Function(func) => {
                        func.call::<_, ()>(MultiValue::new())?;
                    }
                    Value::Table(table) => {
                        if let Ok(func) = table.get::<_, Function>("run") {
                            func.call::<_, ()>(MultiValue::new())?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })?,
    )?;

    let find_context = context.clone();
    globals.set(
        "find_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            if let Some(Value::String(label)) = args.get(0) {
                if let Some(handle) = find_context.borrow().find_script_handle(label.to_str()?) {
                    return Ok(Value::Integer(handle as i64));
                }
            }
            Ok(Value::Nil)
        })?,
    )?;

    Ok(())
}

fn call_boot(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let boot: Function = globals
        .get("BOOT")
        .context("BOOT function missing after loading _system")?;
    boot.call::<_, ()>((false, Value::Nil))
        .context("executing BOOT(false)")?;
    if context.borrow().verbose {
        println!("[lua-runtime] BOOT completed");
    }
    Ok(())
}

fn install_menu_infrastructure(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    install_menu_constants(lua)?;
    install_render_helpers(lua, context.clone())?;
    install_game_pauser(lua, context.clone())?;
    install_game_menu(lua, context.clone())?;
    install_saveload_menu(lua, context)?;
    Ok(())
}

fn install_loading_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("loading_menu"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let menu = build_menu_instance(lua, context.clone(), Some("loading".to_string()))?;
    menu.set("autoFreeze", false)?;

    let loading_state = {
        let mut ctx = context.borrow_mut();
        ctx.ensure_menu_state("loading")
    };

    let run_context = context.clone();
    let run_state = loading_state.clone();
    let run = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, values) = split_self(args);
        if let Some(table) = self_table {
            let auto_freeze = values.get(0).map(value_to_bool).unwrap_or(false);
            table.set("autoFreeze", auto_freeze)?;

            if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
                if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                    pause_fn.call::<_, ()>((game_pauser.clone(), true))?;
                }
            }

            if let Ok(show_fn) = table.get::<_, Function>("show") {
                show_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", true)?;
            }

            if auto_freeze {
                if let Ok(single_start) =
                    lua_ctx.globals().get::<_, Function>("single_start_script")
                {
                    let freeze_fn: Function = table.get("freeze")?;
                    single_start.call::<_, u32>((freeze_fn, table.clone()))?;
                }
            }

            {
                let mut state = run_state.borrow_mut();
                state.auto_freeze = auto_freeze;
                state.last_run_mode = Some(if auto_freeze {
                    "auto".to_string()
                } else {
                    "manual".to_string()
                });
                state.visible = true;
                state.last_action = Some("run".to_string());
            }

            run_context.borrow_mut().log_event(format!(
                "loading_menu.run {}",
                if auto_freeze { "auto" } else { "manual" }
            ));
        }
        Ok(())
    })?;
    menu.set("run", run)?;

    let freeze_context = context.clone();
    let freeze_state = loading_state.clone();
    let freeze = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            if let Ok(hide_fn) = table.get::<_, Function>("hide") {
                hide_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", false)?;
            }
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
            }
        }

        if let Ok(set_mode) = lua_ctx.globals().get::<_, Function>("SetGameRenderMode") {
            set_mode.call::<_, ()>(("exit",))?;
        }

        {
            let mut state = freeze_state.borrow_mut();
            state.visible = false;
            state.last_action = Some("freeze".to_string());
        }

        freeze_context.borrow_mut().log_event("loading_menu.freeze");
        Ok(())
    })?;
    menu.set("freeze", freeze)?;

    let close_context = context.clone();
    let close_state = loading_state.clone();
    let close = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            if let Ok(hide_fn) = table.get::<_, Function>("hide") {
                hide_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", false)?;
            }
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
            }
        }

        {
            let mut state = close_state.borrow_mut();
            state.visible = false;
            state.last_action = Some("close".to_string());
        }

        close_context.borrow_mut().log_event("loading_menu.close");
        Ok(())
    })?;
    menu.set("close", close)?;

    globals.set("loading_menu", menu)?;
    Ok(())
}

fn install_boot_warning_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(
        globals.get::<_, Value>("boot_warning_menu"),
        Ok(Value::Table(_))
    ) {
        return Ok(());
    }

    let menu = build_menu_instance(lua, context.clone(), Some("boot_warning".to_string()))?;

    let boot_state = {
        let mut ctx = context.borrow_mut();
        ctx.ensure_menu_state("boot_warning")
    };

    let run_context = context.clone();
    let run_state = boot_state.clone();
    let run = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            table.set("is_visible", true)?;
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), true))?;
            }
        }

        {
            let mut state = run_state.borrow_mut();
            state.visible = true;
            state.last_action = Some("run".to_string());
        }

        run_context.borrow_mut().log_event("boot_warning_menu.run");
        Ok(())
    })?;
    menu.set("run", run)?;

    let close_context = context.clone();
    let close_state = boot_state.clone();
    let close = lua.create_function(move |lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            table.set("is_visible", false)?;
        }

        if let Ok(game_pauser) = lua_ctx.globals().get::<_, Table>("game_pauser") {
            if let Ok(pause_fn) = game_pauser.get::<_, Function>("pause") {
                pause_fn.call::<_, ()>((game_pauser.clone(), false))?;
            }
        }

        {
            let mut state = close_state.borrow_mut();
            state.visible = false;
            state.last_action = Some("close".to_string());
        }

        close_context
            .borrow_mut()
            .log_event("boot_warning_menu.close");
        Ok(())
    })?;
    menu.set("close", close)?;

    let check_context = context.clone();
    let check_state = boot_state.clone();
    let check = lua.create_function(move |_lua_ctx, args: Variadic<Value>| {
        let (self_table, _values) = split_self(args);
        if let Some(table) = self_table {
            if let Ok(close_fn) = table.get::<_, Function>("close") {
                close_fn.call::<_, ()>((table.clone(),))?;
            } else {
                table.set("is_visible", false)?;
            }
        }
        {
            let mut state = check_state.borrow_mut();
            state.last_action = Some("check_timeout".to_string());
        }
        check_context
            .borrow_mut()
            .log_event("boot_warning_menu.check_timeout");
        Ok(())
    })?;
    menu.set("check_timeout", check)?;

    globals.set("boot_warning_menu", menu)?;
    Ok(())
}

fn install_menu_dialog_stub(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("menu_dialog"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("menu_dialog.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));
    globals.set("menu_dialog", table)?;
    Ok(())
}

fn install_menu_common_stub(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("menu_common"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("menu_common.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));
    globals.set("menu_common", table)?;
    Ok(())
}

fn install_menu_remap_stub(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(
        globals.get::<_, Value>("menu_remap_keys"),
        Ok(Value::Table(_))
    ) {
        return Ok(());
    }

    let table = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("menu_remap_keys.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));
    globals.set("menu_remap_keys", table)?;
    Ok(())
}

fn install_menu_prefs_stub(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("menu_prefs"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("menu_prefs.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));
    globals.set("menu_prefs", table)?;
    Ok(())
}

fn install_dialog_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("dialog"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let dialog = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("dialog.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    dialog.set_metatable(Some(metatable));

    globals.set("dialog", dialog.clone())?;

    // Provide Sentence table placeholder so scripts referencing dialog prototypes still work.
    if matches!(globals.get::<_, Value>("Sentence"), Ok(Value::Nil) | Err(_)) {
        let sentence_context = context.clone();
        let noop = lua.create_function(move |_, _: Variadic<Value>| {
            sentence_context
                .borrow_mut()
                .log_event("dialog.sentence".to_string());
            Ok(())
        })?;
        globals.set("Sentence", noop)?;
    }

    Ok(())
}

fn install_music_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("music"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let music = lua.create_table()?;

    let start_context = context.clone();
    music.set(
        "play",
        lua.create_function(move |_, args: Variadic<Value>| {
            let track = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<unknown>".to_string());
            start_context
                .borrow_mut()
                .log_event(format!("music.play {track}"));
            Ok(())
        })?,
    )?;

    let stop_context = context.clone();
    music.set(
        "stop",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_context.borrow_mut().log_event("music.stop");
            Ok(())
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("music.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    music.set_metatable(Some(metatable));

    globals.set("music", music)?;
    Ok(())
}

fn install_mouse_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("mouse"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let mouse = lua.create_table()?;

    let mode_context = context.clone();
    mouse.set(
        "set_mode",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mode = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<none>".to_string());
            mode_context
                .borrow_mut()
                .log_event(format!("mouse.set_mode {mode}"));
            Ok(())
        })?,
    )?;

    let show_context = context.clone();
    mouse.set(
        "show",
        lua.create_function(move |_, _: Variadic<Value>| {
            show_context.borrow_mut().log_event("mouse.show");
            Ok(())
        })?,
    )?;

    let hide_context = context.clone();
    mouse.set(
        "hide",
        lua.create_function(move |_, _: Variadic<Value>| {
            hide_context.borrow_mut().log_event("mouse.hide");
            Ok(())
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("mouse.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    mouse.set_metatable(Some(metatable));

    globals.set("mouse", mouse)?;
    Ok(())
}

fn install_ui_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("UI"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let ui = lua.create_table()?;
    ui.set("screens", lua.create_table()?)?;

    let screen_ctor_context = context.clone();
    ui.set(
        "create_screen",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let name = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "anonymous".to_string());
            screen_ctor_context
                .borrow_mut()
                .log_event(format!("ui.screen.create {name}"));
            let table = lua_ctx.create_table()?;
            let fallback_context = screen_ctor_context.clone();
            let fallback =
                lua_ctx.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
                    if let Value::String(method) = key {
                        fallback_context
                            .borrow_mut()
                            .log_event(format!("ui.screen.stub {}", method.to_str()?));
                    }
                    let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
                    Ok(Value::Function(noop))
                })?;
            let metatable = lua_ctx.create_table()?;
            metatable.set("__index", fallback)?;
            table.set_metatable(Some(metatable));
            Ok(table)
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("ui.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    ui.set_metatable(Some(metatable));

    globals.set("UI", ui)?;

    let rebuild_context = context.clone();
    globals.set(
        "rebuildButtons",
        lua.create_function(move |_, _: mlua::Variadic<mlua::Value>| {
            rebuild_context
                .borrow_mut()
                .log_event("ui.rebuildButtons".to_string());
            Ok(())
        })?,
    )?;

    Ok(())
}

fn install_inventory_variant_stub(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    base: &str,
) -> Result<()> {
    let globals = lua.globals();
    let room_id = base
        .split(&['\\', '/'][..])
        .last()
        .unwrap_or(base)
        .to_string();
    context.borrow_mut().register_inventory_room(&room_id);

    // expose a stub table under the global named after the script (e.g., mn_inv)
    let global_name = room_id.replace('.', "_");

    if !matches!(
        globals.get::<_, Value>(global_name.as_str()),
        Ok(Value::Table(_))
    ) {
        let table = lua.create_table()?;
        let fallback_context = context.clone();
        let fallback_name = global_name.clone();
        let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
            if let Value::String(method) = key {
                fallback_context.borrow_mut().log_event(format!(
                    "inventory.variant.stub {}.{}",
                    fallback_name,
                    method.to_str()?
                ));
            }
            let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
            Ok(Value::Function(noop))
        })?;
        let metatable = lua.create_table()?;
        metatable.set("__index", fallback)?;
        table.set_metatable(Some(metatable));
        globals.set(global_name, table)?;
    }

    Ok(())
}

fn install_manny_scythe_stub(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("mn_scythe"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;
    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("mn_scythe.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));
    globals.set("mn_scythe", table)?;
    Ok(())
}

fn install_menu_constants(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    globals.set("CACHE_PERSISTENT", 2)?;
    globals.set("CACHE_TEMPORARY", 1)?;
    globals.set("CACHE_NEVER", 0)?;
    globals.set("MENU_MOTHERDUCK", 100)?;
    globals.set("TEXTL_MOTHERDUCK", 200)?;
    globals.set("RENDERMODE_EXITING", "exit")?;
    Ok(())
}

fn install_render_helpers(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let render_context = context.clone();
    globals.set(
        "SetGameRenderMode",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let description = values
                .get(0)
                .map(describe_value)
                .unwrap_or_else(|| "<nil>".to_string());
            render_context
                .borrow_mut()
                .log_event(format!("render.mode {description}"));
            Ok(())
        })?,
    )?;
    Ok(())
}

fn install_game_pauser(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let game_pauser = lua.create_table()?;

    let pause_context = context.clone();
    game_pauser.set(
        "pause",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let active = values.get(0).map(value_to_bool).unwrap_or(false);
            pause_context.borrow_mut().log_event(format!(
                "game_pauser.pause {}",
                if active { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;

    let resume_context = context.clone();
    game_pauser.set(
        "resume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let values = strip_self(args);
            let active = values.get(0).map(value_to_bool).unwrap_or(false);
            resume_context.borrow_mut().log_event(format!(
                "game_pauser.resume {}",
                if active { "on" } else { "off" }
            ));
            Ok(())
        })?,
    )?;

    globals.set("game_pauser", game_pauser)?;
    Ok(())
}

fn install_game_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let game_menu = lua.create_table()?;
    let menu_context = context.clone();
    game_menu.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let values = strip_self(args);
            let name = values
                .get(0)
                .and_then(value_to_string)
                .or_else(|| Some("menu".to_string()));
            build_menu_instance(lua_ctx, menu_context.clone(), name)
        })?,
    )?;
    globals.set("game_menu", game_menu)?;
    Ok(())
}

fn install_saveload_menu(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let saveload = lua.create_table()?;
    saveload.set("name", "SaveLoad")?;
    saveload.set("exit_index", 1)?;

    let menu = lua.create_table()?;
    menu.set("items", lua.create_table()?)?;
    saveload.set("menu", menu)?;

    let noop = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;

    let run_context = context.clone();
    saveload.set(
        "run",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut iter = args.into_iter();
            let _self = iter.next();
            let mode = iter
                .next()
                .as_ref()
                .map(describe_value)
                .unwrap_or_else(|| "<nil>".to_string());
            run_context
                .borrow_mut()
                .log_event(format!("saveload_menu.run {mode}"));
            Ok(())
        })?,
    )?;

    let build_context = context.clone();
    saveload.set(
        "build_menu",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let mut iter = args.into_iter();
            let self_table = match iter.next() {
                Some(Value::Table(table)) => table,
                _ => return Ok(()),
            };

            let exit_index: i64 = self_table.get("exit_index").unwrap_or(1);
            let menu: Table = match self_table.get("menu") {
                Ok(table) => table,
                Err(_) => {
                    let table = lua_ctx.create_table()?;
                    table.set("items", lua_ctx.create_table()?)?;
                    self_table.set("menu", table.clone())?;
                    table
                }
            };

            let items: Table = match menu.get("items") {
                Ok(table) => table,
                Err(_) => {
                    let table = lua_ctx.create_table()?;
                    menu.set("items", table.clone())?;
                    table
                }
            };

            let item_table: Table = match items.get(exit_index) {
                Ok(Value::Table(table)) => table,
                _ => {
                    let table = lua_ctx.create_table()?;
                    items.set(exit_index, table.clone())?;
                    table
                }
            };

            if let Some(method) = iter.next() {
                build_context.borrow_mut().log_event(format!(
                    "saveload_menu.build_menu {}",
                    describe_value(&method)
                ));
            }

            if let Err(_) = item_table.get::<_, Value>("text") {
                item_table.set("text", "")?;
            }

            Ok(())
        })?,
    )?;

    saveload.set("cancel", noop.clone())?;
    saveload.set("destroy", noop.clone())?;
    saveload.set("set_default_focus", noop.clone())?;

    let metatable = lua.create_table()?;
    let fallback = {
        let fallback_context = context.clone();
        lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
            if let Value::String(method) = key {
                let method_name = method.to_str()?.to_string();
                fallback_context
                    .borrow_mut()
                    .log_event(format!("saveload_menu.stub {method_name}"));
            }
            let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
            Ok(Value::Function(noop))
        })?
    };
    metatable.set("__index", fallback)?;
    saveload.set_metatable(Some(metatable));

    globals.set("saveload_menu", saveload)?;
    Ok(())
}

fn build_menu_instance<'lua>(
    lua_ctx: &'lua Lua,
    context: Rc<RefCell<EngineContext>>,
    name: Option<String>,
) -> LuaResult<Table<'lua>> {
    let label = name.unwrap_or_else(|| "menu".to_string());
    let menu = lua_ctx.create_table()?;
    menu.set("name", label.clone())?;
    menu.set("is_visible", false)?;

    let state = {
        let mut ctx = context.borrow_mut();
        ctx.log_event(format!("menu.create {label}"));
        let handle = ctx.ensure_menu_state(&label);
        {
            let mut guard = handle.borrow_mut();
            guard.visible = false;
            guard.auto_freeze = false;
            guard.last_run_mode = None;
            guard.last_action = Some("create".to_string());
        }
        handle
    };

    let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;

    let show_state = state.clone();
    let show_context = context.clone();
    let show_label = label.clone();
    menu.set(
        "show",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", true)?;
            }
            {
                let mut menu_state = show_state.borrow_mut();
                menu_state.visible = true;
                menu_state.last_action = Some("show".to_string());
            }
            show_context
                .borrow_mut()
                .log_event(format!("menu.show {show_label}"));
            Ok(())
        })?,
    )?;

    let hide_state = state.clone();
    let hide_context = context.clone();
    let hide_label = label.clone();
    menu.set(
        "hide",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", false)?;
            }
            {
                let mut menu_state = hide_state.borrow_mut();
                menu_state.visible = false;
                menu_state.last_action = Some("hide".to_string());
            }
            hide_context
                .borrow_mut()
                .log_event(format!("menu.hide {hide_label}"));
            Ok(())
        })?,
    )?;

    let freeze_state = state.clone();
    let freeze_context = context.clone();
    let freeze_label = label.clone();
    menu.set(
        "freeze",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, _values) = split_self(args);
            {
                let mut menu_state = freeze_state.borrow_mut();
                menu_state.last_action = Some("freeze".to_string());
            }
            freeze_context
                .borrow_mut()
                .log_event(format!("menu.freeze {freeze_label}"));
            Ok(())
        })?,
    )?;

    let close_state = state.clone();
    let close_context = context.clone();
    let close_label = label.clone();
    menu.set(
        "close",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                table.set("is_visible", false)?;
            }
            {
                let mut menu_state = close_state.borrow_mut();
                menu_state.visible = false;
                menu_state.last_action = Some("close".to_string());
            }
            close_context
                .borrow_mut()
                .log_event(format!("menu.close {close_label}"));
            Ok(())
        })?,
    )?;

    let cleanup_state = state.clone();
    let cleanup_context = context.clone();
    let cleanup_label = label.clone();
    menu.set(
        "cleanup",
        lua_ctx.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, _values) = split_self(args);
            {
                let mut menu_state = cleanup_state.borrow_mut();
                menu_state.last_action = Some("cleanup".to_string());
            }
            cleanup_context
                .borrow_mut()
                .log_event(format!("menu.cleanup {cleanup_label}"));
            Ok(())
        })?,
    )?;

    menu.set("add_image", noop.clone())?;
    menu.set("add_line", noop.clone())?;
    menu.set("setup", noop.clone())?;
    menu.set("destroy", noop.clone())?;
    menu.set("cancel", noop.clone())?;
    menu.set("refresh", noop.clone())?;
    menu.set("add_button", noop.clone())?;
    menu.set("add_slider", noop.clone())?;
    menu.set("add_toggle", noop.clone())?;
    menu.set("autoFreeze", noop.clone())?;

    let fallback = {
        let fallback_context = context.clone();
        let fallback_label = label.clone();
        lua_ctx.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
            if let Value::String(method) = key {
                let method_name = method.to_str()?.to_string();
                fallback_context
                    .borrow_mut()
                    .log_event(format!("menu.stub {fallback_label}.{method_name}"));
            }
            let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
            Ok(Value::Function(noop))
        })?
    };

    let metatable = lua_ctx.create_table()?;
    metatable.set("__index", fallback)?;
    menu.set_metatable(Some(metatable));

    Ok(menu)
}

fn dump_runtime_summary(state: &EngineContext) {
    println!("Lua runtime summary:");
    match &state.current_set {
        Some(set) => {
            let display = set.display_name.as_deref().unwrap_or(&set.variable_name);
            println!("  Current set: {} ({})", set.set_file, display);
        }
        None => println!("  Current set: <none>"),
    }
    println!(
        "  Selected actor: {}",
        state.selected_actor.as_deref().unwrap_or("<none>")
    );
    if let Some(effect) = &state.voice_effect {
        println!("  Voice effect: {}", effect);
    }
    if let Some(manny) = state.actors.get("manny") {
        if let Some(set) = &manny.current_set {
            println!("  Manny in set: {set}");
        }
        if let Some(costume) = &manny.costume {
            println!("  Manny costume: {costume}");
        }
        if let Some(pos) = manny.position {
            println!(
                "  Manny position: ({:.3}, {:.3}, {:.3})",
                pos.x, pos.y, pos.z
            );
        }
        if let Some(rot) = manny.rotation {
            println!(
                "  Manny rotation: ({:.3}, {:.3}, {:.3})",
                rot.x, rot.y, rot.z
            );
        }
        if !manny.sectors.is_empty() {
            for (kind, hit) in &manny.sectors {
                println!("  Manny sector {kind}: {} (id {})", hit.name, hit.id);
            }
        }
    }
    if !state.inventory.is_empty() {
        let mut items: Vec<_> = state.inventory.iter().collect();
        items.sort();
        let display = items
            .iter()
            .map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory: {}", display);
    }
    if !state.inventory_rooms.is_empty() {
        let mut rooms: Vec<_> = state.inventory_rooms.iter().collect();
        rooms.sort();
        let display = rooms
            .iter()
            .map(|room| room.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory rooms: {}", display);
    }
    if !state.menus.is_empty() {
        println!("  Menus:");
        for (name, menu_state) in &state.menus {
            let snapshot = menu_state.borrow();
            let visibility = if snapshot.visible {
                "visible"
            } else {
                "hidden"
            };
            let mut details = Vec::new();
            if snapshot.auto_freeze {
                details.push("autoFreeze".to_string());
            }
            if let Some(mode) = &snapshot.last_run_mode {
                details.push(format!("run={mode}"));
            }
            if let Some(action) = &snapshot.last_action {
                details.push(format!("last={action}"));
            }
            let extra = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            println!("    - {}: {}{}", name, visibility, extra);
        }
    }
    if !state.scripts.is_empty() {
        println!("  Pending scripts:");
        for (handle, record) in &state.scripts {
            println!(
                "    - {} (#{handle}) yields={}",
                record.label, record.yields
            );
        }
    }
    if !state.events.is_empty() {
        println!("  Event log:");
        for event in &state.events {
            println!("    - {event}");
        }
    }
}

fn create_start_script(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<Function<'_>> {
    let start_state = context.clone();
    let func = lua.create_function(move |lua_ctx, mut args: Variadic<Value>| {
        if args.is_empty() {
            return Ok(0u32);
        }
        let callable = args.remove(0);
        let label = describe_callable_label(&callable)?;
        let function = extract_function(lua_ctx, callable)?;
        let callable_key = if let Some(func) = function.as_ref() {
            Some(lua_ctx.create_registry_value(func.clone())?)
        } else {
            None
        };
        let handle = {
            let mut state = start_state.borrow_mut();
            state.start_script(label.clone(), callable_key)
        };
        if let Some(func) = function {
            let thread = lua_ctx.create_thread(func.clone())?;
            let thread_key = lua_ctx.create_registry_value(thread.clone())?;
            {
                let mut state = start_state.borrow_mut();
                state.attach_script_thread(handle, thread_key);
            }
            let params: Vec<Value> = args.into_iter().collect();
            let initial_args = MultiValue::from_vec(params);
            resume_script(
                lua_ctx,
                start_state.clone(),
                handle,
                Some(thread),
                Some(initial_args),
            )?;
        } else {
            let cleanup = start_state.borrow_mut().complete_script(handle);
            if let Some(key) = cleanup.thread {
                lua_ctx.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua_ctx.remove_registry_value(key)?;
            }
        }
        Ok(handle)
    })?;
    Ok(func)
}

fn create_single_start_script(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
) -> Result<Function<'_>> {
    let single_state = context.clone();
    let func = lua.create_function(move |lua_ctx, mut args: Variadic<Value>| {
        if args.is_empty() {
            return Ok(0u32);
        }
        let callable = args.remove(0);
        let label = describe_callable_label(&callable)?;
        if single_state.borrow().has_script_with_label(&label) {
            return Ok(0u32);
        }
        let function = extract_function(lua_ctx, callable)?;
        let callable_key = if let Some(func) = function.as_ref() {
            Some(lua_ctx.create_registry_value(func.clone())?)
        } else {
            None
        };
        let handle = {
            let mut state = single_state.borrow_mut();
            state.start_script(label.clone(), callable_key)
        };
        if let Some(func) = function {
            let thread = lua_ctx.create_thread(func.clone())?;
            let thread_key = lua_ctx.create_registry_value(thread.clone())?;
            {
                let mut state = single_state.borrow_mut();
                state.attach_script_thread(handle, thread_key);
            }
            let params: Vec<Value> = args.into_iter().collect();
            let initial_args = MultiValue::from_vec(params);
            resume_script(
                lua_ctx,
                single_state.clone(),
                handle,
                Some(thread),
                Some(initial_args),
            )?;
        } else {
            let cleanup = single_state.borrow_mut().complete_script(handle);
            if let Some(key) = cleanup.thread {
                lua_ctx.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua_ctx.remove_registry_value(key)?;
            }
        }
        Ok(handle)
    })?;
    Ok(func)
}

enum ScriptStep {
    Yielded,
    Completed,
}

fn resume_script(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    handle: u32,
    thread_override: Option<Thread>,
    initial_args: Option<MultiValue>,
) -> LuaResult<ScriptStep> {
    let thread = if let Some(thread) = thread_override {
        thread
    } else {
        let thread_value = {
            let state = context.borrow();
            if let Some(key) = state.script_thread_key(handle) {
                lua.registry_value::<Thread>(key)?
            } else {
                return Ok(ScriptStep::Completed);
            }
        };
        thread_value
    };

    if !matches!(thread.status(), ThreadStatus::Resumable) {
        let cleanup = {
            let mut state = context.borrow_mut();
            state.complete_script(handle)
        };
        if let Some(key) = cleanup.thread {
            lua.remove_registry_value(key)?;
        }
        if let Some(key) = cleanup.callable {
            lua.remove_registry_value(key)?;
        }
        return Ok(ScriptStep::Completed);
    }

    let resume_result = if let Some(args) = initial_args {
        thread.resume::<_, MultiValue>(args)
    } else {
        thread.resume::<_, MultiValue>(MultiValue::new())
    };

    match resume_result {
        Ok(_) => match thread.status() {
            ThreadStatus::Resumable => {
                context.borrow_mut().increment_script_yield(handle);
                Ok(ScriptStep::Yielded)
            }
            ThreadStatus::Unresumable | ThreadStatus::Error => {
                let cleanup = {
                    let mut state = context.borrow_mut();
                    state.complete_script(handle)
                };
                if let Some(key) = cleanup.thread {
                    lua.remove_registry_value(key)?;
                }
                if let Some(key) = cleanup.callable {
                    lua.remove_registry_value(key)?;
                }
                Ok(ScriptStep::Completed)
            }
        },
        Err(LuaError::CoroutineInactive) => {
            let cleanup = {
                let mut state = context.borrow_mut();
                state.complete_script(handle)
            };
            if let Some(key) = cleanup.thread {
                lua.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua.remove_registry_value(key)?;
            }
            Ok(ScriptStep::Completed)
        }
        Err(err) => {
            let label = {
                let state = context.borrow();
                state
                    .script_label(handle)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("#{handle}"))
            };
            let message = err.to_string();
            context
                .borrow_mut()
                .log_event(format!("script.error {label}: {message}"));
            let cleanup = {
                let mut state = context.borrow_mut();
                state.complete_script(handle)
            };
            if let Some(key) = cleanup.thread {
                lua.remove_registry_value(key)?;
            }
            if let Some(key) = cleanup.callable {
                lua.remove_registry_value(key)?;
            }
            Err(err)
        }
    }
}

fn wait_for_handle(lua: &Lua, context: Rc<RefCell<EngineContext>>, handle: u32) -> LuaResult<()> {
    const MAX_STEPS: u32 = 10_000;
    let mut steps = 0;
    while context.borrow().is_script_running(handle) {
        resume_script(lua, context.clone(), handle, None, None)?;
        steps += 1;
        if steps >= MAX_STEPS {
            let label = {
                let state = context.borrow();
                state
                    .script_label(handle)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("#{handle}"))
            };
            return Err(LuaError::external(format!(
                "wait_for_script exceeded {MAX_STEPS} resumes for {label}"
            )));
        }
    }
    Ok(())
}

fn drive_active_scripts(
    lua: &Lua,
    context: Rc<RefCell<EngineContext>>,
    max_passes: usize,
    max_yields_per_script: u32,
) -> LuaResult<()> {
    for _ in 0..max_passes {
        let handles = {
            let state = context.borrow();
            state.active_script_handles()
        };
        if handles.is_empty() {
            break;
        }
        let mut progressed = false;
        for handle in handles {
            let yield_count = {
                let state = context.borrow();
                state.script_yield_count(handle).unwrap_or(0)
            };
            if yield_count >= max_yields_per_script {
                continue;
            }
            match resume_script(lua, context.clone(), handle, None, None)? {
                ScriptStep::Yielded | ScriptStep::Completed => {
                    progressed = true;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    Ok(())
}

fn extract_function<'lua>(lua: &'lua Lua, value: Value<'lua>) -> LuaResult<Option<Function<'lua>>> {
    match value {
        Value::Function(f) => Ok(Some(f)),
        Value::String(name) => {
            let globals = lua.globals();
            let func: Function = globals.get(name.to_str()?)?;
            Ok(Some(func))
        }
        Value::Table(table) => {
            if let Ok(func) = table.get::<_, Function>("run") {
                Ok(Some(func))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

fn strip_self(args: Variadic<Value>) -> Vec<Value> {
    let mut iter = args.into_iter();
    match iter.next() {
        Some(Value::Table(_)) => iter.collect(),
        Some(value) => {
            let mut values = vec![value];
            values.extend(iter);
            values
        }
        None => Vec::new(),
    }
}

fn describe_function(func: &Function) -> String {
    let info = func.info();
    if let Some(name) = info.name.clone() {
        if !name.is_empty() {
            return name;
        }
    }
    if let Some(short) = info.short_src.clone() {
        if let Some(line) = info.line_defined {
            if line > 0 {
                return format!("{short}:{line}");
            }
        }
        return format!("function@{short}");
    }
    if let Some(source) = info.source.clone() {
        if let Some(line) = info.line_defined {
            if line > 0 {
                return format!("{source}:{line}");
            }
        }
        return format!("function@{source}");
    }
    match info.what {
        "C" => "<cfunction>".to_string(),
        other => format!("<{other}>"),
    }
}

fn describe_callable_label(value: &Value) -> LuaResult<String> {
    match value {
        Value::Function(func) => Ok(describe_function(func)),
        Value::String(s) => Ok(s.to_str()?.to_string()),
        Value::Table(table) => {
            if let Ok(name) = table.get::<_, String>("name") {
                if !name.is_empty() {
                    return Ok(name);
                }
            }
            if let Ok(label) = table.get::<_, String>("label") {
                if !label.is_empty() {
                    return Ok(label);
                }
            }
            if let Ok(func) = table.get::<_, Function>("run") {
                return Ok(describe_function(&func));
            }
            Ok(format!("table@{:p}", table.to_pointer()))
        }
        Value::Nil => Ok("<nil>".to_string()),
        other => Ok(describe_value(other)),
    }
}

fn value_to_bool(value: &Value) -> bool {
    match value {
        Value::Boolean(flag) => *flag,
        Value::Integer(i) => *i != 0,
        Value::Number(n) => *n != 0.0,
        Value::String(s) => s
            .to_str()
            .map(|text| text != "0" && text != "false")
            .unwrap_or(false),
        _ => false,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => text.to_str().ok().map(|s| s.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn describe_value(value: &Value) -> String {
    value_to_string(value).unwrap_or_else(|| format!("<{value:?}>"))
}

#[cfg(test)]
mod tests {
    use super::{candidate_paths, value_slice_to_vec3, EngineContext, ObjectSnapshot, Vec3};
    use grim_analysis::resources::{ResourceGraph, SetMetadata, SetupSlot};
    use mlua::Value;
    use std::path::PathBuf;
    use std::rc::Rc;

    #[test]
    fn candidate_paths_cover_decompiled_variants() {
        let mut paths = candidate_paths("setfallback.lua");
        paths.sort();
        assert!(paths.contains(&PathBuf::from("setfallback.lua")));
        assert!(paths.contains(&PathBuf::from("setfallback.decompiled.lua")));
        assert!(paths.contains(&PathBuf::from("Scripts/setfallback.lua")));
    }

    #[test]
    fn value_slice_to_vec3_reads_numeric_values() {
        let values = vec![Value::Number(1.0), Value::Integer(2), Value::Number(3.5)];
        let vec = value_slice_to_vec3(&values).expect("vector parsed");
        assert!((vec.x - 1.0).abs() < f32::EPSILON);
        assert!((vec.y - 2.0).abs() < f32::EPSILON);
        assert!((vec.z - 3.5).abs() < f32::EPSILON);
    }

    fn make_context() -> EngineContext {
        let set_metadata = SetMetadata {
            lua_file: "mo.lua".to_string(),
            variable_name: "mo".to_string(),
            set_file: "mo.set".to_string(),
            display_name: Some("Manny's Office".to_string()),
            setup_slots: vec![
                SetupSlot {
                    label: "mo_ddtws".to_string(),
                    index: 0,
                },
                SetupSlot {
                    label: "mo_ddtws2".to_string(),
                    index: 0,
                },
                SetupSlot {
                    label: "mo_winws".to_string(),
                    index: 1,
                },
                SetupSlot {
                    label: "mo_winws2".to_string(),
                    index: 1,
                },
                SetupSlot {
                    label: "mo_comin".to_string(),
                    index: 2,
                },
                SetupSlot {
                    label: "mo_cornr".to_string(),
                    index: 3,
                },
                SetupSlot {
                    label: "overhead".to_string(),
                    index: 4,
                },
                SetupSlot {
                    label: "mo_mcecu".to_string(),
                    index: 5,
                },
                SetupSlot {
                    label: "mo_mnycu".to_string(),
                    index: 6,
                },
            ],
            methods: Vec::new(),
        };
        let mut graph = ResourceGraph::default();
        graph.sets.push(set_metadata);
        EngineContext::new(Rc::new(graph), false)
    }

    fn prepare_manny(ctx: &mut EngineContext, position: Vec3) {
        let (id, _handle) = ctx.register_actor_with_handle("Manny", Some(1001));
        ctx.put_actor_in_set(&id, "Manny", "mo.set");
        ctx.switch_to_set("mo.set");
        ctx.set_actor_position(&id, "Manny", position);
    }

    #[test]
    fn manny_camera_defaults_to_desk_zone() {
        let mut ctx = make_context();
        prepare_manny(
            &mut ctx,
            Vec3 {
                x: 0.62,
                y: 2.05,
                z: 0.0,
            },
        );
        let hit = ctx.manny_office_sector("camera").expect("desk sector");
        assert_eq!(hit.name, "mo_mcecu");
    }

    #[test]
    fn manny_near_door_selects_entry_sector() {
        let mut ctx = make_context();
        prepare_manny(
            &mut ctx,
            Vec3 {
                x: 1.35,
                y: 0.2,
                z: 0.0,
            },
        );
        let camera_hit = ctx.manny_office_sector("camera").expect("door camera");
        assert_eq!(camera_hit.name, "mo_mnycu");
        let hot_hit = ctx.manny_office_sector("hot").expect("door hot");
        assert_eq!(hot_hit.name, "mo_comin");
    }

    #[test]
    fn actor_visibility_controls_object_handles() {
        let mut ctx = make_context();
        let (id, handle) = ctx.register_actor_with_handle("Lamp", Some(2000));
        ctx.put_actor_in_set(&id, "Lamp", "mo.set");
        ctx.switch_to_set("mo.set");
        ctx.register_object(ObjectSnapshot {
            handle: 3000,
            name: "lamp".to_string(),
            string_name: Some("lamp".to_string()),
            set_file: Some("mo.set".to_string()),
            position: None,
            range: 0.0,
            touchable: true,
            visible: true,
            interest_actor: Some(handle),
        });

        assert_eq!(ctx.visible_object_handles(), vec![3000]);

        ctx.set_actor_visibility(&id, "Lamp", false);
        assert!(ctx.visible_object_handles().is_empty());

        ctx.set_actor_visibility(&id, "Lamp", true);
        assert_eq!(ctx.visible_object_handles(), vec![3000]);
    }
}
