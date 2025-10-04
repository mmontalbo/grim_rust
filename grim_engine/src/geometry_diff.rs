use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use grim_analysis::timeline::BootTimeline;

use crate::geometry_snapshot::{LuaGeometrySnapshot, LuaSetSnapshot};
use crate::state::{EngineState, GeometryCall, HookReference, SetState, VisibilityCall};
use regex::Regex;
use serde::Serialize;

pub fn run_geometry_diff(
    timeline: &BootTimeline,
    engine_state: &EngineState,
    snapshot_path: &Path,
    data_root: &Path,
    summary_output: Option<&Path>,
) -> Result<()> {
    let data = fs::read_to_string(snapshot_path)
        .with_context(|| format!("reading geometry snapshot from {}", snapshot_path.display()))?;
    let snapshot: LuaGeometrySnapshot = serde_json::from_str(&data).with_context(|| {
        format!(
            "parsing geometry snapshot JSON from {}",
            snapshot_path.display()
        )
    })?;

    if engine_state.set.is_none() {
        println!("[geometry-diff] no default set present in static timeline; skipping comparison");
        return Ok(());
    }

    let default_set_file = timeline
        .default_set
        .as_ref()
        .map(|set| set.set_file.clone())
        .unwrap_or_default();

    let set_state = engine_state.set.as_ref().unwrap();

    let mut expected_states = build_initial_sector_states(&snapshot);
    let mut issues = GeometryIssues::default();

    apply_geometry_calls(
        set_state,
        &snapshot,
        &default_set_file,
        &mut expected_states,
        &mut issues,
    );

    let object_predictions = load_object_predictions(data_root, set_state)?;

    analyze_visibility_calls(set_state, &snapshot, &mut issues);
    analyze_visibility_metrics(&snapshot, &object_predictions, &mut issues);

    let mismatches = compare_sector_states(&snapshot, &expected_states);
    let summary = GeometryDiffSummary {
        snapshot_path: snapshot_path.display().to_string(),
        sector_mismatches: mismatches,
        issues,
    };

    report_results(&summary);
    if let Some(path) = summary_output {
        write_summary(path, &summary)?;
    }

    Ok(())
}

#[derive(Debug, Default, Serialize)]
struct GeometryIssues {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    unresolved_calls: Vec<UnresolvedCall>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    missing_sectors: Vec<MissingSector>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    visibility_mismatches: Vec<VisibilityIssue>,
}

#[derive(Debug, Serialize)]
struct GeometryDiffSummary {
    snapshot_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    sector_mismatches: Vec<SectorMismatch>,
    issues: GeometryIssues,
}

#[derive(Debug, Serialize)]
struct UnresolvedCall {
    function: String,
    arguments: Vec<String>,
    triggered_by: HookReference,
    trigger_sequence: usize,
    reason: String,
}

#[derive(Debug, Serialize)]
struct MissingSector {
    set_file: String,
    sector: String,
    triggered_by: HookReference,
    trigger_sequence: usize,
}

#[derive(Debug, Serialize)]
struct SectorMismatch {
    set_file: String,
    sector: String,
    expected_active: bool,
    actual_active: bool,
}

#[derive(Debug, Serialize)]
enum VisibilityIssueKind {
    HotlistEmpty,
    HeadTargetMismatch,
    RangeMismatch,
    DistanceMismatch,
    AngleMismatch,
    DistanceMissing,
    AngleMissing,
}

#[derive(Debug, Serialize)]
struct VisibilityIssue {
    kind: VisibilityIssueKind,
    expected: String,
    actual: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    triggered_by: Option<HookReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_sequence: Option<usize>,
}

#[derive(Debug, Clone)]
struct ObjectPrediction {
    position: Vec3,
    range: f32,
}

#[derive(Debug, Clone, Copy)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl From<[f32; 3]> for Vec3 {
    fn from(value: [f32; 3]) -> Self {
        Vec3 {
            x: value[0],
            y: value[1],
            z: value[2],
        }
    }
}

fn build_initial_sector_states(
    snapshot: &LuaGeometrySnapshot,
) -> BTreeMap<String, BTreeMap<String, bool>> {
    let mut states = BTreeMap::new();
    for set in &snapshot.sets {
        let entry = states
            .entry(set.set_file.clone())
            .or_insert_with(BTreeMap::new);
        for sector in &set.sectors {
            entry.insert(sector.name.clone(), sector.default_active);
        }
    }
    states
}

