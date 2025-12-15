//! Cutscene and room telemetry wrappers for the retail Lua runtime.
//!
//! This module replaces a small set of engine-exposed Lua C functions to observe
//! movie playback, skip requests, and post-intro room transitions. State flows
//! through a cutscene state machine and is fanned out to sinks so parity tweaks
//! stay isolated from hook wiring and file IO.
use crate::{
    logging::{log_boot_sequence_complete, log_event, log_event_to_writer, log_line, LuaEvent},
    lua_api::{
        call_real_lua_getcfunction, call_real_lua_getglobal, call_real_lua_getparam,
        call_real_lua_getstring, call_real_lua_isstring, call_real_lua_push_c_closure,
        call_real_lua_setglobal, LuaCFunction,
    },
};
use grim_telemetry_schema::{
    normalized_movie_label, CutscenePhase, CutscenePlaying, CutsceneResult, CutsceneSkipPhase,
    IntroTimelineData, JsonlWriter, TimelineEvent, INTRO_TIMELINE_LABEL,
};
use std::{
    cell::Cell,
    collections::HashSet,
    ffi::CString,
    sync::{Mutex, OnceLock},
    time::Instant,
};

const TELEMETRY_PATH: &str = "mods/telemetry_events.jsonl";

static TELEMETRY: OnceLock<Mutex<TelemetryHooks>> = OnceLock::new();

struct TelemetryHooks {
    start_fullscreen_original: Option<LuaCFunction>,
    start_movie_original: Option<LuaCFunction>,
    fullscreen_poll_original: Option<LuaCFunction>,
    movie_poll_original: Option<LuaCFunction>,
    stop_movie_original: Option<LuaCFunction>,
    installed: bool,
    install_logged_failure: bool,
    state: CutsceneStateMachine,
    sinks: TelemetrySinks,
}

impl TelemetryHooks {
    /// Returns the global telemetry hook state, initializing it on first access.
    fn shared() -> &'static Mutex<Self> {
        TELEMETRY.get_or_init(|| {
            Mutex::new(Self {
                start_fullscreen_original: None,
                start_movie_original: None,
                fullscreen_poll_original: None,
                movie_poll_original: None,
                stop_movie_original: None,
                installed: false,
                install_logged_failure: false,
                state: CutsceneStateMachine::default(),
                sinks: TelemetrySinks::default(),
            })
        })
    }

    /// Installs Lua wrappers once; defers to later ticks on failure.
    fn maybe_install_hooks(&mut self) {
        if self.installed {
            return;
        }
        let installed_any = self.install_movie_wrappers();
        if installed_any {
            self.installed = true;
            self.install_logged_failure = false;
        } else if !self.install_logged_failure {
            log_line("failed to install movie telemetry wrappers; retry on next tick");
            self.install_logged_failure = true;
        }
    }

    /// Hooks engine movie functions, storing originals and replacing globals.
    fn install_movie_wrappers(&mut self) -> bool {
        let start_fullscreen = resolve_cfunction("StartFullscreenMovie");
        let start_movie = resolve_cfunction("StartMovie");
        let fullscreen_poll = resolve_cfunction("IsFullscreenMoviePlaying");
        let movie_poll = resolve_cfunction("IsMoviePlaying");
        let stop_movie = resolve_cfunction("StopMovie");

        self.start_fullscreen_original = start_fullscreen;
        self.start_movie_original = start_movie;
        self.fullscreen_poll_original = fullscreen_poll;
        self.movie_poll_original = movie_poll;
        self.stop_movie_original = stop_movie;

        let start_fullscreen_wrapped = wrap_if_present(
            "StartFullscreenMovie",
            self.start_fullscreen_original,
            start_fullscreen_movie_wrapper,
        );
        let start_movie_wrapped =
            wrap_if_present("StartMovie", self.start_movie_original, start_movie_wrapper);
        let fullscreen_poll_wrapped = wrap_if_present(
            "IsFullscreenMoviePlaying",
            self.fullscreen_poll_original,
            is_fullscreen_movie_playing_wrapper,
        );
        let movie_poll_wrapped = wrap_if_present(
            "IsMoviePlaying",
            self.movie_poll_original,
            is_movie_playing_wrapper,
        );
        let stop_movie_wrapped =
            wrap_if_present("StopMovie", self.stop_movie_original, stop_movie_wrapper);

        start_fullscreen_wrapped
            || start_movie_wrapped
            || fullscreen_poll_wrapped
            || movie_poll_wrapped
            || stop_movie_wrapped
    }

    fn record_fullscreen_start(&mut self, movie_name: &str) {
        let events = self.state.record_fullscreen_start(movie_name);
        self.sinks.emit(events);
    }

    fn record_fullscreen_poll(&mut self, playing: PlayingState) {
        let events = self.state.record_fullscreen_poll(playing);
        self.sinks.emit(events);
    }

    fn end_active_movie(&mut self, playing: PlayingState, reason: Option<EndReason>) {
        let events = self.state.end_active_movie(playing, reason);
        self.sinks.emit(events);
    }

    fn record_cutscene_skip_request(&mut self) {
        let events = self.state.record_cutscene_skip_request();
        self.sinks.emit(events);
    }
}

