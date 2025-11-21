use crate::{
    logging::log_line,
    lua_api::{
        call_real_lua_call, call_real_lua_callfunction, call_real_lua_dobuffer,
        call_real_lua_dofile, call_real_lua_dostring, call_real_lua_getcfunction,
        call_real_lua_getglobal, call_real_lua_getobjname, call_real_lua_getref,
        call_real_lua_push_c_closure, call_real_lua_ref, call_real_lua_setglobal,
        call_real_lua_settagmethod, LuaCFunction, LuaObject,
    },
    symbol_map::lookup_symbol_from_map,
};
use libc::{c_char, c_int, size_t, Dl_info};
use std::{
    collections::HashMap,
    env,
    ffi::{c_void, CStr, CString},
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

static CLOSURE_PUSH_COUNTER: AtomicU64 = AtomicU64::new(0);
static CALLFUNCTION_TRACKER: OnceLock<Mutex<CallfunctionTracker>> = OnceLock::new();
static GLOBAL_ACCESS_TRACKER: OnceLock<Mutex<GlobalAccessTracker>> = OnceLock::new();

pub(crate) unsafe fn trace_lua_push_closure(label: &str, func: LuaCFunction, upvalues: c_int) {
    let sequence = CLOSURE_PUSH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let func_addr = func as *const c_void as usize;
    let origin = ClosureOrigin::new(func as *const c_void);
    let origin_fragment = format_origin_fragment(Some(&origin));

    log_line(&format!(
        "event=push_cclosure name={label} seq={sequence:06} func=0x{func_addr:08x} upvalues={upvalues}{origin_fragment}"
    ));

    if !call_real_lua_push_c_closure(func, upvalues) {
        log_line("unable to forward lua_pushCclosure call; retail VM may misbehave");
    }
}

pub(crate) unsafe fn trace_lua_dofile(path: *const c_char) -> c_int {
    let label = cstr_opt(path).unwrap_or_else(|| "<null>".to_string());
    log_line(&format!("event=dofile path={label}"));
    forward_int_result("lua_dofile", call_real_lua_dofile(path))
}

pub(crate) unsafe fn trace_lua_dostring(chunk: *const c_char) -> c_int {
    let snippet = cstr_opt(chunk)
        .map(|s| truncate_for_log(&s, 80))
        .unwrap_or_else(|| "<null>".to_string());
    log_line(&format!("event=dostring snippet=\"{snippet}\""));
    forward_int_result("lua_dostring", call_real_lua_dostring(chunk))
}

pub(crate) unsafe fn trace_lua_dobuffer(
    buffer: *const c_char,
    size: size_t,
    name: *const c_char,
) -> c_int {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    log_line(&format!("event=dobuffer name={label} size={size}"));
    forward_int_result("lua_dobuffer", call_real_lua_dobuffer(buffer, size, name))
}

pub(crate) unsafe fn trace_lua_call(name: *const c_char) -> c_int {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());
    log_line(&format!("event=call name={label}"));
    forward_int_result("lua_call", call_real_lua_call(name))
}

pub(crate) unsafe fn trace_lua_setglobal(name: *const c_char) {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    if call_real_lua_setglobal(name) {
        if let Some(handle) = call_real_lua_getglobal(name) {
            let origin = call_real_lua_getcfunction(handle)
                .map(|func| ClosureOrigin::new(func as *const c_void));

            if let Ok(mut tracker) = callfunction_tracker().lock() {
                tracker.remember_label(handle, format!("global:{label}"));
                if let Some(origin) = origin.clone() {
                    tracker.remember_origin(handle, origin);
                }
            } else {
                log_line("lua_setglobal tracker mutex poisoned; skipping cache update");
            }

            log_line(&format!(
                "event=bind_global name={label} handle=0x{handle:08x} label=global:{label}{origin_fragment}",
                origin_fragment = format_origin_fragment(origin.as_ref())
            ));
        }
    }
}

