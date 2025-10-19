mod context;
mod state_update;
mod types;

pub use context::AudioCallback;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use grim_analysis::resources::ResourceGraph;
use grim_stream::{MovieAction, MovieControl};
use mlua::{Function, Lua, LuaOptions, StdLib, Value};

use crate::lab_collection::LabCollection;
use crate::stream::{MovieControlEvents, StreamServer, StreamViewerGate};
use context::EngineContextHandle;
use crossbeam_channel::TryRecvError;
use state_update::StateUpdateBuilder;

pub fn run_boot_sequence(
    data_root: &Path,
    lab_root: Option<&Path>,
    verbose: bool,
    headless: bool,
    geometry_json: Option<&Path>,
    audio_callback: Option<Rc<dyn AudioCallback>>,
    stream: Option<StreamServer>,
    stream_ready: Option<PathBuf>,
) -> Result<Option<EngineRuntime>> {
    let resources = Rc::new(
        ResourceGraph::from_data_root(data_root)
            .with_context(|| format!("loading resource graph from {}", data_root.display()))?,
    );

    let lab_root_path = lab_root
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("dev-install"));
    let lab_collection = if lab_root_path.is_dir() {
        match LabCollection::load_from_dir(&lab_root_path) {
            Ok(collection) => Some(Rc::new(collection)),
            Err(err) => {
                eprintln!(
                    "[grim_engine] warning: failed to load LAB archives from {}: {:?}",
                    lab_root_path.display(),
                    err
                );
                None
            }
        }
    } else {
        if verbose {
            eprintln!(
                "[grim_engine] info: LAB root {} missing; continuing without geometry",
                lab_root_path.display()
            );
        }
        None
    };

    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .context("initialising Lua runtime with standard libraries")?;
    let context = Rc::new(RefCell::new(context::EngineContext::new(
        resources,
        verbose,
        lab_collection,
        audio_callback,
        lab_root_path.clone(),
    )));
    let context_handle = context::EngineContextHandle::new(context.clone());

    context::install_package_path(&lua, data_root)?;
    context::install_globals(&lua, data_root, context.clone())?;
    context::load_system_script(&lua, data_root)?;
    context::override_boot_stubs(&lua, context.clone())?;
    context::call_boot(&lua, context.clone())?;
    context::drive_active_scripts(&lua, context.clone(), 8, 32)?;
    let defer_intro_playback = !headless && stream.is_some();
    if !defer_intro_playback
        && context::ensure_intro_cutscene(&lua, context.clone(), defer_intro_playback)?
    {
        context::drive_active_scripts(&lua, context.clone(), 16, 64)?;
    }

    let snapshot = context.borrow();
    context::dump_runtime_summary(&snapshot);
    let initial_event_cursor = snapshot.events().len();
    let initial_coverage = snapshot.coverage_counts().clone();
    if let Some(path) = geometry_json {
        let snapshot_data = snapshot.geometry_snapshot();
        let json = serde_json::to_string_pretty(&snapshot_data)
            .context("serializing Lua geometry snapshot to JSON")?;
        fs::write(path, &json)
            .with_context(|| format!("writing Lua geometry snapshot to {}", path.display()))?;
        println!("Saved Lua geometry snapshot to {}", path.display());
    }
    drop(snapshot);

    let runtime_needed = headless || stream.is_some();
    let start_gate = if headless {
        None
    } else {
        stream_ready.map(StreamReadyGate::new)
    };
    let runtime = if runtime_needed {
        // The EngineRuntime owns the Lua VM when we are actively streaming state.
        Some(EngineRuntime::new(
            lua,
            context,
            context_handle,
            stream,
            headless,
            initial_event_cursor,
            initial_coverage.clone(),
            start_gate,
            defer_intro_playback,
        ))
    } else {
        None
    };

    Ok(runtime)
}

