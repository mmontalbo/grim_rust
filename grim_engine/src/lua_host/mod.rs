mod context;
mod telemetry;
mod types;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grim_analysis::resources::ResourceGraph;
use mlua::{Lua, LuaOptions, StdLib};

use context::TubePoseAliasCache;

pub fn run_boot_sequence(
    data_root: &Path,
    verbose: bool,
    headless: bool,
) -> Result<EngineRuntime> {
    let resources = Rc::new(
        ResourceGraph::from_data_root(data_root)
            .with_context(|| format!("loading resource graph from {}", data_root.display()))?,
    );

    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let tube_pose_aliases: TubePoseAliasCache = Rc::new(RefCell::new(None));
    let context = Rc::new(RefCell::new(context::EngineContext::new(
        resources,
        verbose,
        headless,
        data_root.to_path_buf(),
        tube_pose_aliases.clone(),
    )));

    context::install_package_path(&lua, data_root)?;
    context::install_globals(&lua, data_root, context.clone())?;
    context::load_system_script(&lua, data_root)?;
    context::override_boot_stubs(&lua, context.clone())?;
    context::call_boot(&lua, context.clone())?;
    context::drive_active_scripts(&lua, context.clone(), 8, 128)?;
    if context::ensure_intro_cutscene(&lua, context.clone(), false)? {
        context::drive_active_scripts(&lua, context.clone(), 16, 128)?;
    }

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
            intro_finished: false,
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
        let (was_intro, had_any_before) = {
            let ctx = self.context.borrow();
            let active = ctx.active_fullscreen_movie();
            let intro_active = active
                .as_deref()
                .map(|name| name.eq_ignore_ascii_case("intro"))
                .unwrap_or(false);
            (intro_active, active.is_some())
        };
        self.intro_started |= was_intro;
        let had_any_after = {
            let ctx = self.context.borrow();
            let active = ctx.active_fullscreen_movie();
            self.intro_movie_active = active
                .as_deref()
                .map(|name| name.eq_ignore_ascii_case("intro"))
                .unwrap_or(false);
            active.is_some()
        };

        if was_intro && !self.intro_movie_active {
            self.intro_finished = true;
        } else if (self.intro_started || had_any_before) && !had_any_after {
            self.intro_finished = true;
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