pub(crate) unsafe fn trace_lua_getglobal(name: *const c_char) -> LuaObject {
    let label = cstr_opt(name).unwrap_or_else(|| "<null>".to_string());

    let handle = match call_real_lua_getglobal(name) {
        Some(handle) => handle,
        None => {
            log_line("lua_getglobal symbol missing; returning null handle");
            return 0;
        }
    };

    if let Ok(mut tracker) = global_access_tracker().lock() {
        if let Some(count) = tracker.record(&label) {
            log_line(&format!(
                "event=get_global name={label} handle=0x{handle:08x} label=global:{label} count={count}"
            ));
        }
    } else {
        log_line("lua_getglobal tracker mutex poisoned; skipping access log");
    }

    handle
}

pub(crate) unsafe fn trace_lua_ref(lock: c_int) -> c_int {
    match call_real_lua_ref(lock) {
        Some(reference) => {
            let handle = call_real_lua_getref(reference);
            match handle {
                Some(handle) => {
                    let label = resolve_lua_function_label(handle);
                    let origin = call_real_lua_getcfunction(handle)
                        .map(|func| ClosureOrigin::new(func as *const c_void));
                    if let Ok(mut tracker) = callfunction_tracker().lock() {
                        tracker.remember_label_if_missing(handle, format!("ref:{reference}"));
                        if let Some(origin) = origin.clone() {
                            tracker.remember_origin_if_missing(handle, origin);
                        }
                    } else {
                        log_line("lua_ref tracker mutex poisoned; skipping cache update");
                    }
                    log_line(&format!(
                        "event=store_ref lock={lock} ref={reference} handle=0x{handle:08x} label={label}{origin_fragment}",
                        origin_fragment = format_origin_fragment(origin.as_ref())
                    ));
                }
                None => {
                    log_line(&format!(
                        "event=store_ref lock={lock} ref={reference} handle=<unknown> label=ref:{reference} note=lua_getref_missing"
                    ));
                }
            }
            reference
        }
        None => {
            log_line("lua_ref symbol missing; returning failure to keep engine alive");
            -1
        }
    }
}

pub(crate) unsafe fn trace_lua_getref(reference: c_int) -> LuaObject {
    match call_real_lua_getref(reference) {
        Some(handle) => {
            let label = resolve_lua_function_label(handle);
            let origin = call_real_lua_getcfunction(handle)
                .map(|func| ClosureOrigin::new(func as *const c_void));
            if let Ok(mut tracker) = callfunction_tracker().lock() {
                tracker.remember_label_if_missing(handle, format!("ref:{reference}"));
                if let Some(origin) = origin.clone() {
                    tracker.remember_origin_if_missing(handle, origin);
                }
            } else {
                log_line("lua_getref tracker mutex poisoned; skipping cache update");
            }
            log_line(&format!(
                "event=fetch_ref ref={reference} handle=0x{handle:08x} label={label}{origin_fragment}",
                origin_fragment = format_origin_fragment(origin.as_ref())
            ));
            handle
        }
        None => {
            log_line(&format!(
                "event=fetch_ref ref={reference} handle=<unknown> note=lua_getref_symbol_missing"
            ));
            0
        }
    }
}

pub(crate) unsafe fn trace_lua_settagmethod(tag: c_int, event: *const c_char) {
    let event_label = cstr_opt(event).unwrap_or_else(|| "<null>".to_string());
    if call_real_lua_settagmethod(tag, event) {
        log_line(&format!(
            "event=set_tagmethod tag={tag} event_name={event_label}"
        ));
    }
}

pub(crate) unsafe fn trace_lua_callfunction(func: *mut c_void) -> c_int {
    let handle = func as usize as LuaObject;
    let label = resolve_lua_function_label(handle);

    if let Ok(mut tracker) = callfunction_tracker().lock() {
        if let Some(sample) = tracker.record(handle, &label) {
            log_line(&format!(
                "event=call_func handle=0x{handle:08x} label={label} calls={count}{origin}",
                count = sample.count,
                origin = format_origin_fragment(sample.origin.as_ref())
            ));
        }
    } else {
        log_line("lua_callfunction tracker mutex poisoned; falling back to minimal log");
        log_line(&format!(
            "event=call_func handle=0x{handle:08x} label={label} note=tracker_poisoned"
        ));
    }

    forward_int_result("lua_callfunction", call_real_lua_callfunction(handle))
}

