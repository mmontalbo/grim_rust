mod context;
mod legacy_lua;
mod telemetry;

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grim_telemetry_common::EventBuilder;
use mlua::{Lua, LuaOptions, StdLib, Table, Value};

pub fn log_engine_exit(status: &str, note: Option<&str>) {
    telemetry::log_engine_exit(status, note);
}

fn run_phase<T>(name: &str, phase: impl FnOnce() -> Result<T>) -> Result<T> {
    let start = Instant::now();
    telemetry::log_event(
        EventBuilder::new("engine_boot_phase")
            .kv("phase", name)
            .kv("status", "start"),
    );
    let result = phase();
    let status = if result.is_ok() { "ok" } else { "error" };
    telemetry::log_event(
        EventBuilder::new("engine_boot_phase")
            .kv("phase", name)
            .kv("status", status)
            .kv("elapsed_ms", start.elapsed().as_millis() as i64),
    );
    result
}

pub fn run_boot_sequence(data_root: &Path, verbose: bool, headless: bool) -> Result<EngineRuntime> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(context::EngineContext::new(verbose, headless)));

    let setup_result: Result<()> = (|| {
        run_phase("package_path", || {
            context::install_package_path(&lua, data_root)
        })?;
        run_phase("globals_pre_system", || {
            context::install_globals_pre_system(&lua, data_root, context.clone())
        })?;
        run_phase("load_system_script", || {
            context::load_system_script(&lua, data_root)
        })?;
        run_phase("globals_post_system", || {
            context::install_globals_post_system(&lua, context.clone())
        })?;
        run_phase("boot_overrides", || {
            context::override_boot_stubs(&lua, context.clone())
        })?;
        run_phase("boot_call", || context::call_boot(&lua, context.clone()))?;
        run_phase("drive_active_scripts_initial", || {
            context::drive_active_scripts(&lua, context.clone(), 8, 128)?;
            Ok(())
        })?;
        run_phase("ensure_intro_cutscene", || {
            if context::ensure_intro_cutscene(&lua, context.clone(), false)? {
                context::drive_active_scripts(&lua, context.clone(), 16, 128)?;
            }
            Ok(())
        })?;
        Ok(())
    })();
    if verbose {
        if let Err(err) = scan_parent_cycles(&lua) {
            eprintln!("[grim_engine][scan_parent_cycles] {err}");
        }
    }
    setup_result?;

    let snapshot = context.borrow();
    context::dump_runtime_summary(&snapshot);
    let initial_event_cursor = snapshot.events().len();
    drop(snapshot);

    Ok(EngineRuntime::new(
        lua,
        context,
        headless,
        initial_event_cursor,
    ))
}

/// Drives the embedded Lua runtime until the intro movie finishes.
pub struct EngineRuntime {
    lua: Lua,
    context: Rc<RefCell<context::EngineContext>>,
    headless: bool,
    frame: u32,
    intro_started: bool,
    event_cursor: usize,
    intro_movie_active: bool,
    intro_finished: bool,
}

impl EngineRuntime {
    fn new(
        lua: Lua,
        context: Rc<RefCell<context::EngineContext>>,
        headless: bool,
        initial_event_cursor: usize,
    ) -> Self {
        let intro_movie_active = {
            let ctx = context.borrow();
            ctx.active_fullscreen_movie()
                .as_deref()
                .map(|name| name.eq_ignore_ascii_case("intro"))
                .unwrap_or(false)
        };
        Self {
            lua,
            context,
            headless,
            frame: 0,
            intro_started: intro_movie_active,
            intro_movie_active,
            intro_finished: !intro_movie_active,
            event_cursor: initial_event_cursor,
        }
    }

    pub fn run(mut self) -> Result<()> {
        const FRAME_DURATION: Duration = Duration::from_millis(33);

        loop {
            let tick_start = Instant::now();
            context::drive_active_scripts(&self.lua, self.context.clone(), 8, 128)?;
            self.frame = self.frame.wrapping_add(1);
            self.progress_movies();
            self.flush_new_events();

            if self.intro_finished {
                break;
            }

            let elapsed = tick_start.elapsed();
            if elapsed < FRAME_DURATION {
                thread::sleep(FRAME_DURATION - elapsed);
            }
        }
        Ok(())
    }

    fn progress_movies(&mut self) {
        use context::MovieStep;

        let step = {
            let mut ctx = self.context.borrow_mut();
            ctx.step_fullscreen_movie()
        };

        match step {
            MovieStep::Idle => {
                self.intro_movie_active = false;
                if !self.intro_started {
                    self.intro_finished = true;
                }
            }
            MovieStep::Active(name) => {
                let intro_active = name.eq_ignore_ascii_case("intro");
                self.intro_started |= intro_active;
                self.intro_movie_active = intro_active;
            }
            MovieStep::Finished(name) => {
                let intro_done = name.eq_ignore_ascii_case("intro");
                self.intro_started |= intro_done;
                self.intro_movie_active = false;
                self.intro_finished = true;
            }
        }
    }

