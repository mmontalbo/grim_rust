#[derive(Debug, Clone)]
pub(super) struct CutSceneRecord {
    pub(super) label: Option<String>,
    pub(super) flags: Vec<String>,
    pub(super) set_file: Option<String>,
    pub(super) sector: Option<String>,
    pub(super) suppressed: bool,
}

impl CutSceneRecord {
    pub(super) fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or("<unnamed>")
    }
}

#[derive(Debug, Clone)]
pub(super) struct CommentaryRecord {
    pub(super) label: Option<String>,
    pub(super) object_handle: Option<i64>,
    pub(super) active: bool,
    pub(super) suppressed_reason: Option<String>,
}

impl CommentaryRecord {
    pub(super) fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or("<none>")
    }
}

use crate::lua_host::telemetry::{log_event, EventBuilder};
use grim_telemetry_common::{
    default_fullscreen_duration_ms, normalized_movie_label, DEFAULT_POLL_STEP_MS,
};
use serde_json::json;
use std::fmt::Display;

#[derive(Debug, Clone)]
pub(super) struct FullscreenMovieState {
    name: String,
    label: Option<String>,
    polls: u64,
    elapsed_ms: u128,
    duration_ms: u128,
    skip_requested: bool,
}

#[derive(Debug, Clone, Copy)]
enum PlayingState {
    Known(bool),
    Unknown,
}

