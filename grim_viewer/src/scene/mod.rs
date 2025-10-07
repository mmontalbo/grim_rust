//! Builds the high-level `ViewerScene` overlay data from engine exports. This
//! module bridges timeline manifests, movement traces, hotspot logs, and Lua
//! geometry snapshots so the viewer can project Manny's runtime state onto a
//! single background plate. It also houses the Manny-office pruning rules and
//! camera recovery helpers that keep `viewer::markers` aligned with the decoded
//! bitmap.

mod manny;

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use glam::{Mat3, Mat4, Vec3, Vec4};
use serde::Deserialize;
use serde_json::Value;

use crate::texture::load_asset_bytes;
use crate::timeline::{
    HookLookup, HookReference, TimelineSummary, build_timeline_summary, parse_hook_reference,
};
use grim_formats::SetFile;
use grim_formats::set::Setup;

#[derive(Debug, Clone, Deserialize)]
pub struct MovementSample {
    pub frame: u32,
    pub position: [f32; 3],
    #[serde(default)]
    pub yaw: Option<f32>,
    #[serde(default)]
    pub sector: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HotspotEventLog {
    events: Vec<HotspotEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HotspotEvent {
    #[allow(dead_code)]
    pub sequence: u32,
    #[serde(default)]
    pub frame: Option<u32>,
    pub label: String,
}

impl HotspotEvent {
    pub fn kind(&self) -> HotspotEventKind {
        if self.label.starts_with("set.setup.")
            || self.label.starts_with("set.switch")
            || self.label.starts_with("actor.select")
        {
            HotspotEventKind::Selection
        } else if self.label.starts_with("hotspot.") {
            HotspotEventKind::Hotspot
        } else if self.label.starts_with("actor.manny.head_target") {
            HotspotEventKind::HeadTarget
        } else if self.label.starts_with("actor.manny.ignore_boxes") {
            HotspotEventKind::IgnoreBoxes
        } else if self.label.starts_with("actor.manny.chore") {
            HotspotEventKind::Chore
        } else if self.label.starts_with("dialog.") {
            HotspotEventKind::Dialog
        } else {
            HotspotEventKind::Other
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotspotEventKind {
    Hotspot,
    HeadTarget,
    IgnoreBoxes,
    Chore,
    Dialog,
    Selection,
    Other,
}

pub fn event_marker_style(kind: HotspotEventKind) -> (f32, [f32; 3], f32) {
    match kind {
        HotspotEventKind::Hotspot => (0.05, [0.95, 0.85, 0.35], 0.4),
        HotspotEventKind::HeadTarget => (0.045, [0.35, 0.9, 0.95], 0.35),
        HotspotEventKind::IgnoreBoxes => (0.045, [0.95, 0.45, 0.35], 0.35),
        HotspotEventKind::Chore => (0.042, [0.6, 0.4, 0.95], 0.25),
        HotspotEventKind::Dialog => (0.042, [0.95, 0.65, 0.75], 0.3),
        HotspotEventKind::Selection => (0.044, [0.45, 0.95, 0.55], 0.32),
        HotspotEventKind::Other => (0.04, [0.78, 0.78, 0.78], 0.2),
    }
}

include!("movement.rs");

include!("viewer_scene.rs");

fn read_timeline_manifest(path: &Path) -> Result<Value> {
    let data =
        fs::read(path).with_context(|| format!("reading timeline manifest {}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&data)
        .with_context(|| format!("parsing timeline manifest {}", path.display()))?;
    Ok(manifest)
}

/// Minimal metadata about the active set extracted from the timeline manifest.
#[derive(Debug, Default, Clone)]
struct SetContext {
    file_name: Option<String>,
    variable_name: Option<String>,
    display_name: Option<String>,
}

impl SetContext {
    /// Pull set identifiers (file name, variable name, display label) out of
    /// the timeline JSON so downstream helpers can specialise behaviour (e.g.,
    /// Manny office pruning).
    fn from_manifest(manifest: &Value) -> Self {
        let set_info = manifest
            .get("engine_state")
            .and_then(|state| state.get("set"));

        Self {
            file_name: set_info
                .and_then(|set| set.get("set_file"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            variable_name: set_info
                .and_then(|set| set.get("variable_name"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            display_name: set_info
                .and_then(|set| set.get("display_name"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }
    }

    fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }

    fn variable_name(&self) -> Option<&str> {
        self.variable_name.as_deref()
    }

    fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }
}

/// Construct `SceneEntity` values from the timeline manifest. Returns both the
/// entities and any detected Manny office setup hint so camera recovery can be
/// biased towards the active object state.
fn build_scene_entities(
    manifest: &Value,
    hook_lookup: &HookLookup,
) -> (Vec<SceneEntity>, Option<String>) {
    let mut builders: BTreeMap<SceneEntityKey, SceneEntityBuilder> = BTreeMap::new();
    let mut setup_hint: Option<String> = None;

    if let Some(engine_state) = manifest.get("engine_state") {
        if let Some(actor_map) = engine_state
            .get("replay_snapshot")
            .and_then(|replay| replay.get("actors"))
            .and_then(|actors| actors.as_object())
        {
            for (key, value) in actor_map {
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(key)
                    .to_string();
                let entry = builders
                    .entry(SceneEntityKey::new(SceneEntityKind::Actor, name.clone()))
                    .or_insert_with(|| SceneEntityBuilder::new(SceneEntityKind::Actor, name));
                entry.apply_actor_snapshot(value, hook_lookup);
            }
        }

        if let Some(events) = engine_state
            .get("subsystem_delta_events")
            .and_then(|v| v.as_array())
        {
            for event in events {
                let subsystem = event
                    .get("subsystem")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = match event.get("target").and_then(|v| v.as_str()) {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => continue,
                };

                let kind = match subsystem {
                    "Objects" => SceneEntityKind::Object,
                    "InterestActors" => SceneEntityKind::InterestActor,
                    "Actors" => SceneEntityKind::Actor,
                    _ => continue,
                };

                let entry = builders
                    .entry(SceneEntityKey::new(kind, name.clone()))
                    .or_insert_with(|| SceneEntityBuilder::new(kind, name.clone()));

                let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let args: Vec<String> = event
                    .get("arguments")
                    .and_then(|v| v.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let trigger = event.get("triggered_by").and_then(parse_hook_reference);

                if subsystem.eq_ignore_ascii_case("Objects")
                    && name.eq_ignore_ascii_case("mo")
                    && method.eq_ignore_ascii_case("add_object_state")
                {
                    if let Some(first) = args.first() {
                        setup_hint = Some(first.clone());
                    }
                }

                entry.apply_event(method, &args, trigger, hook_lookup);
            }
        }
    }

    let mut entities: Vec<SceneEntity> = builders
        .into_iter()
        .map(|(_, builder)| builder.build())
        .collect();
    entities.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));

    (entities, setup_hint)
}

/// Derive world-space bounds across all entities, used for minimap axes and to
/// incorporate movement traces.
fn compute_entity_bounds(entities: &[SceneEntity]) -> Option<SceneBounds> {
    let mut bounds = None;

    for entity in entities {
        if let Some(position) = entity.position {
            bounds
                .get_or_insert(SceneBounds {
                    min: position,
                    max: position,
                })
                .update(position);
        }
    }

    bounds
}

/// Assemble a `ViewerScene` from the timeline manifest exported by the engine.
/// Attaches optional geometry snapshots and returns a camera recovered from the
/// set file when possible.
pub fn load_scene_from_timeline(
    path: &Path,
    manifest_path: &Path,
    active_asset: Option<&str>,
    geometry: Option<&LuaGeometrySnapshot>,
) -> Result<ViewerScene> {
    let manifest = read_timeline_manifest(path)?;
    let set_context = SetContext::from_manifest(&manifest);

    let timeline_summary = build_timeline_summary(&manifest)?;
    let hook_lookup = HookLookup::new(timeline_summary.as_ref());

    let (mut entities, setup_hint) = build_scene_entities(&manifest, &hook_lookup);

    entities = manny::prune_entities_for_set(
        entities,
        set_context.variable_name(),
        set_context.display_name(),
    );

    if let Some(snapshot) = geometry {
        manny::apply_geometry_overrides(
            &mut entities,
            snapshot,
            set_context.variable_name(),
            set_context.display_name(),
        );
    }

    let bounds = compute_entity_bounds(&entities);

    let mut scene = ViewerScene {
        entities,
        position_bounds: bounds,
        timeline: timeline_summary,
        movement: None,
        hotspot_events: Vec::new(),
        camera: None,
        active_setup: setup_hint.clone(),
    };

    if let Some(set_file) = set_context.file_name() {
        match recover_camera_from_set(manifest_path, set_file, setup_hint.as_deref(), active_asset)
        {
            Ok(Some(camera)) => {
                scene.active_setup = Some(camera.name.clone());
                scene.camera = Some(camera);
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: unable to recover camera from {}: {err}",
                    set_file
                );
            }
        }
    }

    Ok(scene)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct GeometryPose {
    position: [f32; 3],
    rotation: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LuaGeometrySnapshot {
    actors: BTreeMap<String, LuaActorSnapshot>,
    objects: Vec<LuaObjectSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct LuaActorSnapshot {
    name: Option<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
}

impl LuaActorSnapshot {
    fn pose(&self) -> Option<GeometryPose> {
        self.position.map(|position| GeometryPose {
            position,
            rotation: self.rotation,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct LuaObjectSnapshot {
    name: Option<String>,
    string_name: Option<String>,
    position: Option<[f32; 3]>,
    interest_actor: Option<LuaObjectActorLink>,
}

impl LuaObjectSnapshot {
    fn pose(&self) -> Option<GeometryPose> {
        self.position.map(|position| GeometryPose {
            position,
            rotation: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct LuaObjectActorLink {
    actor_id: Option<String>,
    actor_label: Option<String>,
}

pub fn load_lua_geometry_snapshot(path: &Path) -> Result<LuaGeometrySnapshot> {
    let data = fs::read(path)
        .with_context(|| format!("reading Lua geometry snapshot {}", path.display()))?;
    let snapshot: LuaGeometrySnapshot = serde_json::from_slice(&data)
        .with_context(|| format!("parsing Lua geometry snapshot {}", path.display()))?;
    Ok(snapshot)
}

fn recover_camera_from_set(
    manifest_path: &Path,
    set_file_name: &str,
    setup_hint: Option<&str>,
    active_asset: Option<&str>,
) -> Result<Option<CameraParameters>> {
    let (_, set_bytes, _) = load_asset_bytes(manifest_path, set_file_name)
        .with_context(|| format!("loading set file {}", set_file_name))?;
    let set = SetFile::parse(&set_bytes)
        .with_context(|| format!("parsing set file {}", set_file_name))?;

    let mut selected_setup: Option<&Setup> = None;
    if let Some(hint) = setup_hint {
        selected_setup = set
            .setups
            .iter()
            .find(|setup| setup.name.eq_ignore_ascii_case(hint));
    }

    if selected_setup.is_none() {
        if let Some(asset) = active_asset {
            selected_setup = set.setups.iter().find(|setup| {
                setup
                    .background
                    .as_ref()
                    .map(|bg| bg.eq_ignore_ascii_case(asset))
                    .unwrap_or(false)
                    || setup
                        .zbuffer
                        .as_ref()
                        .map(|zb| zb.eq_ignore_ascii_case(asset))
                        .unwrap_or(false)
            });

            if selected_setup.is_none() {
                let lower = asset.to_ascii_lowercase();
                selected_setup = set.setups.iter().find(|setup| {
                    setup
                        .background
                        .as_ref()
                        .map(|bg| bg.to_ascii_lowercase() == lower)
                        .unwrap_or(false)
                        || setup
                            .zbuffer
                            .as_ref()
                            .map(|zb| zb.to_ascii_lowercase() == lower)
                            .unwrap_or(false)
                });
            }
        }
    }

    if selected_setup.is_none() {
        selected_setup = set.setups.first();
    }

    if let Some(setup) = selected_setup {
        if let Some(camera) = CameraParameters::from_setup(&setup.name, setup) {
            return Ok(Some(camera));
        }
    }

    Ok(None)
}

pub fn load_movement_trace(path: &Path) -> Result<MovementTrace> {
    let data =
        fs::read(path).with_context(|| format!("reading movement log {}", path.display()))?;
    let samples: Vec<MovementSample> = serde_json::from_slice(&data)
        .with_context(|| format!("parsing movement log {}", path.display()))?;
    MovementTrace::from_samples(samples)
        .with_context(|| format!("summarising movement trace from {}", path.display()))
}

#[cfg(test)]
mod movement_log_io_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn movement_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/tests/movement_log.json")
    }

    #[test]
    fn load_movement_trace_summarises_baseline_fixture() {
        let trace = load_movement_trace(&movement_fixture_path()).expect("movement trace");

        assert_eq!(trace.sample_count(), 114);
        assert_eq!(trace.first_frame, 1);
        assert_eq!(trace.last_frame, 114);
        assert!((trace.total_distance - 1.1599987).abs() < 1e-6);

        let yaw_range = trace.yaw_range().expect("yaw range");
        assert!(yaw_range.0.abs() < 1e-6);
        assert!((yaw_range.1 - 270.0).abs() < 1e-6);

        let sectors = trace.dominant_sectors(3);
        assert_eq!(sectors.len(), 3);
        assert_eq!(sectors[0], ("floor_17", 42));
        assert_eq!(sectors[1], ("floor_21", 25));
        assert_eq!(sectors[2], ("floor_1734", 18));

        assert!((trace.bounds.min[0] - 0.607).abs() < 1e-6);
        assert!((trace.bounds.max[0] - 1.086_999_5).abs() < 1e-6);
        assert!((trace.bounds.min[1] - 2.021).abs() < 1e-6);
        assert!((trace.bounds.max[1] - 2.140_999_8).abs() < 1e-6);
    }

    #[test]
    fn load_movement_trace_surfaces_parse_errors() {
        let mut temp = NamedTempFile::new().expect("temp file");
        writeln!(temp, "this is not valid JSON").expect("write invalid content");

        let error = load_movement_trace(temp.path()).expect_err("expected parse failure");
        let message = format!("{error}");
        assert!(message.contains("parsing movement log"));
    }
}

#[cfg(test)]
mod hotspot_log_tests {
    use super::*;

    fn hotspot_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/tests/hotspot_events.json")
    }

    #[test]
    fn load_hotspot_event_log_matches_fixture_snapshot() {
        let events =
            load_hotspot_event_log(&hotspot_fixture_path()).expect("load hotspot event fixture");

        assert_eq!(events.len(), 38);
        assert_eq!(events[0].label, "actor.select manny");
        assert_eq!(events[0].kind(), HotspotEventKind::Selection);
        assert!(
            events
                .iter()
                .any(|event| event.label == "hotspot.demo.start computer")
        );

        let mut selections = 0usize;
        let mut hotspots = 0usize;
        let mut head_targets = 0usize;
        let mut ignore_boxes = 0usize;
        let mut chores = 0usize;
        let mut dialogs = 0usize;
        let mut other = 0usize;

        for event in &events {
            match event.kind() {
                HotspotEventKind::Selection => selections += 1,
                HotspotEventKind::Hotspot => hotspots += 1,
                HotspotEventKind::HeadTarget => head_targets += 1,
                HotspotEventKind::IgnoreBoxes => ignore_boxes += 1,
                HotspotEventKind::Chore => chores += 1,
                HotspotEventKind::Dialog => dialogs += 1,
                HotspotEventKind::Other => other += 1,
            }
        }

        assert_eq!(selections, 8);
        assert_eq!(hotspots, 4);
        assert_eq!(head_targets, 4);
        assert_eq!(ignore_boxes, 2);
        assert_eq!(chores, 3);
        assert_eq!(dialogs, 8);
        assert_eq!(other, 9);
    }
}

pub fn load_hotspot_event_log(path: &Path) -> Result<Vec<HotspotEvent>> {
    let data =
        fs::read(path).with_context(|| format!("reading hotspot event log {}", path.display()))?;
    let log: HotspotEventLog = serde_json::from_slice(&data)
        .with_context(|| format!("parsing hotspot event log {}", path.display()))?;
    Ok(log.events)
}

pub fn print_scene_summary(scene: &ViewerScene) {
    println!();
    println!(
        "Scene bootstrap: {} entit{} from timeline manifest",
        scene.entities.len(),
        if scene.entities.len() == 1 {
            "y"
        } else {
            "ies"
        }
    );
    for entity in &scene.entities {
        println!("  - {}", entity.describe());
    }
    if let Some(setup) = scene.active_setup() {
        println!("Active camera setup: {}", setup);
        if scene.camera.is_some() {
            println!(
                "Markers overlay renders Manny/head targets in plate space using this camera."
            );
        }
    }
    if !scene.entities.is_empty() {
        println!("\nUse ←/→ to cycle entity focus while the viewer is running.");
        println!(
            "Entity focus drives the highlighted marker, timeline overlay, and console dump for the active actor/object."
        );
        println!(
            "Markers overlay: color-coded discs track entities (red = selected) and mirror the minimap anchors."
        );
    }
    if let Some(trace) = scene.movement_trace() {
        print_movement_trace_summary(trace);
    }
    let event_preview = scene.hotspot_events();
    if !event_preview.is_empty() {
        print_hotspot_preview(event_preview);
    }
    println!();
}

pub fn print_movement_trace_summary(trace: &MovementTrace) {
    println!(
        "Movement trace: {} samples (frames {}–{}), distance {:.3}",
        trace.sample_count(),
        trace.first_frame,
        trace.last_frame,
        trace.total_distance
    );
    let sectors = trace.dominant_sectors(3);
    if !sectors.is_empty() {
        let preview: Vec<String> = sectors
            .iter()
            .map(|(name, count)| format!("{}×{}", count, name))
            .collect();
        println!("  sectors: {}", preview.join(", "));
    }
    if let Some((min_yaw, max_yaw)) = trace.yaw_range() {
        println!("  yaw range: {:.3} – {:.3}", min_yaw, max_yaw);
    }
    println!(
        "  Overlay markers: jade = desk anchor, violet = path, amber = tube anchor, teal = Manny, gold = highlighted hotspot, red = entity selection."
    );
    println!("  Scrubber controls: '['/']' step Manny frames; '{{'/'}}' jump head-target markers.");
}

pub fn print_hotspot_preview(events: &[HotspotEvent]) {
    println!("Hotspot event log: {} entries", events.len());
    for event in events.iter().take(6) {
        let frame_label = event
            .frame
            .map(|frame| frame.to_string())
            .unwrap_or_else(|| String::from("--"));
        println!("  [{frame_label}] {}", event.label);
    }
    if events.len() > 6 {
        println!("  ... +{} more", events.len() - 6);
    }
}

fn format_hook_reference(reference: &HookReference) -> String {
    let defined_in = reference.defined_in().unwrap_or("unknown.lua");
    let line_suffix = reference
        .defined_at_line()
        .map(|line| format!(":{}", line))
        .unwrap_or_default();

    match reference.stage_label.as_deref() {
        Some(label) => format!(
            "{} @ {}{} [{}]",
            reference.name(),
            defined_in,
            line_suffix,
            label
        ),
        None => format!("{} @ {}{}", reference.name(), defined_in, line_suffix),
    }
}

fn parse_vec3_object(value: &Value) -> Option<[f32; 3]> {
    let x = value.get("x")?.as_f64()? as f32;
    let y = value.get("y")?.as_f64()? as f32;
    let z = value.get("z")?.as_f64()? as f32;
    Some([x, y, z])
}

fn parse_vec3_args(args: &[String]) -> Option<[f32; 3]> {
    if args.len() < 3 {
        return None;
    }
    let mut values = [0.0f32; 3];
    for (idx, slot) in values.iter_mut().enumerate() {
        let value = args[idx].trim();
        if value == "<expr>" {
            return None;
        }
        *slot = parse_f32(value)?;
    }
    Some(values)
}

fn parse_f32(value: &str) -> Option<f32> {
    let trimmed = value.trim().trim_matches('"');
    trimmed.parse::<f32>().ok()
}