    fn flush_new_events(&mut self) {
        let (new_events, new_cursor) = {
            let ctx = self.context.borrow();
            let events = ctx.events();
            let new_cursor = events.len();
            let slice = if self.event_cursor < events.len() {
                events[self.event_cursor..].to_vec()
            } else {
                Vec::new()
            };
            (slice, new_cursor)
        };

        self.event_cursor = new_cursor;

        if self.headless {
            for event in new_events {
                println!("[grim_engine][headless] {event}");
            }
        }
    }
}

const PARENT_SCAN_TABLE_LIMIT: usize = 4096;
const PARENT_SCAN_CHAIN_LIMIT: usize = 128;

fn scan_parent_cycles(lua: &Lua) -> mlua::Result<()> {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut reported = HashSet::new();
    let globals = lua.globals();
    telemetry::register_table_label(globals.to_pointer(), "global:_G");
    queue.push_back((globals, Some("global:_G".to_string())));

    while let Some((table, label)) = queue.pop_front() {
        if visited.len() >= PARENT_SCAN_TABLE_LIMIT {
            break;
        }
        let ptr = table.to_pointer() as usize;
        if !visited.insert(ptr) {
            continue;
        }
        if let Some(ref name) = label {
            telemetry::register_table_label(table.to_pointer(), name.clone());
        }
        if let Some(chain) = find_parent_cycle(&table, label.as_deref(), &mut reported)? {
            log_parent_cycle(chain);
        }

        for pair in table.clone().pairs::<Value, Value>() {
            let Ok((key, value)) = pair else { continue };
            if let Value::Table(child) = value {
                let child_label = derive_child_label(label.as_deref(), &key);
                if let Some(ref name) = child_label {
                    telemetry::register_table_label(child.to_pointer(), name.clone());
                }
                queue.push_back((child, child_label.or_else(|| label.clone())));
            }
            if visited.len() >= PARENT_SCAN_TABLE_LIMIT {
                break;
            }
        }
    }
    Ok(())
}

fn find_parent_cycle(
    table: &Table,
    label: Option<&str>,
    reported: &mut HashSet<usize>,
) -> mlua::Result<Option<Vec<(usize, String, Option<String>)>>> {
    let mut chain: Vec<(usize, String, Option<String>)> = Vec::new();
    let mut seen = HashSet::new();
    let mut current = table.clone();
    let mut current_label = label.map(str::to_string);

    for _ in 0..PARENT_SCAN_CHAIN_LIMIT {
        let info = describe_table(&current, current_label.as_deref());
        if !seen.insert(info.0) {
            if reported.contains(&info.0) {
                return Ok(None);
            }
            for entry in &chain {
                reported.insert(entry.0);
            }
            reported.insert(info.0);
            chain.push(info);
            return Ok(Some(chain));
        }
        current_label = info.2.clone();
        chain.push(info);
        match current.raw_get::<_, Value>("parent") {
            Ok(Value::Table(parent)) => {
                current = parent;
            }
            _ => break,
        }
    }

    Ok(None)
}

fn describe_table(table: &Table, fallback_label: Option<&str>) -> (usize, String, Option<String>) {
    let ptr = table.to_pointer();
    let handle = telemetry::ptr_to_handle(ptr);
    let label = telemetry::table_label(ptr).or_else(|| fallback_label.map(str::to_string));
    (ptr as usize, handle, label)
}

fn derive_child_label(parent: Option<&str>, key: &Value) -> Option<String> {
    let parent = parent?;
    let suffix = match key {
        Value::String(text) => text.to_str().ok().map(|s| s.to_string()),
        Value::Integer(num) => Some(num.to_string()),
        Value::Number(num) => Some(num.to_string()),
        _ => None,
    }?;
    Some(format!("{parent}.{suffix}"))
}

fn log_parent_cycle(chain: Vec<(usize, String, Option<String>)>) {
    if chain.is_empty() {
        return;
    }
    let mut path = Vec::new();
    for (_, handle, label) in &chain {
        path.push(label.clone().unwrap_or_else(|| handle.clone()));
    }
    let mut event = EventBuilder::new("lua_parent_cycle_scan")
        .kv("table", chain[0].1.clone())
        .kv("depth", chain.len() as i64)
        .kv("path", path.join(" -> "));
    if let Some(label) = chain[0].2.as_ref() {
        event = event.kv("label", label.clone());
    }
    telemetry::log_event(event);
}