fn forward_int_result(label: &str, result: Option<c_int>) -> c_int {
    match result {
        Some(value) => value,
        None => {
            log_line(&format!(
                "{label} symbol missing; returning failure to keep engine alive"
            ));
            -1
        }
    }
}

fn truncate_for_log(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut truncated = text[..max_len].to_string();
    truncated.push_str("...");
    truncated
}

struct ClosureDetails {
    module: Option<String>,
    module_base: Option<usize>,
    symbol: Option<String>,
    demangled: Option<String>,
}

fn describe_closure_target(ptr: *const c_void) -> ClosureDetails {
    unsafe {
        let mut info = MaybeUninit::<Dl_info>::zeroed();
        if libc::dladdr(ptr, info.as_mut_ptr()) == 0 {
            return ClosureDetails {
                module: None,
                module_base: None,
                symbol: None,
                demangled: None,
            };
        }
        let info = info.assume_init();
        let module = cstr_opt(info.dli_fname);
        let module_base = if info.dli_fbase.is_null() {
            None
        } else {
            Some(info.dli_fbase as usize)
        };
        let symbol = cstr_opt(info.dli_sname);
        let demangled = symbol
            .as_ref()
            .and_then(|symbol| demangle_symbol(symbol.as_str()));
        ClosureDetails {
            module,
            module_base,
            symbol,
            demangled,
        }
    }
}

unsafe fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

fn demangle_symbol(symbol: &str) -> Option<String> {
    // retail binaries are C++ and often expose mangled names; try to recover readable signatures
    let Ok(symbol) = CString::new(symbol) else {
        return None;
    };
    let mut status: c_int = 0;
    let ptr = unsafe {
        __cxa_demangle(
            symbol.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut status,
        )
    };
    if ptr.is_null() || status != 0 {
        return None;
    }
    let demangled = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
    unsafe {
        libc::free(ptr as *mut c_void);
    }
    Some(demangled)
}

extern "C" {
    fn __cxa_demangle(
        mangled_name: *const c_char,
        output_buffer: *mut c_char,
        length: *mut usize,
        status: *mut c_int,
    ) -> *mut c_char;
}

struct CallfunctionTracker {
    counts: HashMap<LuaObject, u64>,
    labels: HashMap<LuaObject, String>,
    origins: HashMap<LuaObject, ClosureOrigin>,
    verbose: bool,
}

impl CallfunctionTracker {
    fn new(verbose: bool) -> Self {
        Self {
            counts: HashMap::new(),
            labels: HashMap::new(),
            origins: HashMap::new(),
            verbose,
        }
    }

    fn remember_label<S: Into<String>>(&mut self, handle: LuaObject, label: S) {
        self.labels.insert(handle, label.into());
    }

    fn remember_label_if_missing<S: Into<String>>(&mut self, handle: LuaObject, label: S) {
        self.labels.entry(handle).or_insert_with(|| label.into());
    }

    fn remember_origin(&mut self, handle: LuaObject, origin: ClosureOrigin) {
        self.origins.insert(handle, origin);
    }

    fn remember_origin_if_missing(&mut self, handle: LuaObject, origin: ClosureOrigin) {
        self.origins.entry(handle).or_insert(origin);
    }

    fn label_for(&self, handle: LuaObject) -> Option<String> {
        self.labels.get(&handle).cloned()
    }

    fn origin_for(&self, handle: LuaObject) -> Option<ClosureOrigin> {
        self.origins.get(&handle).cloned()
    }

    fn record(&mut self, handle: LuaObject, label: &str) -> Option<CallSample> {
        let count = {
            let entry = self.counts.entry(handle).or_insert(0);
            *entry += 1;
            *entry
        };
        if !self.labels.contains_key(&handle) {
            self.labels.insert(handle, label.to_string());
        }
        let origin = self.origin_for(handle);
        if self.verbose || should_emit_sample(count) {
            Some(CallSample { count, origin })
        } else {
            None
        }
    }
}