fn apply_geometry_calls(
    set_state: &SetState,
    snapshot: &LuaGeometrySnapshot,
    default_set_file: &str,
    states: &mut BTreeMap<String, BTreeMap<String, bool>>,
    issues: &mut GeometryIssues,
) {
    let set_lookup = build_set_lookup(snapshot);
    for application in &set_state.hook_applications {
        for call in &application.geometry_calls {
            if !call.function.eq_ignore_ascii_case("MakeSectorActive") {
                continue;
            }
            let sector_name = match call.arguments.get(0) {
                Some(name) if !name.is_empty() => name.clone(),
                _ => {
                    issues.unresolved_calls.push(UnresolvedCall {
                        function: call.function.clone(),
                        arguments: call.arguments.clone(),
                        triggered_by: call.triggered_by.clone(),
                        trigger_sequence: call.trigger_sequence,
                        reason: "missing sector name argument".to_string(),
                    });
                    continue;
                }
            };

            let active = call
                .arguments
                .get(1)
                .and_then(|value| parse_bool(value))
                .unwrap_or(true);

            let set_file =
                resolve_set_for_call(snapshot, &set_lookup, &sector_name, call, default_set_file);

            let Some(set_file) = set_file else {
                issues.unresolved_calls.push(UnresolvedCall {
                    function: call.function.clone(),
                    arguments: call.arguments.clone(),
                    triggered_by: call.triggered_by.clone(),
                    trigger_sequence: call.trigger_sequence,
                    reason: format!("could not resolve set for sector {sector_name}"),
                });
                continue;
            };

            let entry = states.entry(set_file.clone()).or_insert_with(BTreeMap::new);
            if let Some(slot) = entry.get_mut(&sector_name) {
                *slot = active;
            } else {
                issues.missing_sectors.push(MissingSector {
                    set_file: set_file.clone(),
                    sector: sector_name.clone(),
                    triggered_by: call.triggered_by.clone(),
                    trigger_sequence: call.trigger_sequence,
                });
                entry.insert(sector_name.clone(), active);
            }
        }
    }
}

fn analyze_visibility_calls(
    set_state: &SetState,
    snapshot: &LuaGeometrySnapshot,
    issues: &mut GeometryIssues,
) {
    let mut calls: Vec<&VisibilityCall> = Vec::new();
    for application in &set_state.hook_applications {
        for call in &application.visibility_calls {
            calls.push(call);
        }
    }
    if calls.is_empty() {
        return;
    }

    calls.sort_by_key(|call| call.trigger_sequence);
    analyze_hotlist_expectation(&calls, snapshot, issues);
    analyze_head_control_expectation(&calls, snapshot, issues);
}

fn analyze_hotlist_expectation(
    calls: &[&VisibilityCall],
    snapshot: &LuaGeometrySnapshot,
    issues: &mut GeometryIssues,
) {
    let last_call = calls
        .iter()
        .rev()
        .find(|call| call.function.eq_ignore_ascii_case("build_hotlist"));
    let Some(call) = last_call else {
        return;
    };

    if snapshot.hotlist_handles.is_empty() {
        let expected = format!("build_hotlist({})", call.arguments.join(", "));
        issues.visibility_mismatches.push(VisibilityIssue {
            kind: VisibilityIssueKind::HotlistEmpty,
            expected,
            actual: "runtime reported empty hotlist".to_string(),
            triggered_by: Some(call.triggered_by.clone()),
            trigger_sequence: Some(call.trigger_sequence),
        });
    }
}