enum TelemetryEvent {
    Structured(LuaEvent),
    IntroTimeline(TimelineEvent),
}

#[derive(Default)]
struct TelemetrySinks {
    writer: Option<JsonlWriter>,
}

impl TelemetrySinks {
    fn emit(&mut self, events: Vec<TelemetryEvent>) {
        for event in events {
            match event {
                TelemetryEvent::Structured(event) => log_event(event),
                TelemetryEvent::IntroTimeline(event) => {
                    if let Err(err) = self.emit_intro_timeline(&event) {
                        log_line(&format!(
                            "failed to write intro.timeline event to {TELEMETRY_PATH}: {err}"
                        ));
                        self.writer = None;
                    }
                }
            }
        }
    }

    fn emit_intro_timeline(&mut self, event: &TimelineEvent) -> std::io::Result<()> {
        let writer = self.ensure_writer()?;
        log_event_to_writer(event.clone(), writer).map(|_| ())
    }

    fn ensure_writer(&mut self) -> std::io::Result<&mut JsonlWriter> {
        if self.writer.is_none() {
            self.writer = Some(JsonlWriter::open(TELEMETRY_PATH)?);
        }
        Ok(self.writer.as_mut().expect("writer missing after init"))
    }
}

/// Attempts to install telemetry hooks; called from traced Lua entry points.
pub(crate) fn observe_lua_activity() {
    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.maybe_install_hooks();
    }
}

/// Wraps `StartFullscreenMovie`, calling the original and recording start metadata.
unsafe extern "C" fn start_fullscreen_movie_wrapper() {
    let movie = read_movie_name_arg();
    let original = {
        let hooks = TelemetryHooks::shared()
            .lock()
            .expect("telemetry mutex poisoned");
        hooks.start_fullscreen_original
    };
    if let Some(func) = original {
        func();
    } else {
        log_line("StartFullscreenMovie wrapper missing original target");
    }
    if let Some(movie) = movie {
        if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
            hooks.record_fullscreen_start(&movie);
        }
    }
}

/// Wraps `StartMovie`, calling the original and recording start metadata.
unsafe extern "C" fn start_movie_wrapper() {
    let movie = read_movie_name_arg();
    let original = {
        let hooks = TelemetryHooks::shared()
            .lock()
            .expect("telemetry mutex poisoned");
        hooks.start_movie_original
    };
    if let Some(func) = original {
        func();
    } else {
        log_line("StartMovie wrapper missing original target");
    }
    if let Some(movie) = movie {
        if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
            hooks.record_fullscreen_start(&movie);
        }
    }
}

/// Wraps `IsFullscreenMoviePlaying`, capturing the playing flag pushed by the VM.
unsafe extern "C" fn is_fullscreen_movie_playing_wrapper() {
    let playing = capture_playing_from_poll(|| {
        let original = {
            let hooks = TelemetryHooks::shared()
                .lock()
                .expect("telemetry mutex poisoned");
            hooks.fullscreen_poll_original
        };
        if let Some(func) = original {
            func();
            Some(())
        } else {
            log_line("IsFullscreenMoviePlaying wrapper missing original target");
            None
        }
    });
    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.record_fullscreen_poll(playing);
    }
}

/// Wraps `IsMoviePlaying`, capturing the playing flag pushed by the VM.
unsafe extern "C" fn is_movie_playing_wrapper() {
    let playing = capture_playing_from_poll(|| {
        let original = {
            let hooks = TelemetryHooks::shared()
                .lock()
                .expect("telemetry mutex poisoned");
            hooks.movie_poll_original
        };
        if let Some(func) = original {
            func();
            Some(())
        } else {
            log_line("IsMoviePlaying wrapper missing original target");
            None
        }
    });
    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.record_fullscreen_poll(playing);
    }
}

