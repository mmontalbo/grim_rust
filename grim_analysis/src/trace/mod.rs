//! Trace instrumentation for the Lua 3.1 C API.
//!
//! Each `trace_lua_*` function mirrors the retail VM behavior while logging both
//! raw events and higher-level semantics (push previews, table mutations, globals,
//! references, and tag operations). Symbol resolution and value inspection are
//! delegated to `lua_api`, and telemetry is emitted via `logging`.
use crate::{
    logging::{
        log_line, LuaEvent, LuaSemanticEvent, OriginFields, UpvaluePreview, ValueFields, ValueType,
    },
    lua_api::{
        call_real_lua_getcfunction, call_real_lua_getnumber, call_real_lua_getobjname,
        call_real_lua_getstring, call_real_lua_iscfunction, call_real_lua_isfunction,
        call_real_lua_isnumber, call_real_lua_isstring, call_real_lua_istable,
        call_real_lua_isuserdata, call_real_lua_tag, LuaObject,
    },
    symbol_map::lookup_symbol_from_map,
};
use grim_telemetry_common::trace_utils::{
    caller_origin_details, describe_closure_target, format_number_for_log,
    origin_fields_from_details, ref_alias, remember_ref_alias, semantic_set_table_entry,
    should_skip_caller_frame as common_should_skip_caller_frame, truncate_for_log,
    upvalue_preview_from_meta, value_fields_from_meta, ValueMeta,
};
use libc::c_int;
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    sync::{Mutex, OnceLock},
};

pub(crate) use grim_telemetry_common::trace_utils::{handle_hex, LOG_PREVIEW_MAX_LEN};

mod calls;
mod globals;
mod openlib;
mod push;
mod refs;
mod state;
mod tables;
mod tags;

pub(crate) use calls::{
    trace_lua_call, trace_lua_callfunction, trace_lua_collectgarbage, trace_lua_dobuffer,
    trace_lua_dofile, trace_lua_dostring, trace_lua_error,
};
pub(crate) use globals::{
    trace_lua_getglobal, trace_lua_rawgetglobal, trace_lua_rawsetglobal, trace_lua_setglobal,
};
pub(crate) use openlib::trace_lua_openlib;
pub(crate) use push::{
    trace_lua_push_closure, trace_lua_pushlstring, trace_lua_pushnil, trace_lua_pushnumber,
    trace_lua_pushobject, trace_lua_pushstring, trace_lua_pushusertag, trace_lua_pushvalue,
};
pub(crate) use refs::{trace_lua_getref, trace_lua_ref, trace_lua_unref};
pub(crate) use state::{trace_lua_newstate, trace_lua_newthread, trace_lua_open};
pub(crate) use tables::{
    trace_lua_createtable, trace_lua_gettable, trace_lua_rawgettable, trace_lua_rawsettable,
    trace_lua_settable,
};
pub(crate) use tags::{
    trace_lua_copytagmethods, trace_lua_newtag, trace_lua_setfallback, trace_lua_settag,
    trace_lua_settagmethod,
};

const PUSH_RING_CAPACITY: usize = 16;
const MAX_PENDING_REGISTERED_GLOBALS: usize = 8;
static CALLFUNCTION_TRACKER: OnceLock<Mutex<CallfunctionTracker>> = OnceLock::new();
static GLOBAL_ACCESS_TRACKER: OnceLock<Mutex<GlobalAccessTracker>> = OnceLock::new();
static HANDLE_LABELS: OnceLock<Mutex<HandleLabelTracker>> = OnceLock::new();
static REF_ALIAS_TRACKER: OnceLock<Mutex<RefAliasTracker>> = OnceLock::new();
static REF_BATCH_EXIT_FLUSH: OnceLock<()> = OnceLock::new();
static PUSH_EVENT_TRACKER: OnceLock<Mutex<PushEventTracker>> = OnceLock::new();
static REGISTERED_GLOBAL_CANDIDATES: OnceLock<Mutex<RegisteredGlobalTracker>> = OnceLock::new();