fn analyze_head_control_expectation(
    calls: &[&VisibilityCall],
    snapshot: &LuaGeometrySnapshot,
    issues: &mut GeometryIssues,
) {
    let mut head_calls: Vec<&VisibilityCall> = calls
        .iter()
        .filter(|call| call.function.to_ascii_lowercase().contains("head_look_at"))
        .copied()
        .collect();
    if head_calls.is_empty() {
        return;
    }
    head_calls.sort_by_key(|call| call.trigger_sequence);
    let last_call = head_calls.last().unwrap();
    let maybe_argument = last_call.arguments.first().map(|arg| arg.as_str());
    let expected_has_target = maybe_argument
        .map(|arg| !argument_represents_nil(arg))
        .unwrap_or(false);

    let Some(manny) = find_manny_actor(snapshot) else {
        return;
    };
    let actual_has_target = manny
        .head_target
        .as_ref()
        .map(|value| !value.is_empty())
        .unwrap_or(false);

    if expected_has_target != actual_has_target {
        let expected = match maybe_argument {
            Some(arg) => format!("head_look_at({})", arg),
            None => "head_look_at(<missing argument>)".to_string(),
        };
        let actual = match manny.head_target.as_ref() {
            Some(target) if !target.is_empty() => format!("runtime targeting {}", target),
            _ => "runtime cleared head target".to_string(),
        };
        issues.visibility_mismatches.push(VisibilityIssue {
            kind: VisibilityIssueKind::HeadTargetMismatch,
            expected,
            actual,
            triggered_by: Some(last_call.triggered_by.clone()),
            trigger_sequence: Some(last_call.trigger_sequence),
        });
    }
}

const RANGE_TOLERANCE: f32 = 0.01;
const DISTANCE_TOLERANCE: f32 = 0.01;
const ANGLE_TOLERANCE: f32 = 0.1;

fn load_object_predictions(
    data_root: &Path,
    set_state: &SetState,
) -> Result<BTreeMap<String, ObjectPrediction>> {
    let candidate = set_state
        .hook_applications
        .iter()
        .map(|app| app.reference.defined_in.clone())
        .find(|name| name.to_ascii_lowercase().ends_with(".lua"))
        .unwrap_or_else(|| format!("{}.decompiled.lua", set_state.variable_name));
    let script_path = data_root.join(&candidate);
    let source = fs::read_to_string(&script_path)
        .with_context(|| format!("reading set script {}", script_path.display()))?;
    let pattern = format!(
        r#"(?m)^\s*{var}\.(\w+)\s*=\s*Object:create\(\s*{var}\s*,\s*"([^"]+)"\s*,\s*([-0-9.eE]+)\s*,\s*([-0-9.eE]+)\s*,\s*([-0-9.eE]+)\s*,\s*\{{[^}}]*range\s*=\s*([-0-9.eE]+)"#,
        var = regex::escape(&set_state.variable_name),
    );
    let regex = Regex::new(&pattern).context("building object definition pattern")?;
    let mut predictions = BTreeMap::new();
    for caps in regex.captures_iter(&source) {
        let object_path = caps.get(2).unwrap().as_str().to_string();
        let x = parse_component(caps.get(3).unwrap().as_str(), &object_path, "x")?;
        let y = parse_component(caps.get(4).unwrap().as_str(), &object_path, "y")?;
        let z = parse_component(caps.get(5).unwrap().as_str(), &object_path, "z")?;
        let range = parse_component(caps.get(6).unwrap().as_str(), &object_path, "range")?;
        predictions.insert(
            object_path.clone(),
            ObjectPrediction {
                position: Vec3 { x, y, z },
                range,
            },
        );
    }
    Ok(predictions)
}

fn parse_component(value: &str, object_path: &str, label: &str) -> Result<f32> {
    value
        .trim()
        .parse::<f32>()
        .with_context(|| format!("parsing {label} component for {object_path}"))
}

