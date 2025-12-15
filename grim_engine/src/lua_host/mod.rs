mod context;
mod legacy_lua;
mod telemetry;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mlua::{Lua, LuaOptions, StdLib};

pub fn log_engine_exit(
    status: &str,
    note: Option<&str>,
    code: Option<i32>,
    signal: Option<i32>,
    cause: Option<&str>,
) {
    telemetry::log_engine_exit(status, note, code, signal, cause);
}

fn run_phase<T>(name: &str, phase: impl FnOnce() -> Result<T>) -> Result<T> {
    let _ = name;
    phase()
}

pub fn run_boot_sequence(data_root: &Path, verbose: bool, headless: bool) -> Result<EngineRuntime> {
    telemetry::log_boot_sequence_start();
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
    setup_result?;
    telemetry::log_boot_sequence_complete(None);

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

// Parent-cycle scan removed; boot telemetry now covers the boot window only.