/// Caches metadata about a push so later table operations can correlate key/value pairs.
fn record_push_preview(log_seq: u64, preview: UpvaluePreview, handle: Option<LuaObject>) {
    // We keep a tiny ring buffer of recent push events so table set operations can pair
    // their key/value pushes with the destination table. Any non-push clears the ring
    // to avoid crossing event boundaries.
    if let Ok(mut tracker) = push_event_tracker().lock() {
        tracker.record_push(log_seq, preview, handle);
    } else {
        log_line("push event tracker mutex poisoned; skipping push capture");
    }
}

/// Clears pending push context when a non-push API call occurs.
fn record_non_push_event() {
    record_non_push_event_with_flush(true);
}

/// Clears push context without flushing pending ref batches (used by lua_ref).
fn record_non_push_event_skip_ref_batch() {
    record_non_push_event_with_flush(false);
}

fn record_non_push_event_with_flush(flush_ref_batch: bool) {
    if flush_ref_batch {
        flush_ref_batch_event();
        if let Ok(mut tracker) = ref_alias_tracker().lock() {
            tracker.clear_alias();
        }
    }
    // A non-push (e.g. call, get) means pending push context is no longer meaningful.
    if let Ok(mut tracker) = push_event_tracker().lock() {
        tracker.record_non_push();
    } else {
        log_line("push event tracker mutex poisoned; skipping activity record");
    }
}

/// Returns the most recent captured pushes needed to describe a table write.
fn take_recent_pushes(count: usize) -> Option<Vec<TrackedPush>> {
    match push_event_tracker().lock() {
        Ok(mut tracker) => {
            let pushes = tracker.snapshot_recent(count);
            tracker.clear();
            pushes
        }
        Err(_) => {
            log_line("push event tracker mutex poisoned; skipping push capture");
            None
        }
    }
}

/// Emits semantic/log events describing a table entry mutation if we have enough context.
fn emit_set_table_entry(
    table_handle: Option<LuaObject>,
    pushes: Option<Vec<TrackedPush>>,
    caller: OriginFields,
    note: Option<String>,
) {
    // settable/rawsettable expect the last two pushes to be key then value; we only log
    // when we have both, falling back to the table handle passed on the stack or the
    // first table push we observed.
    let pushes = match pushes {
        Some(pushes) if pushes.len() >= 2 => pushes,
        _ => return,
    };
    let handle = match table_handle.or_else(|| table_handle_from_pushes(&pushes)) {
        Some(handle) => handle,
        None => return,
    };
    let table_handle_hex = handle_hex(handle as usize);
    let table_handle_label = handle_label_for(handle);
    let table_fields = describe_lua_value(handle)
        .map(|value| value_fields_from_details(&value))
        .or_else(|| {
            Some(ValueFields {
                value_type: Some(ValueType::Table),
                ..ValueFields::default()
            })
        });
    let key_push = &pushes[pushes.len() - 2];
    let value_push = pushes.last().unwrap();
    let value_handle = value_push
        .handle
        .map(|value_handle| handle_hex(value_handle as usize));
    let value_handle_label = value_push.handle.and_then(handle_label_for);
    let value_fields = value_push
        .handle
        .and_then(describe_lua_value)
        .map(|details| value_fields_from_details(&details));
    if let Some(alias) = ref_alias_from_table(table_handle_label.as_ref(), &key_push.preview) {
        remember_ref_alias_candidate(alias);
    }
    let semantic_event = semantic_set_table_entry(
        table_handle_hex,
        table_handle_label,
        table_fields,
        key_push.preview.clone(),
        value_push.preview.clone(),
        value_handle,
        value_handle_label,
        value_fields,
        note,
        caller,
    );
    log_semantic_event(semantic_event);
}

/// Picks a table handle from captured pushes when none was provided directly.
fn table_handle_from_pushes(pushes: &[TrackedPush]) -> Option<LuaObject> {
    pushes
        .iter()
        .filter(|push| matches!(push.preview.kind, ValueType::Table) && push.handle.is_some())
        .min_by_key(|push| push.log_seq)
        .and_then(|push| push.handle)
}