fn analyze_visibility_metrics(
    snapshot: &LuaGeometrySnapshot,
    predictions: &BTreeMap<String, ObjectPrediction>,
    issues: &mut GeometryIssues,
) {
    let Some(manny) = find_manny_actor(snapshot) else {
        return;
    };
    let Some(manny_position) = manny.position else {
        return;
    };
    let manny_vec = Vec3::from(manny_position);

    for visible in &snapshot.visible_objects {
        let Some(prediction) = predictions.get(&visible.name) else {
            continue;
        };

        let range_diff = (visible.range - prediction.range).abs();
        if range_diff > RANGE_TOLERANCE {
            issues.visibility_mismatches.push(VisibilityIssue {
                kind: VisibilityIssueKind::RangeMismatch,
                expected: format!("{} range {:.3}", visible.name, prediction.range),
                actual: format!("{:.3}", visible.range),
                triggered_by: None,
                trigger_sequence: None,
            });
        }

        match visible.distance {
            Some(actual_distance) => {
                let expected_distance = distance_between(manny_vec, prediction.position);
                if (actual_distance - expected_distance).abs() > DISTANCE_TOLERANCE {
                    issues.visibility_mismatches.push(VisibilityIssue {
                        kind: VisibilityIssueKind::DistanceMismatch,
                        expected: format!("{} distance {:.3}", visible.name, expected_distance),
                        actual: format!("{:.3}", actual_distance),
                        triggered_by: None,
                        trigger_sequence: None,
                    });
                }
            }
            None => {
                issues.visibility_mismatches.push(VisibilityIssue {
                    kind: VisibilityIssueKind::DistanceMissing,
                    expected: format!("{} distance expected", visible.name),
                    actual: "runtime omitted distance".to_string(),
                    triggered_by: None,
                    trigger_sequence: None,
                });
            }
        }

        match visible.angle {
            Some(actual_angle) => {
                let expected_angle = heading_between(manny_vec, prediction.position);
                if angle_difference(actual_angle, expected_angle) > ANGLE_TOLERANCE {
                    issues.visibility_mismatches.push(VisibilityIssue {
                        kind: VisibilityIssueKind::AngleMismatch,
                        expected: format!("{} angle {:.2}°", visible.name, expected_angle),
                        actual: format!("{:.2}°", actual_angle),
                        triggered_by: None,
                        trigger_sequence: None,
                    });
                }
            }
            None => {
                issues.visibility_mismatches.push(VisibilityIssue {
                    kind: VisibilityIssueKind::AngleMissing,
                    expected: format!("{} angle expected", visible.name),
                    actual: "runtime omitted angle".to_string(),
                    triggered_by: None,
                    trigger_sequence: None,
                });
            }
        }
    }
}

fn distance_between(a: Vec3, b: Vec3) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let dz = b.z - a.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn heading_between(from: Vec3, to: Vec3) -> f32 {
    let dx = (to.x - from.x) as f64;
    let dy = (to.y - from.y) as f64;
    let mut angle = dy.atan2(dx).to_degrees();
    if angle < 0.0 {
        angle += 360.0;
    }
    angle as f32
}

fn angle_difference(a: f32, b: f32) -> f32 {
    let diff = (a - b).abs();
    if diff > 180.0 {
        360.0 - diff
    } else {
        diff
    }
}

fn argument_represents_nil(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "nil" | "false" | "0")
}

fn find_manny_actor(
    snapshot: &LuaGeometrySnapshot,
) -> Option<&crate::geometry_snapshot::LuaActorSnapshot> {
    snapshot
        .actors
        .values()
        .find(|actor| actor.name.eq_ignore_ascii_case("manny"))
}

fn compare_sector_states(
    snapshot: &LuaGeometrySnapshot,
    expected: &BTreeMap<String, BTreeMap<String, bool>>,
) -> Vec<SectorMismatch> {
    let mut mismatches = Vec::new();
    for set in &snapshot.sets {
        if let Some(expected_map) = expected.get(&set.set_file) {
            for sector in &set.sectors {
                let expected_active = expected_map
                    .get(&sector.name)
                    .copied()
                    .unwrap_or(sector.default_active);
                if expected_active != sector.active {
                    mismatches.push(SectorMismatch {
                        set_file: set.set_file.clone(),
                        sector: sector.name.clone(),
                        expected_active,
                        actual_active: sector.active,
                    });
                }
            }
        }
    }
    mismatches
}

