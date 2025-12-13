use crate::trace_utils::handle_hex;
use libc::pid_t;
use serde::{
    de::{self, Deserializer, Visitor},
    ser::Serializer,
    Deserialize, Serialize,
};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use std::{
    env, fmt,
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub mod trace_utils;

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
    log_seq: AtomicU64,
    raw_seq: AtomicU64,
    semantic_seq: AtomicU64,
    run_id: OnceLock<Option<String>>,
}

struct SequenceNumbers {
    log_seq_display: String,
    stream_seq: u64,
    stream_seq_display: String,
}

impl TelemetryLogger {
    pub const fn new(config: TelemetryConfig) -> Self {
        Self {
            config,
            sink: OnceLock::new(),
            log_seq: AtomicU64::new(0),
            raw_seq: AtomicU64::new(0),
            semantic_seq: AtomicU64::new(0),
            run_id: OnceLock::new(),
        }
    }

    pub fn log_line(&self, message: &str) {
        if self.config.line_prefix.is_empty() {
            eprintln!("{message}");
        } else {
            eprintln!("[{}] {message}", self.config.line_prefix);
        }
    }

    pub fn log_event(&self, event: impl Into<EventBuilder>) {
        let _ = self.log_event_with_seq(event);
    }

    pub fn log_event_with_seq(&self, event: impl Into<EventBuilder>) -> u64 {
        let event = event.into();
        let stream = event.stream_kind();
        let seqs = self.next_sequences(stream);
        let stream_seq = seqs.stream_seq;
        self.log_event_inner(event, seqs);
        stream_seq
    }

    fn log_event_inner(&self, event: EventBuilder, seqs: SequenceNumbers) {
        let ts = elapsed_millis();
        let run_id = self
            .run_id
            .get_or_init(|| self.config.run_id_env.and_then(|name| env::var(name).ok()))
            .clone()
            .unwrap_or_default();
        let SequenceNumbers {
            log_seq_display,
            stream_seq_display,
            ..
        } = seqs;
        let mut object = event.finish();
        object.insert("seq".to_string(), JsonValue::String(stream_seq_display));
        object.insert("log_seq".to_string(), JsonValue::String(log_seq_display));
        object.insert("ts".to_string(), JsonValue::String(format!("{ts:08}")));
        object.insert(
            "engine".to_string(),
            JsonValue::String(self.config.engine_id.to_string()),
        );
        object.insert(
            "vm_id".to_string(),
            JsonValue::String(self.config.vm_id.to_string()),
        );
        if !self.config.line_prefix.is_empty() {
            object.insert(
                "logger".to_string(),
                JsonValue::String(self.config.line_prefix.to_string()),
            );
        }
        if !run_id.is_empty() {
            object.insert("run_id".to_string(), JsonValue::String(run_id));
        }
        object.insert("wall_ts".to_string(), JsonValue::String(format_timestamp()));
        object.insert(
            "pid".to_string(),
            JsonValue::Number(JsonNumber::from(unsafe { libc::getpid() } as u64)),
        );
        object.insert(
            "tid".to_string(),
            JsonValue::Number(JsonNumber::from(current_tid() as u64)),
        );

        let sink = self
            .sink
            .get_or_init(|| LogSink::init(self.config.log_env_vars));
        sink.write_json(object);
    }