fn ref_alias_from_table(
    table_handle_label: Option<&String>,
    key: &UpvaluePreview,
) -> Option<String> {
    if !matches!(key.kind, ValueType::String) {
        return None;
    }
    let key_text = key
        .preview
        .as_ref()
        .or(key.value.as_ref())
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())?;
    if let Some(label) = table_handle_label {
        Some(format!("{label}:{key_text}"))
    } else {
        Some(key_text.to_string())
    }
}

/// Emits a semantic binding event for a function registered as a global.
fn emit_registered_global(
    name: &str,
    handle: LuaObject,
    label: String,
    upvalues: c_int,
    values: ValueFields,
    origin: Option<ClosureOrigin>,
) {
    let origin_fields = origin_fields(origin.as_ref());
    log_semantic_event(LuaSemanticEvent::SemanticBindGlobalClosure {
        name: name.to_string(),
        handle: handle_hex(handle as usize),
        label: Some(label.clone()),
        values: values.clone(),
        upvalues: Some(upvalues),
        origin: origin_fields.clone(),
    });
}

/// Emits a semantic binding event for a constant registered as a global.
fn emit_registered_constant(
    name: &str,
    handle: LuaObject,
    label: String,
    values: ValueFields,
    origin: Option<ClosureOrigin>,
) {
    let origin_fields = origin_fields(origin.as_ref());
    log_semantic_event(LuaSemanticEvent::SemanticBindGlobalConstant {
        name: name.to_string(),
        handle: handle_hex(handle as usize),
        label: Some(label.clone()),
        values: values.clone(),
        origin: origin_fields.clone(),
    });
}

/// Records that a closure was just pushed so a subsequent global set can attribute it.
fn remember_registered_global_candidate(
    func_addr: usize,
    upvalues: c_int,
    origin: Option<ClosureOrigin>,
) {
    match registered_global_tracker().lock() {
        Ok(mut tracker) => {
            tracker.remember(PendingRegisteredGlobal {
                func_addr,
                upvalues,
                origin,
            });
        }
        Err(_) => {
            log_line("registered global tracker mutex poisoned; dropping candidate");
        }
    }
}

/// Retrieves a pending registered-global candidate for the given function address.
fn take_registered_global_candidate(func_addr: usize) -> Option<PendingRegisteredGlobal> {
    match registered_global_tracker().lock() {
        Ok(mut tracker) => tracker.take(func_addr),
        Err(_) => {
            log_line("registered global tracker mutex poisoned; skipping candidate lookup");
            None
        }
    }
}
/// Singleton accessor for the push event tracker ring buffer.
fn push_event_tracker() -> &'static Mutex<PushEventTracker> {
    PUSH_EVENT_TRACKER.get_or_init(|| Mutex::new(PushEventTracker::new(PUSH_RING_CAPACITY)))
}

/// Singleton accessor for tracking recently pushed closures destined for globals.
fn registered_global_tracker() -> &'static Mutex<RegisteredGlobalTracker> {
    REGISTERED_GLOBAL_CANDIDATES
        .get_or_init(|| Mutex::new(RegisteredGlobalTracker::new(MAX_PENDING_REGISTERED_GLOBALS)))
}

/// Inspects a Lua handle into a structured value description.
fn describe_lua_value(handle: LuaObject) -> Option<ValueDetails> {
    if handle == 0 {
        return Some(ValueDetails::Nil);
    }
    let tag = call_real_lua_tag(handle);
    if call_real_lua_isnumber(handle) {
        if let Some(value) = call_real_lua_getnumber(handle) {
            return Some(ValueDetails::Number(value));
        }
    }
    if call_real_lua_isstring(handle) {
        if let Some(value) = call_real_lua_getstring(handle) {
            return Some(ValueDetails::String(value));
        }
    }
    if call_real_lua_istable(handle) {
        return Some(ValueDetails::Table { tag });
    }
    if call_real_lua_iscfunction(handle) {
        let func = call_real_lua_getcfunction(handle).map(|f| f as *const c_void as usize);
        return Some(ValueDetails::CFunction { tag, func });
    }
    if call_real_lua_isfunction(handle) {
        return Some(ValueDetails::Function { tag });
    }
    if call_real_lua_isuserdata(handle) {
        return Some(ValueDetails::Userdata { tag });
    }
    Some(ValueDetails::Unknown { tag })
}

