use crate::{
    logging::log_line,
    lua_api::{
        call_real_lua_getcfunction, call_real_lua_getglobal, call_real_lua_getnumber,
        call_real_lua_getparam, call_real_lua_getstring, call_real_lua_isnil,
        call_real_lua_isnumber, call_real_lua_isstring, call_real_lua_push_c_closure,
        call_real_lua_setglobal, LuaCFunction,
    },
};
use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    sync::{Mutex, OnceLock},
};

const TELEMETRY_PATH: &str = "mods/telemetry_events.jsonl";

static TELEMETRY: OnceLock<Mutex<TelemetryHooks>> = OnceLock::new();

struct TelemetryHooks {
    start_fullscreen_original: Option<LuaCFunction>,
    fullscreen_poll_original: Option<LuaCFunction>,
    active_movie_label: Option<String>,
    writer: Option<TelemetryWriter>,
    next_seq: u64,
    installed: bool,
}

impl TelemetryHooks {
    fn shared() -> &'static Mutex<Self> {
        TELEMETRY.get_or_init(|| {
            Mutex::new(Self {
                start_fullscreen_original: None,
                fullscreen_poll_original: None,
                active_movie_label: None,
                writer: None,
                next_seq: 1,
                installed: false,
            })
        })
    }

    fn maybe_install_hooks(&mut self) {
        if self.installed {
            return;
        }
        if self.install_movie_wrappers() {
            self.installed = true;
        }
    }

    fn install_movie_wrappers(&mut self) -> bool {
        let Some((start_func, poll_func)) = resolve_movie_functions() else {
            return false;
        };
        self.start_fullscreen_original = Some(start_func);
        self.fullscreen_poll_original = Some(poll_func);
        if !replace_global("StartFullscreenMovie", start_fullscreen_movie_wrapper) {
            log_line("unable to replace StartFullscreenMovie with telemetry wrapper");
            return false;
        }
        if !replace_global(
            "IsFullscreenMoviePlaying",
            is_fullscreen_movie_playing_wrapper,
        ) {
            log_line("unable to replace IsFullscreenMoviePlaying with telemetry wrapper");
            return false;
        }
        true
    }

    fn record_fullscreen_start(&mut self, movie_name: &str) {
        let Some(label) = normalized_movie_label(movie_name) else {
            return;
        };
        if self
            .active_movie_label
            .as_deref()
            .is_some_and(|current| current == label)
        {
            // Duplicate start for the same movie; ignore.
            return;
        }
        self.active_movie_label = Some(label.to_string());
        self.emit_intro_timeline_event(&format!("{label}.start"));
    }

    fn record_fullscreen_poll(&mut self, playing: bool) {
        if playing {
            return;
        }
        if let Some(label) = self.active_movie_label.take() {
            self.emit_intro_timeline_event(&format!("{label}.end"));
        }
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

unsafe extern "C" fn is_fullscreen_movie_playing_wrapper() {
    let original = {
        let hooks = TelemetryHooks::shared()
            .lock()
            .expect("telemetry mutex poisoned");
        hooks.fullscreen_poll_original
    };
    if let Some(func) = original {
        func();
    } else {
        log_line("IsFullscreenMoviePlaying wrapper missing original target");
        return;
    }

    if let Some(playing) = read_first_result_truthy() {
        if let Ok(mut hooks) = TelemetryHooks::shared().lock() {
            hooks.record_fullscreen_poll(playing);
        }
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

fn resolve_movie_functions() -> Option<(LuaCFunction, LuaCFunction)> {
    let start = resolve_cfunction("StartFullscreenMovie")?;
    let poll = resolve_cfunction("IsFullscreenMoviePlaying")?;
    Some((start, poll))
}

fn resolve_cfunction(name: &str) -> Option<LuaCFunction> {
    let Ok(cname) = CString::new(name) else {
        log_line(&format!("invalid cstring for Lua lookup: {name}"));
        return None;
    };
    let handle = call_real_lua_getglobal(cname.as_ptr())?;
    call_real_lua_getcfunction(handle)
}

fn normalized_movie_label(movie: &str) -> Option<&'static str> {
    let normalized = movie.trim().trim_end_matches(".snm").to_ascii_lowercase();
    match normalized.as_str() {
        "intro" => Some("movie.intro"),
        "logos" => Some("movie.logos"),
        "mo_ts" => Some("movie.mo_ts"),
        _ => None,
    }
}

fn read_movie_name_arg() -> Option<String> {
    let first = call_real_lua_getparam(1)?;
    if !call_real_lua_isstring(first) {
        return None;
    }
    call_real_lua_getstring(first)
}

fn read_first_result_truthy() -> Option<bool> {
    let first = call_real_lua_getparam(1)?;
    if call_real_lua_isnil(first) {
        return Some(false);
    }
    if call_real_lua_isnumber(first) {
        if let Some(value) = call_real_lua_getnumber(first) {
            return Some(value != 0.0);
        }
    }
    Some(true)
}