/// Wraps `StopMovie`, recording skip requests and end-of-playback events.
unsafe extern "C" fn stop_movie_wrapper() {
    let original = {
        let hooks = TelemetryHooks::shared()
            .lock()
            .expect("telemetry mutex poisoned");
        hooks.stop_movie_original
    };
    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.record_cutscene_skip_request();
    }
    if let Some(func) = original {
        func();
    } else {
        log_line("StopMovie wrapper missing original target");
        return;
    }

    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.end_active_movie(PlayingState::Known(false), Some(EndReason::StopCalled));
    }
}

/// Replaces a Lua global with a wrapper closure, returning success.
fn replace_global(name: &str, wrapper: LuaCFunction) -> bool {
    let Ok(cstr) = CString::new(name) else {
        log_line(&format!("invalid global name for telemetry hook: {name}"));
        return false;
    };
    if !call_real_lua_push_c_closure(wrapper, 0) {
        return false;
    }
    call_real_lua_setglobal(cstr.as_ptr())
}

/// Resolves a Lua C function by global name, logging if it is absent.
fn resolve_cfunction(name: &str) -> Option<LuaCFunction> {
    let Ok(cname) = CString::new(name) else {
        log_line(&format!("invalid cstring for Lua lookup: {name}"));
        return None;
    };
    let handle = call_real_lua_getglobal(cname.as_ptr())?;
    let func = call_real_lua_getcfunction(handle);
    if func.is_none() {
        log_missing_cfunction(name);
    }
    func
}

/// Logs a missing C function once to avoid flooding output.
fn log_missing_cfunction(name: &str) {
    static MISSING_REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let set = MISSING_REPORTED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut already_reported = false;
    if let Ok(mut seen) = set.lock() {
        if !seen.insert(name.to_string()) {
            already_reported = true;
        }
    }
    if !already_reported {
        log_line(&format!("required Lua C function missing: {name}"));
    }
}

/// Installs `wrapper` only if the original target function exists.
fn wrap_if_present(name: &str, target: Option<LuaCFunction>, wrapper: LuaCFunction) -> bool {
    let Some(_) = target else {
        return false;
    };
    if !replace_global(name, wrapper) {
        log_line(&format!(
            "unable to replace {name} with telemetry wrapper; leaving target uninstrumented"
        ));
        return false;
    }
    true
}

/// Reads the first Lua argument as a movie name string.
fn read_movie_name_arg() -> Option<String> {
    read_string_arg(1)
}

/// Reads the argument at `index` as a string if it is present and typed as string.
fn read_string_arg(index: i32) -> Option<String> {
    let first = call_real_lua_getparam(index)?;
    if !call_real_lua_isstring(first) {
        return None;
    }
    call_real_lua_getstring(first)
}

/// Captures a boolean-ish result pushed by poll wrappers while restoring prior state.
fn capture_playing_from_poll<F: FnOnce() -> Option<()>>(call_original: F) -> PlayingState {
    // Poll wrappers mark capture active, let the original call run (which may push
    // a boolean-ish value), then latch the pushed number/nil into POLL_CAPTURE_RESULT.
    POLL_CAPTURE_ACTIVE.with(|active| {
        POLL_CAPTURE_RESULT.with(|result| {
            let already_active = active.replace(true);
            let prior_result = result.replace(None);
            let playing = match call_original() {
                Some(()) => result.get().unwrap_or(PlayingState::Unknown),
                None => PlayingState::Unknown,
            };
            active.set(already_active);
            result.set(prior_result);
            playing
        })
    })
}

/// Records a pushed number as a playback flag when a poll hook is active.
pub(crate) fn record_pushed_number(value: f64) {
    // record_pushed_* are invoked from push hooks; when a poll is active they capture
    // the effective playing flag for the surrounding call.
    POLL_CAPTURE_ACTIVE.with(|active| {
        if !active.get() {
            return;
        }
        POLL_CAPTURE_RESULT.with(|result| {
            if result.get().is_none() {
                result.set(Some(PlayingState::Known(value != 0.0)));
            }
        })
    })
}

/// Records a pushed nil as a playback flag when a poll hook is active.
pub(crate) fn record_pushed_nil() {
    POLL_CAPTURE_ACTIVE.with(|active| {
        if !active.get() {
            return;
        }
        POLL_CAPTURE_RESULT.with(|result| {
            if result.get().is_none() {
                result.set(Some(PlayingState::Known(false)));
            }
        })
    })
}