/// Converts a value description into telemetry-ready `ValueFields`.
fn value_fields_from_details(value: &ValueDetails) -> ValueFields {
    value_fields_from_meta(&value_meta_from_details(value))
}

/// Converts a value description into a compact upvalue preview.
fn upvalue_preview_from_details(value: &ValueDetails) -> UpvaluePreview {
    upvalue_preview_from_meta(&value_meta_from_details(value))
}

fn value_meta_from_details(value: &ValueDetails) -> ValueMeta {
    match value {
        ValueDetails::Number(value) => ValueMeta {
            kind: ValueType::Number,
            value: Some(format_number_for_log(*value)),
            ..ValueMeta::default()
        },
        ValueDetails::String(text) => ValueMeta {
            kind: ValueType::String,
            value_len: Some(text.len()),
            preview: Some(truncate_for_log(text, LOG_PREVIEW_MAX_LEN)),
            ..ValueMeta::default()
        },
        ValueDetails::Nil => ValueMeta {
            kind: ValueType::Nil,
            ..ValueMeta::default()
        },
        ValueDetails::Table { tag } => ValueMeta {
            kind: ValueType::Table,
            tag: *tag,
            ..ValueMeta::default()
        },
        ValueDetails::Function { tag } => ValueMeta {
            kind: ValueType::Function,
            tag: *tag,
            ..ValueMeta::default()
        },
        ValueDetails::CFunction { tag, func } => ValueMeta {
            kind: ValueType::Cfunction,
            tag: *tag,
            func: func.map(|addr| handle_hex(addr)),
            ..ValueMeta::default()
        },
        ValueDetails::Userdata { tag } => ValueMeta {
            kind: ValueType::Userdata,
            tag: *tag,
            ..ValueMeta::default()
        },
        ValueDetails::Unknown { tag } => ValueMeta {
            kind: ValueType::Unknown,
            tag: *tag,
            ..ValueMeta::default()
        },
    }
}

enum ValueDetails {
    Number(f64),
    String(String),
    Nil,
    Table {
        tag: Option<c_int>,
    },
    Function {
        tag: Option<c_int>,
    },
    CFunction {
        tag: Option<c_int>,
        func: Option<usize>,
    },
    Userdata {
        tag: Option<c_int>,
    },
    Unknown {
        tag: Option<c_int>,
    },
}

/// Normalizes optional origin metadata into telemetry fields.
fn origin_fields(origin: Option<&ClosureOrigin>) -> OriginFields {
    let mut fields = OriginFields::default();
    if let Some(origin) = origin {
        fields.origin = Some(handle_hex(origin.func_addr));
        if let Some(module) = &origin.module {
            fields.module = Some(module.clone());
        }
        let mut map_symbol_used = false;
        if let Some(symbol) = &origin.symbol {
            fields.symbol = Some(symbol.clone());
        }
        if let Some(map_symbol) = &origin.map_symbol {
            if fields.symbol.is_none() {
                fields.symbol = Some(render_map_symbol(map_symbol));
                map_symbol_used = true;
            }
        }
        if map_symbol_used {
            fields.symbol_source = Some("map".to_string());
        }
    }
    fields
}

/// Captures the immediate non-shim caller using a backtrace and resolves it to module/symbol info.
fn caller_origin_fields() -> OriginFields {
    caller_origin_details(should_skip_caller_frame)
        .map(|details| origin_fields_with_map(&details))
        .unwrap_or_default()
}

