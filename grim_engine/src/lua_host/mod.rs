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

use anyhow::{Context, Result};
use grim_analysis::resources::ResourceGraph;
use mlua::{Lua, LuaOptions, StdLib};

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
    if context::ensure_intro_cutscene(&lua, context.clone())? {
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
    ) -> Self {
        let stream = stream.map(Rc::new);
        {
            let mut ctx = context.borrow_mut();
            ctx.set_stream(stream.clone());
        }
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
        }
    }

    pub fn run(mut self) -> Result<()> {
        const FRAME_DURATION: Duration = Duration::from_millis(33);

        self.await_live_preview_handshake()?;

        loop {
            let tick_start = Instant::now();
            context::drive_active_scripts(&self.lua, self.context.clone(), 8, 32)?;
            self.frame = self.frame.wrapping_add(1);
            self.poll_movie_controls();

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
        let (Some(stream), Some(events)) = (self.stream.as_ref(), self.movie_controls.as_ref())
        else {
            return;
        };
        let current_generation = stream.current_generation();
        loop {
            match events.try_recv() {
                Ok(event) => {
                    if event.generation != current_generation {
                        continue;
                    }
                    self.context
                        .borrow_mut()
                        .handle_movie_control(event.control.clone(), event.generation);
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
