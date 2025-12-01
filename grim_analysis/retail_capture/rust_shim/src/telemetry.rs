use crate::{
    logging::{log_event, log_line, LuaEvent},
    lua_api::{
        call_real_lua_getcfunction, call_real_lua_getglobal, call_real_lua_getnumber,
        call_real_lua_getparam, call_real_lua_getstring, call_real_lua_isnumber,
        call_real_lua_isstring, call_real_lua_push_c_closure, call_real_lua_setglobal,
        LuaCFunction,
    },
};
use grim_telemetry_common::{
    normalized_movie_label, CutscenePhase, CutscenePlaying, CutsceneResult, CutsceneSkipPhase,
};
use std::{
    cell::Cell,
    collections::HashSet,
    ffi::CString,
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
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
    make_current_set_original: Option<LuaCFunction>,
    make_current_setup_original: Option<LuaCFunction>,
    active_movie_label: Option<String>,
    active_movie_name: Option<String>,
    last_finished_movie_label: Option<String>,
    start_instant: Option<Instant>,
    poll_count: u64,
    skip_requested: bool,
    post_intro_room_pending: bool,
    post_intro_room_reported: bool,
    post_intro_pending_set: Option<String>,
    post_intro_pending_setup: Option<String>,
    writer: Option<TelemetryWriter>,
    next_seq: u64,
    installed: bool,
    install_logged_failure: bool,
}

