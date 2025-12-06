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

    pub fn next_seq(&self) -> u64 {
        self.event_seq.fetch_add(1, Ordering::Relaxed) + 1
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
    for token in line.split_whitespace() {
        if let Some(value) = token.strip_prefix("stream=") {
            return StreamKind::from_field(value);
        }
    }
    if line.contains("event=semantic_") {
        StreamKind::Semantic
    } else {
        StreamKind::Other
    }
}

pub fn parse_seq_field(line: &str) -> Option<SeqRange> {
    for token in line.split_whitespace() {
        if let Some(value) = token.strip_prefix("seq=") {
            return parse_seq_range(value);
        }
    }
    None
}

pub fn normalize_seq_for_filter(
    stream: StreamKind,
    seq: SeqRange,
    filter: StreamFilter,
    semantic_counter: &mut u64,
) -> Option<SeqRange> {
    match filter {
        StreamFilter::Semantic => {
            if matches!(stream, StreamKind::Semantic) {
                *semantic_counter = semantic_counter.saturating_add(1);
                Some(SeqRange::new(*semantic_counter, *semantic_counter))
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
        StreamFilter::All => {
            if matches!(stream, StreamKind::Semantic) {
                *semantic_counter = semantic_counter.saturating_add(1);
                Some(SeqRange::new(*semantic_counter, *semantic_counter))
            } else {
                Some(seq)
            }
        }
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
    SemanticBindGlobal {
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
    SemanticBindConstant {
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
        #[serde(flatten)]
        caller: OriginFields,
    },
    SemanticStoreRef {
        lock: i32,
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    SemanticFetchRef {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        origin: OriginFields,
    },
    SemanticUnref {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SemanticSetFallback {
        fallback: String,
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
        origin: OriginFields,
        #[serde(flatten)]
        caller: OriginFields,
    },
    SemanticSetTagmethod {
        tag: i32,
        event_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
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
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "lua_createtable")]
    CreateTable {
        handle: String,
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
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
    FetchRef {
        #[serde(rename = "ref")]
        reference: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
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
        #[serde(flatten)]
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
        #[serde(flatten)]
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
        #[serde(flatten)]
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
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "register_native")]
    RegisterNative {
        name: String,
        handle: String,
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
        #[serde(flatten)]
        values: ValueFields,
        #[serde(flatten)]
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
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "lua_settable")]
    SetTable {
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(flatten)]
        caller: OriginFields,
    },
    #[serde(rename = "lua_settag")]
    SetTag {
        tag: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    #[serde(rename = "lua_settagmethod")]
    SetTagmethod {
        tag: i32,
        event_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(flatten)]
        values: ValueFields,
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

fn builder_from_object(obj: &serde_json::Map<String, JsonValue>) -> EventBuilder {
    let Some(event_name) = obj.get("event").and_then(|v| v.as_str()) else {
        panic!("telemetry event serialized without event tag");
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
    fn stream_detection_prefers_semantic_tag() {
        let raw_line = "[grim] seq=000001 stream=raw event=lua_pushnil";
        assert_eq!(stream_kind_from_line(raw_line), StreamKind::Raw);
        let semantic_line = "seq=000002 event=semantic_bind_global";
        assert_eq!(stream_kind_from_line(semantic_line), StreamKind::Semantic);
    }

    #[test]
    fn normalize_seq_handles_filters() {
        let mut counter = 0;
        let seq = SeqRange::new(5, 5);
        let semantic = normalize_seq_for_filter(
            StreamKind::Semantic,
            seq,
            StreamFilter::Semantic,
            &mut counter,
        )
        .unwrap();
        assert_eq!(semantic.min, 1);
        assert!(normalize_seq_for_filter(
            StreamKind::Semantic,
            seq,
            StreamFilter::Raw,
            &mut counter
        )
        .is_none());
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
        assert!(!fields.iter().any(|f| f == "polls=0"));
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
        assert!(fields.iter().any(|f| f == "event=lua_pushusertag"));
        assert!(fields.iter().any(|f| f == "id=0x00000007"));
        assert!(fields.iter().any(|f| f == "tag=42"));
        assert!(fields.iter().any(|f| f == "value_type=userdata"));
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
        assert!(fields.iter().any(|f| f == "event=lua_getglobal"));
        assert!(fields.iter().any(|f| f == "label=global:foo"));
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
        assert!(fields.iter().any(|f| f == "event=registered_global"));
        assert!(fields.iter().any(|f| f == "upvalues=2"));
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
        assert!(fields.iter().any(|f| f == "event=registered_constant"));
        assert!(fields.iter().any(|f| f == "value_type=string"));
        assert!(fields.iter().any(|f| f == "value_len=3"));
        assert!(fields.iter().any(|f| f == "value_preview=bar"));
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
        assert!(fields.iter().any(|f| f == "event=set_table_entry"));
        assert!(fields.iter().any(|f| f == "table_handle=0x0000000a"));
        assert!(fields
            .iter()
            .any(|f| f == "table_handle_label=table:example"));
        assert!(fields
            .iter()
            .any(|f| f.starts_with("key={\"kind\":\"string\"")));
        assert!(fields
            .iter()
            .any(|f| f.starts_with("value={\"kind\":\"number\"")));
        assert!(fields.iter().any(|f| f == "origin=0x0000cafe"));
        assert!(fields.iter().any(|f| f == "note=via_rawsettable"));
    }

    #[test]
    fn raw_events_include_stream_marker() {
        let fields = EventBuilder::from(LuaEvent::CollectGarbage {}).finish();
        assert!(fields.iter().any(|f| f == "stream=raw"));
    }

    #[test]
    fn semantic_events_serialize_with_stream() {
        let event = LuaSemanticEvent::SemanticBindGlobal {
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
        assert!(fields.iter().any(|f| f == "event=semantic_bind_global"));
        assert!(fields.iter().any(|f| f == "stream=semantic"));
        assert!(fields.iter().any(|f| f == "label=global:foo"));
    }
}