thread_local! {
    static POLL_CAPTURE_ACTIVE: Cell<bool> = Cell::new(false);
    static POLL_CAPTURE_RESULT: Cell<Option<PlayingState>> = Cell::new(None);
}

struct CutsceneStateMachine {
    active_movie_label: Option<String>,
    active_movie_name: Option<String>,
    last_finished_movie_label: Option<String>,
    start_instant: Option<Instant>,
    skip_requested: bool,
    next_seq: u64,
}

impl Default for CutsceneStateMachine {
    fn default() -> Self {
        Self {
            active_movie_label: None,
            active_movie_name: None,
            last_finished_movie_label: None,
            start_instant: None,
            skip_requested: false,
            next_seq: 1,
        }
    }
}

impl CutsceneStateMachine {
    fn record_fullscreen_start(&mut self, movie_name: &str) -> Vec<TelemetryEvent> {
        let mut events = Vec::new();
        if self.active_movie_name.is_some() || self.active_movie_label.is_some() {
            events.extend(
                self.end_active_movie(PlayingState::Known(false), Some(EndReason::Replaced)),
            );
        }

        let label = normalized_movie_label(movie_name);
        self.active_movie_label = label.map(str::to_string);
        self.active_movie_name = Some(movie_name.to_string());
        self.last_finished_movie_label = None;
        self.start_instant = Some(Instant::now());
        self.skip_requested = false;
        if let Some(label) = label {
            events.push(self.intro_timeline_event(format!("{label}.start")));
        }
        log_boot_sequence_complete(None);
        events.push(self.cutscene_event(
            CutscenePhase::Start,
            PlayingState::Known(true),
            CutsceneMeta {
                elapsed_ms: Some(0),
                result: None,
            },
        ));
        events
    }

    fn record_fullscreen_poll(&mut self, playing: PlayingState) -> Vec<TelemetryEvent> {
        if self.active_movie_label.is_none() && self.active_movie_name.is_none() {
            return Vec::new();
        }
        let mut events = Vec::new();
        if let PlayingState::Known(false) = playing {
            let reason = if self.skip_requested {
                EndReason::StopCalled
            } else {
                EndReason::PollStopped
            };
            events.extend(self.end_active_movie(playing, Some(reason)));
        }
        events
    }

    fn record_cutscene_skip_request(&mut self) -> Vec<TelemetryEvent> {
        if self.active_movie_label.is_none() && self.active_movie_name.is_none() {
            return Vec::new();
        }
        self.skip_requested = true;
        vec![self.cutscene_skip_event(
            CutsceneSkipPhase::Request,
            self.active_movie_name
                .as_deref()
                .or(self.active_movie_label.as_deref()),
            self.active_movie_label.as_deref(),
            self.elapsed_ms(),
        )]
    }

    fn end_active_movie(
        &mut self,
        playing: PlayingState,
        reason: Option<EndReason>,
    ) -> Vec<TelemetryEvent> {
        if self.active_movie_label.is_none() && self.active_movie_name.is_none() {
            return Vec::new();
        }
        let mut events = Vec::new();
        let active_label = self.active_movie_label.clone();
        let active_name = self.active_movie_name.clone();

        if self.skip_requested {
            let movie = active_name
                .as_deref()
                .or(self.last_finished_movie_label.as_deref());
            let label = active_label
                .as_deref()
                .or(self.last_finished_movie_label.as_deref());
            events.push(self.cutscene_skip_event(
                CutsceneSkipPhase::Complete,
                movie,
                label,
                self.elapsed_ms(),
            ));
        }

        if let Some(label) = active_label.as_deref() {
            events.push(self.intro_timeline_event(format!("{label}.end")));
        }

        let meta = CutsceneMeta {
            elapsed_ms: self.elapsed_ms(),
            result: reason,
        };
        events.push(self.cutscene_event(CutscenePhase::End, playing, meta));

        if let Some(label) = active_label {
            self.last_finished_movie_label = Some(label);
        }
        self.active_movie_name = None;
        self.active_movie_label = None;
        self.start_instant = None;
        self.skip_requested = false;

        events
    }

    fn elapsed_ms(&self) -> Option<u128> {
        self.start_instant.map(|start| start.elapsed().as_millis())
    }

    fn intro_timeline_event(&mut self, event: String) -> TelemetryEvent {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        TelemetryEvent::IntroTimeline(TimelineEvent::IntroTimeline {
            label: INTRO_TIMELINE_LABEL.to_string(),
            data: IntroTimelineData { event },
            seq: Some(seq),
        })
    }