impl PlayingState {
    fn as_str(&self) -> &'static str {
        match self {
            PlayingState::Known(true) => "true",
            PlayingState::Known(false) => "false",
            PlayingState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CutsceneEndReason {
    Poll,
    StopMovie,
    Replaced,
}

impl CutsceneEndReason {
    fn as_str(&self) -> &'static str {
        match self {
            CutsceneEndReason::Poll => "poll",
            CutsceneEndReason::StopMovie => "stop_movie",
            CutsceneEndReason::Replaced => "replaced",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct DialogState {
    pub(super) actor_id: String,
    pub(super) actor_label: String,
    pub(super) line: String,
}

#[derive(Debug, Default, Clone)]
pub(super) struct CutsceneRuntime {
    cut_scene_stack: Vec<CutSceneRecord>,
    commentary: Option<CommentaryRecord>,
    active_dialog: Option<DialogState>,
    speaking_actor: Option<String>,
    message_active: bool,
    fullscreen_movie: Option<FullscreenMovieState>,
}

impl CutsceneRuntime {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn push_cut_scene(
        &mut self,
        label: Option<String>,
        flags: Vec<String>,
        set_file: Option<String>,
        sector: Option<String>,
        suppressed: bool,
    ) {
        self.cut_scene_stack.push(CutSceneRecord {
            label,
            flags,
            set_file,
            sector,
            suppressed,
        });
    }

    pub(super) fn pop_cut_scene(&mut self) -> bool {
        self.cut_scene_stack.pop().is_some()
    }

    pub(super) fn handle_sector_activation(&mut self, set_file: &str, sector: &str, active: bool) {
        for record in &mut self.cut_scene_stack {
            let matches_set = record
                .set_file
                .as_ref()
                .map(|file| file.eq_ignore_ascii_case(set_file))
                .unwrap_or(false);
            if !matches_set {
                continue;
            }
            if let Some(record_sector) = record.sector.as_ref() {
                if record_sector.eq_ignore_ascii_case(sector) {
                    if active && record.suppressed {
                        record.suppressed = false;
                    } else if !active && !record.suppressed {
                        record.suppressed = true;
                    }
                }
            }
        }
    }

    pub(super) fn set_commentary(&mut self, record: CommentaryRecord) -> Option<String> {
        let log_needed = match self.commentary.as_ref() {
            Some(existing) => {
                existing.label != record.label
                    || existing.object_handle != record.object_handle
                    || existing.active != record.active
                    || existing.suppressed_reason != record.suppressed_reason
            }
            None => true,
        };
        let display = record.display_label().to_string();
        let message = if record.active {
            format!("commentary.active {}", display)
        } else {
            format!("commentary.suppressed {}", display)
        };
        self.commentary = Some(record);
        log_needed.then_some(message)
    }

    pub(super) fn disable_commentary(&mut self) -> String {
        match self.commentary.take() {
            Some(record) => {
                let display = record.display_label().to_string();
                format!("commentary.active off ({display})")
            }
            None => "commentary.active off".to_string(),
        }
    }

    pub(super) fn update_commentary_visibility(
        &mut self,
        visible: bool,
        suppressed_reason: &str,
    ) -> Option<String> {
        let record = self.commentary.as_mut()?;
        match (record.active, visible) {
            (true, false) => {
                record.active = false;
                record.suppressed_reason = Some(suppressed_reason.to_string());
                let display = record.display_label().to_string();
                Some(format!("commentary.suspend {}", display))
            }
            (false, true) => {
                record.active = true;
                record.suppressed_reason = None;
                let display = record.display_label().to_string();
                Some(format!("commentary.resume {}", display))
            }
            _ => None,
        }
    }

    pub(super) fn commentary(&self) -> Option<&CommentaryRecord> {
        self.commentary.as_ref()
    }

    pub(super) fn cut_scene_stack(&self) -> &[CutSceneRecord] {
        &self.cut_scene_stack
    }

    pub(super) fn set_dialog_state(&mut self, state: DialogState) {
        self.speaking_actor = Some(state.actor_id.clone());
        self.message_active = true;
        self.active_dialog = Some(state);
    }

    pub(super) fn active_dialog(&self) -> Option<&DialogState> {
        self.active_dialog.as_ref()
    }

    pub(super) fn take_active_dialog(&mut self) -> Option<DialogState> {
        self.active_dialog.take()
    }

    pub(super) fn clear_dialog_flags(&mut self) {
        self.speaking_actor = None;
        self.message_active = false;
    }

    pub(super) fn is_message_active(&self) -> bool {
        self.message_active
    }

    pub(super) fn speaking_actor(&self) -> Option<&str> {
        self.speaking_actor.as_deref()
    }

    pub(super) fn start_fullscreen_movie(
        &mut self,
        events: &mut Vec<String>,
        movie: String,
        yields: Option<u32>,
    ) -> bool {
        if let Some(state) = self.fullscreen_movie.take() {
            self.finish_fullscreen_movie(
                events,
                state,
                PlayingState::Known(false),
                Some(CutsceneEndReason::Replaced),
            );
        }

        let duration_ms = simulated_duration_ms(&movie, yields);
        let label = normalized_movie_label(&movie).map(|value| value.to_string());
        if let Some(label) = label.as_deref() {
            events.push(intro_timeline_json(&format!("{label}.start")));
        }

        log_cutscene_event(
            events,
            CutsceneEventFields {
                phase: "start",
                movie: &movie,
                movie_label: label.as_deref(),
                playing: PlayingState::Known(true),
                elapsed_ms: Some(0),
                polls: Some(0),
                result: None,
            },
        );

        self.fullscreen_movie = Some(FullscreenMovieState {
            name: movie,
            label,
            polls: 0,
            elapsed_ms: 0,
            duration_ms,
            skip_requested: false,
        });
        true
    }

    pub(super) fn poll_fullscreen_movie(&mut self, events: &mut Vec<String>) -> bool {
        let Some(mut state) = self.fullscreen_movie.take() else {
            log_cutscene_event(
                events,
                CutsceneEventFields {
                    phase: "poll",
                    movie: "<none>",
                    movie_label: None,
                    playing: PlayingState::Unknown,
                    elapsed_ms: None,
                    polls: None,
                    result: None,
                },
            );
            return false;
        };

        state.polls = state.polls.saturating_add(1);
        state.elapsed_ms = state.elapsed_ms.saturating_add(DEFAULT_POLL_STEP_MS);
        let playing = if state.elapsed_ms < state.duration_ms {
            PlayingState::Known(true)
        } else {
            PlayingState::Known(false)
        };

        log_cutscene_event(
            events,
            CutsceneEventFields {
                phase: "poll",
                movie: &state.name,
                movie_label: state.label.as_deref(),
                playing,
                elapsed_ms: Some(state.elapsed_ms),
                polls: Some(state.polls),
                result: None,
            },
        );

        if matches!(playing, PlayingState::Known(true)) {
            self.fullscreen_movie = Some(state);
            true
        } else {
            let reason = if state.skip_requested {
                CutsceneEndReason::StopMovie
            } else {
                CutsceneEndReason::Poll
            };
            self.finish_fullscreen_movie(events, state, playing, Some(reason));
            false
        }
    }

    pub(super) fn request_cutscene_skip(&mut self, events: &mut Vec<String>) {
        let Some(state) = self.fullscreen_movie.as_mut() else {
            return;
        };
        if state.skip_requested {
            return;
        }
        state.skip_requested = true;
        log_cutscene_skip_event(
            events,
            "request",
            state.label.as_deref(),
            &state.name,
            Some(state.elapsed_ms),
            Some(state.polls),
        );
    }

    pub(super) fn stop_fullscreen_movie(&mut self, events: &mut Vec<String>) {
        let Some(state) = self.fullscreen_movie.take() else {
            return;
        };
        self.finish_fullscreen_movie(
            events,
            state,
            PlayingState::Known(false),
            Some(CutsceneEndReason::StopMovie),
        );
    }

    fn finish_fullscreen_movie(
        &mut self,
        events: &mut Vec<String>,
        state: FullscreenMovieState,
        playing: PlayingState,
        reason: Option<CutsceneEndReason>,
    ) {
        if state.skip_requested {
            log_cutscene_skip_event(
                events,
                "complete",
                state.label.as_deref(),
                &state.name,
                Some(state.elapsed_ms),
                Some(state.polls),
            );
        }
        if let Some(label) = state.label.as_deref() {
            events.push(intro_timeline_json(&format!("{label}.end")));
        }
        log_cutscene_event(
            events,
            CutsceneEventFields {
                phase: "end",
                movie: &state.name,
                movie_label: state.label.as_deref(),
                playing,
                elapsed_ms: Some(state.elapsed_ms),
                polls: Some(state.polls),
                result: reason.map(|value| value.as_str()),
            },
        );
    }
}

/// Couples cutscene runtime state with the engine event log.
pub(super) struct CutsceneRuntimeAdapter<'a> {
    runtime: &'a mut CutsceneRuntime,
    events: &'a mut Vec<String>,
}

/// Provides read-only accessors for cutscene state.
pub(super) struct CutsceneRuntimeView<'a> {
    runtime: &'a CutsceneRuntime,
}

impl<'a> CutsceneRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut CutsceneRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn push_cut_scene(
        &mut self,
        label: Option<String>,
        flags: Vec<String>,
        set_file: Option<String>,
        sector: Option<String>,
        suppressed: bool,
    ) {
        self.runtime
            .push_cut_scene(label, flags, set_file, sector, suppressed);
    }