/// Drives the embedded Lua runtime and publishes live state over GrimStream.
pub struct EngineRuntime {
    lua: Lua,
    context: Rc<RefCell<context::EngineContext>>,
    stream: Option<Rc<StreamServer>>,
    headless: bool,
    frame: u32,
    /// Keeps track of deltas so state updates stay compact.
    state_builder: StateUpdateBuilder,
    start_gate: Option<StreamReadyGate>,
    viewer_gate: Option<StreamViewerGate>,
    movie_controls: Option<MovieControlEvents>,
    log_file: Option<File>,
    defer_intro_cutscene: bool,
    intro_movie_active: bool,
    manny_office_booted: bool,
}

impl EngineRuntime {
    fn new(
        lua: Lua,
        context: Rc<RefCell<context::EngineContext>>,
        context_handle: EngineContextHandle,
        stream: Option<StreamServer>,
        headless: bool,
        initial_event_cursor: usize,
        initial_coverage: BTreeMap<String, u64>,
        start_gate: Option<StreamReadyGate>,
        defer_intro_cutscene: bool,
    ) -> Self {
        let stream = stream.map(Rc::new);
        {
            let mut ctx = context.borrow_mut();
            ctx.set_stream(stream.clone());
        }
        let intro_movie_active = {
            let ctx = context.borrow();
            ctx.active_fullscreen_movie()
                .as_deref()
                .map(|name| name.eq_ignore_ascii_case("intro"))
                .unwrap_or(false)
        };
        let viewer_gate = if headless {
            None
        } else {
            stream.as_ref().map(|s| s.viewer_gate())
        };
        let movie_controls = stream.as_ref().map(|s| s.movie_controls());
        Self {
            lua,
            context,
            stream,
            headless,
            frame: 0,
            state_builder: StateUpdateBuilder::new(
                context_handle,
                initial_event_cursor,
                initial_coverage,
            ),
            start_gate,
            viewer_gate,
            movie_controls,
            log_file: open_live_preview_log(),
            defer_intro_cutscene,
            intro_movie_active,
            manny_office_booted: false,
        }
    }

    pub fn run(mut self) -> Result<()> {
        const FRAME_DURATION: Duration = Duration::from_millis(33);

        self.await_live_preview_handshake()?;

        if self.defer_intro_cutscene {
            if context::ensure_intro_cutscene(&self.lua, self.context.clone(), false)? {
                context::drive_active_scripts(&self.lua, self.context.clone(), 16, 64)?;
            }
            self.defer_intro_cutscene = false;
        }

        self.observe_intro_movie_completion()?;
        self.refresh_manny_office_state()?;

        if self.headless {
            self.force_active_movie_completion(MovieAction::Finished)?;
        }

        loop {
            let tick_start = Instant::now();
            context::drive_active_scripts(&self.lua, self.context.clone(), 8, 32)?;
            self.frame = self.frame.wrapping_add(1);
            self.poll_movie_controls();
            self.observe_intro_movie_completion()?;
            self.refresh_manny_office_state()?;
            if self.headless {
                self.force_active_movie_completion(MovieAction::Finished)?;
            }

            if let Some(update) = self
                .state_builder
                .build(self.frame, &self.context)
                .context("building state update")?
            {
                if self.headless && !update.events.is_empty() {
                    for event in &update.events {
                        println!("[grim_engine][headless] {event}");
                    }
                }
                if let Some(stream) = self.stream.as_ref() {
                    if let Err(err) = stream.send_state_update(update) {
                        eprintln!(
                            "[grim_engine] failed to publish state update: {err:?}; continuing"
                        );
                    }
                }
            }

            let elapsed = tick_start.elapsed();
            if elapsed < FRAME_DURATION {
                thread::sleep(FRAME_DURATION - elapsed);
            }
        }
    }