    fn cutscene_event(
        &self,
        phase: CutscenePhase,
        playing: PlayingState,
        meta: CutsceneMeta,
    ) -> TelemetryEvent {
        let movie_label = self.active_movie_label.clone();
        let movie = self
            .active_movie_name
            .as_deref()
            .or(movie_label.as_deref())
            .unwrap_or("<unknown>")
            .to_string();
        let playing = match playing {
            PlayingState::Known(true) => CutscenePlaying::Playing,
            PlayingState::Known(false) => CutscenePlaying::Stopped,
            PlayingState::Unknown => CutscenePlaying::Unknown,
        };
        let result = meta.result.map(|reason| match reason {
            EndReason::PollStopped => CutsceneResult::PollStopped,
            EndReason::StopCalled => CutsceneResult::StopCalled,
            EndReason::Replaced => CutsceneResult::Replaced,
        });
        TelemetryEvent::Structured(LuaEvent::Cutscene {
            movie,
            movie_label,
            phase,
            playing,
            elapsed_ms: meta.elapsed_ms,
            result,
        })
    }

    fn cutscene_skip_event(
        &self,
        phase: CutsceneSkipPhase,
        movie: Option<&str>,
        movie_label: Option<&str>,
        elapsed_ms: Option<u128>,
    ) -> TelemetryEvent {
        TelemetryEvent::Structured(LuaEvent::CutsceneSkip {
            phase,
            movie: movie.map(str::to_string),
            movie_label: movie_label.map(str::to_string),
            elapsed_ms,
        })
    }
}

#[derive(Default)]
struct CutsceneMeta {
    elapsed_ms: Option<u128>,
    result: Option<EndReason>,
}

#[derive(Clone, Copy)]
enum EndReason {
    PollStopped,
    StopCalled,
    Replaced,
}

impl std::fmt::Display for EndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            EndReason::PollStopped => "poll",
            EndReason::StopCalled => "stop_movie",
            EndReason::Replaced => "replaced",
        };
        write!(f, "{label}")
    }
}

#[derive(Clone, Copy)]
enum PlayingState {
    Known(bool),
    Unknown,
}

impl std::fmt::Display for PlayingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayingState::Known(value) => write!(f, "{value}"),
            PlayingState::Unknown => write!(f, "unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intro_start_events(state: &mut CutsceneStateMachine) -> Vec<TelemetryEvent> {
        state.record_fullscreen_start("intro.snm")
    }

    #[test]
    fn intro_start_and_end_emit_timeline_and_cutscene() {
        let mut state = CutsceneStateMachine::default();
        let start_events = intro_start_events(&mut state);
        assert_eq!(start_events.len(), 2);
        assert!(matches!(
            start_events[0],
            TelemetryEvent::IntroTimeline(TimelineEvent::IntroTimeline {
                seq: Some(1),
                ref data,
                ..
            }) if data.event == "movie.intro.start"
        ));
        assert!(matches!(
            start_events[1],
            TelemetryEvent::Structured(LuaEvent::Cutscene {
                phase: CutscenePhase::Start,
                ..
            })
        ));

        let end_events = state.record_fullscreen_poll(PlayingState::Known(false));
        assert!(end_events.iter().any(|event| matches!(
            event,
            TelemetryEvent::IntroTimeline(TimelineEvent::IntroTimeline { ref data, .. })
                if data.event == "movie.intro.end"
        )));
        assert!(end_events.iter().any(|event| matches!(
            event,
            TelemetryEvent::Structured(LuaEvent::Cutscene {
                phase: CutscenePhase::End,
                ..
            })
        )));
    }

    #[test]
    fn skip_request_and_complete_are_emitted() {
        let mut state = CutsceneStateMachine::default();
        intro_start_events(&mut state);
        let skip_request = state.record_cutscene_skip_request();
        assert!(matches!(
            skip_request.as_slice(),
            [TelemetryEvent::Structured(LuaEvent::CutsceneSkip {
                phase: CutsceneSkipPhase::Request,
                ..
            })]
        ));

        let end_events =
            state.end_active_movie(PlayingState::Known(false), Some(EndReason::StopCalled));
        assert!(end_events.iter().any(|event| matches!(
            event,
            TelemetryEvent::Structured(LuaEvent::CutsceneSkip {
                phase: CutsceneSkipPhase::Complete,
                ..
            })
        )));
    }
}