fn report_results(summary: &GeometryDiffSummary) {
    println!("\nGeometry diff against {}:", summary.snapshot_path);

    let issues = &summary.issues;
    let mismatches = &summary.sector_mismatches;

    if mismatches.is_empty()
        && issues.unresolved_calls.is_empty()
        && issues.missing_sectors.is_empty()
        && issues.visibility_mismatches.is_empty()
    {
        println!("  Sector activation matches static timeline expectations.");
        return;
    }

    if !mismatches.is_empty() {
        println!("  Sector mismatches:");
        for mismatch in mismatches {
            println!(
                "    {} :: {} => expected {} but runtime had {}",
                mismatch.set_file,
                mismatch.sector,
                bool_label(mismatch.expected_active),
                bool_label(mismatch.actual_active)
            );
        }
    }

    if !issues.missing_sectors.is_empty() {
        println!("  Sector toggles targeting unseen geometry:");
        for missing in &issues.missing_sectors {
            println!(
                "    {} :: {} ({} step #{})",
                missing.set_file,
                missing.sector,
                format_reference(&missing.triggered_by),
                missing.trigger_sequence
            );
        }
    }

    if !issues.unresolved_calls.is_empty() {
        println!("  Unresolved geometry calls:");
        for call in &issues.unresolved_calls {
            println!(
                "    {}({}) -> {} at {} step #{}",
                call.function,
                call.arguments.join(", "),
                call.reason,
                format_reference(&call.triggered_by),
                call.trigger_sequence
            );
        }
    }
    if !issues.visibility_mismatches.is_empty() {
        println!("  Visibility/head-control mismatches:");
        for mismatch in &issues.visibility_mismatches {
            let location = match (&mismatch.triggered_by, mismatch.trigger_sequence) {
                (Some(reference), Some(seq)) => {
                    format!("{} step #{}", format_reference(reference), seq)
                }
                (Some(reference), None) => format_reference(reference),
                _ => "unknown origin".to_string(),
            };
            match mismatch.kind {
                VisibilityIssueKind::HotlistEmpty => {
                    println!(
                        "    {} -> {} ({})",
                        mismatch.expected, mismatch.actual, location
                    );
                }
                VisibilityIssueKind::HeadTargetMismatch => {
                    println!(
                        "    Head_Control expected {} but {} ({})",
                        mismatch.expected, mismatch.actual, location
                    );
                }
                VisibilityIssueKind::RangeMismatch => {
                    println!(
                        "    Range mismatch: {} vs {} ({})",
                        mismatch.expected, mismatch.actual, location
                    );
                }
                VisibilityIssueKind::DistanceMismatch => {
                    println!(
                        "    Distance mismatch: {} vs {} ({})",
                        mismatch.expected, mismatch.actual, location
                    );
                }
                VisibilityIssueKind::AngleMismatch => {
                    println!(
                        "    Angle mismatch: {} vs {} ({})",
                        mismatch.expected, mismatch.actual, location
                    );
                }
                VisibilityIssueKind::DistanceMissing => {
                    println!("    Distance missing: {} ({})", mismatch.expected, location);
                }
                VisibilityIssueKind::AngleMissing => {
                    println!("    Angle missing: {} ({})", mismatch.expected, location);
                }
            }
        }
    }
}

fn write_summary(path: &Path, summary: &GeometryDiffSummary) -> Result<()> {
    let json = serde_json::to_string_pretty(summary)
        .context("serializing geometry diff summary to JSON")?;
    fs::write(path, json)
        .with_context(|| format!("writing geometry diff summary to {}", path.display()))?;
    println!("Saved geometry diff summary to {}", path.display());
    Ok(())
}

fn bool_label(value: bool) -> &'static str {
    if value {
        "ACTIVE"
    } else {
        "INACTIVE"
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn build_set_lookup(snapshot: &LuaGeometrySnapshot) -> BTreeMap<String, BTreeSet<String>> {
    let mut lookup: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for set in &snapshot.sets {
        let mut aliases = BTreeSet::new();
        aliases.insert(set.set_file.to_ascii_lowercase());
        if let Some(variable) = &set.variable_name {
            aliases.insert(variable.to_ascii_lowercase());
        }
        if let Some(display) = &set.display_name {
            aliases.insert(display.to_ascii_lowercase());
        }
        aliases.insert(strip_extension(&set.set_file));
        lookup.insert(set.set_file.clone(), aliases);
    }
    lookup
}

fn resolve_set_for_call(
    snapshot: &LuaGeometrySnapshot,
    lookup: &BTreeMap<String, BTreeSet<String>>,
    sector_name: &str,
    call: &GeometryCall,
    default_set_file: &str,
) -> Option<String> {
    if let Some(set_hint) = call.arguments.get(2) {
        let hint = set_hint.to_ascii_lowercase();
        for (set_file, aliases) in lookup {
            if aliases.contains(&hint) {
                return Some(set_file.clone());
            }
        }
    }

    let mut matches: Vec<&LuaSetSnapshot> = snapshot
        .sets
        .iter()
        .filter(|set| {
            set.sectors
                .iter()
                .any(|sector| sector.name.eq_ignore_ascii_case(sector_name))
        })
        .collect();

    if matches.is_empty() && !default_set_file.is_empty() {
        return Some(default_set_file.to_string());
    }

    matches.sort_by_key(|set| &set.set_file);
    matches.first().map(|set| set.set_file.clone())
}

fn strip_extension(value: &str) -> String {
    value
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_ascii_lowercase())
        .unwrap_or_else(|| value.to_ascii_lowercase())
}

