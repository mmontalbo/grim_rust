use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

mod actors;
mod audio;
mod cutscenes;
mod geometry;
mod geometry_export;
mod inventory;
mod menus;
mod pause;

use actors::{ActorSnapshot, ActorStore};
pub use audio::AudioCallback;
use audio::{
    install_music_scaffold, AudioRuntime, MusicState, SfxState, FOOTSTEP_PROFILES,
    IM_SOUND_PLAY_COUNT, IM_SOUND_VOL,
};
use cutscenes::{CommentaryRecord, CutsceneRuntime, DialogState};
use geometry::{ParsedSetGeometry, SectorHit, SetDescriptor, SetSnapshot, SetupInfo};
use inventory::InventoryState;
use menus::{
    install_boot_warning_menu, install_dialog_scaffold, install_loading_menu, install_menu_common,
    install_menu_dialog, install_menu_infrastructure, install_menu_prefs, install_menu_remap,
    MenuRegistry, MenuState,
};
use pause::{PauseLabel, PauseState};

use super::types::{Vec3, MANNY_OFFICE_SEED_POS, MANNY_OFFICE_SEED_ROT};
use crate::geometry_snapshot::LuaGeometrySnapshot;
use crate::lab_collection::LabCollection;
use anyhow::{anyhow, Context, Result};
use grim_analysis::resources::{normalize_legacy_lua, ResourceGraph};
use grim_formats::{SectorKind as SetSectorKind, SetFile as SetFileData};
use mlua::{
    Error as LuaError, Function, Lua, MultiValue, RegistryKey, Result as LuaResult, Table, Thread,
    ThreadStatus, Value, Variadic,
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
#[derive(Debug)]
enum SectorToggleResult {
    Applied {
        set_file: String,
        sector: String,
        known_sector: bool,
    },
    NoChange {
        set_file: String,
        sector: String,
        known_sector: bool,
    },
    NoSet,
}

#[derive(Debug, Default, Clone)]
struct AchievementState {
    eligible: bool,
    established: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ObjectSectorRef {
    name: String,
    kind: SetSectorKind,
}

#[derive(Debug, Clone)]
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
    sectors: Vec<ObjectSectorRef>,
}

#[derive(Debug, Clone)]
struct VisibleObjectInfo {
    handle: i64,
    name: String,
    string_name: Option<String>,
    range: f32,
    distance: Option<f32>,
    angle: Option<f32>,
    within_range: Option<bool>,
    in_hotlist: bool,
}

impl VisibleObjectInfo {
    fn display_name(&self) -> &str {
        self.string_name.as_deref().unwrap_or(self.name.as_str())
    }
}

#[derive(Clone)]
pub struct EngineContextHandle {
    inner: Rc<RefCell<EngineContext>>,
}

impl EngineContextHandle {
    pub fn new(inner: Rc<RefCell<EngineContext>>) -> Self {
        Self { inner }
    }

    pub fn resolve_actor_handle(&self, candidates: &[&str]) -> Option<(u32, String)> {
        self.inner
            .borrow()
            .resolve_actor_handle(candidates)
            .map(|(handle, id)| (handle, id.clone()))
    }

    pub fn walk_actor_vector(
        &self,
        handle: u32,
        delta: Vec3,
        adjust_y: Option<f32>,
        heading_offset: Option<f32>,
    ) -> bool {
        self.inner
            .borrow_mut()
            .walk_actor_vector(handle, delta, adjust_y, heading_offset)
    }

    pub fn log_event(&self, event: impl Into<String>) {
        self.inner.borrow_mut().log_event(event);
    }

    pub fn actor_position(&self, handle: u32) -> Option<Vec3> {
        self.inner.borrow().actor_position_by_handle(handle)
    }

    pub fn actor_rotation_y(&self, handle: u32) -> Option<f32> {
        self.inner
            .borrow()
            .actor_rotation_by_handle(handle)
            .map(|rot| rot.y)
    }

    pub fn geometry_sector_name(&self, actor_id: &str, kind: &str) -> Option<String> {
        self.inner.borrow().geometry_sector_name(actor_id, kind)
    }

    pub fn actor_costume(&self, actor: &str) -> Option<String> {
        self.inner
            .borrow()
            .actor_costume(actor)
            .map(|costume| costume.to_string())
    }

    pub fn is_message_active(&self) -> bool {
        self.inner.borrow().is_message_active()
    }

    pub fn run_scripts(
        &self,
        lua: &Lua,
        max_passes: usize,
        max_yields_per_script: u32,
    ) -> LuaResult<()> {
        drive_active_scripts(lua, self.inner.clone(), max_passes, max_yields_per_script)
    }
}

pub(super) struct EngineContext {
    verbose: bool,
    _resources: Rc<ResourceGraph>,
    next_script_handle: u32,
    scripts: BTreeMap<u32, ScriptRecord>,
    events: Vec<String>,
    current_set: Option<SetSnapshot>,
    actors: ActorStore,
    available_sets: BTreeMap<String, SetDescriptor>,
    loaded_sets: BTreeSet<String>,
    current_setups: BTreeMap<String, i32>,
    inventory: InventoryState,
    menus: MenuRegistry,
    voice_effect: Option<String>,
    objects: BTreeMap<i64, ObjectSnapshot>,
    objects_by_name: BTreeMap<String, i64>,
    objects_by_actor: BTreeMap<u32, i64>,
    achievements: BTreeMap<String, AchievementState>,
    visible_objects: Vec<VisibleObjectInfo>,
    hotlist_handles: Vec<i64>,
    cutscenes: CutsceneRuntime,
    pause: PauseState,
    audio: AudioRuntime,
    lab_collection: Option<Rc<LabCollection>>,
    set_geometry: BTreeMap<String, ParsedSetGeometry>,
    sector_states: BTreeMap<String, BTreeMap<String, bool>>,
}

impl EngineContext {
    pub(super) fn new(
        resources: Rc<ResourceGraph>,
        verbose: bool,
        lab_collection: Option<Rc<LabCollection>>,
        audio_callback: Option<Rc<dyn AudioCallback>>,
    ) -> Self {
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
            actors: ActorStore::new(1100),
            available_sets,
            loaded_sets: BTreeSet::new(),
            current_setups: BTreeMap::new(),
            inventory: InventoryState::new(),
            menus: MenuRegistry::new(),
            voice_effect: None,
            objects: BTreeMap::new(),
            objects_by_name: BTreeMap::new(),
            objects_by_actor: BTreeMap::new(),
            achievements: BTreeMap::new(),
            visible_objects: Vec::new(),
            hotlist_handles: Vec::new(),
            cutscenes: CutsceneRuntime::new(),
            pause: PauseState::default(),
            audio: AudioRuntime::new(audio_callback),
            lab_collection,
            set_geometry: BTreeMap::new(),
            sector_states: BTreeMap::new(),
        }
    }

    pub(super) fn log_event(&mut self, event: impl Into<String>) {
        self.events.push(event.into());
    }

    pub(super) fn pause_state(&self) -> &PauseState {
        &self.pause
    }

    pub(super) fn handle_pause_request(&mut self, label: PauseLabel, active: bool) {
        self.pause.record(label, active);
        let verb = if active { "on" } else { "off" };
        self.log_event(format!("game_pauser.{} {}", label.as_str(), verb));
    }

    fn push_cut_scene(&mut self, label: Option<String>, flags: Vec<String>) {
        let set_file = self
            .current_set
            .as_ref()
            .map(|snapshot| snapshot.set_file.clone());
        let sector_hit = set_file.as_ref().and_then(|_| {
            self.geometry_sector_hit("manny", "hot")
                .or_else(|| self.geometry_sector_hit("manny", "walk"))
        });
        let sector = sector_hit.as_ref().map(|hit| hit.name.clone());
        let suppressed = match (&set_file, &sector) {
            (Some(set), Some(name)) => !self.is_sector_active(set, name),
            _ => false,
        };
        let message = self
            .cutscenes
            .push_cut_scene(label, flags, set_file, sector, suppressed);
        self.log_event(message);
    }

    fn pop_cut_scene(&mut self) {
        if let Some(message) = self.cutscenes.pop_cut_scene() {
            self.log_event(message);
        }
    }

    fn push_override(&mut self, description: String) {
        let message = self.cutscenes.push_override(description);
        self.log_event(message);
    }

    fn pop_override(&mut self) -> bool {
        if let Some(message) = self.cutscenes.pop_override() {
            self.log_event(message);
            true
        } else {
            false
        }
    }

    fn clear_overrides(&mut self) {
        for message in self.cutscenes.take_all_overrides() {
            self.log_event(message);
        }
    }

    fn begin_dialog_line(&mut self, id: &str, label: &str, line: &str) {
        let actor = self.ensure_actor_mut(id, label);
        actor.speaking = true;
        actor.last_line = Some(line.to_string());
        let record = DialogState {
            actor_id: id.to_string(),
            actor_label: label.to_string(),
            line: line.to_string(),
        };
        self.log_event(format!("dialog.begin {} {}", id, line));
        self.cutscenes.set_dialog_state(record);
    }

    fn finish_dialog_line(&mut self, expected_actor: Option<&str>) -> Option<DialogState> {
        let should_finish = match (self.cutscenes.active_dialog(), expected_actor) {
            (None, _) => false,
            (Some(state), Some(expected)) => state.actor_id.eq_ignore_ascii_case(expected),
            (Some(_), None) => true,
        };
        if !should_finish {
            return None;
        }
        let record = self.cutscenes.take_active_dialog();
        if let Some(state) = &record {
            if let Some(actor) = self.actors.get_mut(&state.actor_id) {
                actor.speaking = false;
            }
            self.log_event(format!("dialog.end {} {}", state.actor_id, state.line));
        } else {
            self.log_event("dialog.end <none>".to_string());
        }
        self.cutscenes.clear_dialog_flags();
        record
    }

    pub(super) fn is_message_active(&self) -> bool {
        self.cutscenes.is_message_active()
    }

    fn speaking_actor(&self) -> Option<&str> {
        self.cutscenes.speaking_actor()
    }

    fn play_music(&mut self, track: String, params: Vec<String>) {
        let event = self.audio.play_music(track, params);
        self.log_event(event);
    }

    fn queue_music(&mut self, track: String, params: Vec<String>) {
        let event = self.audio.queue_music(track, params);
        self.log_event(event);
    }

    fn stop_music(&mut self, mode: Option<String>) {
        let event = self.audio.stop_music(mode);
        self.log_event(event);
    }

    fn pause_music(&mut self) {
        let event = self.audio.pause_music();
        self.log_event(event);
    }

    fn resume_music(&mut self) {
        let event = self.audio.resume_music();
        self.log_event(event);
    }

    fn set_music_state(&mut self, state: Option<String>) {
        let event = self.audio.set_music_state(state);
        self.log_event(event);
    }

    fn push_music_state(&mut self, state: Option<String>) {
        let event = self.audio.push_music_state(state);
        self.log_event(event);
    }

    fn pop_music_state(&mut self) {
        let event = self.audio.pop_music_state();
        self.log_event(event);
    }

    fn mute_music_group(&mut self, group: Option<String>) {
        let event = self.audio.mute_music_group(group);
        self.log_event(event);
    }

    fn unmute_music_group(&mut self, group: Option<String>) {
        let event = self.audio.unmute_music_group(group);
        self.log_event(event);
    }

    fn set_music_volume(&mut self, volume: Option<f32>) {
        let event = self.audio.set_music_volume(volume);
        self.log_event(event);
    }

    fn play_sound_effect(&mut self, cue: String, params: Vec<String>) -> String {
        let (handle, event) = self.audio.play_sound_effect(cue, params);
        self.log_event(event);
        handle
    }

    fn stop_sound_effect(&mut self, target: Option<String>) {
        let event = self.audio.stop_sound_effect(target);
        self.log_event(event);
    }

    fn start_imuse_sound(&mut self, cue: String, priority: Option<i32>, group: Option<i32>) -> i64 {
        let mut params = Vec::new();
        if let Some(value) = priority {
            params.push(format!("priority={value}"));
        }
        if let Some(value) = group {
            params.push(format!("group={value}"));
        }
        let handle = self.play_sound_effect(cue, params);
        if let Some(instance) = self.audio.sfx_mut().active.get_mut(&handle) {
            instance.group = group;
            instance.play_count = 1;
            instance.numeric
        } else {
            -1
        }
    }

    fn stop_sound_effect_by_numeric(&mut self, numeric: i64) {
        let event = self.audio.stop_sound_effect_by_numeric(numeric);
        self.log_event(event);
    }

    fn set_sound_param(&mut self, numeric: i64, param: i32, value: i32) {
        if let Some(event) = self.audio.set_sound_param(numeric, param, value) {
            self.log_event(event);
        }
    }

    fn get_sound_param(&self, numeric: i64, param: i32) -> Option<i32> {
        self.audio.get_sound_param(numeric, param)
    }

    fn music_state(&self) -> &MusicState {
        self.audio.music()
    }

    fn sfx_state(&self) -> &SfxState {
        self.audio.sfx()
    }

    fn ensure_menu_state(&mut self, name: &str) -> Rc<RefCell<MenuState>> {
        self.menus.ensure(name)
    }

    fn set_achievement_eligibility(&mut self, id: &str, eligible: bool) {
        let entry = self
            .achievements
            .entry(id.to_string())
            .or_insert_with(AchievementState::default);
        entry.eligible = eligible;
        entry.established = true;
        let state = if eligible { "eligible" } else { "ineligible" };
        self.log_event(format!("achievement.{id} {state}"));
    }

    fn achievement_is_eligible(&self, id: &str) -> bool {
        self.achievements
            .get(id)
            .map(|state| state.eligible)
            .unwrap_or(false)
    }

    fn achievement_has_been_established(&self, id: &str) -> bool {
        self.achievements
            .get(id)
            .map(|state| state.established)
            .unwrap_or(false)
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
        self.actors.ensure_actor_mut(id, label)
    }

    fn select_actor(&mut self, id: &str, label: &str) {
        self.actors.select_actor(id, label);
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
        self.load_set_geometry(set_file);
    }

    fn load_set_geometry(&mut self, set_file: &str) {
        if self.set_geometry.contains_key(set_file) {
            return;
        }
        let Some(collection) = &self.lab_collection else {
            return;
        };
        match collection.find_entry(set_file) {
            Some((archive, entry)) => {
                let bytes = archive.read_entry_bytes(entry);
                match SetFileData::parse(&bytes) {
                    Ok(file) => {
                        let geometry = ParsedSetGeometry::from_set_file(file);
                        if geometry.has_geometry() {
                            if self.verbose {
                                self.log_event(format!(
                                    "set.geometry {set_file} sectors={} setups={}",
                                    geometry.sectors.len(),
                                    geometry.setups.len()
                                ));
                            }
                            self.sector_states
                                .entry(set_file.to_string())
                                .or_insert_with(|| {
                                    let mut map = BTreeMap::new();
                                    for sector in &geometry.sectors {
                                        map.insert(sector.name.clone(), sector.default_active);
                                    }
                                    map
                                });
                            self.set_geometry.insert(set_file.to_string(), geometry);
                        } else if self.verbose {
                            eprintln!(
                                "[grim_engine] info: {} contained no geometry data",
                                set_file
                            );
                        }
                    }
                    Err(err) => {
                        if self.verbose {
                            eprintln!(
                                "[grim_engine] warning: failed to parse {}: {:?}",
                                set_file, err
                            );
                        }
                    }
                }
            }
            None => {
                if self.verbose {
                    eprintln!(
                        "[grim_engine] info: no LAB entry for {} when loading geometry",
                        set_file
                    );
                }
            }
        }
    }

    fn ensure_sector_state_map(&mut self, set_file: &str) -> bool {
        if !self.sector_states.contains_key(set_file) {
            if !self.set_geometry.contains_key(set_file) {
                self.load_set_geometry(set_file);
            }
            let mut map = BTreeMap::new();
            if let Some(geometry) = self.set_geometry.get(set_file) {
                for sector in &geometry.sectors {
                    map.insert(sector.name.clone(), sector.default_active);
                }
            }
            self.sector_states.insert(set_file.to_string(), map);
        } else if let Some(geometry) = self.set_geometry.get(set_file) {
            let entries: Vec<(String, bool)> = geometry
                .sectors
                .iter()
                .map(|sector| (sector.name.clone(), sector.default_active))
                .collect();
            if let Some(states) = self.sector_states.get_mut(set_file) {
                for (name, default_active) in entries {
                    states.entry(name).or_insert(default_active);
                }
            }
        }
        self.set_geometry.contains_key(set_file)
    }

    fn canonical_sector_name(&self, set_file: &str, sector: &str) -> Option<String> {
        let lower = sector.to_ascii_lowercase();
        if let Some(geometry) = self.set_geometry.get(set_file) {
            if let Some(poly) = geometry
                .sectors
                .iter()
                .find(|poly| poly.name.to_ascii_lowercase() == lower)
            {
                return Some(poly.name.clone());
            }
        }
        self.sector_states.get(set_file).and_then(|map| {
            map.keys()
                .find(|name| name.to_ascii_lowercase() == lower)
                .cloned()
        })
    }

    fn set_sector_active(
        &mut self,
        set_file_hint: Option<&str>,
        sector_name: &str,
        active: bool,
    ) -> SectorToggleResult {
        let set_file = match set_file_hint {
            Some(file) if !file.is_empty() => file.to_string(),
            _ => match self.current_set.as_ref() {
                Some(snapshot) => snapshot.set_file.clone(),
                None => return SectorToggleResult::NoSet,
            },
        };

        let has_geometry = self.ensure_sector_state_map(&set_file);
        let canonical = self
            .canonical_sector_name(&set_file, sector_name)
            .unwrap_or_else(|| sector_name.to_string());
        let known_sector = if has_geometry {
            self.set_geometry
                .get(&set_file)
                .map(|geometry| {
                    geometry
                        .sectors
                        .iter()
                        .any(|poly| poly.name.eq_ignore_ascii_case(&canonical))
                })
                .unwrap_or(false)
        } else {
            false
        };

        let states = self
            .sector_states
            .get_mut(&set_file)
            .expect("sector state map missing");
        let previous = states.insert(canonical.clone(), active);
        let state = if active { "on" } else { "off" };

        let result = match previous {
            Some(prev) if prev == active => {
                self.log_event(format!(
                    "sector.active {set_file}:{canonical} already {state}"
                ));
                SectorToggleResult::NoChange {
                    set_file: set_file.clone(),
                    sector: canonical.clone(),
                    known_sector,
                }
            }
            _ => {
                self.log_event(format!("sector.active {set_file}:{canonical} {state}"));
                SectorToggleResult::Applied {
                    set_file: set_file.clone(),
                    sector: canonical.clone(),
                    known_sector,
                }
            }
        };

        self.handle_sector_dependents(&set_file, &canonical, active);

        result
    }

    fn is_sector_active(&self, set_file: &str, sector_name: &str) -> bool {
        let key = self
            .canonical_sector_name(set_file, sector_name)
            .unwrap_or_else(|| sector_name.to_string());
        self.sector_states
            .get(set_file)
            .and_then(|map| map.get(&key))
            .copied()
            .unwrap_or(true)
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
            Some(ref name) => {
                if let Some(slot) = actor.costume_stack.last_mut() {
                    *slot = name.clone();
                } else {
                    actor.costume_stack.push(name.clone());
                }
                self.log_event(format!("actor.{id}.costume {name}"));
            }
            None => {
                actor.costume_stack.clear();
                self.log_event(format!("actor.{id}.costume <nil>"));
            }
        }
    }

    fn set_actor_base_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        let actor = self.ensure_actor_mut(id, label);
        actor.base_costume = costume.clone();
        actor.costume_stack.clear();
        match costume {
            Some(ref name) => {
                actor.costume_stack.push(name.clone());
                self.log_event(format!("actor.{id}.base_costume {name}"));
            }
            None => self.log_event(format!("actor.{id}.base_costume <nil>")),
        }
    }

    pub(super) fn actor_costume(&self, id: &str) -> Option<&str> {
        self.actors
            .get(id)
            .and_then(|actor| actor.costume.as_deref())
    }

    fn actor_base_costume(&self, id: &str) -> Option<&str> {
        self.actors
            .get(id)
            .and_then(|actor| actor.base_costume.as_deref())
    }

    fn push_actor_costume(&mut self, id: &str, label: &str, costume: String) -> usize {
        let depth;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.costume_stack.push(costume.clone());
            actor.costume = Some(costume.clone());
            depth = actor.costume_stack.len();
        }
        self.log_event(format!("actor.{id}.push_costume {costume} depth {depth}"));
        depth
    }

    fn pop_actor_costume(&mut self, id: &str, label: &str) -> Option<String> {
        let mut removed: Option<String> = None;
        let mut next: Option<String> = None;
        let blocked;
        {
            let actor = self.ensure_actor_mut(id, label);
            if actor.costume_stack.len() <= 1 {
                blocked = true;
            } else {
                blocked = false;
                removed = actor.costume_stack.pop();
                next = actor.costume_stack.last().cloned();
                actor.costume = next.clone();
            }
        }
        if blocked {
            self.log_event(format!("actor.{id}.pop_costume blocked"));
            None
        } else {
            let name = removed.as_deref().unwrap_or("<nil>").to_string();
            self.log_event(format!("actor.{id}.pop_costume {name}"));
            next
        }
    }

    fn set_actor_current_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.current_chore = chore.clone();
            actor.last_chore_costume = costume.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!("actor.{id}.chore {chore_label} {costume_label}"));
    }

    fn set_actor_walk_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.walk_chore = chore.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!(
            "actor.{id}.walk_chore {chore_label} {costume_label}"
        ));
    }

    fn set_actor_talk_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        drop: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, drop_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.talk_chore = chore.clone();
            actor.talk_drop_chore = drop.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            drop_label = drop.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!(
            "actor.{id}.talk_chore {chore_label} drop {drop_label} costume {costume_label}"
        ));
    }

    fn set_actor_mumble_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        let (chore_label, costume_label);
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.mumble_chore = chore.clone();
            chore_label = chore.as_deref().unwrap_or("<nil>").to_string();
            costume_label = costume.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!(
            "actor.{id}.mumble_chore {chore_label} costume {costume_label}"
        ));
    }

    fn set_actor_talk_color(&mut self, id: &str, label: &str, color: Option<String>) {
        let display;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.talk_color = color.clone();
            display = color.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!("actor.{id}.talk_color {display}"));
    }

    fn set_actor_head_target(&mut self, id: &str, label: &str, target: Option<String>) {
        let display;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.head_target = target.clone();
            display = target.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!("actor.{id}.head_target {display}"));
    }

    fn set_actor_head_look_rate(&mut self, id: &str, label: &str, rate: Option<f32>) {
        let snapshot;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.head_look_rate = rate;
            snapshot = actor.head_look_rate;
        }
        match snapshot {
            Some(value) => self.log_event(format!("actor.{id}.head_rate {value:.3}")),
            None => self.log_event(format!("actor.{id}.head_rate <nil>")),
        }
    }

    fn set_actor_collision_mode(&mut self, id: &str, label: &str, mode: Option<String>) {
        let display;
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.collision_mode = mode.clone();
            display = mode.as_deref().unwrap_or("<nil>").to_string();
        }
        self.log_event(format!("actor.{id}.collision_mode {display}"));
    }

    fn set_actor_ignore_boxes(&mut self, id: &str, label: &str, ignore: bool) {
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.ignoring_boxes = ignore;
        }
        self.log_event(format!("actor.{id}.ignore_boxes {}", ignore));
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

    fn set_actor_scale(&mut self, id: &str, label: &str, scale: Option<f32>) {
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.scale = scale;
        }
        let display = scale
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "<nil>".to_string());
        self.log_event(format!("actor.{id}.scale {display}"));
    }

    fn set_actor_collision_scale(&mut self, id: &str, label: &str, scale: Option<f32>) {
        {
            let actor = self.ensure_actor_mut(id, label);
            actor.collision_scale = scale;
        }
        let display = scale
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "<nil>".to_string());
        self.log_event(format!("actor.{id}.collision_scale {display}"));
    }

    pub(super) fn walk_actor_vector(
        &mut self,
        handle: u32,
        delta: Vec3,
        adjust_y: Option<f32>,
        heading_offset: Option<f32>,
    ) -> bool {
        let Some(actor_id) = self.actors.actor_id_for_handle(handle).cloned() else {
            self.log_event(format!("walk.delta unknown_handle #{handle}"));
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
                    .or_else(|| self.current_set.as_ref().map(|set| set.set_file.clone())),
                snapshot.position.unwrap_or(MANNY_OFFICE_SEED_POS),
            )
        };

        self.log_event(format!(
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
            if self.set_geometry.contains_key(set_file)
                && !self.point_in_active_walk(set_file, (next.x, next.y))
            {
                self.log_event(format!(
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

        if let Some(hit) = self.geometry_sector_hit(&actor_id, "walk") {
            self.record_sector_hit(&actor_id, &label, hit);
        }

        true
    }

    fn set_voice_effect(&mut self, effect: &str) {
        self.voice_effect = Some(effect.to_string());
        self.log_event(format!("prefs.voice_effect {}", effect));
    }

    fn add_inventory_item(&mut self, name: &str) {
        if self.inventory.add_item(name) {
            self.log_event(format!("inventory.add {name}"));
        }
    }

    fn register_inventory_room(&mut self, name: &str) {
        if self.inventory.register_room(name) {
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

    fn resolve_sector_hit(&self, actor_id: &str, kind: &str) -> Option<SectorHit> {
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

        if let Some(current) = &self.current_set {
            if let Some(descriptor) = self.available_sets.get(&current.set_file) {
                match request {
                    "camera" => {
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

    fn register_actor_with_handle(
        &mut self,
        label: &str,
        preferred_handle: Option<u32>,
    ) -> (String, u32) {
        let (id, handle, newly_assigned) = self
            .actors
            .register_actor_with_handle(label, preferred_handle);
        if newly_assigned {
            self.log_event(format!("actor.register {} (#{handle})", label));
        }
        (id, handle)
    }

    fn mark_actors_installed(&mut self) {
        self.actors.mark_actors_installed();
    }

    fn actors_installed(&self) -> bool {
        self.actors.actors_installed()
    }

    fn compute_object_sectors(&mut self, set_file: &str, position: Vec3) -> Vec<ObjectSectorRef> {
        if !self.ensure_sector_state_map(set_file) {
            return Vec::new();
        }
        let Some(geometry) = self.set_geometry.get(set_file) else {
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

    fn object_is_in_active_sector(&self, set_file: &str, snapshot: &ObjectSnapshot) -> bool {
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
                if self.is_sector_active(set_file, &sector.name) {
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

    fn register_object(&mut self, mut snapshot: ObjectSnapshot) {
        let handle = snapshot.handle;
        if let Some(existing) = self.objects.get(&handle) {
            if let Some(actor_handle) = existing.interest_actor {
                self.objects_by_actor.remove(&actor_handle);
            }
        }
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
                if let Some(current) = &self.current_set {
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
        self.refresh_commentary_visibility();
    }

    fn unregister_object(&mut self, handle: i64) {
        if let Some(snapshot) = self.objects.remove(&handle) {
            if let Some(actor_handle) = snapshot.interest_actor {
                self.objects_by_actor.remove(&actor_handle);
            }
            self.objects_by_name.retain(|_, value| *value != handle);
            self.log_event(format!("object.remove {} (#{handle})", snapshot.name));
        }
        self.refresh_commentary_visibility();
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
                        && self.object_is_in_active_sector(current_file, object)
                })
                .map(|object| object.handle)
                .collect()
        } else {
            Vec::new()
        }
    }

    fn record_visible_objects(&mut self, handles: &[i64]) {
        self.visible_objects.clear();
        self.hotlist_handles.clear();
        if handles.is_empty() {
            self.log_event("scene.visible <none>".to_string());
            return;
        }

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

        let mut names = Vec::new();
        let mut visible_infos: Vec<VisibleObjectInfo> = Vec::new();

        for handle in handles {
            if let Some(object) = self.objects.get(handle).cloned() {
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
                    object
                        .interest_actor
                        .and_then(|h| self.actor_position_by_handle(h))
                });
                if let (Some(actor_pos), Some(obj_pos)) = (actor_position, object_position) {
                    let distance = distance_between(actor_pos, obj_pos);
                    info.distance = Some(distance);
                    info.within_range = Some(distance <= object.range + f32::EPSILON);
                }

                if let (Some(actor_handle), Some(target_handle)) =
                    (actor_handle, object.interest_actor)
                {
                    if let (Some(actor_pos), Some(target_pos)) = (
                        self.actor_position_by_handle(actor_handle),
                        self.actor_position_by_handle(target_handle),
                    ) {
                        info.angle = Some(heading_between(actor_pos, target_pos) as f32);
                    }
                }

                visible_infos.push(info);
            }
        }

        if names.is_empty() {
            self.log_event("scene.visible <unknown>".to_string());
        } else {
            self.log_event(format!("scene.visible {}", names.join(", ")));
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
            self.log_event(format!("scene.hotlist {}", hot_names.join(", ")));
        }

        self.hotlist_handles = visible_infos
            .iter()
            .filter(|info| info.in_hotlist)
            .map(|info| info.handle)
            .collect();
        self.visible_objects = visible_infos;
        self.refresh_commentary_visibility();
    }

    fn object_position_by_actor(&self, actor_handle: u32) -> Option<Vec3> {
        self.objects_by_actor
            .get(&actor_handle)
            .and_then(|object_handle| self.objects.get(object_handle))
            .and_then(|object| object.position)
    }

    fn update_object_position_for_actor(&mut self, actor_handle: u32, position: Vec3) {
        if let Some(object_handle) = self.objects_by_actor.get(&actor_handle).copied() {
            let actor_set = self
                .actors
                .actor_id_for_handle(actor_handle)
                .and_then(|id| self.actors.get(id))
                .and_then(|actor| actor.current_set.clone())
                .or_else(|| {
                    self.current_set
                        .as_ref()
                        .map(|snapshot| snapshot.set_file.clone())
                });
            let mut object_name = None;
            let mut set_for_recalc: Option<(String, Vec3)> = None;
            {
                if let Some(object) = self.objects.get_mut(&object_handle) {
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
                if let Some(object) = self.objects.get_mut(&object_handle) {
                    object.sectors = sectors;
                }
            }
            if let Some(name) = object_name {
                self.log_event(format!(
                    "object.actor#{}.pos {} {:.3},{:.3},{:.3}",
                    actor_handle, name, position.x, position.y, position.z
                ));
            }
        }
        self.refresh_commentary_visibility();
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
        self.refresh_commentary_visibility();
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
        self.refresh_commentary_visibility();
    }

    fn commentary_candidate_handle(&self) -> Option<i64> {
        self.hotlist_handles
            .first()
            .copied()
            .or_else(|| self.visible_objects.first().map(|info| info.handle))
    }

    fn commentary_object_visible(&self, record: &CommentaryRecord) -> bool {
        if let Some(handle) = record.object_handle {
            if let Some(object) = self.objects.get(&handle) {
                if !object.visible || !object.touchable {
                    return false;
                }
                if let Some(set_file) = object.set_file.as_ref() {
                    if let Some(current) = &self.current_set {
                        if !current.set_file.eq_ignore_ascii_case(set_file) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                    return self.object_is_in_active_sector(set_file, object);
                }
            } else {
                return false;
            }
        }
        !self.hotlist_handles.is_empty() || !self.visible_objects.is_empty()
    }

    fn refresh_commentary_visibility(&mut self) {
        let Some(record) = self.cutscenes.commentary().cloned() else {
            return;
        };
        let visible = self.commentary_object_visible(&record);
        if let Some(message) = self
            .cutscenes
            .update_commentary_visibility(visible, "not_visible")
        {
            self.log_event(message);
        }
    }

    fn set_commentary_active(&mut self, enabled: bool, label: Option<String>) {
        if !enabled {
            let message = self.cutscenes.disable_commentary();
            self.log_event(message);
            return;
        }

        let mut record = CommentaryRecord {
            label,
            object_handle: self.commentary_candidate_handle(),
            active: true,
            suppressed_reason: None,
        };

        if !self.commentary_object_visible(&record) {
            record.active = false;
            record.suppressed_reason = Some("not_visible".to_string());
        }

        if let Some(message) = self.cutscenes.set_commentary(record) {
            self.log_event(message);
        }
    }

    fn handle_sector_dependents(&mut self, set_file: &str, sector: &str, active: bool) {
        let messages = self
            .cutscenes
            .handle_sector_activation(set_file, sector, active);
        for message in messages {
            self.log_event(message);
        }
        self.refresh_commentary_visibility();
    }

    pub(super) fn actor_position_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actors
            .actor_position_by_handle(handle)
            .or_else(|| self.object_position_by_actor(handle))
    }
    pub(super) fn actor_rotation_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actors.actor_rotation_by_handle(handle)
    }

    pub(super) fn resolve_actor_handle(&self, candidates: &[&str]) -> Option<(u32, String)> {
        self.actors.resolve_actor_handle(candidates)
    }

    fn actor_identity_by_handle(&self, handle: u32) -> Option<(String, String)> {
        self.actors.actor_identity_by_handle(handle)
    }

    fn set_actor_rotation_by_handle(&mut self, handle: u32, rotation: Vec3) -> bool {
        let Some((id, label)) = self.actor_identity_by_handle(handle) else {
            self.log_event(format!("actor.rot.unknown_handle #{handle}"));
            return false;
        };
        self.set_actor_rotation(&id, &label, rotation);
        true
    }

    fn set_actor_scale_by_handle(&mut self, handle: u32, scale: Option<f32>) -> bool {
        let Some((id, label)) = self.actor_identity_by_handle(handle) else {
            self.log_event(format!("actor.scale.unknown_handle #{handle}"));
            return false;
        };
        self.set_actor_scale(&id, &label, scale);
        true
    }

    fn set_actor_collision_scale_by_handle(&mut self, handle: u32, scale: Option<f32>) -> bool {
        let Some((id, label)) = self.actor_identity_by_handle(handle) else {
            self.log_event(format!("actor.collision_scale.unknown_handle #{handle}"));
            return false;
        };
        self.set_actor_collision_scale(&id, &label, scale);
        true
    }

    fn set_actor_moving(&mut self, handle: u32, moving: bool) {
        self.actors.set_actor_moving(handle, moving);
    }

    fn is_actor_moving(&self, handle: u32) -> bool {
        self.actors.is_actor_moving(handle)
    }

    fn walk_actor_to_handle(&mut self, handle: u32, target: Vec3) -> bool {
        let Some(current) = self.actor_position_by_handle(handle) else {
            self.log_event(format!("walk.to unknown_handle #{handle}"));
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

        self.set_actor_moving(handle, true);
        let moved = self.walk_actor_vector(handle, delta, None, None);
        self.set_actor_moving(handle, false);
        moved
    }

    fn point_in_active_walk(&self, set_file: &str, point: (f32, f32)) -> bool {
        if let Some(geometry) = self.set_geometry.get(set_file) {
            for sector in geometry
                .sectors
                .iter()
                .filter(|sector| matches!(sector.kind, SetSectorKind::Walk))
            {
                if sector.contains(point) && self.is_sector_active(set_file, &sector.name) {
                    return true;
                }
            }
            return false;
        }
        true
    }

    fn actor_snapshot(&self, actor_id: &str) -> Option<&ActorSnapshot> {
        self.actors.actor_snapshot(actor_id)
    }

    fn actor_position_xy(&self, actor_id: &str) -> Option<(f32, f32)> {
        self.actors.actor_position_xy(actor_id)
    }

    fn geometry_sector_hit(&self, actor_id: &str, raw_kind: &str) -> Option<SectorHit> {
        let current = self.current_set.as_ref()?;
        let geometry = self.set_geometry.get(&current.set_file)?;
        let point = self.actor_position_xy(actor_id)?;
        match raw_kind {
            "camera" | "2" | "hot" | "1" => {
                let request = if matches!(raw_kind, "hot" | "1") {
                    "hot"
                } else {
                    "camera"
                };
                if let Some(setup) = geometry.best_setup_for_point(point) {
                    return self.sector_hit_from_setup(&current.set_file, &setup.name, request);
                }
            }
            "walk" | "0" => {
                if let Some(polygon) = geometry.find_polygon(SetSectorKind::Walk, point) {
                    if self.is_sector_active(&current.set_file, &polygon.name) {
                        return Some(SectorHit::new(polygon.id, polygon.name.clone(), "WALK"));
                    }
                }
            }
            _ => {
                if let Some(kind) = match raw_kind {
                    "camera" | "2" => Some(SetSectorKind::Camera),
                    "walk" | "0" => Some(SetSectorKind::Walk),
                    _ => None,
                } {
                    if let Some(polygon) = geometry.find_polygon(kind, point) {
                        if self.is_sector_active(&current.set_file, &polygon.name) {
                            return Some(SectorHit::new(
                                polygon.id,
                                polygon.name.clone(),
                                raw_kind.to_ascii_uppercase(),
                            ));
                        }
                    }
                }
            }
        }
        None
    }

    pub(super) fn geometry_sector_name(&self, actor_id: &str, raw_kind: &str) -> Option<String> {
        self.geometry_sector_hit(actor_id, raw_kind)
            .map(|hit| hit.name)
    }

    fn visible_sector_hit(&self, _actor_id: &str, request: &str) -> Option<SectorHit> {
        let current = self.current_set.as_ref()?;
        let geometry = self.set_geometry.get(&current.set_file)?;

        let mut handles: Vec<i64> = Vec::new();
        handles.extend(self.hotlist_handles.iter().copied());
        for info in &self.visible_objects {
            if !handles.contains(&info.handle) {
                handles.push(info.handle);
            }
        }

        if handles.is_empty() {
            return None;
        }

        for handle in handles {
            let object = self.objects.get(&handle)?;
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
                            self.sector_hit_from_setup(&current.set_file, &setup.name, request)
                        {
                            return Some(hit);
                        }
                    }
                }
                "walk" => {
                    if let Some(polygon) = geometry.find_polygon(SetSectorKind::Walk, point) {
                        if self.is_sector_active(&current.set_file, &polygon.name) {
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
                            if self.is_sector_active(&current.set_file, &polygon.name) {
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
        if let Some((id, label)) = self.actors.actor_identity_by_handle(handle) {
            self.put_actor_in_set(&id, &label, set_file);
        }
    }

    pub(super) fn events(&self) -> &[String] {
        &self.events
    }

    pub(super) fn geometry_snapshot(&self) -> LuaGeometrySnapshot {
        geometry_export::build_snapshot(self.snapshot_state())
    }

    fn snapshot_state(&self) -> geometry_export::SnapshotState {
        geometry_export::SnapshotState {
            current_set: self.current_set.clone(),
            selected_actor: self.actors.selected_actor_id().map(|id| id.to_string()),
            voice_effect: self.voice_effect.clone(),
            loaded_sets: self.loaded_sets.clone(),
            current_setups: self.current_setups.clone(),
            available_sets: self.available_sets.clone(),
            set_geometry: self.set_geometry.clone(),
            sector_states: self.sector_states.clone(),
            actors: self.actors.clone_map(),
            objects: self.objects.clone(),
            actor_handles: self.actors.clone_handles(),
            visible_objects: self.visible_objects.clone(),
            hotlist_handles: self.hotlist_handles.clone(),
            inventory: self.inventory.clone_items(),
            inventory_rooms: self.inventory.clone_rooms(),
            commentary: self.cutscenes.clone_commentary(),
            cut_scene_stack: self.cutscenes.clone_cut_scene_stack(),
            music: self.audio.music().clone(),
            sfx: self.audio.sfx().clone(),
            events: self.events.clone(),
        }
    }
}

fn vec3_to_array(vec: Vec3) -> [f32; 3] {
    [vec.x, vec.y, vec.z]
}

pub(super) fn install_package_path(lua: &Lua, data_root: &Path) -> Result<()> {
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

pub(super) fn install_globals(
    lua: &Lua,
    data_root: &Path,
    context: Rc<RefCell<EngineContext>>,
) -> Result<()> {
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
            "_achievement.lua" | "_achievement.decompiled.lua" => {
                install_achievement_scaffold(lua, context.clone()).map_err(LuaError::external)?;
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
                install_menu_dialog(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_common.lua" | "menu_common.decompiled.lua" => {
                install_menu_common(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_remap_keys.lua" | "menu_remap_keys.decompiled.lua" => {
                install_menu_remap(lua, context.clone()).map_err(LuaError::external)?;
                return Ok(Some(Value::Nil));
            }
            "menu_prefs.lua" | "menu_prefs.decompiled.lua" => {
                install_menu_prefs(lua, context.clone()).map_err(LuaError::external)?;
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

    let play_context = context.clone();
    sfx.set(
        "play",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            if values.is_empty() {
                return Ok(());
            }
            let cue = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let params = values
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect::<Vec<_>>();
            play_context.borrow_mut().play_sound_effect(cue, params);
            Ok(())
        })?,
    )?;

    let stop_context = context.clone();
    sfx.set(
        "stop",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let target = values.get(0).and_then(|value| value_to_string(value));
            stop_context.borrow_mut().stop_sound_effect(target);
            Ok(())
        })?,
    )?;

    let stop_all_context = context.clone();
    sfx.set(
        "stop_all",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_all_context.borrow_mut().stop_sound_effect(None);
            Ok(())
        })?,
    )?;

    let stop_all_camel_context = context.clone();
    sfx.set(
        "stopAll",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_all_camel_context.borrow_mut().stop_sound_effect(None);
            Ok(())
        })?,
    )?;

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

    let imstart_context = context.clone();
    globals.set(
        "ImStartSound",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let cue_value = values.next().unwrap_or(Value::Nil);
            let Some(cue) = value_to_string(&cue_value) else {
                return Ok(Value::Integer(0));
            };
            let priority = values.next().and_then(|value| value_to_i32(&value));
            let group = values.next().and_then(|value| value_to_i32(&value));
            let handle = {
                let mut ctx = imstart_context.borrow_mut();
                ctx.start_imuse_sound(cue, priority, group)
            };
            Ok(Value::Integer(handle.max(0)))
        })?,
    )?;

    let imstop_context = context.clone();
    globals.set(
        "ImStopSound",
        lua.create_function(move |_, value: Value| {
            if let Some(handle) = value_to_object_handle(&value) {
                imstop_context
                    .borrow_mut()
                    .stop_sound_effect_by_numeric(handle);
            }
            Ok(())
        })?,
    )?;

    let imset_context = context.clone();
    globals.set(
        "ImSetParam",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle = args.get(0).and_then(|value| value_to_object_handle(value));
            let param = args.get(1).and_then(|value| value_to_i32(value));
            let value = args.get(2).and_then(|value| value_to_i32(value));
            if let (Some(handle), Some(param), Some(value)) = (handle, param, value) {
                imset_context
                    .borrow_mut()
                    .set_sound_param(handle, param, value);
            }
            Ok(())
        })?,
    )?;

    let imgp_context = context.clone();
    globals.set(
        "ImGetParam",
        lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<i64> {
            let handle = args.get(0).and_then(|value| value_to_object_handle(value));
            let param = args.get(1).and_then(|value| value_to_i32(value));
            if let (Some(handle), Some(param)) = (handle, param) {
                if let Some(value) = imgp_context.borrow().get_sound_param(handle, param) {
                    return Ok(value as i64);
                }
            }
            Ok(0)
        })?,
    )?;

    let imfade_context = context.clone();
    globals.set(
        "ImFadeParam",
        lua.create_function(move |_, args: Variadic<Value>| {
            let handle = args.get(0).and_then(|value| value_to_object_handle(value));
            let param = args.get(1).and_then(|value| value_to_i32(value));
            let value = args.get(2).and_then(|value| value_to_i32(value));
            if let (Some(handle), Some(param), Some(value)) = (handle, param, value) {
                imfade_context
                    .borrow_mut()
                    .set_sound_param(handle, param, value);
            }
            Ok(())
        })?,
    )?;

    let start_sfx_context = context.clone();
    globals.set(
        "start_sfx",
        lua.create_function(move |_, args: Variadic<Value>| {
            let cue = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<unknown>".to_string());
            let priority = args.get(1).and_then(|value| value_to_i32(value));
            let volume = args
                .get(2)
                .and_then(|value| value_to_i32(value))
                .unwrap_or(127);
            let handle = {
                let mut ctx = start_sfx_context.borrow_mut();
                let id = ctx.start_imuse_sound(cue.clone(), priority, Some(1));
                if id >= 0 {
                    ctx.set_sound_param(id, IM_SOUND_VOL, volume);
                }
                id
            };
            Ok(Value::Integer(handle.max(0)))
        })?,
    )?;

    let single_start_context = context.clone();
    globals.set(
        "single_start_sfx",
        lua.create_function(move |_, args: Variadic<Value>| {
            let cue = args
                .get(0)
                .and_then(|value| value_to_string(value))
                .unwrap_or_else(|| "<unknown>".to_string());
            let priority = args.get(1).and_then(|value| value_to_i32(value));
            let volume = args
                .get(2)
                .and_then(|value| value_to_i32(value))
                .unwrap_or(127);
            let handle = {
                let mut ctx = single_start_context.borrow_mut();
                let id = ctx.start_imuse_sound(cue.clone(), priority, Some(1));
                if id >= 0 {
                    ctx.set_sound_param(id, IM_SOUND_VOL, volume);
                }
                id
            };
            Ok(Value::Integer(handle.max(0)))
        })?,
    )?;

    let sound_playing_context = context.clone();
    globals.set(
        "sound_playing",
        lua.create_function(move |_, value: Value| {
            let Some(handle) = value_to_object_handle(&value) else {
                return Ok(false);
            };
            let playing = sound_playing_context
                .borrow()
                .get_sound_param(handle, IM_SOUND_PLAY_COUNT)
                .unwrap_or(0)
                > 0;
            Ok(playing)
        })?,
    )?;

    let wait_for_sound_context = context.clone();
    globals.set(
        "wait_for_sound",
        lua.create_function(move |_, value: Value| {
            if let Some(handle) = value_to_object_handle(&value) {
                let mut ctx = wait_for_sound_context.borrow_mut();
                ctx.set_sound_param(handle, IM_SOUND_PLAY_COUNT, 0);
            }
            Ok(())
        })?,
    )?;

    let stop_sound_context = context.clone();
    globals.set(
        "stop_sound",
        lua.create_function(move |_, args: Variadic<Value>| {
            let target = args.get(0);
            if let Some(handle) = target.and_then(|value| value_to_object_handle(value)) {
                stop_sound_context
                    .borrow_mut()
                    .stop_sound_effect_by_numeric(handle);
            } else if let Some(label) = target.and_then(|value| value_to_string(value)) {
                stop_sound_context
                    .borrow_mut()
                    .stop_sound_effect(Some(label));
            }
            Ok(())
        })?,
    )?;

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
        "GetControlState",
        lua.create_function(|_, _: Variadic<Value>| Ok(false))?,
    )?;
    globals.set(
        "get_generic_control_state",
        lua.create_function(|_, _: Variadic<Value>| Ok(false))?,
    )?;
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
            let value = args.get(0).cloned().unwrap_or(Value::Nil);
            let label = match &value {
                Value::String(text) => Some(text.to_str()?.to_string()),
                _ => None,
            };
            let enabled = match &value {
                Value::Nil => false,
                Value::Boolean(flag) => *flag,
                Value::Integer(i) => *i != 0,
                Value::Number(n) => *n != 0.0,
                Value::String(_) => true,
                other => value_to_bool(other),
            };
            commentary_ctx
                .borrow_mut()
                .set_commentary_active(enabled, label);
            Ok(())
        })?,
    )?;
    let sector_ctx = context.clone();
    globals.set(
        "MakeSectorActive",
        lua.create_function(move |_, args: Variadic<Value>| {
            let name_value = args.get(0).cloned().unwrap_or(Value::Nil);
            let active = args.get(1).map(value_to_bool).unwrap_or(true);
            let set_hint = args.get(2).and_then(|value| value_to_set_file(value));
            let mut ctx = sector_ctx.borrow_mut();
            let Some(sector_name) = value_to_sector_name(&name_value) else {
                let desc = describe_value(&name_value);
                ctx.log_event(format!("sector.active <invalid> ({desc})"));
                return Ok(());
            };
            match ctx.set_sector_active(set_hint.as_deref(), &sector_name, active) {
                SectorToggleResult::Applied {
                    set_file,
                    sector,
                    known_sector,
                    ..
                }
                | SectorToggleResult::NoChange {
                    set_file,
                    sector,
                    known_sector,
                } => {
                    if !known_sector {
                        ctx.log_event(format!("sector.active.unknown {set_file}:{sector}"));
                    }
                }
                SectorToggleResult::NoSet => {
                    ctx.log_event("sector.active <no current set>".to_string());
                }
            }
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
    let walk_vector_context = context.clone();
    globals.set(
        "WalkActorVector",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();

            let actor_handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value))
                .unwrap_or(0);
            // camera handle is ignored in the prototype but advance the iterator
            let _ = values.next();

            let dx = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let dy = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let dz = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let adjust_y = values.next().and_then(|value| value_to_f32(&value));
            // maintained heading flag (ignored)
            let _ = values.next();
            let heading_offset = values.next().and_then(|value| value_to_f32(&value));

            if actor_handle == 0 {
                return Ok(());
            }

            let mut ctx = walk_vector_context.borrow_mut();
            ctx.walk_actor_vector(
                actor_handle,
                Vec3 {
                    x: dx,
                    y: dy,
                    z: dz,
                },
                adjust_y,
                heading_offset,
            );
            Ok(())
        })?,
    )?;

    let walk_to_ctx = context.clone();
    globals.set(
        "WalkActorTo",
        lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<bool> {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(false);
            };
            let x = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let y = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let z = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let mut ctx = walk_to_ctx.borrow_mut();
            let moved = ctx.walk_actor_to_handle(handle, Vec3 { x, y, z });
            Ok(moved)
        })?,
    )?;

    let is_moving_ctx = context.clone();
    globals.set(
        "IsActorMoving",
        lua.create_function(move |_, actor: Value| {
            let handle = value_to_actor_handle(&actor).unwrap_or(0);
            let moving = if handle == 0 {
                false
            } else {
                is_moving_ctx.borrow().is_actor_moving(handle)
            };
            Ok(moving)
        })?,
    )?;

    let sleep_context = context.clone();
    globals.set(
        "sleep_for",
        lua.create_function(move |_, args: Variadic<Value>| {
            let desc = if args.is_empty() {
                "<none>".to_string()
            } else {
                args.iter()
                    .map(|value| describe_value(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            sleep_context
                .borrow_mut()
                .log_event(format!("sleep_for {}", desc));
            Ok(())
        })?,
    )?;

    let set_override_context = context.clone();
    globals.set(
        "set_override",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut ctx = set_override_context.borrow_mut();
            match args.get(0) {
                Some(Value::Nil) | None => {
                    ctx.pop_override();
                }
                Some(value) => {
                    let description = describe_value(value);
                    ctx.push_override(description);
                }
            }
            Ok(())
        })?,
    )?;

    let kill_override_context = context.clone();
    globals.set(
        "kill_override",
        lua.create_function(move |_, _: Variadic<Value>| {
            let mut ctx = kill_override_context.borrow_mut();
            ctx.clear_overrides();
            Ok(())
        })?,
    )?;

    let fade_context = context.clone();
    globals.set(
        "FadeInChore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let desc = if args.is_empty() {
                "<none>".to_string()
            } else {
                args.iter()
                    .map(|value| describe_value(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            fade_context
                .borrow_mut()
                .log_event(format!("actor.fade_in {}", desc));
            Ok(())
        })?,
    )?;

    let start_cut_scene_context = context.clone();
    globals.set(
        "START_CUT_SCENE",
        lua.create_function(move |_, args: Variadic<Value>| {
            let label = args.get(0).and_then(|value| value_to_string(value));
            let flags: Vec<String> = args
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect();
            start_cut_scene_context
                .borrow_mut()
                .push_cut_scene(label, flags);
            Ok(())
        })?,
    )?;

    let end_cut_scene_context = context.clone();
    globals.set(
        "END_CUT_SCENE",
        lua.create_function(move |_, _: Variadic<Value>| {
            end_cut_scene_context.borrow_mut().pop_cut_scene();
            Ok(())
        })?,
    )?;

    let wait_context = context.clone();
    globals.set(
        "wait_for_message",
        lua.create_function(move |_, args: Variadic<Value>| {
            let actor_hint = if let Some(Value::Table(table)) = args.get(0) {
                Some(actor_identity(&table)?)
            } else {
                None
            };
            let mut ctx = wait_context.borrow_mut();
            let ended = ctx.finish_dialog_line(actor_hint.as_ref().map(|(id, _)| id.as_str()));
            match ended {
                Some(state) => {
                    ctx.log_event(format!("dialog.wait {} {}", state.actor_label, state.line));
                }
                None => {
                    let label = actor_hint
                        .as_ref()
                        .map(|(_, label)| label.as_str())
                        .unwrap_or("<none>");
                    ctx.log_event(format!("dialog.wait {} <idle>", label));
                }
            }
            Ok(())
        })?,
    )?;

    let message_context = context.clone();
    globals.set(
        "IsMessageGoing",
        lua.create_function(move |_, ()| Ok(message_context.borrow().is_message_active()))?,
    )?;
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

    let set_actor_rot_ctx = context.clone();
    globals.set(
        "SetActorRot",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let x = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let y = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let z = values
                .next()
                .and_then(|value| value_to_f32(&value))
                .unwrap_or(0.0);
            let mut ctx = set_actor_rot_ctx.borrow_mut();
            ctx.set_actor_rotation_by_handle(handle, Vec3 { x, y, z });
            Ok(())
        })?,
    )?;

    let set_actor_scale_ctx = context.clone();
    globals.set(
        "SetActorScale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let scale = values.next().and_then(|value| value_to_f32(&value));
            let mut ctx = set_actor_scale_ctx.borrow_mut();
            ctx.set_actor_scale_by_handle(handle, scale);
            Ok(())
        })?,
    )?;

    let set_actor_collision_scale_ctx = context.clone();
    globals.set(
        "SetActorCollisionScale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let mut values = args.into_iter();
            let handle = values
                .next()
                .and_then(|value| value_to_actor_handle(&value));
            let Some(handle) = handle else {
                return Ok(());
            };
            let scale = values.next().and_then(|value| value_to_f32(&value));
            let mut ctx = set_actor_collision_scale_ctx.borrow_mut();
            ctx.set_actor_collision_scale_by_handle(handle, scale);
            Ok(())
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
                        let angle = heading_between(a_pos, b_pos);
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
                        ensure_object_metatable(
                            lua_ctx,
                            &object_table,
                            parent_context.clone(),
                            handle,
                        )?;
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
    system.set("frameTime", 0.016_666_668)?;
    let axis_handler = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
    system.set("axisHandler", axis_handler)?;
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

    let meche_table = {
        let (meche_id, meche_handle) = {
            let mut ctx = context.borrow_mut();
            ctx.register_actor_with_handle("Meche", Some(1002))
        };
        build_actor_table(
            lua,
            context.clone(),
            system_key.clone(),
            meche_id.clone(),
            "Meche".to_string(),
            meche_handle,
        )?
    };

    let globals = lua.globals();
    globals.set("manny", manny_table.clone())?;
    globals.set("meche", meche_table.clone())?;

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

    let scale_context = context.clone();
    actor.set(
        "scale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                let scale = values.get(0).and_then(|value| value_to_f32(value));
                scale_context
                    .borrow_mut()
                    .set_actor_scale(&id, &name, scale);
            }
            Ok(())
        })?,
    )?;

    let set_scale_context = context.clone();
    actor.set(
        "set_scale",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, name) = actor_identity(&table)?;
                let scale = values.get(0).and_then(|value| value_to_f32(value));
                set_scale_context
                    .borrow_mut()
                    .set_actor_scale(&id, &name, scale);
            }
            Ok(())
        })?,
    )?;

    let walkto_object_context = context.clone();
    actor.set(
        "walkto_object",
        lua.create_function(move |_, args: Variadic<Value>| -> LuaResult<bool> {
            let (self_table, values) = split_self(args);
            let Some(actor_table) = self_table else {
                return Ok(false);
            };

            let (actor_id, actor_label) = actor_identity(&actor_table)?;
            let actor_handle = actor_table
                .get::<_, Option<i64>>("hActor")
                .ok()
                .flatten()
                .unwrap_or(0) as u32;
            if actor_handle == 0 {
                return Ok(false);
            }

            let Some(target_value) = values.get(0) else {
                return Ok(false);
            };
            let object_table = match target_value {
                Value::Table(table) => table.clone(),
                _ => return Ok(false),
            };

            let use_out = values
                .get(1)
                .map(|value| value_to_bool(value))
                .unwrap_or(false);
            let run_flag = values
                .get(2)
                .map(|value| value_to_bool(value))
                .unwrap_or(false);

            let object_name = object_table
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .or_else(|| {
                    object_table
                        .get::<_, Option<String>>("string_name")
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| "<object>".to_string());
            let object_handle = object_table
                .get::<_, Option<i64>>("handle")
                .ok()
                .flatten()
                .or_else(|| {
                    object_table
                        .get::<_, Option<i64>>("object_handle")
                        .ok()
                        .flatten()
                })
                .or_else(|| object_table.get::<_, Option<i64>>("hObject").ok().flatten());

            let position = if use_out {
                let x = object_table
                    .get::<_, Option<f32>>("out_pnt_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("out_pnt_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("out_pnt_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            } else {
                let x = object_table
                    .get::<_, Option<f32>>("use_pnt_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("use_pnt_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("use_pnt_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            };

            let rotation = if use_out {
                let x = object_table
                    .get::<_, Option<f32>>("out_rot_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("out_rot_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("out_rot_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            } else {
                let x = object_table
                    .get::<_, Option<f32>>("use_rot_x")
                    .ok()
                    .flatten();
                let y = object_table
                    .get::<_, Option<f32>>("use_rot_y")
                    .ok()
                    .flatten();
                let z = object_table
                    .get::<_, Option<f32>>("use_rot_z")
                    .ok()
                    .flatten();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
                    _ => None,
                }
            };

            let destination_label = object_handle
                .map(|handle| format!("{} (#{handle})", object_name))
                .unwrap_or(object_name.clone());

            let moved = {
                let mut ctx = walkto_object_context.borrow_mut();
                if let Some(target) = position {
                    let moved = ctx.walk_actor_to_handle(actor_handle, target);
                    if moved {
                        if let Some(rot) = rotation {
                            ctx.set_actor_rotation_by_handle(actor_handle, rot);
                        }
                        if run_flag {
                            ctx.log_event(format!("actor.run {} true", actor_id));
                        }
                        ctx.log_event(format!(
                            "actor.walkto_object {} -> {}{}",
                            actor_label,
                            destination_label,
                            if use_out { " [out]" } else { "" }
                        ));
                    } else {
                        ctx.log_event(format!(
                            "actor.walkto_object {} failed {}",
                            actor_label, destination_label
                        ));
                    }
                    moved
                } else {
                    ctx.log_event(format!(
                        "actor.walkto_object {} missing target for {}",
                        actor_label, destination_label
                    ));
                    false
                }
            };

            actor_table.set("is_running", run_flag)?;
            Ok(moved)
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
    let normal_say_line =
        lua.create_function(move |lua_ctx, args: Variadic<Value>| -> LuaResult<()> {
            let (self_table, values) = split_self(args);
            if let Some(actor_table) = self_table {
                let (id, label) = actor_identity(&actor_table)?;
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
                    if !background {
                        ctx.begin_dialog_line(&id, &label, &line);
                    }
                }

                if !background {
                    let system: Table = lua_ctx.registry_value(say_system_key.as_ref())?;
                    system.set("lastActorTalking", actor_table.clone())?;
                }
            }
            Ok(())
        })?;
    actor.set("normal_say_line", normal_say_line.clone())?;
    actor.set("say_line", normal_say_line.clone())?;
    actor.set("underwater_say_line", normal_say_line.clone())?;

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
                    .speaking_actor()
                    .map(|selected| selected.eq_ignore_ascii_case(&id))
                    .unwrap_or(false);
                return Ok(speaking);
            }
            Ok(false)
        })?,
    )?;

    let actor_wait_context = context.clone();
    actor.set(
        "wait_for_message",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let mut ctx = actor_wait_context.borrow_mut();
                if let Some(state) = ctx.finish_dialog_line(Some(&id)) {
                    ctx.log_event(format!("dialog.wait {} {}", state.actor_label, state.line));
                } else {
                    ctx.log_event(format!("dialog.wait {} <idle>", label));
                }
            }
            Ok(())
        })?,
    )?;

    let play_chore_context = context.clone();
    actor.set(
        "play_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| value_to_string(value));
                let costume = values.get(1).and_then(|value| value_to_string(value));
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = play_chore_context.borrow_mut();
                    ctx.set_actor_current_chore(&id, &label, chore.clone(), costume.clone());
                }
                match chore {
                    Some(ref value) => {
                        table.set("last_chore_played", value.clone())?;
                        table.set("current_chore", value.clone())?;
                    }
                    None => {
                        table.set("last_chore_played", Value::Nil)?;
                        table.set("current_chore", Value::Nil)?;
                    }
                }
                match costume {
                    Some(ref value) => table.set("last_cos_played", value.clone())?,
                    None => table.set("last_cos_played", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let pop_costume_context = context.clone();
    actor.set(
        "pop_costume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, _values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let success = {
                    let mut ctx = pop_costume_context.borrow_mut();
                    ctx.pop_actor_costume(&id, &label).is_some()
                };
                {
                    let ctx = pop_costume_context.borrow();
                    if let Some(costume) = ctx.actor_costume(&id) {
                        table.set("current_costume", costume.to_string())?;
                    } else {
                        table.set("current_costume", Value::Nil)?;
                    }
                }
                return Ok(success);
            }
            Ok(false)
        })?,
    )?;

    let head_look_context = context.clone();
    actor.set(
        "head_look_at",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let target_label = values
                    .get(0)
                    .map(|value| match value {
                        Value::Table(actor_table) => {
                            if let Ok(name) = actor_table.get::<_, String>("name") {
                                name
                            } else if let Ok(id) = actor_table.get::<_, String>("id") {
                                format!("table:{id}")
                            } else {
                                describe_value(value)
                            }
                        }
                        other => describe_value(other),
                    })
                    .unwrap_or_else(|| "<nil>".to_string());
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = head_look_context.borrow_mut();
                    ctx.set_actor_head_target(&id, &label, Some(target_label.clone()));
                }
                table.set("head_target_label", target_label)?;
            }
            Ok(())
        })?,
    )?;

    let push_costume_context = context.clone();
    actor.set(
        "push_costume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let Some(costume) = values.get(0).and_then(|value| value_to_string(value)) else {
                    return Ok(false);
                };
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = push_costume_context.borrow_mut();
                    ctx.push_actor_costume(&id, &label, costume.clone());
                }
                table.set("current_costume", costume)?;
                return Ok(true);
            }
            Ok(false)
        })?,
    )?;

    let walk_chore_context = context.clone();
    actor.set(
        "set_walk_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let costume = values.get(1).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = walk_chore_context.borrow_mut();
                    ctx.set_actor_walk_chore(&id, &label, chore.clone(), costume.clone());
                }
                match chore {
                    Some(ref value) => table.set("walk_chore", value.clone())?,
                    None => table.set("walk_chore", Value::Nil)?,
                }
                match costume {
                    Some(ref value) => table.set("walk_chore_costume", value.clone())?,
                    None => table.set("walk_chore_costume", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let talk_color_context = context.clone();
    actor.set(
        "set_talk_color",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let color = values.get(0).and_then(|value| value_to_string(value));
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = talk_color_context.borrow_mut();
                    ctx.set_actor_talk_color(&id, &label, color.clone());
                }
                match color {
                    Some(ref value) => table.set("talk_color", value.clone())?,
                    None => table.set("talk_color", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let mumble_chore_context = context.clone();
    actor.set(
        "set_mumble_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let costume = values.get(1).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = mumble_chore_context.borrow_mut();
                    ctx.set_actor_mumble_chore(&id, &label, chore.clone(), costume.clone());
                }
                match chore {
                    Some(ref value) => table.set("mumble_chore", value.clone())?,
                    None => table.set("mumble_chore", Value::Nil)?,
                }
                match costume {
                    Some(ref value) => table.set("mumble_costume", value.clone())?,
                    None => table.set("mumble_costume", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let talk_chore_context = context.clone();
    actor.set(
        "set_talk_chore",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let chore = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let drop = values.get(1).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let costume = values.get(2).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = talk_chore_context.borrow_mut();
                    ctx.set_actor_talk_chore(
                        &id,
                        &label,
                        chore.clone(),
                        drop.clone(),
                        costume.clone(),
                    );
                }
                match chore {
                    Some(ref value) => table.set("talk_chore", value.clone())?,
                    None => table.set("talk_chore", Value::Nil)?,
                }
                match drop {
                    Some(ref value) => table.set("talk_drop_chore", value.clone())?,
                    None => table.set("talk_drop_chore", Value::Nil)?,
                }
                match costume {
                    Some(ref value) => table.set("talk_chore_costume", value.clone())?,
                    None => table.set("talk_chore_costume", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let set_head_context = context.clone();
    actor.set(
        "set_head",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let (id, label) = actor_identity(&table)?;
                let params = values
                    .iter()
                    .map(|value| describe_value(value))
                    .collect::<Vec<_>>()
                    .join(", ");
                {
                    let mut ctx = set_head_context.borrow_mut();
                    ctx.set_actor_head_target(&id, &label, Some("manual".to_string()));
                    ctx.log_event(format!("actor.{id}.set_head {params}"));
                }
                table.set("head_control", params)?;
            }
            Ok(())
        })?,
    )?;

    let look_rate_context = context.clone();
    actor.set(
        "set_look_rate",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let rate = values.get(0).and_then(|value| value_to_f32(value));
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = look_rate_context.borrow_mut();
                    ctx.set_actor_head_look_rate(&id, &label, rate);
                }
                if let Some(value) = rate {
                    table.set("head_look_rate", value)?;
                } else {
                    table.set("head_look_rate", Value::Nil)?;
                }
            }
            Ok(())
        })?,
    )?;

    let collision_mode_context = context.clone();
    actor.set(
        "set_collision_mode",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let mode = values.get(0).and_then(|value| match value {
                    Value::Nil => None,
                    other => value_to_string(other),
                });
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = collision_mode_context.borrow_mut();
                    ctx.set_actor_collision_mode(&id, &label, mode.clone());
                }
                match mode {
                    Some(ref value) => table.set("collision_mode", value.clone())?,
                    None => table.set("collision_mode", Value::Nil)?,
                }
            }
            Ok(())
        })?,
    )?;

    let ignore_boxes_context = context.clone();
    actor.set(
        "ignore_boxes",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (self_table, values) = split_self(args);
            if let Some(table) = self_table {
                let flag = values
                    .get(0)
                    .map(|value| value_to_bool(value))
                    .unwrap_or(true);
                let (id, label) = actor_identity(&table)?;
                {
                    let mut ctx = ignore_boxes_context.borrow_mut();
                    ctx.set_actor_ignore_boxes(&id, &label, flag);
                }
                table.set("ignoring_boxes", flag)?;
            }
            Ok(())
        })?,
    )?;

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

