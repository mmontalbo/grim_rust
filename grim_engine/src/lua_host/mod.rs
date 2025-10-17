mod context;
mod hotspot;
mod movement;
mod types;

pub use context::AudioCallback;
pub use hotspot::HotspotOptions;
pub use movement::MovementOptions;
#[allow(unused_imports)]
pub use movement::MovementPlan;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grim_analysis::resources::ResourceGraph;
use grim_stream::{CoverageCounter, StateUpdate};
use mlua::{Lua, LuaOptions, StdLib};

use crate::lab_collection::LabCollection;
use crate::stream::StreamServer;
use context::EngineContextHandle;

#[derive(Debug, Clone)]
pub struct EngineRunSummary {
    events: Vec<String>,
    coverage: BTreeMap<String, u64>,
}

impl EngineRunSummary {
    pub fn events(&self) -> &[String] {
        &self.events
    }

    pub fn coverage(&self) -> &BTreeMap<String, u64> {
        &self.coverage
    }
}

pub fn run_boot_sequence(
    data_root: &Path,
    lab_root: Option<&Path>,
    verbose: bool,
    geometry_json: Option<&Path>,
    audio_callback: Option<Rc<dyn AudioCallback>>,
    movement: Option<MovementOptions>,
    hotspot: Option<HotspotOptions>,
    stream: Option<StreamServer>,
    stream_ready: Option<PathBuf>,
) -> Result<(EngineRunSummary, Option<EngineRuntime>)> {
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
    )));
    let context_handle = context::EngineContextHandle::new(context.clone());

    context::install_package_path(&lua, data_root)?;
    context::install_globals(&lua, data_root, context.clone())?;
    context::load_system_script(&lua, data_root)?;
    context::override_boot_stubs(&lua, context.clone())?;
    context::call_boot(&lua, context.clone())?;
    context::drive_active_scripts(&lua, context.clone(), 8, 32)?;

    let mut stream = stream;
    let mut stream_ready = stream_ready;

    if let Some(options) = movement.as_ref() {
        movement::simulate_movement(&lua, &context_handle, options, stream.as_ref())?;
    }

    if let Some(options) = hotspot.as_ref() {
        hotspot::simulate_hotspot_demo(&lua, &context_handle, options)?;
    }

    let snapshot = context.borrow();
    context::dump_runtime_summary(&snapshot);
    let events = snapshot.events().to_vec();
    let coverage = snapshot.coverage_counts().clone();
    if let Some(path) = geometry_json {
        let snapshot_data = snapshot.geometry_snapshot();
        let json = serde_json::to_string_pretty(&snapshot_data)
            .context("serializing Lua geometry snapshot to JSON")?;
        fs::write(path, &json)
            .with_context(|| format!("writing Lua geometry snapshot to {}", path.display()))?;
        println!("Saved Lua geometry snapshot to {}", path.display());
    }
    drop(snapshot);

    let summary = EngineRunSummary { events, coverage };
    let runtime = stream.take().map(|stream| {
        EngineRuntime::new(
            lua,
            context,
            context_handle,
            stream,
            summary.events.len(),
            summary.coverage.clone(),
            stream_ready.take().map(StreamReadyGate::new),
        )
    });

    Ok((summary, runtime))
}

pub struct EngineRuntime {
    lua: Lua,
    context: Rc<RefCell<context::EngineContext>>,
    context_handle: EngineContextHandle,
    stream: StreamServer,
    frame: u32,
    event_cursor: usize,
    prev_coverage: BTreeMap<String, u64>,
    last_position: Option<[f32; 3]>,
    last_yaw: Option<f32>,
    last_setup: Option<String>,
    last_hotspot: Option<String>,
    manny_handle: Option<u32>,
    manny_actor_id: Option<String>,
    sent_initial: bool,
    start_gate: Option<StreamReadyGate>,
}

impl EngineRuntime {
    fn new(
        lua: Lua,
        context: Rc<RefCell<context::EngineContext>>,
        context_handle: EngineContextHandle,
        stream: StreamServer,
        initial_event_cursor: usize,
        initial_coverage: BTreeMap<String, u64>,
        start_gate: Option<StreamReadyGate>,
    ) -> Self {
        Self {
            lua,
            context,
            context_handle,
            stream,
            frame: 0,
            event_cursor: initial_event_cursor,
            prev_coverage: initial_coverage,
            last_position: None,
            last_yaw: None,
            last_setup: None,
            last_hotspot: None,
            manny_handle: None,
            manny_actor_id: None,
            sent_initial: false,
            start_gate,
        }
    }