fn format_reference(reference: &HookReference) -> String {
    format!("{} ({})", reference.name, reference.kind.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry_snapshot::{
        LuaActorSnapshot, LuaMusicSnapshot, LuaSectorSnapshot, LuaSetSnapshot, LuaSfxSnapshot,
        LuaVisibleObjectSnapshot,
    };
    use crate::state::{HookApplication, HookReference, SetState, VisibilityCall};
    use grim_analysis::simulation::FunctionSimulation;
    use grim_analysis::timeline::{HookKind, HookTimelineEntry};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn sample_snapshot(active: bool) -> LuaGeometrySnapshot {
        LuaGeometrySnapshot {
            current_set: None,
            selected_actor: None,
            voice_effect: None,
            loaded_sets: vec!["mo.set".to_string()],
            current_setups: BTreeMap::new(),
            sets: vec![LuaSetSnapshot {
                set_file: "mo.set".to_string(),
                variable_name: Some("mo".to_string()),
                display_name: None,
                has_geometry: true,
                current_setup: None,
                setups: Vec::new(),
                sectors: vec![LuaSectorSnapshot {
                    id: 1,
                    name: "door".to_string(),
                    kind: "walk".to_string(),
                    default_active: false,
                    active,
                    vertices: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                    centroid: [0.5, 0.5],
                }],
                active_sectors: BTreeMap::new(),
            }],
            actors: BTreeMap::new(),
            objects: Vec::new(),
            visible_objects: Vec::new(),
            hotlist_handles: Vec::new(),
            inventory: Vec::new(),
            inventory_rooms: Vec::new(),
            commentary: None,
            cut_scenes: Vec::new(),
            music: LuaMusicSnapshot::default(),
            sfx: LuaSfxSnapshot::default(),
            events: Vec::new(),
        }
    }

    fn sample_set_state(active_argument: &str) -> SetState {
        let reference = HookReference {
            name: "enter".to_string(),
            kind: HookKind::Enter,
            defined_in: "test.lua".to_string(),
            defined_at_line: Some(1),
            stage: None,
        };
        let entry = HookTimelineEntry {
            hook_name: reference.name.clone(),
            kind: reference.kind,
            defined_in: reference.defined_in.clone(),
            defined_at_line: reference.defined_at_line,
            parameters: Vec::new(),
            stage: None,
            simulation: FunctionSimulation::default(),
        };
        let geometry_call = GeometryCall {
            function: "MakeSectorActive".to_string(),
            arguments: vec!["door".to_string(), active_argument.to_string()],
            triggered_by: reference.clone(),
            trigger_sequence: 1,
        };
        let application = HookApplication {
            sequence_index: 0,
            entry,
            reference: reference.clone(),
            created_actors: Vec::new(),
            stateful_mutations: Vec::new(),
            ancillary_calls: Vec::new(),
            queued_scripts: Vec::new(),
            queued_movies: Vec::new(),
            geometry_calls: vec![geometry_call],
            visibility_calls: Vec::new(),
        };
        SetState {
            variable_name: "mo".to_string(),
            set_file: "mo.set".to_string(),
            display_name: None,
            actors: BTreeMap::new(),
            subsystems: BTreeMap::new(),
            hook_applications: vec![application],
        }
    }

    #[test]
    fn apply_geometry_calls_tracks_expected_state() {
        let snapshot = sample_snapshot(true);
        let set_state = sample_set_state("TRUE");
        let mut issues = GeometryIssues::default();
        let mut expected = build_initial_sector_states(&snapshot);
        apply_geometry_calls(&set_state, &snapshot, "mo.set", &mut expected, &mut issues);

        assert!(issues.unresolved_calls.is_empty());
        assert!(issues.missing_sectors.is_empty());
        let mismatches = compare_sector_states(&snapshot, &expected);
        assert!(mismatches.is_empty());
    }

    fn visibility_set_state(specs: &[(&str, Vec<&str>, usize)]) -> SetState {
        let reference = HookReference {
            name: "enter".to_string(),
            kind: HookKind::Enter,
            defined_in: "test.lua".to_string(),
            defined_at_line: Some(1),
            stage: None,
        };
        let entry = HookTimelineEntry {
            hook_name: reference.name.clone(),
            kind: reference.kind,
            defined_in: reference.defined_in.clone(),
            defined_at_line: reference.defined_at_line,
            parameters: Vec::new(),
            stage: None,
            simulation: FunctionSimulation::default(),
        };
        let visibility_calls = specs
            .iter()
            .map(|(function, args, seq)| VisibilityCall {
                function: (*function).to_string(),
                arguments: args.iter().map(|s| (*s).to_string()).collect(),
                triggered_by: reference.clone(),
                trigger_sequence: *seq,
            })
            .collect();
        let application = HookApplication {
            sequence_index: 0,
            entry,
            reference: reference.clone(),
            created_actors: Vec::new(),
            stateful_mutations: Vec::new(),
            ancillary_calls: Vec::new(),
            queued_scripts: Vec::new(),
            queued_movies: Vec::new(),
            geometry_calls: Vec::new(),
            visibility_calls,
        };
        SetState {
            variable_name: "mo".to_string(),
            set_file: "mo.set".to_string(),
            display_name: None,
            actors: BTreeMap::new(),
            subsystems: BTreeMap::new(),
            hook_applications: vec![application],
        }
    }

    fn snapshot_with_hotlist_and_manny(
        hotlist: Vec<i64>,
        head_target: Option<&str>,
    ) -> LuaGeometrySnapshot {
        let mut snapshot = sample_snapshot(true);
        snapshot.hotlist_handles = hotlist;
        let manny = LuaActorSnapshot {
            name: "Manny".to_string(),
            costume: None,
            base_costume: None,
            current_set: None,
            at_interest: false,
            position: None,
            rotation: None,
            is_selected: false,
            is_visible: true,
            handle: 1001,
            sectors: BTreeMap::new(),
            costume_stack: Vec::new(),
            current_chore: None,
            walk_chore: None,
            talk_chore: None,
            talk_drop_chore: None,
            mumble_chore: None,
            talk_color: None,
            head_target: head_target.map(|value| value.to_string()),
            head_look_rate: None,
            collision_mode: None,
            ignoring_boxes: false,
            last_chore_costume: None,
            speaking: false,
            last_line: None,
        };
        snapshot.actors.insert("manny".to_string(), manny);
        snapshot
    }

    #[test]
    fn analyze_visibility_flags_empty_hotlist() {
        let set_state = visibility_set_state(&[("Build_Hotlist", vec!["hot_object"], 1)]);
        let snapshot = snapshot_with_hotlist_and_manny(Vec::new(), Some("/motx083/tube"));
        let mut issues = GeometryIssues::default();
        analyze_visibility_calls(&set_state, &snapshot, &mut issues);
        assert_eq!(issues.visibility_mismatches.len(), 1);
        assert!(matches!(
            issues.visibility_mismatches[0].kind,
            VisibilityIssueKind::HotlistEmpty
        ));
    }

    #[test]
    fn analyze_visibility_flags_head_target_mismatch() {
        let set_state =
            visibility_set_state(&[("system.currentActor:head_look_at", vec!["hot_object"], 2)]);
        let snapshot = snapshot_with_hotlist_and_manny(vec![1102], None);
        let mut issues = GeometryIssues::default();
        analyze_visibility_calls(&set_state, &snapshot, &mut issues);
        assert_eq!(issues.visibility_mismatches.len(), 1);
        assert!(matches!(
            issues.visibility_mismatches[0].kind,
            VisibilityIssueKind::HeadTargetMismatch
        ));
    }

    #[test]
    fn load_object_predictions_parses_objects() {
        let dir = tempdir().unwrap();
        let script_path = dir.path().join("mo.decompiled.lua");
        fs::write(
            &script_path,
            r#"mo.tube = Object:create(mo, "/motx083/tube", 0.7, 2.2, 0.25, { range = 0.6 })"#,
        )
        .unwrap();

        let reference = HookReference {
            name: "enter".to_string(),
            kind: HookKind::Enter,
            defined_in: "mo.decompiled.lua".to_string(),
            defined_at_line: Some(1),
            stage: None,
        };
        let entry = HookTimelineEntry {
            hook_name: reference.name.clone(),
            kind: reference.kind,
            defined_in: reference.defined_in.clone(),
            defined_at_line: reference.defined_at_line,
            parameters: Vec::new(),
            stage: None,
            simulation: FunctionSimulation::default(),
        };
        let application = HookApplication {
            sequence_index: 0,
            entry,
            reference: reference.clone(),
            created_actors: Vec::new(),
            stateful_mutations: Vec::new(),
            ancillary_calls: Vec::new(),
            queued_scripts: Vec::new(),
            queued_movies: Vec::new(),
            geometry_calls: Vec::new(),
            visibility_calls: Vec::new(),
        };
        let set_state = SetState {
            variable_name: "mo".to_string(),
            set_file: "mo.set".to_string(),
            display_name: None,
            actors: BTreeMap::new(),
            subsystems: BTreeMap::new(),
            hook_applications: vec![application],
        };

        let predictions = load_object_predictions(dir.path(), &set_state).unwrap();
        let tube = predictions.get("/motx083/tube").expect("tube parsed");
        assert!((tube.position.x - 0.7).abs() < f32::EPSILON);
        assert!((tube.position.y - 2.2).abs() < f32::EPSILON);
        assert!((tube.position.z - 0.25).abs() < f32::EPSILON);
        assert!((tube.range - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn analyze_visibility_metrics_flags_distance_mismatch() {
        let mut snapshot = sample_snapshot(true);
        let manny = LuaActorSnapshot {
            name: "manny".to_string(),
            costume: None,
            base_costume: None,
            current_set: None,
            at_interest: false,
            position: Some([0.0, 0.0, 0.0]),
            rotation: None,
            is_selected: false,
            is_visible: true,
            handle: 1001,
            sectors: BTreeMap::new(),
            costume_stack: Vec::new(),
            current_chore: None,
            walk_chore: None,
            talk_chore: None,
            talk_drop_chore: None,
            mumble_chore: None,
            talk_color: None,
            head_target: None,
            head_look_rate: None,
            collision_mode: None,
            ignoring_boxes: false,
            last_chore_costume: None,
            speaking: false,
            last_line: None,
        };
        snapshot.actors.insert("manny".to_string(), manny);
        snapshot.visible_objects.push(LuaVisibleObjectSnapshot {
            handle: 1102,
            name: "/motx083/tube".to_string(),
            string_name: Some("tube".to_string()),
            display_name: "tube".to_string(),
            range: 0.6,
            distance: Some(1.0),
            angle: Some(10.0),
            within_range: Some(true),
            in_hotlist: false,
        });
        let mut predictions = BTreeMap::new();
        predictions.insert(
            "/motx083/tube".to_string(),
            ObjectPrediction {
                position: Vec3 {
                    x: 2.0,
                    y: 0.0,
                    z: 0.0,
                },
                range: 0.6,
            },
        );
        let mut issues = GeometryIssues::default();
        analyze_visibility_metrics(&snapshot, &predictions, &mut issues);
        assert!(issues
            .visibility_mismatches
            .iter()
            .any(|issue| matches!(issue.kind, VisibilityIssueKind::DistanceMismatch)));
    }

    #[test]
    fn geometry_diff_flags_sector_mismatch() {
        let snapshot = sample_snapshot(false);
        // runtime left sector inactive; static timeline expects activation
        let set_state = sample_set_state("TRUE");
        let mut expected = build_initial_sector_states(&snapshot);
        let mut issues = GeometryIssues::default();
        apply_geometry_calls(&set_state, &snapshot, "mo.set", &mut expected, &mut issues);
        let mismatches = compare_sector_states(&snapshot, &expected);
        assert_eq!(mismatches.len(), 1);
        let mismatch = &mismatches[0];
        assert_eq!(mismatch.set_file, "mo.set");
        assert_eq!(mismatch.sector, "door");
        assert!(mismatch.expected_active);
        assert!(!mismatch.actual_active);
    }
    #[test]
    fn write_summary_exports_json() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("summary.json");
        let summary = GeometryDiffSummary {
            snapshot_path: "snapshot.json".to_string(),
            sector_mismatches: vec![SectorMismatch {
                set_file: "mo.set".to_string(),
                sector: "door".to_string(),
                expected_active: true,
                actual_active: false,
            }],
            issues: GeometryIssues::default(),
        };
        write_summary(&output, &summary).unwrap();
        let contents = fs::read_to_string(&output).unwrap();
        let value: Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(value["snapshot_path"], "snapshot.json");
        assert_eq!(value["sector_mismatches"].as_array().unwrap().len(), 1);
    }
}