impl TelemetryHooks {
    fn shared() -> &'static Mutex<Self> {
        TELEMETRY.get_or_init(|| {
            Mutex::new(Self {
                start_fullscreen_original: None,
                start_movie_original: None,
                fullscreen_poll_original: None,
                movie_poll_original: None,
                stop_movie_original: None,
                make_current_set_original: None,
                make_current_setup_original: None,
                active_movie_label: None,
                active_movie_name: None,
                last_finished_movie_label: None,
                start_instant: None,
                poll_count: 0,
                skip_requested: false,
                post_intro_room_pending: false,
                post_intro_room_reported: false,
                post_intro_pending_set: None,
                post_intro_pending_setup: None,
                writer: None,
                next_seq: 1,
                installed: false,
                install_logged_failure: false,
            })
        })
    }

    fn maybe_install_hooks(&mut self) {
        if self.installed {
            return;
        }
        let installed_any = self.install_movie_wrappers() | self.install_room_wrappers();
        if installed_any {
            self.installed = true;
            self.install_logged_failure = false;
        } else if !self.install_logged_failure {
            log_line("failed to install movie telemetry wrappers; retry on next tick");
            self.install_logged_failure = true;
        }
    }

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

    fn install_room_wrappers(&mut self) -> bool {
        let make_set = resolve_cfunction("MakeCurrentSet");
        let make_setup = resolve_cfunction("MakeCurrentSetup");

        self.make_current_set_original = make_set;
        self.make_current_setup_original = make_setup;

        let set_wrapped = wrap_if_present(
            "MakeCurrentSet",
            self.make_current_set_original,
            make_current_set_wrapper,
        );
        let setup_wrapped = wrap_if_present(
            "MakeCurrentSetup",
            self.make_current_setup_original,
            make_current_setup_wrapper,
        );

        set_wrapped || setup_wrapped
    }

    fn record_fullscreen_start(&mut self, movie_name: &str) {
        if self.active_movie_name.is_some() || self.active_movie_label.is_some() {
            self.end_active_movie(PlayingState::Known(false), Some(EndReason::Replaced));
        }
        let label = normalized_movie_label(movie_name);
        self.active_movie_label = label.map(str::to_string);
        self.active_movie_name = Some(movie_name.to_string());
        self.last_finished_movie_label = None;
        self.start_instant = Some(Instant::now());
        self.poll_count = 0;
        self.skip_requested = false;
        if let Some(label) = label {
            self.emit_intro_timeline_event(&format!("{label}.start"));
        }
        self.emit_cutscene_event(
            CutscenePhase::Start,
            PlayingState::Known(true),
            CutsceneMeta {
                elapsed_ms: Some(0),
                poll_count: Some(0),
                result: None,
            },
        );
    }

    fn record_fullscreen_poll(&mut self, playing: PlayingState) {
        self.poll_count = self.poll_count.saturating_add(1);
        let elapsed = self.elapsed_ms();
        self.emit_cutscene_event(
            CutscenePhase::Poll,
            playing,
            CutsceneMeta {
                elapsed_ms: elapsed,
                poll_count: Some(self.poll_count),
                result: None,
            },
        );
        if let PlayingState::Known(false) = playing {
            let reason = if self.skip_requested {
                EndReason::StopCalled
            } else {
                EndReason::PollStopped
            };
            self.end_active_movie(playing, Some(reason));
        }
    }

    fn end_active_movie(&mut self, playing: PlayingState, reason: Option<EndReason>) {
        if self.skip_requested {
            self.record_cutscene_skip_complete();
        }
        if let Some(label) = self.active_movie_label.take() {
            self.emit_intro_timeline_event(&format!("{label}.end"));
            if label == "movie.intro" {
                self.post_intro_room_pending = true;
                self.post_intro_room_reported = false;
                self.post_intro_pending_set = None;
                self.post_intro_pending_setup = None;
            }
            self.last_finished_movie_label = Some(label);
        }
        let meta = CutsceneMeta {
            elapsed_ms: self.elapsed_ms(),
            poll_count: Some(self.poll_count),
            result: reason,
        };
        self.emit_cutscene_event(CutscenePhase::End, playing, meta);
        self.active_movie_name = None;
        self.start_instant = None;
        self.poll_count = 0;
        self.skip_requested = false;
    }

    fn emit_intro_timeline_event(&mut self, event: &str) {
        let seq = self.next_seq;
        let line = format!(
            r#"{{"seq":{},"label":"intro.timeline","data":{{"event":"{}"}}}}"#,
            seq, event
        );
        let result = {
            let Some(writer) = self.ensure_writer() else {
                return;
            };
            writer.write_line(&line)
        };
        if let Err(err) = result {
            log_line(&format!(
                "failed to write intro.timeline event to {TELEMETRY_PATH}: {err}"
            ));
            self.writer = None;
            return;
        }
        self.next_seq += 1;
    }

    fn ensure_writer(&mut self) -> Option<&mut TelemetryWriter> {
        if self.writer.is_none() {
            if let Err(err) = fs::create_dir_all("mods") {
                log_line(&format!(
                    "failed to create mods directory for telemetry: {err}"
                ));
                return None;
            }
            match TelemetryWriter::open(TELEMETRY_PATH) {
                Ok(writer) => self.writer = Some(writer),
                Err(err) => {
                    log_line(&format!(
                        "failed to open {TELEMETRY_PATH} for telemetry: {err}"
                    ));
                    return None;
                }
            }
        }
        self.writer.as_mut()
    }

    fn emit_cutscene_event(&self, phase: CutscenePhase, playing: PlayingState, meta: CutsceneMeta) {
        let movie = self
            .active_movie_name
            .as_deref()
            .or(self.active_movie_label.as_deref())
            .unwrap_or("<unknown>")
            .to_string();
        let movie_label = self.active_movie_label.clone();
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
        log_event(LuaEvent::Cutscene {
            movie,
            movie_label,
            phase,
            playing,
            elapsed_ms: meta.elapsed_ms,
            polls: meta.poll_count,
            result,
        });
    }

    fn record_cutscene_skip_request(&mut self) {
        self.skip_requested = true;
        log_event(LuaEvent::CutsceneSkip {
            phase: CutsceneSkipPhase::Request,
            movie: self
                .active_movie_name
                .as_deref()
                .or(self.active_movie_label.as_deref())
                .map(str::to_string),
            movie_label: self.active_movie_label.clone(),
            elapsed_ms: self.elapsed_ms(),
            polls: Some(self.poll_count),
        });
    }

    fn record_cutscene_skip_complete(&mut self) {
        log_event(LuaEvent::CutsceneSkip {
            phase: CutsceneSkipPhase::Complete,
            movie: self
                .active_movie_name
                .as_deref()
                .or(self.last_finished_movie_label.as_deref())
                .map(str::to_string),
            movie_label: self
                .active_movie_label
                .as_deref()
                .or(self.last_finished_movie_label.as_deref())
                .map(str::to_string),
            elapsed_ms: self.elapsed_ms(),
            polls: Some(self.poll_count),
        });
    }

    fn elapsed_ms(&self) -> Option<u128> {
        self.start_instant.map(|start| start.elapsed().as_millis())
    }

    fn record_post_intro_room_event(
        &mut self,
        source: &str,
        set: Option<String>,
        setup: Option<String>,
    ) {
        if !self.post_intro_room_pending || self.post_intro_room_reported {
            return;
        }

        if let Some(value) = set {
            self.post_intro_pending_set.get_or_insert(value);
        }
        if let Some(value) = setup {
            self.post_intro_pending_setup.get_or_insert(value);
        }

        log_event(LuaEvent::PostIntroRoom {
            source: source.to_string(),
            set: self.post_intro_pending_set.as_deref().map(str::to_string),
            setup: self.post_intro_pending_setup.as_deref().map(str::to_string),
            after_movie: self.last_finished_movie_label.clone(),
        });
        self.post_intro_room_reported = true;
        self.post_intro_room_pending = false;
    }
}

