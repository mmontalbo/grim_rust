use libc::pid_t;
use serde::{
    de::{self, Deserializer, Visitor},
    ser::Serializer,
    Deserialize, Serialize,
};
use serde_json::Value as JsonValue;
use std::{
    env,
    fmt::{self, Display},
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

    pub fn log_event(&self, event: impl Into<EventBuilder>) {
        let _ = self.log_event_with_seq(event);
    }

    pub fn log_event_with_seq(&self, event: impl Into<EventBuilder>) -> u64 {
        let event = event.into();
        let seq = self.event_seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.log_event_with_seq_display(event, format!("{seq:06}"));
        seq
    }

    pub fn log_event_with_seq_display(
        &self,
        event: impl Into<EventBuilder>,
        seq_display: impl Into<String>,
    ) {
        self.log_event_inner(event.into(), seq_display.into());
    }

    fn log_event_inner(&self, event: EventBuilder, seq_display: String) {
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
        parts.push(format!("seq={seq_display}"));
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
        self.kv_mut(key, value);
        self
    }

    pub fn kv_mut(&mut self, key: &str, value: impl Display) {
        let mut value = value.to_string();
        let needs_quotes = value.contains(|c: char| c.is_whitespace());
        if needs_quotes {
            value = value.replace('"', "\\\"");
            self.fields.push(format!("{key}=\"{value}\""));
        } else {
            self.fields.push(format!("{key}={value}"));
        }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OriginFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demangled: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Number,
    String,
    Nil,
    Table,
    Function,
    Cfunction,
    Userdata,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutscenePhase {
    Start,
    Poll,
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutsceneSkipPhase {
    Request,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutscenePlaying {
    Playing,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutsceneResult {
    PollStopped,
    StopCalled,
    Replaced,
}

fn serialize_pointer_hex<S>(value: &i32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("0x{addr:08x}", addr = *value as u32))
}

fn deserialize_pointer_hex<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    struct PointerVisitor;

    impl<'de> Visitor<'de> for PointerVisitor {
        type Value = i32;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("hex pointer string or integer")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value as i32)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value as i32)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            parse_pointer_string(value)
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    fn parse_pointer_string<'de, E>(value: &str) -> Result<i32, E>
    where
        E: de::Error,
    {
        let trimmed = value.trim();
        let no_prefix = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        u32::from_str_radix(no_prefix, 16)
            .map(|num| num as i32)
            .or_else(|_| trimmed.parse::<i32>())
            .map_err(|_| E::custom(format!("invalid pointer value: {value}")))
    }

    deserializer.deserialize_any(PointerVisitor)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValueFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_type: Option<ValueType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub func: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_hex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpvaluePreview {
    pub kind: ValueType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LuaEvent {
    #[serde(rename = "bind_global")]
    BindGlobal {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "registered_global")]
    RegisteredGlobal {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        push_seq: u64,
        func: String,
        upvalues: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        upvalue_previews: Option<Vec<UpvaluePreview>>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
    },
    Call {
        name: String,
    },
    #[serde(rename = "call_func")]
    CallFunc {
        handle: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        calls: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "collect_garbage")]
    CollectGarbage {},
    #[serde(rename = "copy_tagmethods")]
    CopyTagmethods {
        to: i32,
        from: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<i32>,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "create_table")]
    CreateTable {
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "dobuffer")]
    Dobuffer {
        name: String,
        size: usize,
    },
    Dofile {
        path: String,
    },
    Dostring {
        snippet: String,
    },
    #[serde(rename = "cutscene")]
    Cutscene {
        movie: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        movie_label: Option<String>,
        phase: CutscenePhase,
        playing: CutscenePlaying,
        #[serde(skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u128>,
        #[serde(skip_serializing_if = "Option::is_none")]
        polls: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<CutsceneResult>,
    },
    #[serde(rename = "cutscene_skip")]
    CutsceneSkip {
        phase: CutsceneSkipPhase,
        #[serde(skip_serializing_if = "Option::is_none")]
        movie: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        movie_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u128>,
        #[serde(skip_serializing_if = "Option::is_none")]
        polls: Option<u64>,
    },
    #[serde(rename = "fetch_ref")]
    FetchRef {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "get_global")]
    GetGlobal {
        name: String,
        handle: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        count: u64,
    },
    #[serde(rename = "get_table")]
    GetTable {
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "lua_error")]
    LuaError {
        message: String,
    },
    #[serde(rename = "push_cclosure")]
    PushCclosure {
        name: String,
        func: String,
        push_seq: u64,
        upvalues: i32,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "push_lstring")]
    PushLstring {
        len: usize,
        preview: String,
    },
    #[serde(rename = "push_nil")]
    PushNil {},
    #[serde(rename = "push_number")]
    PushNumber {
        value: String,
    },
    #[serde(rename = "push_object")]
    PushObject {
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "push_string")]
    PushString {
        len: usize,
        preview: String,
    },
    #[serde(rename = "push_usertag")]
    PushUsertag {
        #[serde(
            serialize_with = "serialize_pointer_hex",
            deserialize_with = "deserialize_pointer_hex"
        )]
        id: i32,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "raw_get_global")]
    RawGetGlobal {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "raw_set_global")]
    RawSetGlobal {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "rawget_table")]
    RawgetTable {
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "rawset_table")]
    RawsetTable {
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "register_native")]
    RegisterNative {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        func: String,
        upvalues: i32,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "set_constant")]
    SetConstant {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "set_fallback")]
    SetFallback {
        fallback: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "set_table")]
    SetTable {
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "set_tag")]
    SetTag {
        tag: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_label: Option<String>,
    },
    #[serde(rename = "set_tagmethod")]
    SetTagmethod {
        tag: i32,
        event_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_label: Option<String>,
    },
    #[serde(rename = "post_intro_room")]
    PostIntroRoom {
        source: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        set: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        setup: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_movie: Option<String>,
    },
    #[serde(rename = "store_ref")]
    StoreRef {
        lock: i32,
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "tag_state")]
    TagState {
        tag: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_label: Option<String>,
        uses: u64,
        changed: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        methods: Option<String>,
    },
    Unref {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

impl From<LuaEvent> for EventBuilder {
    fn from(event: LuaEvent) -> Self {
        let value = serde_json::to_value(event).expect("serialize LuaEvent");
        let obj = value
            .as_object()
            .expect("LuaEvent should serialize to an object");
        let Some(event_name) = obj.get("event").and_then(|v| v.as_str()) else {
            panic!("LuaEvent serialized without event tag");
        };

        let mut builder = EventBuilder::new(event_name);
        // Use a stable order for deterministic logs.
        let mut keys: Vec<_> = obj.keys().filter(|k| k.as_str() != "event").collect();
        keys.sort();
        for key in keys {
            if let Some(value) = obj.get(key) {
                let rendered = render_json_value(value);
                builder.kv_mut(key, rendered);
            }
        }
        builder
    }
}

fn render_json_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutscene_serializes_with_tag_and_fields() {
        let event = LuaEvent::Cutscene {
            movie: "intro.snm".to_string(),
            movie_label: Some("movie.intro".to_string()),
            phase: CutscenePhase::Start,
            playing: CutscenePlaying::Playing,
            elapsed_ms: Some(0),
            polls: Some(0),
            result: None,
        };
        let fields = EventBuilder::from(event).finish();
        assert!(fields.iter().any(|f| f == "event=cutscene"));
        assert!(fields.iter().any(|f| f == "movie=intro.snm"));
        assert!(fields.iter().any(|f| f == "movie_label=movie.intro"));
        assert!(fields.iter().any(|f| f == "phase=start"));
        assert!(fields.iter().any(|f| f == "playing=playing"));
        assert!(fields.iter().any(|f| f == "elapsed_ms=0"));
        assert!(fields.iter().any(|f| f == "polls=0"));
    }

    #[test]
    fn cutscene_skip_serializes_with_optional_fields() {
        let event = LuaEvent::CutsceneSkip {
            phase: CutsceneSkipPhase::Complete,
            movie: None,
            movie_label: Some("movie.intro".to_string()),
            elapsed_ms: Some(123),
            polls: None,
        };
        let fields = EventBuilder::from(event).finish();
        assert!(fields.iter().any(|f| f == "event=cutscene_skip"));
        assert!(fields.iter().any(|f| f == "phase=complete"));
        assert!(fields.iter().any(|f| f == "movie_label=movie.intro"));
        assert!(fields.iter().any(|f| f == "elapsed_ms=123"));
        assert!(fields.iter().any(|f| f == "polls=0") == false);
    }

    #[test]
    fn push_usertag_serializes_value_fields() {
        let event = LuaEvent::PushUsertag {
            id: 7,
            values: ValueFields {
                value_type: Some(ValueType::Userdata),
                tag: Some(42),
                ..Default::default()
            },
            caller: OriginFields::default(),
        };
        let fields = EventBuilder::from(event).finish();
        assert!(fields.iter().any(|f| f == "event=push_usertag"));
        assert!(fields.iter().any(|f| f == "id=0x00000007"));
        assert!(fields.iter().any(|f| f == "tag=42"));
        assert!(fields.iter().any(|f| f == "value_type=userdata"));
        assert!(fields
            .iter()
            .all(|f| f.starts_with("payload_hex=") == false));
    }

    #[test]
    fn set_tag_includes_label_when_present() {
        let event = LuaEvent::SetTag {
            tag: 9,
            note: None,
            tag_label: Some("example".to_string()),
        };
        let fields = EventBuilder::from(event).finish();
        assert!(fields.iter().any(|f| f == "tag=9"));
        assert!(fields.iter().any(|f| f == "tag_label=example"));
    }

    #[test]
    fn get_global_carries_handle_label() {
        let event = LuaEvent::GetGlobal {
            name: "foo".to_string(),
            handle: "0x00000001".to_string(),
            handle_label: Some("global:foo".to_string()),
            label: "global:foo".to_string(),
            count: 2,
        };
        let fields = EventBuilder::from(event).finish();
        assert!(fields.iter().any(|f| f == "handle_label=global:foo"));
        assert!(fields.iter().any(|f| f == "label=global:foo"));
    }

    #[test]
    fn registered_global_serializes_with_upvalues() {
        let event = LuaEvent::RegisteredGlobal {
            name: "foo".to_string(),
            handle: "0x00000001".to_string(),
            handle_label: Some("global:foo".to_string()),
            label: Some("global:foo".to_string()),
            push_seq: 3,
            func: "0x0000abcd".to_string(),
            upvalues: 2,
            upvalue_previews: Some(vec![UpvaluePreview {
                kind: ValueType::Number,
                value: Some("7".to_string()),
                value_len: None,
                preview: None,
                tag: None,
            }]),
            values: ValueFields {
                value_type: Some(ValueType::Cfunction),
                ..Default::default()
            },
            origin: OriginFields {
                origin: Some("0x0000dead".to_string()),
                ..Default::default()
            },
        };
        let fields = EventBuilder::from(event).finish();
        assert!(fields.iter().any(|f| f == "event=registered_global"));
        assert!(fields.iter().any(|f| f == "push_seq=3"));
        assert!(fields.iter().any(|f| f == "upvalues=2"));
        assert!(fields
            .iter()
            .any(|f| f.starts_with("upvalue_previews=[{\"kind\":\"number\"")));
    }
}
