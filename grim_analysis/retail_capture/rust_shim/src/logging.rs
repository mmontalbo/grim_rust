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

enum LogTarget {
    Stderr(io::Stderr),
    File(BufWriter<std::fs::File>),
}

struct LogSink {
    target: Mutex<LogTarget>,
}

static LOG_SINK: OnceLock<LogSink> = OnceLock::new();
static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) const ENGINE_ID: &str = "retail";
pub(crate) const VM_ID: &str = "lua32";

pub(crate) fn log_line(message: &str) {
    let sink = LOG_SINK.get_or_init(LogSink::init);
    sink.write_line(message);
}

pub(crate) fn log_event(event: EventBuilder) {
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let ts = elapsed_millis();
    let fields = event.finish();
    let mut parts = Vec::with_capacity(fields.len() + 4);
    parts.push(format!("engine={ENGINE_ID}"));
    parts.push(format!("vm_id={VM_ID}"));
    parts.push(format!("seq={seq}"));
    parts.push(format!("ts={ts}"));
    parts.extend(fields);
    log_line(&parts.join(" "));
}

pub(crate) struct EventBuilder {
    fields: Vec<String>,
}

impl EventBuilder {
    pub(crate) fn new(event: impl Into<String>) -> Self {
        Self {
            fields: vec![format!("event={}", event.into())],
        }
    }

    pub(crate) fn kv(mut self, key: &str, value: impl Display) -> Self {
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

    fn finish(self) -> Vec<String> {
        self.fields
    }
}

impl LogSink {
    fn init() -> Self {
        if let Ok(path) = env::var("GRIM_SHIM_LOG") {
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => {
                    let writer = BufWriter::new(file);
                    return Self {
                        target: Mutex::new(LogTarget::File(writer)),
                    };
                }
                Err(err) => {
                    eprintln!(
                        "[grim-rust-shim] failed to open {path} for logging: {err}; falling back to stderr"
                    );
                }
            }
        }

        Self {
            target: Mutex::new(LogTarget::Stderr(io::stderr())),
        }
    }

    fn write_line(&self, message: &str) {
        let timestamp = format_timestamp();
        let pid = unsafe { libc::getpid() };
        let tid = current_tid();
        let mut guard = self
            .target
            .lock()
            .expect("log sink mutex should never be poisoned");
        let line = format!("[grim-rust-shim ts={timestamp} pid={pid} tid={tid}] {message}\n");
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