    fn next_sequences(&self, stream: StreamKind) -> SequenceNumbers {
        let log_seq = self.log_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let stream_seq = match stream {
            StreamKind::Raw => self.raw_seq.fetch_add(1, Ordering::Relaxed) + 1,
            StreamKind::Semantic => self.semantic_seq.fetch_add(1, Ordering::Relaxed) + 1,
            StreamKind::Other => log_seq,
        };

        SequenceNumbers {
            log_seq_display: format!("{log_seq:06}"),
            stream_seq,
            stream_seq_display: format!("{stream_seq:06}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqRange {
    pub min: u64,
    pub max: u64,
}

impl SeqRange {
    pub const fn new(seq_a: u64, seq_b: u64) -> Self {
        if seq_a <= seq_b {
            Self {
                min: seq_a,
                max: seq_b,
            }
        } else {
            Self {
                min: seq_b,
                max: seq_a,
            }
        }
    }

    pub fn from_seqs<I>(seqs: I) -> Option<Self>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut iter = seqs.into_iter();
        let first = iter.next()?;
        let mut min = first;
        let mut max = first;
        for seq in iter {
            min = min.min(seq);
            max = max.max(seq);
        }
        Some(Self { min, max })
    }

    pub const fn include(self, seq: u64) -> Self {
        let min = if self.min < seq { self.min } else { seq };
        let max = if self.max > seq { self.max } else { seq };
        Self { min, max }
    }

    pub fn display(&self) -> String {
        if self.min == self.max {
            format!("{:06}", self.min)
        } else {
            format!("{:06}-{:06}", self.min, self.max)
        }
    }
}

impl fmt::Display for SeqRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

pub fn parse_seq_range(text: &str) -> Option<SeqRange> {
    if let Some((min, max)) = text.split_once('-') {
        let seq_min = min.parse::<u64>().ok()?;
        let seq_max = max.parse::<u64>().ok()?;
        Some(SeqRange::new(seq_min, seq_max))
    } else {
        let value = text.parse::<u64>().ok()?;
        Some(SeqRange::new(value, value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamKind {
    Semantic,
    Raw,
    Other,
}

impl StreamKind {
    pub fn from_field(value: &str) -> Self {
        match value {
            "raw" => StreamKind::Raw,
            "semantic" => StreamKind::Semantic,
            _ => StreamKind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamFilter {
    Semantic,
    Raw,
    All,
}

impl StreamFilter {
    pub fn matches(self, stream: StreamKind) -> bool {
        match self {
            StreamFilter::Semantic => matches!(stream, StreamKind::Semantic),
            StreamFilter::Raw => !matches!(stream, StreamKind::Semantic),
            StreamFilter::All => true,
        }
    }

    pub fn next(self) -> Self {
        match self {
            StreamFilter::Semantic => StreamFilter::Raw,
            StreamFilter::Raw => StreamFilter::All,
            StreamFilter::All => StreamFilter::Semantic,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StreamFilter::Semantic => "semantic",
            StreamFilter::Raw => "raw",
            StreamFilter::All => "all",
        }
    }
}

pub fn stream_kind_from_line(line: &str) -> StreamKind {
    let Ok(JsonValue::Object(obj)) = serde_json::from_str::<JsonValue>(line) else {
        return StreamKind::Other;
    };
    stream_kind_from_object(&obj)
}

fn stream_kind_from_object(obj: &JsonMap<String, JsonValue>) -> StreamKind {
    if let Some(stream) = obj.get("stream").and_then(|v| v.as_str()) {
        return StreamKind::from_field(stream);
    }
    if let Some(event) = obj.get("event").and_then(|v| v.as_str()) {
        if event.starts_with("semantic_") || event == "component_exit" || event == "engine_exit" {
            return StreamKind::Semantic;
        }
    }
    StreamKind::Other
}

pub fn parse_seq_field(line: &str) -> Option<SeqRange> {
    let Ok(JsonValue::Object(obj)) = serde_json::from_str::<JsonValue>(line) else {
        return None;
    };
    parse_seq_from_object(&obj, "seq")
}

pub fn parse_log_seq_field(line: &str) -> Option<SeqRange> {
    let Ok(JsonValue::Object(obj)) = serde_json::from_str::<JsonValue>(line) else {
        return None;
    };
    parse_seq_from_object(&obj, "log_seq")
}

fn parse_seq_from_object(obj: &JsonMap<String, JsonValue>, key: &str) -> Option<SeqRange> {
    let seq_value = obj.get(key)?;
    let seq_text = match seq_value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Number(num) => num.to_string(),
        _ => return None,
    };
    parse_seq_range(&seq_text)
}

pub fn normalize_seq_for_filter(
    stream: StreamKind,
    seq: SeqRange,
    filter: StreamFilter,
) -> Option<SeqRange> {
    match filter {
        StreamFilter::Semantic => {
            if matches!(stream, StreamKind::Semantic) {
                Some(seq)
            } else {
                None
            }
        }
        StreamFilter::Raw => {
            if matches!(stream, StreamKind::Semantic) {
                None
            } else {
                Some(seq)
            }
        }
        StreamFilter::All => Some(seq),
    }
}

pub struct EventBuilder {
    fields: JsonMap<String, JsonValue>,
}

impl EventBuilder {
    pub fn new(event: impl Into<String>) -> Self {
        let mut fields = JsonMap::new();
        fields.insert("event".to_string(), JsonValue::String(event.into()));
        Self { fields }
    }

    pub fn kv(mut self, key: &str, value: impl Serialize) -> Self {
        self.kv_mut(key, value);
        self
    }

    pub fn kv_mut(&mut self, key: &str, value: impl Serialize) {
        let json_value =
            serde_json::to_value(value).unwrap_or_else(|_| JsonValue::String("<?>".to_string()));
        self.fields.insert(key.to_string(), json_value);
    }

    pub fn kv_json_mut(&mut self, key: &str, value: JsonValue) {
        self.fields.insert(key.to_string(), value);
    }

    pub fn finish(self) -> JsonMap<String, JsonValue> {
        self.fields
    }

    fn stream_kind(&self) -> StreamKind {
        stream_kind_from_object(&self.fields)
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

    fn write_json(&self, object: JsonMap<String, JsonValue>) {
        let value = JsonValue::Object(object);
        let mut guard = self
            .target
            .lock()
            .expect("log sink mutex should never be poisoned");
        match &mut *guard {
            LogTarget::Stderr(stderr) => {
                let _ = serde_json::to_writer(&mut *stderr, &value);
                let _ = stderr.write_all(b"\n");
                let _ = stderr.flush();
            }
            LogTarget::File(file) => {
                let _ = serde_json::to_writer(&mut *file, &value);
                let _ = file.write_all(b"\n");
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
    pub symbol_source: Option<String>,
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
    serializer.serialize_str(&handle_hex(*value as usize))
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

    fn parse_pointer_string<E>(value: &str) -> Result<i32, E>
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
pub enum LuaSemanticEvent {
    #[serde(rename = "semantic_bind_global_closure")]
    SemanticBindGlobalClosure {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(skip_serializing_if = "Option::is_none")]
        upvalues: Option<i32>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "semantic_bind_global_constant")]
    SemanticBindGlobalConstant {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
    },
    SemanticSetTableEntry {
        table_handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        table_handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        table_fields: Option<ValueFields>,
        key: UpvaluePreview,
        value: UpvaluePreview,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_handle_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_fields: Option<ValueFields>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "semantic_tag_alias")]
    SemanticTagAlias {
        tag: i32,
        alias: String,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "semantic_ref_store")]
    SemanticStoreRef {
        lock: i32,
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_kind: Option<ValueType>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_fields: Option<ValueFields>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "semantic_ref_load")]
    SemanticLoadRef {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_kind: Option<ValueType>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "semantic_ref_unref")]
    SemanticUnref {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_kind: Option<ValueType>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "semantic_ref_batch")]
    SemanticRefBatch {
        kind: String,
        count: u32,
        start_ref: i32,
    },
    SemanticSetFallback {
        fallback: String,
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    SemanticSetTagmethod {
        tag: i32,
        event_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_alias: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LuaEvent {
    #[serde(rename = "lua_setglobal")]
    BindGlobal {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "registered_constant")]
    RegisteredConstant {
        name: String,
        handle: String,
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
        label: Option<String>,
        func: String,
        upvalues: i32,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "lua_call")]
    Call { name: String },
    #[serde(rename = "lua_callfunction")]
    CallFunc {
        handle: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        calls: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_id: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ref_alias: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ref_value_kind: Option<ValueType>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "lua_collectgarbage")]
    CollectGarbage {},
    #[serde(rename = "lua_copytagmethods")]
    CopyTagmethods {
        to: i32,
        from: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<i32>,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_createtable")]
    CreateTable {
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_dobuffer")]
    Dobuffer { name: String, size: usize },
    #[serde(rename = "lua_dofile")]
    Dofile { path: String },
    #[serde(rename = "lua_dostring")]
    Dostring { snippet: String },
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
    #[serde(rename = "lua_getref")]
    LoadRef {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_kind: Option<ValueType>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "lua_getglobal")]
    GetGlobal {
        name: String,
        handle: String,
        label: String,
        count: u64,
    },
    #[serde(rename = "lua_gettable")]
    GetTable {
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "lua_error")]
    LuaError { message: String },
    #[serde(rename = "lua_pushcclosure")]
    PushCclosure {
        name: String,
        func: String,
        upvalues: i32,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_pushlstring")]
    PushLstring { len: usize, preview: String },
    #[serde(rename = "lua_pushnil")]
    PushNil {},
    #[serde(rename = "lua_pushnumber")]
    PushNumber { value: String },
    #[serde(rename = "lua_pushobject")]
    PushObject {
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "lua_pushstring")]
    PushString { len: usize, preview: String },
    #[serde(rename = "lua_pushvalue")]
    PushValue {
        index: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "lua_pushusertag")]
    PushUsertag {
        #[serde(
            serialize_with = "serialize_pointer_hex",
            deserialize_with = "deserialize_pointer_hex"
        )]
        id: i32,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_rawgetglobal")]
    RawGetGlobal {
        name: String,
        handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "lua_rawsetglobal")]
    RawSetGlobal {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_rawgettable")]
    RawgetTable {
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
    },
    #[serde(rename = "lua_rawsettable")]
    RawsetTable {
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "set_table_entry")]
    SetTableEntry {
        table_handle: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        table_handle_label: Option<String>,
        key: UpvaluePreview,
        value: UpvaluePreview,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "register_native")]
    RegisterNative {
        name: String,
        handle: String,
        func: String,
        upvalues: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        library: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "set_constant")]
    SetConstant {
        name: String,
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_setfallback")]
    SetFallback {
        fallback: String,
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_settable")]
    SetTable {
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(rename = "caller")]
        caller: OriginFields,
    },
    #[serde(rename = "lua_settag")]
    SetTag {
        tag: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_alias: Option<String>,
    },
    #[serde(rename = "lua_settagmethod")]
    SetTagmethod {
        tag: i32,
        event_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_alias: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
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
    #[serde(rename = "lua_ref")]
    StoreRef {
        lock: i32,
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_fields: Option<ValueFields>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    #[serde(rename = "tag_state")]
    TagState {
        tag: i32,
        uses: u64,
        changed: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        methods: Option<String>,
    },
    #[serde(rename = "lua_unref")]
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
        let mut builder = builder_from_object(obj);
        builder.kv_mut("stream", "raw");
        builder
    }
}

impl From<LuaSemanticEvent> for EventBuilder {
    fn from(event: LuaSemanticEvent) -> Self {
        let value = serde_json::to_value(event).expect("serialize LuaSemanticEvent");
        let obj = value
            .as_object()
            .expect("LuaSemanticEvent should serialize to an object");
        let mut builder = builder_from_object(obj);
        builder.kv_mut("stream", "semantic");
        builder
    }
}

fn builder_from_object(obj: &JsonMap<String, JsonValue>) -> EventBuilder {
    let Some(event_name) = obj.get("event").and_then(|v| v.as_str()) else {
        panic!("telemetry event serialized without event tag");
    };
    let mut builder = EventBuilder::new(event_name);
    // Use a stable order for deterministic logs.
    let mut keys: Vec<_> = obj.keys().filter(|k| k.as_str() != "event").collect();
    keys.sort();
    for key in keys {
        if let Some(value) = obj.get(key) {
            builder.kv_json_mut(key, value.clone());
        }
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_detection_prefers_semantic_tag() {
        let raw_line = r#"{"seq":"000001","stream":"raw","event":"lua_pushnil"}"#;
        assert_eq!(stream_kind_from_line(raw_line), StreamKind::Raw);
        let semantic_line = r#"{"seq":"000002","event":"semantic_bind_global_closure"}"#;
        assert_eq!(stream_kind_from_line(semantic_line), StreamKind::Semantic);
    }

    #[test]
    fn normalize_seq_handles_filters() {
        let seq = SeqRange::new(5, 5);
        let semantic =
            normalize_seq_for_filter(StreamKind::Semantic, seq, StreamFilter::Semantic).unwrap();
        assert_eq!(semantic.min, 5);
        assert!(normalize_seq_for_filter(StreamKind::Semantic, seq, StreamFilter::Raw).is_none());
    }

    #[test]
    fn stream_sequences_use_stream_counters() {
        let path = env::temp_dir().join(format!(
            "grim_telemetry_seq_test_{}.log",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("timestamp")
                .as_nanos()
        ));
        env::set_var("GRIM_TELEMETRY_TEST_LOG", &path);
        let logger = TelemetryLogger::new(TelemetryConfig {
            engine_id: "test_engine",
            vm_id: "test_vm",
            log_env_vars: &["GRIM_TELEMETRY_TEST_LOG"],
            line_prefix: "test",
            run_id_env: None,
        });

        let raw_seq1 = logger.log_event_with_seq(LuaEvent::CollectGarbage {});
        let semantic_seq = logger.log_event_with_seq(LuaSemanticEvent::SemanticTagAlias {
            tag: 1,
            alias: "alias".to_string(),
            origin: OriginFields::default(),
        });
        let raw_seq2 = logger.log_event_with_seq(LuaEvent::CollectGarbage {});

        let contents = std::fs::read_to_string(&path).expect("read log file");
        let mut lines = contents.lines();
        let raw_line: serde_json::Value =
            serde_json::from_str(lines.next().expect("raw line")).expect("raw json");
        let semantic_line: serde_json::Value =
            serde_json::from_str(lines.next().expect("semantic line")).expect("semantic json");
        let raw_line_2: serde_json::Value =
            serde_json::from_str(lines.next().expect("second raw line")).expect("raw json");

        assert_eq!(raw_seq1, 1);
        assert_eq!(semantic_seq, 1);
        assert_eq!(raw_seq2, 2);
        assert_eq!(raw_line.get("seq").and_then(|v| v.as_str()), Some("000001"));
        assert_eq!(
            raw_line.get("log_seq").and_then(|v| v.as_str()),
            Some("000001")
        );
        assert_eq!(
            semantic_line.get("seq").and_then(|v| v.as_str()),
            Some("000001")
        );
        assert_eq!(
            semantic_line.get("log_seq").and_then(|v| v.as_str()),
            Some("000002")
        );
        assert_eq!(
            raw_line_2.get("seq").and_then(|v| v.as_str()),
            Some("000002")
        );
        assert_eq!(
            raw_line_2.get("log_seq").and_then(|v| v.as_str()),
            Some("000003")
        );

        let _ = std::fs::remove_file(&path);
        env::remove_var("GRIM_TELEMETRY_TEST_LOG");
    }

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
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("cutscene")
        );
        assert_eq!(
            fields.get("movie").and_then(|v| v.as_str()),
            Some("intro.snm")
        );
        assert_eq!(
            fields.get("movie_label").and_then(|v| v.as_str()),
            Some("movie.intro")
        );
        assert_eq!(fields.get("phase").and_then(|v| v.as_str()), Some("start"));
        assert_eq!(
            fields.get("playing").and_then(|v| v.as_str()),
            Some("playing")
        );
        assert_eq!(fields.get("elapsed_ms").and_then(|v| v.as_i64()), Some(0));
        assert_eq!(fields.get("polls").and_then(|v| v.as_i64()), Some(0));
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
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("cutscene_skip")
        );
        assert_eq!(
            fields.get("phase").and_then(|v| v.as_str()),
            Some("complete")
        );
        assert_eq!(
            fields.get("movie_label").and_then(|v| v.as_str()),
            Some("movie.intro")
        );
        assert_eq!(fields.get("elapsed_ms").and_then(|v| v.as_i64()), Some(123));
        assert!(!fields.contains_key("polls"));
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
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("lua_pushusertag")
        );
        assert_eq!(
            fields.get("id").and_then(|v| v.as_str()),
            Some("0x00000007")
        );
        assert_eq!(fields.get("tag").and_then(|v| v.as_i64()), Some(42));
        assert_eq!(
            fields.get("value_type").and_then(|v| v.as_str()),
            Some("userdata")
        );
    }

