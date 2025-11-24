use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

pub(super) type TubePoseAliasCache = Rc<RefCell<Option<BTreeMap<String, String>>>>;

fn normalize_tube_chore_token(map: &BTreeMap<String, String>, raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }

    if let Some(alias) = map.get(raw) {
        return Some(alias.clone());
    }

    if let Ok(value) = raw.parse::<f64>() {
        let key = if (value - value.trunc()).abs() < f64::EPSILON {
            (value as i64).to_string()
        } else {
            value.to_string()
        };
        if let Some(alias) = map.get(&key) {
            return Some(alias.clone());
        }
    }

    None
}

pub(super) struct ActorEventParts<'a> {
    pub head: &'a str,
    pub actor_id: &'a str,
    pub method: &'a str,
    pub tokens: Vec<&'a str>,
}

pub(super) fn parse_actor_event(event: &str) -> Option<ActorEventParts<'_>> {
    let tokens: Vec<&str> = event.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let head = tokens[0];
    if !head.starts_with("actor.") {
        return None;
    }
    let Some(method_sep) = head.rfind('.') else {
        return None;
    };
    let prefix_len = "actor.".len();
    if method_sep <= prefix_len {
        return None;
    }
    let method = &head[method_sep + 1..];
    let actor_id = &head[prefix_len..method_sep];
    Some(ActorEventParts {
        head,
        actor_id,
        method,
        tokens,
    })
}

pub(super) fn normalize_tube_event(map: &BTreeMap<String, String>, event: &str) -> Option<String> {
    let parts = parse_actor_event(event)?;
    if parts.method != "complete_chore"
        && parts.method != "chore"
        && parts.method != "walk_chore"
        && parts.method != "talk_chore"
        && parts.method != "mumble_chore"
    {
        return None;
    }

    if !parts.actor_id.contains("tube") {
        return None;
    }

    if parts.tokens.len() < 2 {
        return None;
    }

    let target = parts.tokens[1];
    let Some(alias) = normalize_tube_chore_token(map, target) else {
        return None;
    };

    let mut updated = Vec::with_capacity(parts.tokens.len());
    updated.push(parts.head.to_string());
    updated.push(alias);
    for token in parts.tokens.iter().skip(2) {
        updated.push((*token).to_string());
    }
    Some(updated.join(" "))
}

mod actors;
mod audio;
mod bindings;
mod cutscenes;
mod geometry;
mod menus;
mod movement;
mod movies;
mod objects;
mod pause;
mod scripts;
mod sets;

use actors::{runtime::ActorRuntime, ActorSnapshot, ActorStore};
use audio::{AudioRuntime, AudioRuntimeAdapter, AudioRuntimeView};
use cutscenes::{
    CommentaryRecord, CutsceneRuntime, CutsceneRuntimeAdapter, CutsceneRuntimeView, DialogState,
};
use geometry::SectorHit;
use menus::{MenuRegistry, MenuRegistryView, MenuState};
use movement::{MovementRuntimeAdapter, MovementRuntimeView};
use movies::select_playback;
use objects::{ObjectRuntime, ObjectRuntimeAdapter, ObjectSnapshot};
use pause::{PauseLabel, PauseRuntimeView, PauseState};
use scripts::{ScriptCleanup, ScriptRuntime, ScriptRuntimeAdapter, ScriptRuntimeView};
use sets::{SectorToggleResult, SetRuntime, SetRuntimeAdapter, SetRuntimeView};

pub(super) use bindings::{
    call_boot, describe_value, drive_active_scripts, dump_runtime_summary, ensure_intro_cutscene,
    install_globals, install_package_path, load_system_script, override_boot_stubs, split_self,
    strip_self, value_to_bool, value_to_f32, value_to_string,
};

use super::types::{Vec3, MANNY_OFFICE_SEED_POS, MANNY_OFFICE_SEED_ROT};
use grim_analysis::resources::ResourceGraph;
use mlua::RegistryKey;

#[cfg(test)]
mod hook_tests {
    use super::normalize_tube_event;
    use std::collections::BTreeMap;

    #[test]
    fn normalize_tube_event_translates_numeric_chore() {
        let mut map = BTreeMap::new();
        map.insert("9".to_string(), "mo_tube_set_closed_w_can".to_string());
        let event = "actor.motx083tube.complete_chore 9 mo_tube.cos";
        let normalized = normalize_tube_event(&map, event).expect("normalized event");
        assert_eq!(
            normalized,
            "actor.motx083tube.complete_chore mo_tube_set_closed_w_can mo_tube.cos"
        );
    }
}

