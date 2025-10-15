use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

mod actors;
mod audio;
mod bindings;
mod cutscenes;
mod geometry;
mod geometry_export;
mod inventory;
mod menus;
mod objects;
mod pause;
mod scripts;
mod sets;

use actors::{runtime::ActorRuntime, ActorSnapshot, ActorStore};
pub use audio::AudioCallback;
use audio::{AudioRuntime, AudioRuntimeAdapter, MusicState, SfxState};
use cutscenes::{CommentaryRecord, CutsceneRuntime, DialogState};
use geometry::SectorHit;
use inventory::InventoryState;
use menus::{MenuRegistry, MenuState};
use objects::{ObjectRuntime, ObjectSectorRef, ObjectSnapshot};
use pause::{PauseLabel, PauseState};
use scripts::{ScriptCleanup, ScriptRuntime};
use sets::{SectorToggleResult, SetRuntime, SetRuntimeSnapshot};

pub(super) use bindings::{
    call_boot, describe_value, drive_active_scripts, dump_runtime_summary, install_globals,
    install_package_path, install_render_helpers, load_system_script, override_boot_stubs,
    split_self, strip_self, value_to_bool, value_to_f32, value_to_string,
};

use super::types::{Vec3, MANNY_OFFICE_SEED_POS, MANNY_OFFICE_SEED_ROT};
use crate::geometry_snapshot::LuaGeometrySnapshot;
use crate::lab_collection::LabCollection;
use grim_analysis::resources::ResourceGraph;
use grim_formats::SectorKind as SetSectorKind;
use mlua::{Lua, RegistryKey, Result as LuaResult};

#[derive(Debug, Default, Clone)]
struct AchievementState {
    eligible: bool,
    established: bool,
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
    scripts: ScriptRuntime,
    events: Vec<String>,
    sets: SetRuntime,
    actors: ActorStore,
    inventory: InventoryState,
    menus: MenuRegistry,
    voice_effect: Option<String>,
    objects: ObjectRuntime,
    achievements: BTreeMap<String, AchievementState>,
    cutscenes: CutsceneRuntime,
    pause: PauseState,
    audio: AudioRuntime,
}

impl EngineContext {
    pub(super) fn new(
        resources: Rc<ResourceGraph>,
        verbose: bool,
        lab_collection: Option<Rc<LabCollection>>,
        audio_callback: Option<Rc<dyn AudioCallback>>,
    ) -> Self {
        let sets = SetRuntime::new(resources, verbose, lab_collection);
        EngineContext {
            verbose,
            scripts: ScriptRuntime::new(),
            events: Vec::new(),
            sets,
            actors: ActorStore::new(1100),
            inventory: InventoryState::new(),
            menus: MenuRegistry::new(),
            voice_effect: None,
            objects: ObjectRuntime::new(),
            achievements: BTreeMap::new(),
            cutscenes: CutsceneRuntime::new(),
            pause: PauseState::default(),
            audio: AudioRuntime::new(audio_callback),
        }
    }