    #[test]
    fn get_global_carries_label() {
        let event = LuaEvent::GetGlobal {
            name: "foo".to_string(),
            handle: "0x00000001".to_string(),
            label: "global:foo".to_string(),
            count: 2,
        };
        let fields = EventBuilder::from(event).finish();
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("lua_getglobal")
        );
        assert_eq!(
            fields.get("label").and_then(|v| v.as_str()),
            Some("global:foo")
        );
    }

    #[test]
    fn registered_global_serializes_with_upvalues() {
        let event = LuaEvent::RegisteredGlobal {
            name: "foo".to_string(),
            handle: "0x00000001".to_string(),
            label: Some("global:foo".to_string()),
            func: "0x0000abcd".to_string(),
            upvalues: 2,
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
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("registered_global")
        );
        assert_eq!(fields.get("upvalues").and_then(|v| v.as_i64()), Some(2));
    }

    #[test]
    fn registered_constant_serializes_value_fields() {
        let event = LuaEvent::RegisteredConstant {
            name: "foo".to_string(),
            handle: "0x00000001".to_string(),
            label: Some("global:foo".to_string()),
            values: ValueFields {
                value_type: Some(ValueType::String),
                value_len: Some(3),
                value_preview: Some("bar".to_string()),
                ..Default::default()
            },
            origin: OriginFields::default(),
        };
        let fields = EventBuilder::from(event).finish();
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("registered_constant")
        );
        assert_eq!(
            fields.get("value_type").and_then(|v| v.as_str()),
            Some("string")
        );
        assert_eq!(fields.get("value_len").and_then(|v| v.as_i64()), Some(3));
        assert_eq!(
            fields.get("value_preview").and_then(|v| v.as_str()),
            Some("bar")
        );
    }

    #[test]
    fn set_table_entry_serializes_with_key_and_value() {
        let event = LuaEvent::SetTableEntry {
            table_handle: "0x0000000a".to_string(),
            table_handle_label: Some("table:example".to_string()),
            key: UpvaluePreview {
                kind: ValueType::String,
                value: None,
                value_len: Some(3),
                preview: Some("key".to_string()),
                tag: None,
            },
            value: UpvaluePreview {
                kind: ValueType::Number,
                value: Some("42".to_string()),
                value_len: None,
                preview: None,
                tag: None,
            },
            note: Some("via_rawsettable".to_string()),
            caller: OriginFields {
                origin: Some("0x0000cafe".to_string()),
                ..Default::default()
            },
        };
        let fields = EventBuilder::from(event).finish();
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("set_table_entry")
        );
        assert_eq!(
            fields.get("table_handle").and_then(|v| v.as_str()),
            Some("0x0000000a")
        );
        assert_eq!(
            fields.get("table_handle_label").and_then(|v| v.as_str()),
            Some("table:example")
        );
        let key = fields.get("key").and_then(|v| v.as_object()).expect("key");
        assert_eq!(key.get("kind").and_then(|v| v.as_str()), Some("string"));
        let value = fields
            .get("value")
            .and_then(|v| v.as_object())
            .expect("value");
        assert_eq!(value.get("kind").and_then(|v| v.as_str()), Some("number"));
        assert_eq!(
            fields
                .get("caller")
                .and_then(|v| v.as_object())
                .and_then(|caller| caller.get("origin"))
                .and_then(|v| v.as_str()),
            Some("0x0000cafe")
        );
        assert_eq!(
            fields.get("note").and_then(|v| v.as_str()),
            Some("via_rawsettable")
        );
    }

    #[test]
    fn raw_events_include_stream_marker() {
        let fields = EventBuilder::from(LuaEvent::CollectGarbage {}).finish();
        assert_eq!(fields.get("stream").and_then(|v| v.as_str()), Some("raw"));
    }

    #[test]
    fn semantic_events_serialize_with_stream() {
        let event = LuaSemanticEvent::SemanticBindGlobalClosure {
            name: "foo".to_string(),
            handle: "0x00000002".to_string(),
            label: Some("global:foo".to_string()),
            values: ValueFields {
                value_type: Some(ValueType::Cfunction),
                func: Some("0x0000beef".to_string()),
                ..Default::default()
            },
            upvalues: Some(1),
            origin: OriginFields::default(),
        };
        let fields = EventBuilder::from(event).finish();
        assert_eq!(
            fields.get("event").and_then(|v| v.as_str()),
            Some("semantic_bind_global_closure")
        );
        assert_eq!(
            fields.get("stream").and_then(|v| v.as_str()),
            Some("semantic")
        );
        assert_eq!(
            fields.get("label").and_then(|v| v.as_str()),
            Some("global:foo")
        );
    }
}