/// Builds origin fields from a frame address and its resolved module/symbol details.
fn origin_fields_with_map(
    details: &grim_telemetry_common::trace_utils::ClosureDetails,
) -> OriginFields {
    let mut fields = origin_fields_from_details(details);
    if fields.symbol.is_none() {
        if let Some(map_symbol) = lookup_symbol_from_map(
            details.address,
            details.module.as_deref(),
            details.module_base,
        ) {
            fields.symbol = Some(render_symbol_with_offset(
                &map_symbol.name,
                map_symbol.distance,
            ));
            fields.symbol_source = Some("map".to_string());
        }
    }
    fields
}

/// Filters out frames from the shim or libc so we attribute the caller to retail code.
fn should_skip_caller_frame(module_path: Option<&str>, _symbol: Option<&str>) -> bool {
    common_should_skip_caller_frame(module_path, _symbol, |module_path, _| {
        module_path
            .map(|path| path.to_ascii_lowercase().contains("libgrim_analysis"))
            .unwrap_or(false)
    })
}

struct PushEventTracker {
    pushes: VecDeque<TrackedPush>,
    capacity: usize,
}

impl PushEventTracker {
    /// Creates a ring buffer for recent push operations.
    fn new(capacity: usize) -> Self {
        Self {
            pushes: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Records a push event with its sequence and optional handle.
    fn record_push(&mut self, log_seq: u64, preview: UpvaluePreview, handle: Option<LuaObject>) {
        if self.pushes.len() == self.capacity {
            self.pushes.pop_front();
        }
        self.pushes.push_back(TrackedPush {
            log_seq,
            preview,
            handle,
        });
    }

    /// Clears tracked pushes when a non-push event happens.
    fn record_non_push(&mut self) {
        self.pushes.clear();
    }

    /// Returns the `count` most recent pushes if available.
    fn snapshot_recent(&self, count: usize) -> Option<Vec<TrackedPush>> {
        if count == 0 {
            return Some(Vec::new());
        }
        if self.pushes.len() < count {
            return None;
        }
        let start = self.pushes.len() - count;
        Some(self.pushes.iter().skip(start).cloned().collect())
    }

    /// Empties the buffer of tracked pushes.
    fn clear(&mut self) {
        self.pushes.clear();
    }
}

#[derive(Clone)]
struct TrackedPush {
    log_seq: u64,
    preview: UpvaluePreview,
    handle: Option<LuaObject>,
}

struct PendingRegisteredGlobal {
    func_addr: usize,
    upvalues: c_int,
    origin: Option<ClosureOrigin>,
}

struct RegisteredGlobalTracker {
    pending: HashMap<usize, VecDeque<PendingRegisteredGlobal>>,
    max_per_func: usize,
}

impl RegisteredGlobalTracker {
    /// Creates a bounded tracker for recent registered-global candidates.
    fn new(max_per_func: usize) -> Self {
        Self {
            pending: HashMap::new(),
            max_per_func,
        }
    }

    /// Records a pushed closure as a potential future global binding.
    fn remember(&mut self, candidate: PendingRegisteredGlobal) {
        let queue = self.pending.entry(candidate.func_addr).or_default();
        queue.push_back(candidate);
        if queue.len() > self.max_per_func {
            queue.pop_front();
        }
    }

    /// Retrieves the most recent candidate for a given function address, pruning when empty.
    fn take(&mut self, func_addr: usize) -> Option<PendingRegisteredGlobal> {
        let candidate = self
            .pending
            .get_mut(&func_addr)
            .and_then(|queue| queue.pop_back());
        let should_remove = self
            .pending
            .get(&func_addr)
            .map(|queue| queue.is_empty())
            .unwrap_or(false);
        if should_remove {
            self.pending.remove(&func_addr);
        }
        candidate
    }
}

struct CallfunctionTracker {
    counts: HashMap<LuaObject, u64>,
    labels: HashMap<LuaObject, String>,
    origins: HashMap<LuaObject, ClosureOrigin>,
    ref_meta: RefHandleTracker,
}

impl CallfunctionTracker {
    // Provides shared labeling/origin metadata across call/ref/global hooks so we only
    // resolve expensive symbols once and can keep per-handle call counts.
    /// Initializes trackers for call counts, labels, and origins.
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
            labels: HashMap::new(),
            origins: HashMap::new(),
            ref_meta: RefHandleTracker::new(),
        }
    }