    fn actor_runtime(&mut self) -> ActorRuntime<'_> {
        ActorRuntime::new(&mut self.actors, &mut self.events)
    }

    fn audio_runtime(&mut self) -> AudioRuntimeAdapter<'_> {
        AudioRuntimeAdapter::new(&mut self.audio, &mut self.events)
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
            .sets
            .current_set()
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
        self.audio_runtime().play_music(track, params);
    }

    fn queue_music(&mut self, track: String, params: Vec<String>) {
        self.audio_runtime().queue_music(track, params);
    }

    fn stop_music(&mut self, mode: Option<String>) {
        self.audio_runtime().stop_music(mode);
    }

    fn pause_music(&mut self) {
        self.audio_runtime().pause_music();
    }

    fn resume_music(&mut self) {
        self.audio_runtime().resume_music();
    }

    fn set_music_state(&mut self, state: Option<String>) {
        self.audio_runtime().set_music_state(state);
    }

    fn push_music_state(&mut self, state: Option<String>) {
        self.audio_runtime().push_music_state(state);
    }

    fn pop_music_state(&mut self) {
        self.audio_runtime().pop_music_state();
    }

    fn mute_music_group(&mut self, group: Option<String>) {
        self.audio_runtime().mute_music_group(group);
    }

    fn unmute_music_group(&mut self, group: Option<String>) {
        self.audio_runtime().unmute_music_group(group);
    }

    fn set_music_volume(&mut self, volume: Option<f32>) {
        self.audio_runtime().set_music_volume(volume);
    }

    fn play_sound_effect(&mut self, cue: String, params: Vec<String>) -> String {
        self.audio_runtime().play_sound_effect(cue, params)
    }

    fn stop_sound_effect(&mut self, target: Option<String>) {
        self.audio_runtime().stop_sound_effect(target);
    }

    fn start_imuse_sound(&mut self, cue: String, priority: Option<i32>, group: Option<i32>) -> i64 {
        let mut params = Vec::new();
        if let Some(value) = priority {
            params.push(format!("priority={value}"));
        }
        if let Some(value) = group {
            params.push(format!("group={value}"));
        }
        let mut runtime = self.audio_runtime();
        let handle = runtime.play_sound_effect(cue, params);
        if let Some(instance) = runtime.sfx_mut().active.get_mut(&handle) {
            instance.group = group;
            instance.play_count = 1;
            instance.numeric
        } else {
            -1
        }
    }

    fn stop_sound_effect_by_numeric(&mut self, numeric: i64) {
        self.audio_runtime().stop_sound_effect_by_numeric(numeric);
    }

    fn set_sound_param(&mut self, numeric: i64, param: i32, value: i32) {
        self.audio_runtime()
            .set_sound_param(numeric, param, value);
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
        let (handle, event) = self.scripts.start_script(label, callable);
        self.log_event(event);
        handle
    }

    fn has_script_with_label(&self, label: &str) -> bool {
        self.scripts.has_label(label)
    }

    fn attach_script_thread(&mut self, handle: u32, key: RegistryKey) {
        self.scripts.attach_thread(handle, key);
    }

    fn script_thread_key(&self, handle: u32) -> Option<&RegistryKey> {
        self.scripts.thread_key(handle)
    }

    fn increment_script_yield(&mut self, handle: u32) {
        self.scripts.increment_yield(handle);
    }

    fn script_yield_count(&self, handle: u32) -> Option<u32> {
        self.scripts.yield_count(handle)
    }

    fn script_label(&self, handle: u32) -> Option<&str> {
        self.scripts.label(handle)
    }

    fn active_script_handles(&self) -> Vec<u32> {
        self.scripts.active_handles()
    }

    fn is_script_running(&self, handle: u32) -> bool {
        self.scripts.is_running(handle)
    }

    fn complete_script(&mut self, handle: u32) -> ScriptCleanup {
        let (cleanup, event) = self.scripts.complete_script(handle);
        if let Some(message) = event {
            self.log_event(message);
        }
        cleanup
    }

    fn ensure_actor_mut(&mut self, id: &str, label: &str) -> &mut ActorSnapshot {
        self.actors.ensure_actor_mut(id, label)
    }

    fn select_actor(&mut self, id: &str, label: &str) {
        self.actor_runtime().select_actor(id, label);
    }

    fn switch_to_set(&mut self, set_file: &str) {
        {
            self.sets.switch_to_set(&mut self.events, set_file);
        }
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
        self.sets.mark_set_loaded(&mut self.events, set_file);
    }

    fn ensure_sector_state_map(&mut self, set_file: &str) -> bool {
        self.sets
            .ensure_sector_state_map(&mut self.events, set_file)
    }

    fn set_sector_active(
        &mut self,
        set_file_hint: Option<&str>,
        sector_name: &str,
        active: bool,
    ) -> SectorToggleResult {
        let result =
            self.sets
                .set_sector_active(&mut self.events, set_file_hint, sector_name, active);
        if let SectorToggleResult::Applied {
            set_file, sector, ..
        }
        | SectorToggleResult::NoChange {
            set_file, sector, ..
        } = &result
        {
            self.handle_sector_dependents(set_file, sector, active);
        }
        result
    }

    fn is_sector_active(&self, set_file: &str, sector_name: &str) -> bool {
        self.sets.is_sector_active(set_file, sector_name)
    }

    fn record_current_setup(&mut self, set_file: &str, setup: i32) {
        self.sets.record_current_setup(set_file, setup);
    }

    fn current_setup_for(&self, set_file: &str) -> Option<i32> {
        self.sets.current_setup_for(set_file)
    }

    fn set_actor_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        self.actor_runtime().set_actor_costume(id, label, costume);
    }

    fn set_actor_base_costume(&mut self, id: &str, label: &str, costume: Option<String>) {
        self.actor_runtime()
            .set_actor_base_costume(id, label, costume);
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
        self.actor_runtime().push_actor_costume(id, label, costume)
    }

    fn pop_actor_costume(&mut self, id: &str, label: &str) -> Option<String> {
        self.actor_runtime().pop_actor_costume(id, label)
    }

    fn set_actor_current_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        self.actor_runtime()
            .set_actor_current_chore(id, label, chore, costume);
    }

    fn set_actor_walk_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        self.actor_runtime()
            .set_actor_walk_chore(id, label, chore, costume);
    }

    fn set_actor_talk_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        drop: Option<String>,
        costume: Option<String>,
    ) {
        self.actor_runtime()
            .set_actor_talk_chore(id, label, chore, drop, costume);
    }

    fn set_actor_mumble_chore(
        &mut self,
        id: &str,
        label: &str,
        chore: Option<String>,
        costume: Option<String>,
    ) {
        self.actor_runtime()
            .set_actor_mumble_chore(id, label, chore, costume);
    }

    fn set_actor_talk_color(&mut self, id: &str, label: &str, color: Option<String>) {
        self.actor_runtime().set_actor_talk_color(id, label, color);
    }

    fn set_actor_head_target(&mut self, id: &str, label: &str, target: Option<String>) {
        self.actor_runtime()
            .set_actor_head_target(id, label, target);
    }

    fn set_actor_head_look_rate(&mut self, id: &str, label: &str, rate: Option<f32>) {
        self.actor_runtime()
            .set_actor_head_look_rate(id, label, rate);
    }

    fn set_actor_collision_mode(&mut self, id: &str, label: &str, mode: Option<String>) {
        self.actor_runtime()
            .set_actor_collision_mode(id, label, mode);
    }

    fn set_actor_ignore_boxes(&mut self, id: &str, label: &str, ignore: bool) {
        self.actor_runtime()
            .set_actor_ignore_boxes(id, label, ignore);
    }

    fn put_actor_in_set(&mut self, id: &str, label: &str, set_file: &str) {
        self.actor_runtime().put_actor_in_set(id, label, set_file);
    }

    fn actor_at_interest(&mut self, id: &str, label: &str) {
        self.actor_runtime().actor_at_interest(id, label);
    }

    fn set_actor_position(&mut self, id: &str, label: &str, position: Vec3) {
        let handle = self.actor_runtime().set_actor_position(id, label, position);
        if handle != 0 {
            self.update_object_position_for_actor(handle, position);
        }
    }

    fn set_actor_rotation(&mut self, id: &str, label: &str, rotation: Vec3) {
        self.actor_runtime().set_actor_rotation(id, label, rotation);
    }

    fn set_actor_scale(&mut self, id: &str, label: &str, scale: Option<f32>) {
        self.actor_runtime().set_actor_scale(id, label, scale);
    }

    fn set_actor_collision_scale(&mut self, id: &str, label: &str, scale: Option<f32>) {
        self.actor_runtime()
            .set_actor_collision_scale(id, label, scale);
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
                    .or_else(|| self.sets.current_set().map(|set| set.set_file.clone())),
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
            if self.sets.set_geometry().contains_key(set_file)
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
        self.actor_runtime().record_sector_hit(id, label, hit);
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

        if let Some(current) = self.sets.current_set() {
            if let Some(descriptor) = self.sets.available_sets().get(&current.set_file) {
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
    fn evaluate_sector_name(&self, actor_id: &str, query: &str) -> bool {
        if actor_id.eq_ignore_ascii_case("manny") {
            matches!(query, "manny" | "office" | "desk")
        } else {
            false
        }
    }

    fn find_script_handle(&self, label: &str) -> Option<u32> {
        self.scripts.find_handle(label)
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

    fn register_object(&mut self, mut snapshot: ObjectSnapshot) {
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
        let existed = self.objects.register(snapshot);
        if let Some(actor_handle) = interest_actor {
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
        if let Some(snapshot) = self.objects.unregister(handle) {
            self.log_event(format!("object.remove {} (#{handle})", snapshot.name));
        }
        self.refresh_commentary_visibility();
    }

    fn visible_object_handles(&self) -> Vec<i64> {
        let current = self.sets.current_set().map(|set| set.set_file.as_str());
        self.objects
            .visible_handles(current, |set, sector| self.is_sector_active(set, sector))
    }

    fn record_visible_objects(&mut self, handles: &[i64]) {
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
        self.objects.record_visible_objects(
            handles,
            &self.actors,
            actor_position,
            actor_handle,
            |message| log_messages.push(message),
        );
        for message in log_messages {
            self.log_event(message);
        }
        self.refresh_commentary_visibility();
    }

    fn object_position_by_actor(&self, actor_handle: u32) -> Option<Vec3> {
        self.objects.object_position_by_actor(actor_handle)
    }

    fn update_object_position_for_actor(&mut self, actor_handle: u32, position: Vec3) {
        if let Some(object_handle) = self.objects.handle_for_actor(actor_handle) {
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
                if let Some(object) = self.objects.object_mut(object_handle) {
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
                if let Some(object) = self.objects.object_mut(object_handle) {
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
        if let Some(object) = self.objects.object_mut(handle) {
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
        if let Some(object) = self.objects.object_mut(handle) {
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
        self.objects.commentary_candidate_handle()
    }

    fn commentary_object_visible(&self, record: &CommentaryRecord) -> bool {
        let current = self.sets.current_set().map(|set| set.set_file.as_str());
        self.objects
            .commentary_object_visible(record, current, |set, sector| {
                self.is_sector_active(set, sector)
            })
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
        self.sets.point_in_active_walk(set_file, point)
    }

    fn actor_snapshot(&self, actor_id: &str) -> Option<&ActorSnapshot> {
        self.actors.actor_snapshot(actor_id)
    }

    fn actor_position_xy(&self, actor_id: &str) -> Option<(f32, f32)> {
        self.actors.actor_position_xy(actor_id)
    }

    fn geometry_sector_hit(&self, actor_id: &str, raw_kind: &str) -> Option<SectorHit> {
        self.sets.current_set()?;
        let point = self.actor_position_xy(actor_id)?;
        self.sets.geometry_sector_hit(raw_kind, point)
    }

    pub(super) fn geometry_sector_name(&self, actor_id: &str, raw_kind: &str) -> Option<String> {
        self.geometry_sector_hit(actor_id, raw_kind)
            .map(|hit| hit.name)
    }

    fn visible_sector_hit(&self, _actor_id: &str, request: &str) -> Option<SectorHit> {
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
            if let Some(object_handle) = self.objects.handle_for_actor(actor.handle) {
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
        let SetRuntimeSnapshot {
            current_set,
            loaded_sets,
            current_setups,
            available_sets,
            set_geometry,
            sector_states,
        } = self.sets.snapshot();
        geometry_export::SnapshotState {
            current_set,
            selected_actor: self.actors.selected_actor_id().map(|id| id.to_string()),
            voice_effect: self.voice_effect.clone(),
            loaded_sets,
            current_setups,
            available_sets,
            set_geometry,
            sector_states,
            actors: self.actors.clone_map(),
            objects: self.objects.clone_records(),
            actor_handles: self.actors.clone_handles(),
            visible_objects: self.objects.visible_objects().to_vec(),
            hotlist_handles: self.objects.hotlist_handles().to_vec(),
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

pub(crate) fn heading_between(from: Vec3, to: Vec3) -> f64 {
    let dx = (to.x - from.x) as f64;
    let dy = (to.y - from.y) as f64;
    let mut angle = dy.atan2(dx).to_degrees();
    if angle < 0.0 {
        angle += 360.0;
    }
    angle
}

pub(crate) fn distance_between(a: Vec3, b: Vec3) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let dz = b.z - a.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::super::types::Vec3;
    use super::bindings::{candidate_paths, value_slice_to_vec3};
    use super::menus::install_menu_common;
    use super::objects::ObjectSnapshot;
    use super::pause::{install_game_pauser, PauseEvent, PauseLabel};
    use super::geometry::ParsedSetGeometry;
    use super::{AudioCallback, EngineContext, EngineContextHandle};
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
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
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
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
            ParsedSetGeometry::from_set_file(sample_geometry_set()),
        );
        ctx.switch_to_set("mo.set");
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
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
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
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
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
        let sectors = ctx.objects.object(3200).expect("object").sectors.clone();
        assert!(!sectors.is_empty(), "expected computed sectors");
        assert!(sectors.iter().any(|sector| sector.name == "desk_walk"));
    }

    #[test]
    fn commentary_respects_sector_activation() {
        let mut ctx = make_context();
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
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
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
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
        ctx.sets.insert_geometry_for_tests(
            "mo.set",
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