pub(super) struct EngineContext {
    verbose: bool,
    headless: bool,
    install_root: PathBuf,
    scripts: ScriptRuntime,
    events: Vec<String>,
    sets: SetRuntime,
    actors: ActorStore,
    menus: MenuRegistry,
    voice_effect: Option<String>,
    objects: ObjectRuntime,
    cutscenes: CutsceneRuntime,
    pause: PauseState,
    audio: AudioRuntime,
    tube_pose_aliases: TubePoseAliasCache,
}

impl EngineContext {
    pub(super) fn new(
        resources: Rc<ResourceGraph>,
        verbose: bool,
        headless: bool,
        install_root: PathBuf,
        tube_pose_aliases: TubePoseAliasCache,
    ) -> Self {
        let sets = SetRuntime::new(resources.clone());
        EngineContext {
            verbose,
            headless,
            install_root,
            scripts: ScriptRuntime::new(),
            events: Vec::new(),
            sets,
            actors: ActorStore::new(1100),
            menus: MenuRegistry::new(),
            voice_effect: None,
            objects: ObjectRuntime::new(),
            cutscenes: CutsceneRuntime::new(),
            pause: PauseState::default(),
            audio: AudioRuntime::new(),
            tube_pose_aliases,
        }
    }

    fn actor_runtime(&mut self) -> ActorRuntime<'_> {
        ActorRuntime::new(
            &mut self.actors,
            &mut self.events,
            self.tube_pose_aliases.clone(),
        )
    }

    fn set_runtime(&mut self) -> SetRuntimeAdapter<'_> {
        SetRuntimeAdapter::new(&mut self.sets, &mut self.events)
    }

    fn audio_runtime(&mut self) -> AudioRuntimeAdapter<'_> {
        AudioRuntimeAdapter::new(&mut self.audio, &mut self.events)
    }

    fn audio_view(&self) -> AudioRuntimeView<'_> {
        AudioRuntimeView::new(&self.audio)
    }

    fn set_view(&self) -> SetRuntimeView<'_> {
        SetRuntimeView::new(&self.sets)
    }

    fn menu_view(&self) -> MenuRegistryView<'_> {
        MenuRegistryView::new(&self.menus)
    }

    fn object_runtime(&mut self) -> ObjectRuntimeAdapter<'_> {
        ObjectRuntimeAdapter::new(&mut self.objects, &mut self.events, &self.actors)
    }

    fn cutscene_runtime(&mut self) -> CutsceneRuntimeAdapter<'_> {
        CutsceneRuntimeAdapter::new(&mut self.cutscenes, &mut self.events)
    }

    fn cutscene_view(&self) -> CutsceneRuntimeView<'_> {
        CutsceneRuntimeView::new(&self.cutscenes)
    }

    fn script_runtime(&mut self) -> ScriptRuntimeAdapter<'_> {
        ScriptRuntimeAdapter::new(&mut self.scripts, &mut self.events)
    }

    fn script_view(&self) -> ScriptRuntimeView<'_> {
        ScriptRuntimeView::new(&self.scripts)
    }

    fn movement_runtime(&mut self) -> MovementRuntimeAdapter<'_> {
        MovementRuntimeAdapter::new(&mut self.actors, &mut self.events, self.tube_pose_aliases.clone())
    }

    fn movement_view(&self) -> MovementRuntimeView<'_> {
        MovementRuntimeView::new(&self.actors)
    }

    pub(super) fn log_event(&mut self, event: impl Into<String>) {
        let mut message = event.into();
        if let Some(updated) = {
            let cache = self.tube_pose_aliases.borrow();
            cache
                .as_ref()
                .and_then(|map| normalize_tube_event(map, &message))
        } {
            message = updated;
        }
        let interest_alias = parse_actor_event(&message).and_then(|parts| {
            if parts.method == "complete_chore"
                && parts.actor_id != "mo.tube.interest_actor"
                && parts.actor_id.contains("tube")
                && parts.tokens.len() >= 2
            {
                let alias = parts.tokens[1];
                if !alias.chars().all(|c| c.is_ascii_digit()) {
                    Some(alias.to_string())
                } else {
                    None
                }
            } else {
                None
            }
        });
        self.events.push(message.clone());
        if is_intro_timeline_log(&message) {
            eprintln!("[grim_engine] {message}");
        }
        if let Some(alias) = interest_alias {
            self.events.push(format!(
                "actor.mo.tube.interest_actor.complete_chore {alias}"
            ));
        }
    }

    fn pause_view(&self) -> PauseRuntimeView<'_> {
        PauseRuntimeView::new(&self.pause)
    }

    pub(super) fn handle_pause_request(&mut self, label: PauseLabel, active: bool) {
        self.pause.record(label, active);
        let verb = if active { "on" } else { "off" };
        self.log_event(format!("game_pauser.{} {}", label.as_str(), verb));
    }

    fn push_cut_scene(&mut self, label: Option<String>, flags: Vec<String>) {
        let set_file = self
            .set_view()
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
        self.cutscene_runtime()
            .push_cut_scene(label, flags, set_file, sector, suppressed);
    }

    fn pop_cut_scene(&mut self) {
        self.cutscene_runtime().pop_cut_scene();
    }

    fn start_fullscreen_movie(&mut self, movie: String, yields: Option<u32>) -> bool {
        select_playback(&self.install_root, &movie, self.headless);
        self.cutscene_runtime()
            .start_fullscreen_movie(movie, yields)
    }

    pub(super) fn poll_fullscreen_movie(&mut self) -> bool {
        self.cutscene_runtime().poll_fullscreen_movie()
    }

    fn request_cutscene_skip(&mut self) {
        self.cutscene_runtime().request_cutscene_skip();
    }

    fn stop_fullscreen_movie(&mut self) {
        self.cutscene_runtime().stop_fullscreen_movie();
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
        let should_finish = {
            let view = self.cutscene_view();
            match (view.active_dialog(), expected_actor) {
                (None, _) => false,
                (Some(state), Some(expected)) => state.actor_id.eq_ignore_ascii_case(expected),
                (Some(_), None) => true,
            }
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
        self.cutscene_view().is_message_active()
    }

    pub(super) fn active_fullscreen_movie(&self) -> Option<String> {
        self.cutscene_view()
            .fullscreen_movie_name()
            .map(|value| value.to_string())
    }

    fn speaking_actor(&self) -> Option<String> {
        let view = self.cutscene_view();
        view.speaking_actor().map(|value| value.to_string())
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
        self.audio_runtime().set_sound_param(numeric, param, value);
    }

    fn get_sound_param(&self, numeric: i64, param: i32) -> Option<i32> {
        self.audio_view().get_sound_param(numeric, param)
    }

    fn ensure_menu_state(&mut self, name: &str) -> Rc<RefCell<MenuState>> {
        self.menus.ensure(name)
    }

    fn start_script(&mut self, label: String, callable: Option<RegistryKey>) -> u32 {
        self.script_runtime().start_script(label, callable)
    }

    fn has_script_with_label(&self, label: &str) -> bool {
        self.script_view().has_label(label)
    }

    fn attach_script_thread(&mut self, handle: u32, key: RegistryKey) {
        self.script_runtime().attach_thread(handle, key);
    }

    fn with_script_thread_key<R>(
        &self,
        handle: u32,
        f: impl FnOnce(Option<&RegistryKey>) -> R,
    ) -> R {
        let view = self.script_view();
        f(view.thread_key(handle))
    }

    fn increment_script_yield(&mut self, handle: u32) {
        self.script_runtime().increment_yield(handle);
    }

    fn script_yield_count(&self, handle: u32) -> Option<u32> {
        self.script_view().yield_count(handle)
    }

    fn script_label(&self, handle: u32) -> Option<String> {
        let view = self.script_view();
        view.label(handle)
    }

    fn active_script_handles(&self) -> Vec<u32> {
        self.script_view().active_handles()
    }

    fn is_script_running(&self, handle: u32) -> bool {
        self.script_view().is_running(handle)
    }

    fn complete_script(&mut self, handle: u32) -> ScriptCleanup {
        self.script_runtime().complete_script(handle)
    }

    fn ensure_actor_mut(&mut self, id: &str, label: &str) -> &mut ActorSnapshot {
        self.actors.ensure_actor_mut(id, label)
    }

    fn select_actor(&mut self, id: &str, label: &str) {
        self.actor_runtime().select_actor(id, label);
    }

    fn switch_to_set(&mut self, set_file: &str) {
        {
            self.set_runtime().switch_to_set(set_file);
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
        let mut runtime = self.set_runtime();
        runtime.mark_set_loaded(set_file);
    }

    fn set_sector_active(
        &mut self,
        set_file_hint: Option<&str>,
        sector_name: &str,
        active: bool,
    ) -> SectorToggleResult {
        let result = self
            .set_runtime()
            .set_sector_active(set_file_hint, sector_name, active);
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
        self.set_view().is_sector_active(set_file, sector_name)
    }

    fn record_current_setup(&mut self, set_file: &str, setup: i32) {
        self.set_runtime().record_current_setup(set_file, setup);
    }

    fn current_setup_for(&self, set_file: &str) -> Option<i32> {
        self.set_view().current_setup_for(set_file)
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
        self.movement_runtime()
            .set_actor_position(id, label, position);
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
        self.movement_runtime()
            .walk_actor_vector(handle, delta, adjust_y, heading_offset)
    }

    fn set_voice_effect(&mut self, effect: &str) {
        self.voice_effect = Some(effect.to_string());
        self.log_event(format!("prefs.voice_effect {}", effect));
    }

    fn add_inventory_item(&mut self, name: &str) {
        self.log_event(format!("inventory.add {name}"));
    }

    fn register_inventory_room(&mut self, name: &str) {
        self.log_event(format!("inventory.room {name}"));
    }

    fn record_sector_hit(&mut self, id: &str, label: &str, hit: SectorHit) {
        self.actor_runtime().record_sector_hit(id, label, hit);
    }

    fn default_sector_hit(&self, actor_id: &str, requested_kind: Option<&str>) -> SectorHit {
        self.movement_view()
            .default_sector_hit(actor_id, requested_kind)
    }

    fn evaluate_sector_name(&self, actor_id: &str, query: &str) -> bool {
        if actor_id.eq_ignore_ascii_case("manny") {
            matches!(query, "manny" | "office" | "desk")
        } else {
            false
        }
    }

    fn find_script_handle(&self, label: &str) -> Option<u32> {
        self.script_view().find_handle(label)
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

    fn register_object(&mut self, snapshot: ObjectSnapshot) {
        {
            self.object_runtime().register_object(snapshot);
        }
        self.refresh_commentary_visibility();
    }

    fn unregister_object(&mut self, handle: i64) {
        {
            self.object_runtime().unregister_object(handle);
        }
        self.refresh_commentary_visibility();
    }

    fn visible_object_handles(&self) -> Vec<i64> {
        let sets = self.set_view();
        let current = sets.current_set().map(|set| set.set_file.as_str());
        self.objects.visible_handles(current)
    }

    fn record_visible_objects(&mut self, handles: &[i64]) {
        {
            self.object_runtime().record_visible_objects(handles);
        }
        self.refresh_commentary_visibility();
    }

    fn set_object_touchable(&mut self, handle: i64, touchable: bool) {
        {
            self.object_runtime()
                .set_object_touchable(handle, touchable);
        }
        self.refresh_commentary_visibility();
    }

    fn set_object_visibility(&mut self, handle: i64, visible: bool) {
        {
            self.object_runtime().set_object_visibility(handle, visible);
        }
        self.refresh_commentary_visibility();
    }

    fn commentary_candidate_handle(&self) -> Option<i64> {
        self.objects.commentary_candidate_handle()
    }

    fn commentary_object_visible(&self, record: &CommentaryRecord) -> bool {
        self.movement_view().commentary_object_visible(record)
    }

    fn refresh_commentary_visibility(&mut self) {
        self.movement_runtime().refresh_commentary_visibility();
    }

    fn set_commentary_active(&mut self, enabled: bool, label: Option<String>) {
        if !enabled {
            self.cutscene_runtime().disable_commentary();
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

        {
            let mut runtime = self.cutscene_runtime();
            runtime.set_commentary(record);
        }
    }

    fn handle_sector_dependents(&mut self, set_file: &str, sector: &str, active: bool) {
        {
            let mut runtime = self.cutscene_runtime();
            runtime.handle_sector_activation(set_file, sector, active);
        }
        self.refresh_commentary_visibility();
    }

    pub(super) fn actor_position_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.movement_view().actor_position_by_handle(handle)
    }
    pub(super) fn actor_rotation_by_handle(&self, handle: u32) -> Option<Vec3> {
        self.actors.actor_rotation_by_handle(handle)
    }

    pub(super) fn actor_current_chore(&self, id: &str) -> Option<String> {
        self.actors.actor_current_chore(id).map(str::to_string)
    }

    fn actor_identity_by_handle(&self, handle: u32) -> Option<(String, String)> {
        self.actors.actor_identity_by_handle(handle)
    }

    fn set_actor_position_by_handle(&mut self, handle: u32, position: Vec3) -> bool {
        let Some((id, label)) = self.actor_identity_by_handle(handle) else {
            self.log_event(format!("actor.pos.unknown_handle #{handle}"));
            return false;
        };
        self.set_actor_position(&id, &label, position);
        true
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

    fn is_actor_moving(&self, handle: u32) -> bool {
        self.actors.is_actor_moving(handle)
    }

    fn walk_actor_to_handle(&mut self, handle: u32, target: Vec3) -> bool {
        self.movement_runtime().walk_actor_to_handle(handle, target)
    }

    fn geometry_sector_hit(&self, actor_id: &str, raw_kind: &str) -> Option<SectorHit> {
        self.movement_view().geometry_sector_hit(actor_id, raw_kind)
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
}

fn is_intro_timeline_log(message: &str) -> bool {
    message.contains(r#""label":"intro.timeline""#)
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