pub(super) fn split_self<'lua>(
    args: Variadic<Value<'lua>>,
) -> (Option<Table<'lua>>, Vec<Value<'lua>>) {
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

fn value_to_sector_name(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_str().ok()?.to_string()),
        Value::Table(table) => {
            if let Ok(Some(name)) = table.get::<_, Option<String>>("name") {
                return Some(name);
            }
            if let Ok(Some(label)) = table.get::<_, Option<String>>("label") {
                return Some(label);
            }
            None
        }
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
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

fn ensure_object_metatable(
    lua: &Lua,
    object: &Table,
    context: Rc<RefCell<EngineContext>>,
    handle: i64,
) -> LuaResult<()> {
    if let Ok(parent) = object.get::<_, Table>("parent") {
        let metatable = match object.get_metatable() {
            Some(meta) => meta,
            None => lua.create_table()?,
        };
        metatable.set("__index", parent)?;

        let current_newindex = metatable
            .get::<_, Value>("__newindex")
            .unwrap_or(Value::Nil);
        if matches!(current_newindex, Value::Nil) {
            let ctx = context.clone();
            let handler =
                lua.create_function(move |_, (table, key, value): (Table, Value, Value)| {
                    let key_name = match &key {
                        Value::String(text) => Some(text.to_str()?.to_string()),
                        _ => None,
                    };

                    table.raw_set(key.clone(), value.clone())?;

                    if let Some(name) = key_name.as_deref() {
                        match name {
                            "touchable" => {
                                let touchable = value_to_bool(&value);
                                ctx.borrow_mut().set_object_touchable(handle, touchable);
                            }
                            "visible" | "is_visible" => {
                                let visible = value_to_bool(&value);
                                ctx.borrow_mut().set_object_visibility(handle, visible);
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                })?;
            metatable.set("__newindex", handler)?;
        }

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
        let func = lua.create_function(move |_, (this,): (Table,)| {
            this.set("touchable", false)?;
            Ok(())
        })?;
        object.set("make_untouchable", func)?;
    }

    let touchable = object
        .get::<_, Value>("make_touchable")
        .unwrap_or(Value::Nil);
    if matches!(touchable, Value::Nil) {
        let func = lua.create_function(move |_, (this,): (Table,)| {
            this.set("touchable", true)?;
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
        sectors: Vec::new(),
    })
}

pub(super) fn load_system_script(lua: &Lua, data_root: &Path) -> Result<()> {
    let system_path = data_root.join("_system.decompiled.lua");
    let source = fs::read_to_string(&system_path)
        .with_context(|| format!("reading {}", system_path.display()))?;
    let normalized = normalize_legacy_lua(&source);
    let chunk = lua.load(&normalized).set_name("_system.decompiled.lua");
    chunk.exec().context("executing _system.decompiled.lua")?;
    Ok(())
}

pub(super) fn override_boot_stubs(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
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

    let cache_vector_context = context.clone();
    globals.set(
        "CacheCurrentWalkVector",
        lua.create_function(move |_, _: Variadic<Value>| {
            cache_vector_context
                .borrow_mut()
                .log_event("geometry.cache_walk_vector".to_string());
            Ok(())
        })?,
    )?;

    let stop_context = context.clone();
    globals.set(
        "stop_script",
        lua.create_function(move |_, args: Variadic<Value>| {
            let description = args
                .get(0)
                .map(describe_value)
                .unwrap_or_else(|| "<unknown>".to_string());
            stop_context
                .borrow_mut()
                .log_event(format!("script.stop {description}"));
            Ok(())
        })?,
    )?;

    let current_script_context = context.clone();
    globals.set(
        "GetCurrentScript",
        lua.create_function(move |_, _: Variadic<Value>| {
            current_script_context
                .borrow_mut()
                .log_event("script.current".to_string());
            Ok(Value::Nil)
        })?,
    )?;

    if matches!(
        globals.get::<_, Value>("WalkVector"),
        Ok(Value::Nil) | Err(_)
    ) {
        let walk_vector = lua.create_table()?;
        walk_vector.set("x", 0.0)?;
        walk_vector.set("y", 0.0)?;
        walk_vector.set("z", 0.0)?;
        globals.set("WalkVector", walk_vector)?;
    }

    install_cutscene_helpers(lua, context.clone())?;
    install_idle_scaffold(lua, context.clone())?;

    wrap_start_cut_scene(lua, context.clone())?;
    wrap_end_cut_scene(lua, context.clone())?;
    wrap_set_override(lua, context.clone())?;
    wrap_kill_override(lua, context.clone())?;
    wrap_wait_for_message(lua, context.clone())?;

    Ok(())
}

fn install_cutscene_helpers(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();

    let stop_commentary_context = context.clone();
    globals.set(
        "StopCommentaryImmediately",
        lua.create_function(move |_, _: Variadic<Value>| {
            stop_commentary_context
                .borrow_mut()
                .log_event("cut_scene.stop_commentary".to_string());
            Ok(())
        })?,
    )?;

    let kill_render_context = context.clone();
    globals.set(
        "killRenderModeText",
        lua.create_function(move |_, _: Variadic<Value>| {
            kill_render_context
                .borrow_mut()
                .log_event("render.kill_mode_text".to_string());
            Ok(())
        })?,
    )?;

    let destroy_buttons_context = context.clone();
    globals.set(
        "DestroyAllUIButtonsImmediately",
        lua.create_function(move |_, _: Variadic<Value>| {
            destroy_buttons_context
                .borrow_mut()
                .log_event("ui.destroy_buttons_immediate".to_string());
            Ok(())
        })?,
    )?;

    let start_movie_context = context.clone();
    globals.set(
        "StartFullscreenMovie",
        lua.create_function(move |_, args: Variadic<Value>| {
            let movie = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            start_movie_context
                .borrow_mut()
                .log_event(format!("cut_scene.fullscreen.start {movie}"));
            Ok(true)
        })?,
    )?;

    let movie_state_context = context.clone();
    globals.set(
        "IsFullscreenMoviePlaying",
        lua.create_function(move |_, _: Variadic<Value>| {
            movie_state_context
                .borrow_mut()
                .log_event("cut_scene.fullscreen.poll".to_string());
            Ok(false)
        })?,
    )?;

    let hide_skip_context = context.clone();
    globals.set(
        "hideSkipButton",
        lua.create_function(move |_, _: Variadic<Value>| {
            hide_skip_context
                .borrow_mut()
                .log_event("cut_scene.skip.hide".to_string());
            Ok(())
        })?,
    )?;

    let show_skip_context = context;
    globals.set(
        "showSkipButton",
        lua.create_function(move |_, _: Variadic<Value>| {
            show_skip_context
                .borrow_mut()
                .log_event("cut_scene.skip.show".to_string());
            Ok(())
        })?,
    )?;

    Ok(())
}

fn install_idle_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("Idle"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let idle_table = lua.create_table()?;
    let create_context = context.clone();
    idle_table.set(
        "create",
        lua.create_function(move |lua_ctx, args: Variadic<Value>| {
            let name = args
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unnamed>".to_string());
            create_context
                .borrow_mut()
                .log_event(format!("idle.create {name}"));

            let state_table = lua_ctx.create_table()?;
            let add_state_context = create_context.clone();
            state_table.set(
                "add_state",
                lua_ctx.create_function(move |_, args: Variadic<Value>| {
                    let state_name = args
                        .get(0)
                        .and_then(value_to_string)
                        .unwrap_or_else(|| "<unnamed>".to_string());
                    add_state_context
                        .borrow_mut()
                        .log_event(format!("idle.state {state_name}"));
                    Ok(())
                })?,
            )?;
            Ok(state_table)
        })?,
    )?;

    globals.set("Idle", idle_table)?;
    Ok(())
}

fn wrap_start_cut_scene(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("START_CUT_SCENE") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let label = values.get(0).and_then(|value| value_to_string(value));
            let flags: Vec<String> = values
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect();
            ctx.borrow_mut().push_cut_scene(label, flags);
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            Ok(result)
        },
    )?;
    globals.set("START_CUT_SCENE", wrapper)?;
    Ok(())
}

fn wrap_end_cut_scene(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("END_CUT_SCENE") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            ctx.borrow_mut().pop_cut_scene();
            Ok(result)
        },
    )?;
    globals.set("END_CUT_SCENE", wrapper)?;
    Ok(())
}

fn wrap_set_override(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("set_override") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            {
                let mut ctx = ctx.borrow_mut();
                match values.get(0) {
                    Some(Value::Nil) | None => {
                        ctx.pop_override();
                    }
                    Some(value) => {
                        ctx.push_override(describe_value(value));
                    }
                }
            }
            Ok(result)
        },
    )?;
    globals.set("set_override", wrapper)?;
    Ok(())
}