    /// Overrides any existing label for a handle.
    fn remember_label<S: Into<String>>(&mut self, handle: LuaObject, label: S) {
        self.labels.insert(handle, label.into());
    }

    /// Records a label only if one is not already present.
    fn remember_label_if_missing<S: Into<String>>(&mut self, handle: LuaObject, label: S) {
        self.labels.entry(handle).or_insert_with(|| label.into());
    }

    /// Records the origin metadata for a handle.
    fn remember_origin(&mut self, handle: LuaObject, origin: ClosureOrigin) {
        self.origins.insert(handle, origin);
    }

    /// Records origin metadata only if none was cached.
    fn remember_origin_if_missing(&mut self, handle: LuaObject, origin: ClosureOrigin) {
        self.origins.entry(handle).or_insert(origin);
    }

    /// Fetches any cached label for a handle.
    fn label_for(&self, handle: LuaObject) -> Option<String> {
        self.labels.get(&handle).cloned()
    }

    /// Fetches any cached origin for a handle.
    fn origin_for(&self, handle: LuaObject) -> Option<ClosureOrigin> {
        self.origins.get(&handle).cloned()
    }

    /// Increments call counts for the handle and returns the latest sample.
    fn record(&mut self, handle: LuaObject, label: &str) -> CallSample {
        let count = {
            let entry = self.counts.entry(handle).or_insert(0);
            *entry += 1;
            *entry
        };
        self.labels
            .entry(handle)
            .or_insert_with(|| label.to_string());
        let origin = self.origin_for(handle);
        let ref_meta = self.ref_meta.meta_for(handle);
        CallSample {
            count,
            origin,
            ref_meta,
        }
    }

    fn remember_ref_meta(&mut self, handle: LuaObject, meta: RefHandleMeta) {
        self.ref_meta.remember(handle, meta);
    }
}

struct CallSample {
    count: u64,
    origin: Option<ClosureOrigin>,
    ref_meta: Option<RefHandleMeta>,
}

struct GlobalAccessTracker {
    counts: HashMap<String, u64>,
}

