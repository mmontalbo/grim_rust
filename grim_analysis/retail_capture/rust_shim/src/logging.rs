use libc::pid_t;
use std::{
    env,
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

enum LogTarget {
    Stderr(io::Stderr),
    File(BufWriter<std::fs::File>),
}

struct LogSink {
    target: Mutex<LogTarget>,
}

static LOG_SINK: OnceLock<LogSink> = OnceLock::new();

pub(crate) fn log_line(message: &str) {
    let sink = LOG_SINK.get_or_init(LogSink::init);
    sink.write_line(message);
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