    pub fn run(mut self) -> Result<()> {
        const FRAME_DURATION: Duration = Duration::from_millis(33);

        if let Some(gate) = self.start_gate.take() {
            gate.wait()?;
        }

        loop {
            let tick_start = Instant::now();
            context::drive_active_scripts(&self.lua, self.context.clone(), 8, 32)?;
            self.frame = self.frame.wrapping_add(1);

            if let Some(update) = self.build_state_update()? {
                if let Err(err) = self.stream.send_state_update(update) {
                    eprintln!("[grim_engine] failed to publish state update: {err:?}; continuing");
                }
            }

            let elapsed = tick_start.elapsed();
            if elapsed < FRAME_DURATION {
                thread::sleep(FRAME_DURATION - elapsed);
            }
        }
    }

    fn ensure_manny_handle(&mut self) {
        if self.manny_handle.is_some() {
            return;
        }
        if let Some((handle, id)) = self
            .context_handle
            .resolve_actor_handle(&["manny", "Manny"])
        {
            self.manny_handle = Some(handle);
            self.manny_actor_id = Some(id);
        }
    }

    fn build_state_update(&mut self) -> Result<Option<StateUpdate>> {
        self.ensure_manny_handle();

        let (
            position_opt,
            yaw_opt,
            active_setup_opt,
            active_hotspot_opt,
            events_len,
            new_events,
            coverage_samples,
        ) = {
            let ctx = self.context.borrow();

            let position_opt = self
                .manny_handle
                .and_then(|handle| ctx.actor_position_by_handle(handle))
                .map(|vec| [vec.x, vec.y, vec.z]);

            let yaw_opt = self
                .manny_handle
                .and_then(|handle| ctx.actor_rotation_by_handle(handle))
                .map(|rot| rot.y);

            let active_setup_opt = ctx.active_setup_label();

            let active_hotspot_opt = self.manny_actor_id.as_ref().and_then(|actor_id| {
                ctx.geometry_sector_name(actor_id, "hot")
                    .or_else(|| ctx.geometry_sector_name(actor_id, "walk"))
            });

            let events = ctx.events();
            let events_len = events.len();
            let new_events = if self.event_cursor < events_len {
                events[self.event_cursor..].to_vec()
            } else {
                Vec::new()
            };

            let coverage_samples: Vec<(String, u64)> = ctx
                .coverage_counts()
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect();

            (
                position_opt,
                yaw_opt,
                active_setup_opt,
                active_hotspot_opt,
                events_len,
                new_events,
                coverage_samples,
            )
        };

        self.event_cursor = events_len;

        let mut coverage_updates = Vec::new();
        for (key, value) in coverage_samples {
            let previous = self.prev_coverage.insert(key.clone(), value);
            if !self.sent_initial || previous != Some(value) {
                coverage_updates.push(CoverageCounter { key, value });
            }
        }

        let mut changed = !self.sent_initial;

        if let Some(pos) = position_opt {
            if self.last_position != Some(pos) {
                self.last_position = Some(pos);
                changed = true;
            }
        }

        if let Some(yaw) = yaw_opt {
            if self.last_yaw != Some(yaw) {
                self.last_yaw = Some(yaw);
                changed = true;
            }
        }

        if let Some(setup) = active_setup_opt.as_ref() {
            if self.last_setup.as_deref() != Some(setup.as_str()) {
                self.last_setup = Some(setup.clone());
                changed = true;
            }
        }

        if let Some(hotspot) = active_hotspot_opt.as_ref() {
            if self.last_hotspot.as_deref() != Some(hotspot.as_str()) {
                self.last_hotspot = Some(hotspot.clone());
                changed = true;
            }
        }

        if !coverage_updates.is_empty() {
            changed = true;
        }

        if new_events.is_empty() && !changed {
            return Ok(None);
        }

        self.sent_initial = true;

        let update = StateUpdate {
            seq: 0,
            host_time_ns: 0,
            frame: Some(self.frame),
            position: self.last_position,
            yaw: self.last_yaw,
            active_setup: self.last_setup.clone(),
            active_hotspot: self.last_hotspot.clone(),
            coverage: coverage_updates,
            events: new_events,
        };

        Ok(Some(update))
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