impl GlobalAccessTracker {
    /// Creates a tracker for global access counts.
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    /// Increments and returns the access count for a global label.
    fn record(&mut self, label: &str) -> u64 {
        let count = {
            let entry = self.counts.entry(label.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        count
    }
}

struct HandleLabelTracker {
    labels: HashMap<LuaObject, String>,
}

impl HandleLabelTracker {
    /// Creates an empty handle-to-label map.
    fn new() -> Self {
        Self {
            labels: HashMap::new(),
        }
    }

    /// Records or overwrites a label for the given handle.
    fn remember_label(&mut self, handle: LuaObject, label: String) {
        self.labels.insert(handle, label);
    }

    /// Records a label if one does not already exist.
    fn remember_label_if_missing(&mut self, handle: LuaObject, label: String) {
        self.labels.entry(handle).or_insert(label);
    }

    /// Fetches the label for a handle if available.
    fn label_for(&self, handle: LuaObject) -> Option<String> {
        self.labels.get(&handle).cloned()
    }
}

struct RefAliasTracker {
    pending_alias: Option<String>,
    batch: Option<RefBatch>,
}

impl RefAliasTracker {
    fn new() -> Self {
        Self {
            pending_alias: None,
            batch: None,
        }
    }

    fn remember_alias(&mut self, alias: String) {
        self.pending_alias = Some(alias);
    }

    fn take_alias(&mut self) -> Option<String> {
        self.pending_alias.take()
    }

    fn clear_alias(&mut self) {
        self.pending_alias = None;
    }

    fn record_batch(&mut self, alias: Option<&str>, reference: i32) -> Option<RefBatch> {
        let Some(kind) = alias.map(ref_alias_kind) else {
            return self.flush_batch();
        };
        if let Some(batch) = self.batch.as_mut() {
            if batch.kind == kind && reference == batch.last_ref + 1 {
                batch.count += 1;
                batch.last_ref = reference;
                return None;
            }
        }
        let previous = self.batch.take();
        self.batch = Some(RefBatch {
            kind,
            start_ref: reference,
            last_ref: reference,
            count: 1,
        });
        previous
    }

    fn flush_batch(&mut self) -> Option<RefBatch> {
        self.batch.take()
    }
}

struct RefBatch {
    kind: String,
    start_ref: i32,
    last_ref: i32,
    count: u32,
}

impl RefBatch {
    fn into_event(self) -> Option<LuaSemanticEvent> {
        if self.count > 1 {
            Some(LuaSemanticEvent::SemanticRefBatch {
                kind: self.kind,
                count: self.count,
                start_ref: self.start_ref,
            })
        } else {
            None
        }
    }
}

/// Produces a human-readable label for a Lua function handle using caches and `getobjname`.
fn resolve_lua_function_label(handle: LuaObject) -> String {
    // Prefer cached labels/origins from prior binds/refs, falling back to Lua's
    // getobjname and finally a hex handle to ensure every function log has a label.
    if let Ok(tracker) = callfunction_tracker().lock() {
        if let Some(label) = tracker.label_for(handle) {
            return label;
        }
    }

    if let Some((kind, name)) = call_real_lua_getobjname(handle) {
        match (kind.as_deref(), name.as_deref()) {
            (Some(kind), Some(name)) if !kind.is_empty() => {
                return format!("{kind}:{name}");
            }
            (_, Some(name)) => {
                return format!("function:{name}");
            }
            (Some(kind), None) if !kind.is_empty() => {
                return format!("{kind} handle={}", handle_hex(handle as usize));
            }
            _ => {}
        }
    }
    format!("handle={}", handle_hex(handle as usize))
}

/// Singleton accessor for the callfunction tracker.
fn callfunction_tracker() -> &'static Mutex<CallfunctionTracker> {
    CALLFUNCTION_TRACKER.get_or_init(|| Mutex::new(CallfunctionTracker::new()))
}

/// Singleton accessor for the global access tracker.
fn global_access_tracker() -> &'static Mutex<GlobalAccessTracker> {
    GLOBAL_ACCESS_TRACKER.get_or_init(|| Mutex::new(GlobalAccessTracker::new()))
}

/// Singleton accessor for the handle label tracker.
fn handle_label_tracker() -> &'static Mutex<HandleLabelTracker> {
    HANDLE_LABELS.get_or_init(|| Mutex::new(HandleLabelTracker::new()))
}

/// Stores a label for the handle unless the handle is null, logging on mutex failure.
fn remember_handle_label(handle: LuaObject, label: impl Into<String>) {
    if handle == 0 {
        return;
    }
    if let Ok(mut tracker) = handle_label_tracker().lock() {
        tracker.remember_label(handle, label.into());
    } else {
        log_line("handle label tracker mutex poisoned; skipping label update");
    }
}

/// Stores a label only if one is not already present for the handle.
fn remember_handle_label_if_missing(handle: LuaObject, label: impl Into<String>) {
    if handle == 0 {
        return;
    }
    if let Ok(mut tracker) = handle_label_tracker().lock() {
        tracker.remember_label_if_missing(handle, label.into());
    } else {
        log_line("handle label tracker mutex poisoned; skipping label update");
    }
}

/// Fetches a label for the handle if one was recorded.
fn handle_label_for(handle: LuaObject) -> Option<String> {
    if handle == 0 {
        return None;
    }
    handle_label_tracker()
        .lock()
        .ok()
        .and_then(|tracker| tracker.label_for(handle))
}

#[derive(Clone)]
struct RefHandleMeta {
    ref_id: i32,
    alias: Option<String>,
    value_kind: Option<ValueType>,
}

struct RefHandleTracker {
    entries: HashMap<LuaObject, RefHandleMeta>,
}