    pub(super) fn pop_cut_scene(&mut self) {
        self.runtime.pop_cut_scene();
    }

    pub(super) fn handle_sector_activation(&mut self, set_file: &str, sector: &str, active: bool) {
        self.runtime
            .handle_sector_activation(set_file, sector, active);
    }

    pub(super) fn set_commentary(&mut self, record: CommentaryRecord) {
        if let Some(message) = self.runtime.set_commentary(record) {
            self.events.push(message);
        }
    }

    pub(super) fn disable_commentary(&mut self) {
        let message = self.runtime.disable_commentary();
        self.events.push(message);
    }

    pub(super) fn update_commentary_visibility(&mut self, visible: bool, suppressed_reason: &str) {
        if let Some(message) = self
            .runtime
            .update_commentary_visibility(visible, suppressed_reason)
        {
            self.events.push(message);
        }
    }

    pub(super) fn start_fullscreen_movie(&mut self, movie: String, yields: Option<u32>) -> bool {
        self.runtime
            .start_fullscreen_movie(self.events, movie, yields)
    }

    pub(super) fn poll_fullscreen_movie(&mut self) -> bool {
        self.runtime.poll_fullscreen_movie(self.events)
    }

    pub(super) fn request_cutscene_skip(&mut self) {
        self.runtime.request_cutscene_skip(self.events);
    }

