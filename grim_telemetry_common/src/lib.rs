use libc::pid_t;
use std::{
    env,
    fmt::Display,
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_FULLSCREEN_DURATION_MS: u128 = 4_200;
pub const DEFAULT_POLL_STEP_MS: u128 = 80;

#[derive(Clone)]
pub struct TelemetryConfig {
    pub engine_id: &'static str,
    pub vm_id: &'static str,
    pub log_env_vars: &'static [&'static str],
    pub line_prefix: &'static str,
    pub run_id_env: Option<&'static str>,
}

pub struct TelemetryLogger {
    config: TelemetryConfig,
    sink: OnceLock<LogSink>,
    event_seq: AtomicU64,
    run_id: OnceLock<Option<String>>,
}

impl TelemetryLogger {
    pub const fn new(config: TelemetryConfig) -> Self {
        Self {
            config,
            sink: OnceLock::new(),
            event_seq: AtomicU64::new(0),
            run_id: OnceLock::new(),
        }
    }

    pub fn log_line(&self, message: &str) {
        let sink = self
            .sink
            .get_or_init(|| LogSink::init(self.config.log_env_vars));
        sink.write_line(self.config.line_prefix, message);
    }

    pub fn log_event(&self, event: EventBuilder) {
        let seq = self.event_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let ts = elapsed_millis();
        let run_id = self
            .run_id
            .get_or_init(|| self.config.run_id_env.and_then(|name| env::var(name).ok()))
            .as_ref()
            .map(|value| value.as_str());
        let fields = event.finish();
        let event_name = fields
            .iter()
            .find_map(|field| field.strip_prefix("event="))
            .map(|value| value.to_string());
        let mut parts = Vec::with_capacity(fields.len() + 6);
        parts.push(format!("seq={seq:06}"));
        parts.push(format!("ts={ts:08}"));
        if let Some(event_name) = event_name {
            parts.push(format!("event={event_name}"));
        }
        parts.extend(
            fields
                .into_iter()
                .filter(|field| !field.starts_with("event=")),
        );
        parts.push(format!("engine={}", self.config.engine_id));
        parts.push(format!("vm_id={}", self.config.vm_id));
        if let Some(run_id) = run_id {
            parts.push(format!("run_id={run_id}"));
        }
        self.log_line(&parts.join(" "));
    }
}

pub struct EventBuilder {
    fields: Vec<String>,
}

impl EventBuilder {
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            fields: vec![format!("event={}", event.into())],
        }
    }

    pub fn kv(mut self, key: &str, value: impl Display) -> Self {
        let mut value = value.to_string();
        let needs_quotes = value.contains(|c: char| c.is_whitespace());
        if needs_quotes {
            value = value.replace('"', "\\\"");
            self.fields.push(format!("{key}=\"{value}\""));
        } else {
            self.fields.push(format!("{key}={value}"));
        }
        self
    }

    pub fn finish(self) -> Vec<String> {
        self.fields
    }
}

enum LogTarget {
    Stderr(io::Stderr),
    File(BufWriter<std::fs::File>),
}

struct LogSink {
    target: Mutex<LogTarget>,
}

impl LogSink {
    fn init(env_vars: &[&str]) -> Self {
        for var in env_vars {
            if let Ok(path) = env::var(var) {
                match OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(file) => {
                        let writer = BufWriter::new(file);
                        return Self {
                            target: Mutex::new(LogTarget::File(writer)),
                        };
                    }
                    Err(err) => {
                        eprintln!(
                            "[grim-telemetry-common] failed to open {path} for logging: {err}; falling back to stderr"
                        );
                    }
                }
            }
        }

        Self {
            target: Mutex::new(LogTarget::Stderr(io::stderr())),
        }
    }

    fn write_line(&self, prefix: &str, message: &str) {
        let timestamp = format_timestamp();
        let pid = unsafe { libc::getpid() };
        let tid = current_tid();
        let mut guard = self
            .target
            .lock()
            .expect("log sink mutex should never be poisoned");
        let line = format!("[{prefix}] {message} | wall_ts={timestamp} pid={pid} tid={tid}\n");
        match &mut *guard {
            LogTarget::Stderr(stderr) => {
                let _ = stderr.write_all(line.as_bytes());
                let _ = stderr.flush();
            }
            LogTarget::File(file) => {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
    }
}

fn format_timestamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let secs = duration.as_secs();
            let millis = duration.subsec_millis();
            format!("{secs}.{millis:03}")
        }
        Err(_) => "unknown".to_string(),
    }
}

fn current_tid() -> pid_t {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::syscall(libc::SYS_gettid) as pid_t
    }

    #[cfg(not(target_os = "linux"))]
    unsafe {
        libc::pthread_self() as pid_t
    }
}

fn elapsed_millis() -> u128 {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_millis()
}

pub fn normalized_movie_label(movie: &str) -> Option<&'static str> {
    let normalized = movie.trim().trim_end_matches(".snm").to_ascii_lowercase();
    match normalized.as_str() {
        "intro" => Some("movie.intro"),
        "logos" => Some("movie.logos"),
        "mo_ts" => Some("movie.mo_ts"),
        _ => None,
    }
}

pub fn default_fullscreen_duration_ms(movie: &str) -> u128 {
    match normalized_movie_label(movie) {
        Some("movie.logos") => DEFAULT_FULLSCREEN_DURATION_MS,
        Some("movie.intro") => DEFAULT_FULLSCREEN_DURATION_MS,
        Some("movie.mo_ts") => DEFAULT_FULLSCREEN_DURATION_MS,
        _ => DEFAULT_FULLSCREEN_DURATION_MS,
    }
}