#[derive(Default)]
struct CutsceneMeta {
    elapsed_ms: Option<u128>,
    poll_count: Option<u64>,
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

struct TelemetryWriter {
    inner: BufWriter<std::fs::File>,
}

impl TelemetryWriter {
    fn open(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            inner: BufWriter::new(file),
        })
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.inner.write_all(line.as_bytes())?;
        self.inner.write_all(b"\n")?;
        self.inner.flush()
    }
}

pub(crate) fn observe_lua_activity() {
    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.maybe_install_hooks();
    }
}

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

unsafe extern "C" fn make_current_set_wrapper() {
    let set = read_string_arg(1);
    let original = {
        let hooks = TelemetryHooks::shared()
            .lock()
            .expect("telemetry mutex poisoned");
        hooks.make_current_set_original
    };
    if let Some(func) = original {
        func();
    } else {
        log_line("MakeCurrentSet wrapper missing original target");
        return;
    }

    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.record_post_intro_room_event("MakeCurrentSet", set, None);
    }
}

unsafe extern "C" fn make_current_setup_wrapper() {
    let setup = read_string_arg(1).or_else(|| read_number_arg(1).map(format_number_arg));
    let original = {
        let hooks = TelemetryHooks::shared()
            .lock()
            .expect("telemetry mutex poisoned");
        hooks.make_current_setup_original
    };
    if let Some(func) = original {
        func();
    } else {
        log_line("MakeCurrentSetup wrapper missing original target");
        return;
    }

    if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
        hooks.record_post_intro_room_event("MakeCurrentSetup", None, setup);
    }
}

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

fn read_movie_name_arg() -> Option<String> {
    read_string_arg(1)
}

fn read_string_arg(index: i32) -> Option<String> {
    let first = call_real_lua_getparam(index)?;
    if !call_real_lua_isstring(first) {
        return None;
    }
    call_real_lua_getstring(first)
}

fn read_number_arg(index: i32) -> Option<f64> {
    let first = call_real_lua_getparam(index)?;
    if !call_real_lua_isnumber(first) {
        return None;
    }
    call_real_lua_getnumber(first)
}

fn format_number_arg(value: f64) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn capture_playing_from_poll<F: FnOnce() -> Option<()>>(call_original: F) -> PlayingState {
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

pub(crate) fn record_pushed_number(value: f64) {
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