fn wrap_kill_override(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("kill_override") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            {
                let mut ctx = ctx.borrow_mut();
                ctx.clear_overrides();
            }
            Ok(result)
        },
    )?;
    globals.set("kill_override", wrapper)?;
    Ok(())
}

fn wrap_wait_for_message(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    let original: Function = match globals.get("wait_for_message") {
        Ok(func) => func,
        Err(_) => return Ok(()),
    };
    let registry_key = lua.create_registry_value(original)?;
    let ctx = context.clone();
    let wrapper = lua.create_function(
        move |lua_ctx, args: Variadic<Value>| -> LuaResult<MultiValue> {
            let values: Vec<Value> = args.into_iter().collect();
            let original: Function = lua_ctx.registry_value(&registry_key)?;
            let result = original.call::<_, MultiValue>(MultiValue::from_vec(values.clone()))?;
            {
                let mut ctx = ctx.borrow_mut();
                if let Some(state) = ctx.finish_dialog_line(None) {
                    ctx.log_event(format!("dialog.wait {} {}", state.actor_label, state.line));
                } else {
                    ctx.log_event("dialog.wait global <idle>".to_string());
                }
            }
            Ok(result)
        },
    )?;
    globals.set("wait_for_message", wrapper)?;
    Ok(())
}

pub(super) fn call_boot(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
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

fn install_achievement_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("achievement"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let table = lua.create_table()?;

    let set_context = context.clone();
    table.set(
        "setEligible",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let id = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let eligible = values.get(1).map(value_to_bool).unwrap_or(true);
            set_context
                .borrow_mut()
                .set_achievement_eligibility(&id, eligible);
            Ok(())
        })?,
    )?;

    let established_context = context.clone();
    table.set(
        "hasEligibilityBeenEstablished",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let id = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let established = {
                let ctx = established_context.borrow();
                ctx.achievement_has_been_established(&id)
            };
            established_context.borrow_mut().log_event(format!(
                "achievement.check_established {id} -> {established}"
            ));
            Ok(established)
        })?,
    )?;

    let query_context = context.clone();
    table.set(
        "isEligible",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_self_table, values) = split_self(args);
            let id = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let eligible = {
                let ctx = query_context.borrow();
                ctx.achievement_is_eligible(&id)
            };
            query_context
                .borrow_mut()
                .log_event(format!("achievement.query {id} -> {eligible}"));
            Ok(eligible)
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("achievement.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    table.set_metatable(Some(metatable));

    globals.set("achievement", table)?;

    match globals.get::<_, Value>("ACHIEVE_CLASSIC_DRIVER") {
        Ok(Value::Nil) | Err(_) => {
            globals.set("ACHIEVE_CLASSIC_DRIVER", "ACHIEVE_CLASSIC_DRIVER")?;
        }
        _ => {}
    }

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

    let update_buttons_context = context;
    globals.set(
        "UpdateUIButtons",
        lua.create_function(move |_, _: mlua::Variadic<mlua::Value>| {
            update_buttons_context
                .borrow_mut()
                .log_event("ui.update_buttons".to_string());
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

pub(super) fn install_render_helpers(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
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

    let display_context = context;
    globals.set(
        "EngineDisplay",
        lua.create_function(move |_, args: Variadic<Value>| {
            let description = args
                .iter()
                .map(describe_value)
                .collect::<Vec<_>>()
                .join(", ");
            display_context
                .borrow_mut()
                .log_event(format!("render.display [{}]", description));
            Ok(())
        })?,
    )?;
    Ok(())
}

pub(super) fn dump_runtime_summary(state: &EngineContext) {
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
        state
            .actors
            .selected_actor_id()
            .map(|id| id.as_str())
            .unwrap_or("<none>")
    );
    if let Some(effect) = &state.voice_effect {
        println!("  Voice effect: {}", effect);
    }
    let music = state.music_state();
    if let Some(current) = &music.current {
        if current.parameters.is_empty() {
            println!("  Music playing: {}", current.name);
        } else {
            println!(
                "  Music playing: {} [{}]",
                current.name,
                current.parameters.join(", ")
            );
        }
    } else {
        println!("  Music playing: <none>");
    }
    if !music.queued.is_empty() {
        let queued: Vec<_> = music
            .queued
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        println!("  Music queued: {}", queued.join(", "));
    }
    if music.paused {
        println!("  Music paused");
    }
    if state.pause_state().active {
        println!("  Game paused");
    }
    if let Some(state_name) = &music.current_state {
        println!("  Music state: {}", state_name);
    }
    if !music.state_stack.is_empty() {
        println!("  Music state stack: {}", music.state_stack.join(" -> "));
    }
    if !music.muted_groups.is_empty() {
        let groups: Vec<_> = music
            .muted_groups
            .iter()
            .map(|group| group.as_str())
            .collect();
        println!("  Music muted groups: {}", groups.join(", "));
    }
    if let Some(volume) = music.volume {
        println!("  Music volume: {:.3}", volume);
    }
    let sfx = state.sfx_state();
    if !sfx.active.is_empty() {
        println!("  Active SFX:");
        for instance in sfx.active.values() {
            if instance.parameters.is_empty() {
                println!("    - {} ({})", instance.cue, instance.handle);
            } else {
                println!(
                    "    - {} ({}) [{}]",
                    instance.cue,
                    instance.handle,
                    instance.parameters.join(", ")
                );
            }
        }
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
    if let Some(commentary) = state.cutscenes.commentary() {
        let status = if commentary.active {
            "active".to_string()
        } else {
            commentary
                .suppressed_reason
                .as_deref()
                .unwrap_or("suppressed")
                .to_string()
        };
        println!("  Commentary: {} ({})", commentary.display_label(), status);
    }
    let cut_scenes = state.cutscenes.cut_scene_stack();
    if !cut_scenes.is_empty() {
        println!("  Cut scenes:");
        for record in cut_scenes {
            let status = if record.suppressed {
                "blocked"
            } else {
                "active"
            };
            let label = if record.flags.is_empty() {
                record.display_label().to_string()
            } else {
                format!("{} ({})", record.display_label(), record.flags.join(", "))
            };
            match (&record.set_file, &record.sector) {
                (Some(set), Some(sector)) => {
                    println!("    {} [{}] {}:{}", label, status, set, sector)
                }
                (Some(set), None) => println!("    {} [{}] {}", label, status, set),
                (None, Some(sector)) => println!("    {} [{}] sector={}", label, status, sector),
                (None, None) => println!("    {} [{}]", label, status),
            }
        }
    }
    if !state.inventory.items().is_empty() {
        let mut items: Vec<_> = state.inventory.items().iter().collect();
        items.sort();
        let display = items
            .iter()
            .map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory: {}", display);
    }
    if !state.inventory.rooms().is_empty() {
        let mut rooms: Vec<_> = state.inventory.rooms().iter().collect();
        rooms.sort();
        let display = rooms
            .iter()
            .map(|room| room.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Inventory rooms: {}", display);
    }
    if let Some(current) = &state.current_set {
        if let Some(states) = state.sector_states.get(&current.set_file) {
            if let Some(geometry) = state.set_geometry.get(&current.set_file) {
                let mut overrides: Vec<(String, bool)> = Vec::new();
                for sector in &geometry.sectors {
                    if let Some(active) = states.get(&sector.name) {
                        if *active != sector.default_active {
                            overrides.push((sector.name.clone(), *active));
                        }
                    }
                }
                if !overrides.is_empty() {
                    overrides.sort_by(|a, b| a.0.cmp(&b.0));
                    println!("  Sector overrides:");
                    for (name, active) in overrides {
                        println!(
                            "    - {}: {}",
                            name,
                            if active { "active" } else { "inactive" }
                        );
                    }
                }
            }
        }
    }
    if !state.visible_objects.is_empty() {
        println!("  Visible objects:");
        for info in &state.visible_objects {
            let mut details: Vec<String> = Vec::new();
            if let Some(distance) = info.distance {
                details.push(format!("dist={distance:.3}"));
            }
            if let Some(angle) = info.angle {
                details.push(format!("angle={angle:.2}°"));
            }
            if let Some(within) = info.within_range {
                if within {
                    details.push("in-range".to_string());
                } else {
                    details.push("out-of-range".to_string());
                }
                if info.range > 0.0 {
                    details.push(format!("range={:.3}", info.range));
                }
            } else if info.range > 0.0 {
                details.push(format!("range={:.3}", info.range));
            }
            if info.in_hotlist {
                details.push("HOT".to_string());
            }
            let suffix = if details.is_empty() {
                String::new()
            } else {
                format!(" [{}]", details.join(", "))
            };
            println!("    - {} (#{}{})", info.display_name(), info.handle, suffix);
        }
    }
    if !state.menus.is_empty() {
        println!("  Menus:");
        for (name, menu_state) in state.menus.iter() {
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

pub(super) fn drive_active_scripts(
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

pub(super) fn strip_self(args: Variadic<Value>) -> Vec<Value> {
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

pub(super) fn value_to_bool(value: &Value) -> bool {
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

pub(super) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => text.to_str().ok().map(|s| s.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}
pub(super) fn describe_value(value: &Value) -> String {
    if let Some(text) = value_to_string(value) {
        return text;
    }
    match value {
        Value::Function(func) => describe_function(func),
        _ => format!("<{value:?}>"),
    }
}

fn heading_between(from: Vec3, to: Vec3) -> f64 {
    let dx = (to.x - from.x) as f64;
    let dy = (to.y - from.y) as f64;
    let mut angle = dy.atan2(dx).to_degrees();
    if angle < 0.0 {
        angle += 360.0;
    }
    angle
}

fn distance_between(a: Vec3, b: Vec3) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let dz = b.z - a.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::super::types::Vec3;
    use super::menus::install_menu_common;
    use super::pause::{install_game_pauser, PauseEvent, PauseLabel};
    use super::{
        candidate_paths, value_slice_to_vec3, AudioCallback, EngineContext, EngineContextHandle,
        ObjectSnapshot, ParsedSetGeometry,
    };
    use grim_analysis::resources::{ResourceGraph, SetMetadata, SetupSlot};
    use grim_formats::SetFile as SetFileData;
    use mlua::{Function, Lua, Table, Value};
    use std::cell::RefCell;
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

    #[test]
    fn handle_resolves_actor_and_logs_events() {
        let context = Rc::new(RefCell::new(make_context()));
        let handle = EngineContextHandle::new(context.clone());
        let actor_handle = {
            let mut ctx = context.borrow_mut();
            let (actor_id, handle_id) = ctx.register_actor_with_handle("Manny", Some(1400));
            ctx.put_actor_in_set(&actor_id, "Manny", "mo.set");
            ctx.switch_to_set("mo.set");
            handle_id
        };
        let resolved = handle
            .resolve_actor_handle(&["Manny", "manny"])
            .expect("actor handle");
        assert_eq!(resolved.0, actor_handle);
        handle.log_event("handle.test".to_string());
        let guard = context.borrow();
        let events = guard.events();
        assert!(events.iter().any(|event| event == "handle.test"));
    }

    #[test]
    fn achievement_flags_are_tracked() {
        let mut ctx = make_context();
        assert!(!ctx.achievement_has_been_established("ACHIEVE_CLASSIC_DRIVER"));
        ctx.set_achievement_eligibility("ACHIEVE_CLASSIC_DRIVER", true);
        assert!(ctx.achievement_has_been_established("ACHIEVE_CLASSIC_DRIVER"));
        assert!(ctx.achievement_is_eligible("ACHIEVE_CLASSIC_DRIVER"));
        ctx.set_achievement_eligibility("ACHIEVE_CLASSIC_DRIVER", false);
        assert!(!ctx.achievement_is_eligible("ACHIEVE_CLASSIC_DRIVER"));
    }

    fn make_context() -> EngineContext {
        make_context_with_callback(None)
    }

    fn make_context_with_callback(callback: Option<Rc<dyn AudioCallback>>) -> EngineContext {
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
        EngineContext::new(Rc::new(graph), false, None, callback)
    }

    fn install_menu_common_for_tests(lua: &Lua, context: Rc<RefCell<EngineContext>>) {
        install_game_pauser(lua, context.clone()).expect("game pauser installed");
        install_menu_common(lua, context).expect("menu_common installed");
    }

    #[test]
    fn menu_common_show_and_hide_track_visibility() {
        let lua = Lua::new();
        let context = Rc::new(RefCell::new(make_context()));
        install_menu_common_for_tests(&lua, context.clone());

        let globals = lua.globals();
        let menu: Table = globals.get("menu_common").expect("menu table");
        assert!(!menu.get::<_, bool>("is_visible").unwrap_or(true));

        let show: Function = menu.get("show").expect("show function");
        show.call::<_, ()>((menu.clone(),)).expect("show executes");

        {
            let guard = context.borrow();
            let state = guard.menus.get("menu_common").expect("state").borrow();
            assert!(state.visible, "menu state should mark visible");
        }
        assert!(menu.get::<_, bool>("is_visible").unwrap_or(false));

        let hide: Function = menu.get("hide").expect("hide function");
        hide.call::<_, ()>((menu.clone(),)).expect("hide executes");

        {
            let guard = context.borrow();
            {
                let state = guard.menus.get("menu_common").expect("state").borrow();
                assert!(!state.visible, "menu state should mark hidden");
            }
            assert!(guard.events.iter().any(|event| event == "menu_common.show"));
            assert!(guard.events.iter().any(|event| event == "menu_common.hide"));
        }
        assert!(!menu.get::<_, bool>("is_visible").unwrap_or(true));
    }

    #[test]
    fn menu_common_auto_freeze_toggles_game_pause() {
        let lua = Lua::new();
        let context = Rc::new(RefCell::new(make_context()));
        install_menu_common_for_tests(&lua, context.clone());

        let globals = lua.globals();
        let menu: Table = globals.get("menu_common").expect("menu table");
        let auto: Function = menu.get("auto_freeze").expect("auto_freeze");
        auto.call::<_, ()>((menu.clone(), true)).expect("auto on");

        let show: Function = menu.get("show").expect("show function");
        show.call::<_, ()>((menu.clone(),)).expect("show executes");

        let hide: Function = menu.get("hide").expect("hide function");
        hide.call::<_, ()>((menu.clone(),)).expect("hide executes");

        {
            let guard = context.borrow();
            assert!(guard.events.iter().any(|e| e == "game_pauser.pause on"));
            assert!(guard.events.iter().any(|e| e == "game_pauser.pause off"));
            assert!(guard
                .events
                .iter()
                .any(|e| e == "menu_common.auto_freeze on"));

            let history = &guard.pause_state().history;
            assert_eq!(history.len(), 2);
            assert_eq!(
                history[0],
                PauseEvent {
                    label: PauseLabel::Pause,
                    active: true
                }
            );
            assert_eq!(
                history[1],
                PauseEvent {
                    label: PauseLabel::Pause,
                    active: false
                }
            );
            assert!(
                !guard.pause_state().active,
                "auto-freeze should return game to unpaused state"
            );
        }
    }

    fn prepare_manny(ctx: &mut EngineContext, position: Vec3) {
        let (id, _handle) = ctx.register_actor_with_handle("Manny", Some(1001));
        ctx.put_actor_in_set(&id, "Manny", "mo.set");
        ctx.switch_to_set("mo.set");
        ctx.set_actor_position(&id, "Manny", position);
    }

    #[test]
    fn actor_scale_updates_snapshot_and_events() {
        let mut ctx = make_context();
        let (_id, handle) = ctx.register_actor_with_handle("manny", Some(1001));
        assert!(ctx.set_actor_scale_by_handle(handle, Some(1.25)));
        assert!(ctx.set_actor_collision_scale_by_handle(handle, Some(0.35)));

        let actor = ctx
            .actors
            .get("manny")
            .expect("actor registered with scale");
        assert_eq!(actor.scale, Some(1.25));
        assert_eq!(actor.collision_scale, Some(0.35));

        assert!(ctx
            .events
            .iter()
            .any(|event| event == "actor.manny.scale 1.250"));
        assert!(ctx
            .events
            .iter()
            .any(|event| event == "actor.manny.collision_scale 0.350"));

        let snapshot = ctx.geometry_snapshot();
        let manny = snapshot
            .actors
            .get("manny")
            .expect("geometry snapshot actor");
        assert_eq!(manny.scale, Some(1.25));
        assert_eq!(manny.collision_scale, Some(0.35));
    }

    #[derive(Default)]
    struct RecordingCallback {
        events: RefCell<Vec<String>>,
    }

    impl RecordingCallback {
        fn events(&self) -> Vec<String> {
            self.events.borrow().clone()
        }
    }

    impl AudioCallback for RecordingCallback {
        fn music_play(&self, cue: &str, params: &[String]) {
            let detail = if params.is_empty() {
                format!("music.play:{cue}")
            } else {
                format!("music.play:{cue}[{}]", params.join(","))
            };
            self.events.borrow_mut().push(detail);
        }

        fn music_stop(&self, mode: Option<&str>) {
            let label = mode.unwrap_or("<none>");
            self.events.borrow_mut().push(format!("music.stop:{label}"));
        }

        fn sfx_play(&self, cue: &str, params: &[String], handle: &str) {
            let mut detail = format!("sfx.play:{cue}->{handle}");
            if !params.is_empty() {
                detail.push_str(&format!("[{}]", params.join(",")));
            }
            self.events.borrow_mut().push(detail);
        }

        fn sfx_stop(&self, target: Option<&str>) {
            let label = target.unwrap_or("<none>");
            self.events.borrow_mut().push(format!("sfx.stop:{label}"));
        }
    }

    fn manny_geometry_set() -> SetFileData {
        let raw = "section: setups\n\tnumsetups\t5\n\tsetup\tmo_ddtws\n\tposition\t0.6\t2.0\t0.0\n\tinterest\t0.6\t2.2\t0.0\n\tsetup\tmo_winws\n\tposition\t0.2\t2.6\t0.0\n\tinterest\t0.2\t2.8\t0.0\n\tsetup\tmo_comin\n\tposition\t1.35\t0.25\t0.0\n\tinterest\t1.35\t0.45\t0.0\n\tsetup\tmo_mcecu\n\tposition\t0.62\t2.05\t0.0\n\tinterest\t0.62\t2.25\t0.0\n\tsetup\tmo_mnycu\n\tposition\t1.3\t0.2\t0.0\n\tinterest\t1.2\t0.4\t0.0\n\nsection: sectors\n\tsector\t\tmo_walk_default\n\tID\t\t6002\n\ttype\t\twalk\n\tdefault visibility\t\tvisible\n\theight\t\t0.0\n\tnumvertices\t4\n\tvertices:\t\t0.3\t1.7\t0.0\n\t         \t\t0.9\t1.7\t0.0\n\t         \t\t0.9\t2.3\t0.0\n\t         \t\t0.3\t2.3\t0.0\n\tnumtris 2\n\ttriangles:\t\t0 1 2\n\t\t\t\t0 2 3\n\tsector\t\tmo_window_walk\n\tID\t\t6100\n\ttype\t\twalk\n\tdefault visibility\t\tvisible\n\theight\t\t0.0\n\tnumvertices\t4\n\tvertices:\t\t-0.1\t2.3\t0.0\n\t         \t\t0.3\t2.3\t0.0\n\t         \t\t0.3\t2.8\t0.0\n\t         \t\t-0.1\t2.8\t0.0\n\tnumtris 2\n\ttriangles:\t\t0 1 2\n\t\t\t\t0 2 3\n\tsector\t\tmo_entry_walk\n\tID\t\t6200\n\ttype\t\twalk\n\tdefault visibility\t\tvisible\n\theight\t\t0.0\n\tnumvertices\t4\n\tvertices:\t\t1.1\t0.0\t0.0\n\t         \t\t1.6\t0.0\t0.0\n\t         \t\t1.6\t0.5\t0.0\n\t         \t\t1.1\t0.5\t0.0\n\tnumtris 2\n\ttriangles:\t\t0 1 2\n\t\t\t\t0 2 3\n";
        SetFileData::parse(raw.as_bytes()).expect("parse manny geometry")
    }

    fn install_manny_geometry(ctx: &mut EngineContext) {
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(manny_geometry_set()),
        );
        ctx.ensure_sector_state_map("mo.set");
    }

    #[test]
    fn manny_hot_sector_tracks_room_zones() {
        let mut ctx = make_context();
        install_manny_geometry(&mut ctx);

        prepare_manny(
            &mut ctx,
            Vec3 {
                x: 0.62,
                y: 2.05,
                z: 0.0,
            },
        );
        let desk_hit = ctx.default_sector_hit("manny", Some("hot"));
        assert_eq!(desk_hit.name, "mo_ddtws");

        ctx.set_actor_position(
            "manny",
            "Manny",
            Vec3 {
                x: 1.35,
                y: 0.2,
                z: 0.0,
            },
        );
        let door_hit = ctx.default_sector_hit("manny", Some("hot"));
        assert_eq!(door_hit.name, "mo_comin");

        ctx.set_actor_position(
            "manny",
            "Manny",
            Vec3 {
                x: 0.2,
                y: 2.6,
                z: 0.0,
            },
        );
        let window_hit = ctx.default_sector_hit("manny", Some("hot"));
        assert_eq!(window_hit.name, "mo_winws");
    }

    #[test]
    fn audio_callbacks_receive_music_and_sfx_events() {
        let callback = Rc::new(RecordingCallback::default());
        let callback_handle: Rc<dyn AudioCallback> = callback.clone();
        let mut ctx = make_context_with_callback(Some(callback_handle));

        ctx.play_music("intro".to_string(), vec!["loop=true".to_string()]);
        assert_eq!(
            ctx.music_state()
                .current
                .as_ref()
                .map(|cue| cue.name.as_str()),
            Some("intro")
        );

        ctx.stop_music(Some("immediate".to_string()));
        assert!(ctx.music_state().current.is_none());

        let handle = ctx.play_sound_effect("doorbell".to_string(), Vec::new());
        assert!(ctx.sfx_state().active.contains_key(&handle));

        ctx.stop_sound_effect(Some(handle.clone()));
        assert!(!ctx.sfx_state().active.contains_key(&handle));

        let events = callback.events();
        assert_eq!(
            events,
            vec![
                "music.play:intro[loop=true]".to_string(),
                "music.stop:immediate".to_string(),
                format!("sfx.play:doorbell->{handle}"),
                format!("sfx.stop:{handle}"),
            ]
        );

        assert!(ctx
            .music_state()
            .history
            .iter()
            .any(|entry| entry.starts_with("play intro")));
        assert!(ctx
            .music_state()
            .history
            .iter()
            .any(|entry| entry == "stop immediate"));
        assert!(ctx
            .sfx_state()
            .history
            .iter()
            .any(|entry| entry.starts_with("sfx.play doorbell")));
        assert!(ctx
            .sfx_state()
            .history
            .iter()
            .any(|entry| entry.starts_with("sfx.stop")));
    }
    fn sample_geometry_set() -> SetFileData {
        let raw = "section: setups\n\tnumsetups\t1\n\tsetup\tcam_a\n\tposition\t0.0\t0.0\t0.0\n\tinterest\t0.3\t0.3\t0.0\n\troll\t\t0.0\n\tfov\t\t45.0\n\tnclip\t\t0.1\n\tfclip\t\t100.0\n\nsection: sectors\n\tsector\t\tdesk_walk\n\tID\t\t10\n\ttype\t\twalk\n\tdefault visibility\t\tvisible\n\theight\t\t0.0\n\tnumvertices\t4\n\tvertices:\t\t0.0\t0.0\t0.0\n\t         \t\t1.0\t0.0\t0.0\n\t         \t\t1.0\t1.0\t0.0\n\t         \t\t0.0\t1.0\t0.0\n\tnumtris 2\n\ttriangles:\t\t0 1 2\n\t\t\t\t0 2 3\n";
        SetFileData::parse(raw.as_bytes()).expect("parse sample set")
    }

    #[test]
    fn geometry_walk_sector_selected_for_point() {
        let mut ctx = make_context();
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.current_set = Some(super::SetSnapshot {
            set_file: "mo.set".to_string(),
            variable_name: "mo".to_string(),
            display_name: None,
        });
        let (id, _handle) = ctx.register_actor_with_handle("Guard", Some(2002));
        ctx.put_actor_in_set(&id, "Guard", "mo.set");
        ctx.set_actor_position(
            &id,
            "Guard",
            Vec3 {
                x: 0.25,
                y: 0.25,
                z: 0.0,
            },
        );
        let hit = ctx.geometry_sector_hit(&id, "walk").expect("walk sector");
        assert_eq!(hit.name, "desk_walk");
        assert_eq!(hit.kind, "WALK");
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
            sectors: Vec::new(),
        });

        assert_eq!(ctx.visible_object_handles(), vec![3000]);

        ctx.set_actor_visibility(&id, "Lamp", false);
        assert!(ctx.visible_object_handles().is_empty());

        ctx.set_actor_visibility(&id, "Lamp", true);
        assert_eq!(ctx.visible_object_handles(), vec![3000]);
    }

    #[test]
    fn music_state_tracks_basic_transitions() {
        let mut ctx = make_context();
        ctx.play_music("intro".to_string(), vec!["loop=true".to_string()]);
        assert_eq!(
            ctx.music_state()
                .current
                .as_ref()
                .map(|cue| cue.name.as_str()),
            Some("intro")
        );
        assert!(ctx
            .music_state()
            .history
            .last()
            .expect("music history entry")
            .starts_with("play intro"));

        ctx.queue_music("next".to_string(), Vec::new());
        assert_eq!(ctx.music_state().queued.len(), 1);

        ctx.pause_music();
        assert!(ctx.music_state().paused);
        ctx.resume_music();
        assert!(!ctx.music_state().paused);

        ctx.set_music_state(Some("office".to_string()));
        assert_eq!(ctx.music_state().current_state.as_deref(), Some("office"));
        ctx.push_music_state(Some("alert".to_string()));
        assert_eq!(
            ctx.music_state().state_stack.last().map(|s| s.as_str()),
            Some("alert")
        );
        ctx.pop_music_state();
        assert!(ctx.music_state().state_stack.is_empty());

        ctx.stop_music(Some("immediate".to_string()));
        assert!(ctx.music_state().current.is_none());
    }

    #[test]
    fn sfx_state_registers_and_clears_instances() {
        let mut ctx = make_context();
        let handle = ctx.play_sound_effect("door_knock".to_string(), vec!["loop=0".to_string()]);
        assert!(ctx.sfx_state().active.contains_key(&handle));
        ctx.stop_sound_effect(Some(handle.clone()));
        assert!(!ctx.sfx_state().active.contains_key(&handle));

        ctx.play_sound_effect("ambient".to_string(), Vec::new());
        ctx.play_sound_effect("buzz".to_string(), Vec::new());
        assert!(!ctx.sfx_state().active.is_empty());
        ctx.stop_sound_effect(None);
        assert!(ctx.sfx_state().active.is_empty());
    }

    #[test]
    fn visible_objects_respect_sector_activation() {
        let mut ctx = make_context();
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.switch_to_set("mo.set");
        let object_handle = 3100;
        ctx.register_object(ObjectSnapshot {
            handle: object_handle,
            name: "desk".to_string(),
            string_name: Some("desk".to_string()),
            set_file: Some("mo.set".to_string()),
            position: Some(Vec3 {
                x: 0.25,
                y: 0.25,
                z: 0.0,
            }),
            range: 0.5,
            touchable: true,
            visible: true,
            interest_actor: None,
            sectors: Vec::new(),
        });
        assert_eq!(ctx.visible_object_handles(), vec![object_handle]);
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", false);
        assert!(ctx.visible_object_handles().is_empty());
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", true);
        assert_eq!(ctx.visible_object_handles(), vec![object_handle]);
    }

    #[test]
    fn interest_actor_objects_track_sector_activation() {
        let mut ctx = make_context();
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.switch_to_set("mo.set");
        let (actor_id, actor_handle) = ctx.register_actor_with_handle("Helper", Some(2100));
        ctx.put_actor_in_set(&actor_id, "Helper", "mo.set");
        ctx.register_object(ObjectSnapshot {
            handle: 3200,
            name: "helper".to_string(),
            string_name: Some("helper".to_string()),
            set_file: None,
            position: None,
            range: 0.5,
            touchable: true,
            visible: true,
            interest_actor: Some(actor_handle),
            sectors: Vec::new(),
        });
        ctx.set_actor_position(
            &actor_id,
            "Helper",
            Vec3 {
                x: 0.25,
                y: 0.25,
                z: 0.0,
            },
        );
        assert_eq!(ctx.visible_object_handles(), vec![3200]);
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", false);
        assert!(ctx.visible_object_handles().is_empty());
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", true);
        assert_eq!(ctx.visible_object_handles(), vec![3200]);
        let sectors = ctx.objects.get(&3200).expect("object").sectors.clone();
        assert!(!sectors.is_empty(), "expected computed sectors");
        assert!(sectors.iter().any(|sector| sector.name == "desk_walk"));
    }

    #[test]
    fn commentary_respects_sector_activation() {
        let mut ctx = make_context();
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.switch_to_set("mo.set");
        let object_handle = 3300;
        ctx.register_object(ObjectSnapshot {
            handle: object_handle,
            name: "tube_commentary".to_string(),
            string_name: Some("tube".to_string()),
            set_file: Some("mo.set".to_string()),
            position: Some(Vec3 {
                x: 0.25,
                y: 0.25,
                z: 0.0,
            }),
            range: 0.5,
            touchable: true,
            visible: true,
            interest_actor: None,
            sectors: Vec::new(),
        });
        ctx.record_visible_objects(&[object_handle]);
        ctx.set_commentary_active(true, Some("Year1MannysOfficeDesign".to_string()));
        let commentary = ctx.cutscenes.commentary().expect("commentary state");
        assert!(commentary.active, "commentary should start active");
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", false);
        let commentary = ctx.cutscenes.commentary().expect("commentary state");
        assert!(
            !commentary.active,
            "commentary should suspend when sector is inactive"
        );
        assert_eq!(commentary.suppressed_reason.as_deref(), Some("not_visible"));
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", true);
        let commentary = ctx.cutscenes.commentary().expect("commentary state");
        assert!(
            commentary.active,
            "commentary should resume once the sector is reactivated"
        );
    }

    #[test]
    fn cut_scene_tracks_sector_activation() {
        let mut ctx = make_context();
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.switch_to_set("mo.set");
        let (manny_id, _handle) = ctx.register_actor_with_handle("Manny", Some(1001));
        ctx.put_actor_in_set(&manny_id, "Manny", "mo.set");
        ctx.set_actor_position(
            &manny_id,
            "Manny",
            Vec3 {
                x: 0.25,
                y: 0.25,
                z: 0.0,
            },
        );
        ctx.push_cut_scene(Some("demo".to_string()), Vec::new());
        let record = ctx
            .cutscenes
            .cut_scene_stack()
            .last()
            .expect("cut scene record");
        assert_eq!(record.set_file.as_deref(), Some("mo.set"));
        assert_eq!(record.sector.as_deref(), Some("desk_walk"));
        assert!(!record.suppressed, "cut scene should start active");
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", false);
        assert!(
            ctx.cutscenes
                .cut_scene_stack()
                .last()
                .expect("cut scene")
                .suppressed
        );
        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", true);
        assert!(
            !ctx.cutscenes
                .cut_scene_stack()
                .last()
                .expect("cut scene")
                .suppressed
        );
    }

    #[test]
    fn geometry_snapshot_reflects_sector_state() {
        let mut ctx = make_context();
        ctx.set_geometry.insert(
            "mo.set".to_string(),
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.switch_to_set("mo.set");
        let snapshot = ctx.geometry_snapshot();
        let set = snapshot
            .sets
            .iter()
            .find(|set| set.set_file == "mo.set")
            .expect("mo.set snapshot");
        let desk_sector = set
            .sectors
            .iter()
            .find(|sector| sector.name == "desk_walk")
            .expect("desk_walk sector");
        assert!(desk_sector.active, "desk_walk should start active");

        let _ = ctx.set_sector_active(Some("mo.set"), "desk_walk", false);
        let snapshot = ctx.geometry_snapshot();
        let set = snapshot
            .sets
            .iter()
            .find(|set| set.set_file == "mo.set")
            .expect("mo.set snapshot");
        let desk_sector = set
            .sectors
            .iter()
            .find(|sector| sector.name == "desk_walk")
            .expect("desk_walk sector");
        assert!(
            !desk_sector.active,
            "desk_walk should reflect toggled state in snapshot"
        );

        let current = snapshot.current_set.expect("current set snapshot");
        assert_eq!(current.set_file, "mo.set");
    }
}