impl RefHandleTracker {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn remember(&mut self, handle: LuaObject, meta: RefHandleMeta) {
        self.entries.insert(handle, meta);
    }

    fn meta_for(&self, handle: LuaObject) -> Option<RefHandleMeta> {
        self.entries.get(&handle).cloned()
    }
}

fn ref_alias_tracker() -> &'static Mutex<RefAliasTracker> {
    REF_ALIAS_TRACKER.get_or_init(|| Mutex::new(RefAliasTracker::new()))
}

fn remember_ref_alias_candidate(alias: impl Into<String>) {
    let alias = alias.into();
    if alias.is_empty() {
        return;
    }
    if let Ok(mut tracker) = ref_alias_tracker().lock() {
        tracker.remember_alias(alias);
    } else {
        log_line("ref alias tracker mutex poisoned; dropping alias candidate");
    }
}

fn take_ref_alias_candidate() -> Option<String> {
    ref_alias_tracker()
        .lock()
        .ok()
        .and_then(|mut tracker| tracker.take_alias())
}

fn record_ref_batch(alias: Option<&str>, reference: i32) {
    ensure_ref_batch_exit_flush_registered();
    if let Ok(mut tracker) = ref_alias_tracker().lock() {
        let ended = tracker.record_batch(alias, reference);
        if let Some(event) = ended.and_then(|batch| batch.into_event()) {
            log_semantic_event(event);
        }
    } else {
        log_line("ref alias tracker mutex poisoned; skipping batch record");
    }
}

fn flush_ref_batch_event() {
    if let Ok(mut tracker) = ref_alias_tracker().lock() {
        if let Some(event) = tracker.flush_batch().and_then(|batch| batch.into_event()) {
            log_semantic_event(event);
        }
    } else {
        log_line("ref alias tracker mutex poisoned; skipping batch flush");
    }
}

fn ref_alias_kind(alias: &str) -> String {
    alias
        .split_once(':')
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_else(|| alias.to_string())
}

fn ensure_ref_batch_exit_flush_registered() {
    REF_BATCH_EXIT_FLUSH.get_or_init(|| unsafe {
        // Best-effort flush so the last batch is emitted even if no later non-push occurs.
        libc::atexit(flush_ref_batch_atexit);
    });
}

extern "C" fn flush_ref_batch_atexit() {
    flush_ref_batch_event();
}

#[derive(Clone)]
struct ClosureOrigin {
    func_addr: usize,
    module: Option<String>,
    symbol: Option<String>,
    map_symbol: Option<MapSymbol>,
}

impl ClosureOrigin {
    /// Builds origin metadata for a closure pointer, including module and symbol map info.
    fn new(ptr: *const c_void) -> Self {
        let details = describe_closure_target(ptr);
        let map_symbol =
            lookup_symbol_from_map(ptr as usize, details.module.as_deref(), details.module_base)
                .map(|hit| MapSymbol {
                    name: hit.name,
                    distance: hit.distance,
                });
        Self {
            func_addr: ptr as usize,
            module: details.module,
            symbol: details.symbol,
            map_symbol,
        }
    }
}

#[derive(Clone)]
struct MapSymbol {
    name: String,
    distance: usize,
}

/// Renders a symbol name with an offset suffix when present.
fn render_symbol_with_offset(name: &str, distance: usize) -> String {
    if distance == 0 {
        name.to_string()
    } else {
        format!("{name}+0x{distance:x}")
    }
}

/// Formats a `MapSymbol` for logging consistency.
fn render_map_symbol(map_symbol: &MapSymbol) -> String {
    render_symbol_with_offset(&map_symbol.name, map_symbol.distance)
}

/// Emits a structured semantic event through the logging layer.
fn log_semantic_event(event: LuaSemanticEvent) {
    crate::logging::log_event(event);
}

/// Emits a structured event and returns its sequence number for correlation.
fn log_event_with_seq(event: LuaEvent) -> u64 {
    crate::logging::log_event_with_seq(event)
}