fn should_emit_sample(count: u64) -> bool {
    matches!(count, 1 | 10 | 100 | 500 | 1_000 | 5_000) || count % 10_000 == 0
}

fn callfunction_verbose_enabled() -> bool {
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| env::var("GRIM_SHIM_CALLFUNCTION_VERBOSE").is_ok())
}

struct CallSample {
    count: u64,
    origin: Option<ClosureOrigin>,
}

struct GlobalAccessTracker {
    counts: HashMap<String, u64>,
    verbose: bool,
}

impl GlobalAccessTracker {
    fn new(verbose: bool) -> Self {
        Self {
            counts: HashMap::new(),
            verbose,
        }
    }

    fn record(&mut self, label: &str) -> Option<u64> {
        let count = {
            let entry = self.counts.entry(label.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        if self.verbose || should_emit_sample(count) {
            Some(count)
        } else {
            None
        }
    }
}

fn getglobal_verbose_enabled() -> bool {
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| env::var("GRIM_SHIM_GETGLOBAL_VERBOSE").is_ok())
}

fn resolve_lua_function_label(handle: LuaObject) -> String {
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
                return format!("{kind} handle=0x{handle:08x}");
            }
            _ => {}
        }
    }
    format!("handle=0x{handle:08x}")
}

fn callfunction_tracker() -> &'static Mutex<CallfunctionTracker> {
    CALLFUNCTION_TRACKER
        .get_or_init(|| Mutex::new(CallfunctionTracker::new(callfunction_verbose_enabled())))
}

fn global_access_tracker() -> &'static Mutex<GlobalAccessTracker> {
    GLOBAL_ACCESS_TRACKER
        .get_or_init(|| Mutex::new(GlobalAccessTracker::new(getglobal_verbose_enabled())))
}

fn format_origin_fragment(origin: Option<&ClosureOrigin>) -> String {
    origin
        .map(|origin| {
            let fields = origin.format_fields();
            if fields.is_empty() {
                String::new()
            } else {
                format!(" {fields}")
            }
        })
        .unwrap_or_default()
}

#[derive(Clone)]
struct ClosureOrigin {
    func_addr: usize,
    module: Option<String>,
    symbol: Option<String>,
    demangled: Option<String>,
    map_symbol: Option<MapSymbol>,
}

impl ClosureOrigin {
    fn new(ptr: *const c_void) -> Self {
        let details = describe_closure_target(ptr);
        let map_symbol =
            lookup_symbol_from_map(ptr as usize, details.module.as_deref(), details.module_base)
                .map(|hit| MapSymbol {
                    name: hit.name,
                    distance: hit.distance,
                    source_label: hit.source_label,
                });
        Self {
            func_addr: ptr as usize,
            module: details.module,
            symbol: details.symbol,
            demangled: details.demangled,
            map_symbol,
        }
    }

    fn format_fields(&self) -> String {
        let mut parts = Vec::new();
        parts.push(format!("origin=0x{addr:08x}", addr = self.func_addr));
        if let Some(module) = &self.module {
            parts.push(format!("module={module}"));
        }
        let mut has_symbol = false;
        if let Some(symbol) = &self.symbol {
            has_symbol = true;
            if let Some(demangled) = &self.demangled {
                parts.push(format!("symbol={symbol} ({demangled})"));
            } else {
                parts.push(format!("symbol={symbol}"));
            }
        }
        if let Some(map_symbol) = &self.map_symbol {
            if !has_symbol {
                let mut field = format!("symbol={}", map_symbol.name);
                if map_symbol.distance > 0 {
                    field.push_str(&format!("+0x{delta:x}", delta = map_symbol.distance));
                }
                if let Some(source) = &map_symbol.source_label {
                    field.push_str(&format!(" symbol_source={source}"));
                } else {
                    field.push_str(" symbol_source=map");
                }
                parts.push(field);
            } else if let Some(source) = &map_symbol.source_label {
                parts.push(format!("map_source={source}"));
            }
        }
        parts.join(" ")
    }
}

#[derive(Clone)]
struct MapSymbol {
    name: String,
    distance: usize,
    source_label: Option<String>,
}