    pub(super) fn stop_fullscreen_movie(&mut self) {
        self.runtime.stop_fullscreen_movie(self.events);
    }
}

impl<'a> CutsceneRuntimeView<'a> {
    pub(super) fn new(runtime: &'a CutsceneRuntime) -> Self {
        Self { runtime }
    }

    pub(super) fn active_dialog(&self) -> Option<&DialogState> {
        self.runtime.active_dialog()
    }

    pub(super) fn is_message_active(&self) -> bool {
        self.runtime.is_message_active()
    }

    pub(super) fn speaking_actor(&self) -> Option<&str> {
        self.runtime.speaking_actor()
    }

    pub(super) fn fullscreen_movie_name(&self) -> Option<&str> {
        self.runtime
            .fullscreen_movie
            .as_ref()
            .map(|state| state.name.as_str())
    }
}

struct CutsceneEventFields<'a> {
    phase: &'a str,
    movie: &'a str,
    movie_label: Option<&'a str>,
    playing: PlayingState,
    elapsed_ms: Option<u128>,
    polls: Option<u64>,
    result: Option<&'a str>,
}

struct EventLineBuilder {
    builder: EventBuilder,
    parts: Vec<String>,
}

impl EventLineBuilder {
    fn new(event: &str) -> Self {
        Self {
            builder: EventBuilder::new(event),
            parts: vec![format!("event={event}")],
        }
    }

    fn kv(mut self, key: &str, value: impl Display) -> Self {
        let value = value.to_string();
        self.builder = self.builder.kv(key, &value);
        self.parts.push(format!("{key}={value}"));
        self
    }

    fn kv_opt<T: Display>(mut self, key: &str, value: Option<T>) -> Self {
        if let Some(value) = value {
            let value = value.to_string();
            self.builder = self.builder.kv(key, &value);
            self.parts.push(format!("{key}={value}"));
        }
        self
    }

    fn finish(self) -> (EventBuilder, String) {
        (self.builder, self.parts.join(" "))
    }
}

fn log_cutscene_event(events: &mut Vec<String>, fields: CutsceneEventFields<'_>) {
    let (builder, line) = EventLineBuilder::new("cutscene")
        .kv("phase", fields.phase)
        .kv("movie", fields.movie)
        .kv("playing", fields.playing.as_str())
        .kv_opt("movie_label", fields.movie_label)
        .kv_opt("elapsed_ms", fields.elapsed_ms)
        .kv_opt("polls", fields.polls)
        .kv_opt("result", fields.result)
        .finish();
    log_event(builder);
    events.push(line);
}

fn log_cutscene_skip_event(
    events: &mut Vec<String>,
    phase: &str,
    movie_label: Option<&str>,
    movie: &str,
    elapsed_ms: Option<u128>,
    polls: Option<u64>,
) {
    let (builder, line) = EventLineBuilder::new("cutscene_skip")
        .kv("phase", phase)
        .kv("movie", movie)
        .kv_opt("movie_label", movie_label)
        .kv_opt("elapsed_ms", elapsed_ms)
        .kv_opt("polls", polls)
        .finish();
    log_event(builder);
    events.push(line);
}

fn simulated_duration_ms(movie: &str, override_polls: Option<u32>) -> u128 {
    if let Some(polls) = override_polls {
        return (polls.max(1) as u128) * DEFAULT_POLL_STEP_MS;
    }
    default_fullscreen_duration_ms(movie)
}

fn intro_timeline_json(event: &str) -> String {
    json!({
        "label": "intro.timeline",
        "data": {
            "event": event,
        }
    })
    .to_string()
}