    fn poll_movie_controls(&mut self) {
        let current_generation = match self.stream.as_ref() {
            Some(stream) => stream.current_generation(),
            None => return,
        };
        let receiver = match self.movie_controls.as_ref() {
            Some(events) => events.receiver(),
            None => return,
        };
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    if event.generation != current_generation {
                        continue;
                    }
                    let control = event.control.clone();
                    self.context
                        .borrow_mut()
                        .handle_movie_control(control.clone(), event.generation);
                    if let Err(err) = self.process_movie_control(&control) {
                        eprintln!(
                            "[grim_engine] failed to apply movie control side effects: {err:?}"
                        );
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Wait for the viewer and optional capture processes before entering the main loop.
    fn await_live_preview_handshake(&mut self) -> Result<()> {
        if self.headless {
            return Ok(());
        }

        if let Some(gate) = self.viewer_gate.clone() {
            if !gate.is_ready() {
                self.log_gate_event("viewer_ready.wait");
            }
            gate.wait_for_ready();
            self.log_gate_event("viewer_ready.open");
        }

        if let Some(gate) = self.start_gate.take() {
            self.log_gate_event("capture_ready.wait");
            gate.wait()?;
            self.log_gate_event("capture_ready.open");
        }

        Ok(())
    }

    fn log_gate_event(&mut self, message: &str) {
        eprintln!("[grim_engine] {message}");
        if let Some(file) = self.log_file.as_mut() {
            if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
                let secs = now.as_secs();
                let nanos = now.subsec_nanos();
                let _ = writeln!(file, "[{secs}.{nanos:09}] {message}");
            } else {
                let _ = writeln!(file, "[0.000000000] {message}");
            }
            let _ = file.flush();
        }
    }

    fn process_movie_control(&mut self, control: &MovieControl) -> Result<()> {
        if control.name.eq_ignore_ascii_case("intro") {
            match control.action {
                MovieAction::Finished | MovieAction::Skipped | MovieAction::Error => {
                    self.ensure_manny_office_booted()?;
                    self.intro_movie_active = false;
                }
                MovieAction::Ack => {}
            }
        }
        Ok(())
    }

    fn observe_intro_movie_completion(&mut self) -> Result<()> {
        let active = {
            let ctx = self.context.borrow();
            ctx.active_fullscreen_movie()
                .as_deref()
                .map(|name| name.eq_ignore_ascii_case("intro"))
                .unwrap_or(false)
        };
        if self.intro_movie_active && !active {
            self.ensure_manny_office_booted()?;
        }
        self.intro_movie_active = active;
        Ok(())
    }

    fn force_active_movie_completion(&mut self, reason: MovieAction) -> Result<()> {
        let active_movie = {
            let ctx = self.context.borrow();
            ctx.active_fullscreen_movie()
        };
        if let Some(name) = active_movie {
            let mut ctx = self.context.borrow_mut();
            let mut completion_logged = false;

            if ctx.force_movie_completion(reason.clone()) {
                ctx.log_event(format!(
                    "cut_scene.fullscreen.force_complete {} {:?}",
                    name, reason
                ));
                completion_logged = true;
            }

            let mut still_playing = true;
            while still_playing {
                still_playing = ctx.poll_fullscreen_movie();
                if !still_playing && !completion_logged {
                    ctx.log_event(format!(
                        "cut_scene.fullscreen.force_complete {} {:?}",
                        name, reason
                    ));
                    completion_logged = true;
                }
            }
        }
        Ok(())
    }

    fn ensure_manny_office_booted(&mut self) -> Result<()> {
        if self.manny_office_booted {
            return Ok(());
        }

        {
            let mut ctx = self.context.borrow_mut();
            ctx.log_event("manny_office.resume");
        }

        let handle: u32 = {
            let globals = self.lua.globals();
            let start_script: Function = globals
                .get("start_script")
                .context("start_script missing while resuming Manny's office")?;
            let mo_value: Value = globals
                .get("mo")
                .context("mo table missing while resuming Manny's office")?;
            let mo_table = match mo_value {
                Value::Table(table) => table,
                _ => {
                    return Err(anyhow!(
                        "mo global is not a table while resuming Manny's office"
                    ));
                }
            };
            let enter: Function = mo_table
                .get("enter")
                .context("mo.enter missing while resuming Manny's office")?;

            start_script
                .call((enter, mo_table))
                .context("scheduling mo.enter after intro movie")?
        };
        {
            let mut ctx = self.context.borrow_mut();
            ctx.log_event(format!("manny_office.resume.script #{handle}"));
        }

        self.manny_office_booted = true;
        self.refresh_manny_office_state()?;
        Ok(())
    }

    fn refresh_manny_office_state(&mut self) -> Result<()> {
        if !self.manny_office_booted {
            return Ok(());
        }
        let contains = self.read_tube_contains_label()?;
        let pose = self.read_tube_pose_label()?;
        {
            let mut ctx = self.context.borrow_mut();
            ctx.update_tube_contains(contains);
            ctx.update_tube_pose(pose);
        }
        Ok(())
    }

    fn read_tube_contains_label(&self) -> Result<Option<String>> {
        let globals = self.lua.globals();
        let Some(mo_value) = globals.get::<_, Option<Value>>("mo")? else {
            return Ok(None);
        };
        let Value::Table(mo_table) = mo_value else {
            return Ok(None);
        };
        let Some(tube_value) = mo_table.get::<_, Option<Value>>("tube")? else {
            return Ok(None);
        };
        let Value::Table(tube_table) = tube_value else {
            return Ok(None);
        };
        match tube_table.get::<_, Option<Value>>("contains")? {
            Some(Value::Table(table)) => {
                let label = table
                    .get::<_, Option<String>>("string_name")?
                    .or(table.get::<_, Option<String>>("name")?);
                Ok(label)
            }
            _ => Ok(None),
        }
    }

    fn read_tube_pose_label(&self) -> Result<Option<String>> {
        if let Some(chore) = {
            let ctx = self.context.borrow();
            ctx.actor_current_chore("mo.tube.interest_actor")
        } {
            if chore.is_empty() {
                return Ok(None);
            }
            return Ok(Some(chore));
        }

        let globals = self.lua.globals();
        let Some(mo_value) = globals.get::<_, Option<Value>>("mo")? else {
            return Ok(None);
        };
        let Value::Table(mo_table) = mo_value else {
            return Ok(None);
        };
        let Some(tube_value) = mo_table.get::<_, Option<Value>>("tube")? else {
            return Ok(None);
        };
        let Value::Table(tube_table) = tube_value else {
            return Ok(None);
        };
        let Some(actor_value) = tube_table.get::<_, Option<Value>>("interest_actor")? else {
            return Ok(None);
        };
        let Value::Table(actor_table) = actor_value else {
            return Ok(None);
        };
        match actor_table.get::<_, Option<Value>>("current_chore")? {
            Some(Value::String(text)) => Ok(Some(text.to_str()?.to_string())),
            Some(Value::Integer(value)) => Ok(Some(value.to_string())),
            Some(Value::Number(value)) => Ok(Some(value.to_string())),
            _ => Ok(None),
        }
    }
}

struct StreamReadyGate {
    path: PathBuf,
}

impl StreamReadyGate {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn wait(self) -> Result<()> {
        if self.path.exists() {
            eprintln!(
                "[grim_engine] live stream ready marker already present at {}",
                self.path.display()
            );
            return Ok(());
        }

        let mut last_log = Instant::now();
        let log_interval = Duration::from_secs(5);
        loop {
            if self.path.exists() {
                eprintln!(
                    "[grim_engine] live stream ready marker observed at {}",
                    self.path.display()
                );
                return Ok(());
            }
            if last_log.elapsed() >= log_interval {
                eprintln!(
                    "[grim_engine] waiting for retail capture to signal readiness via {}",
                    self.path.display()
                );
                last_log = Instant::now();
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn open_live_preview_log() -> Option<File> {
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/live_preview.log")
    {
        Ok(file) => Some(file),
        Err(err) => {
            eprintln!("[grim_engine] warning: failed to open /tmp/live_preview.log: {err:?}");
            None
        }
    }
}
