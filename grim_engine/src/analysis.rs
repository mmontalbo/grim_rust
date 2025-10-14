use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::assets::MANNY_OFFICE_ASSETS;
use crate::cli::AnalyzeArgs;
use crate::geometry_diff::run_geometry_diff;
use crate::lab_collection::{collect_assets, AssetMetadata, AssetReport, LabCollection};
use crate::lua_host;
use crate::scheduler::{MovieQueue, ScriptScheduler};
use crate::state::{
    EngineState, HookApplication, HookReference, MovieEvent, ScriptEvent, SetState,
    SubsystemReplaySnapshot, SubsystemState,
};

use grim_analysis::boot::{run_boot_pipeline, BootRequest};
use grim_analysis::registry::Registry;
use grim_analysis::resources::ResourceGraph;
use grim_analysis::runtime::build_runtime_model;
use grim_analysis::simulation::{FunctionSimulation, StateSubsystem};
use grim_analysis::timeline::{build_boot_timeline, BootTimeline, HookKind, HookTimelineEntry};

pub fn execute(args: AnalyzeArgs) -> Result<()> {
    let AnalyzeArgs {
        data_root,
        registry,
        verbose,
        lab_root,
        extract_assets,
        timeline_json,
        asset_manifest,
        simulate_scheduler: simulate_scheduler_flag,
        scheduler_json,
        geometry_diff,
        geometry_diff_json,
        lua_geometry_json,
        verify_geometry,
        audio_log_json,
        event_log_json,
        depth_stats_json,
    } = args;

    if let Some(path) = event_log_json.as_ref() {
        eprintln!(
            "[grim_engine] warning: --event-log-json={} ignored without --run-lua",
            path.display()
        );
    }

    if verify_geometry && geometry_diff.is_some() {
        bail!("--geometry-diff cannot be used with --verify-geometry; the snapshot is captured automatically");
    }

    if let Some(path) = depth_stats_json.as_ref() {
        eprintln!(
            "[grim_engine] warning: --depth-stats-json={} ignored without --run-lua",
            path.display()
        );
    }

    if !verify_geometry {
        if let Some(path) = lua_geometry_json.as_ref() {
            eprintln!(
                "[grim_engine] warning: --lua-geometry-json={} ignored without --run-lua",
                path.display()
            );
        }
        if let Some(path) = audio_log_json.as_ref() {
            eprintln!(
                "[grim_engine] warning: --audio-log-json={} ignored without --run-lua",
                path.display()
            );
        }
    }

    if !verify_geometry && geometry_diff.is_none() {
        if let Some(path) = geometry_diff_json.as_ref() {
            eprintln!(
                "[grim_engine] warning: --geometry-diff-json={} ignored without --geometry-diff",
                path.display()
            );
        }
    }

    let mut registry =
        Registry::from_json_file(registry.as_deref()).context("loading registry snapshot")?;
    let resources =
        ResourceGraph::from_data_root(&data_root).context("loading extracted lua resources")?;
    let runtime_model = build_runtime_model(&resources);

    let summary = run_boot_pipeline(
        &mut registry,
        BootRequest { resume_save: false },
        &resources,
    );
    let timeline = build_boot_timeline(&summary, &runtime_model);
    let engine_state = EngineState::from_timeline(&timeline);

    if verify_geometry {
        let (snapshot_path, remove_after) = match lua_geometry_json.as_ref() {
            Some(path) => (path.clone(), false),
            None => (temp_snapshot_path(), true),
        };

        let _ = lua_host::run_boot_sequence(
            &data_root,
            lab_root.as_deref(),
            verbose,
            Some(snapshot_path.as_path()),
            None,
            None,
            None,
        )?;

        run_geometry_diff(
            &timeline,
            &engine_state,
            snapshot_path.as_path(),
            &data_root,
            geometry_diff_json.as_deref(),
        )?;

        if remove_after {
            if let Err(err) = fs::remove_file(&snapshot_path) {
                eprintln!(
                    "[grim_engine] warning: failed to remove temporary geometry snapshot {}: {}",
                    snapshot_path.display(),
                    err
                );
            }
        }
    }
    if let Some(path) = geometry_diff.as_ref() {
        run_geometry_diff(
            &timeline,
            &engine_state,
            path,
            &data_root,
            geometry_diff_json.as_deref(),
        )?;
    }
    if let Some(path) = timeline_json.as_ref() {
        let manifest = TimelineManifest {
            timeline: &timeline,
            engine_state: &engine_state,
        };
        let json = serde_json::to_string_pretty(&manifest)
            .context("serializing boot timeline manifest to JSON")?;
        fs::write(path, json)
            .with_context(|| format!("writing timeline JSON to {}", path.display()))?;
        println!("Saved boot timeline JSON to {}", path.display());
    }

    if let Some(path) = scheduler_json.as_ref() {
        let manifest = SchedulerManifest {
            scripts: &engine_state.queued_scripts,
            movies: &engine_state.queued_movies,
        };
        let json = serde_json::to_string_pretty(&manifest)
            .context("serializing scheduler manifest to JSON")?;
        fs::write(path, json)
            .with_context(|| format!("writing scheduler JSON to {}", path.display()))?;
        println!("Saved scheduler manifest to {}", path.display());
    }

    registry.save()?;

    println!("Boot default set: {}", summary.default_set);
    println!(
        "Intro cutscene scheduled: {} | developer mode: {}",
        summary.time_to_run_intro, summary.developer_mode
    );
    println!(
        "Resources -> years: {} | menus: {} | rooms: {}",
        summary.resource_counts.years, summary.resource_counts.menus, summary.resource_counts.rooms
    );

    println!("\nBoot timeline:");
    for stage in &timeline.stages {
        println!("  {:>2}. {}", stage.index, stage.label);
    }

    match engine_state.set.as_ref() {
        Some(set) => describe_starting_set(set, verbose),
        None => println!(
            "!! could not locate set entry for {} within parsed runtime metadata",
            summary.default_set
        ),
    }

    if lab_root.is_some() || extract_assets.is_some() || asset_manifest.is_some() {
        let lab_root_path = lab_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("dev-install"));
        match LabCollection::load_from_dir(&lab_root_path) {
            Ok(collection) => {
                let extract_root = extract_assets.as_deref();
                match collect_assets(&collection, MANNY_OFFICE_ASSETS, extract_root) {
                    Ok(report) => {
                        if let Some(path) = asset_manifest.as_ref() {
                            if let Err(err) = persist_asset_manifest(path, &report) {
                                eprintln!("[grim_engine] asset manifest error: {err:?}");
                            }
                        }
                        println!("\nManny's office asset scan ({})", lab_root_path.display());
                        for entry in &report.found {
                            let meta = match &entry.metadata {
                                Some(AssetMetadata::Bitmap {
                                    width,
                                    height,
                                    bits_per_pixel,
                                    codec,
                                    frames,
                                    supported,
                                }) => {
                                    let status = if *supported { "classic" } else { "unsupported" };
                                    format!(
                                        " [{}x{} {}bpp codec {codec} frames {} {status}]",
                                        width, height, bits_per_pixel, frames
                                    )
                                }
                                None => String::new(),
                            };
                            println!(
                                "  - {name:<24} {size:>8} bytes @ 0x{offset:08X} <= {archive}{meta}",
                                name = entry.asset_name,
                                size = entry.size,
                                offset = entry.offset,
                                archive = entry.archive_path.display(),
                                meta = meta
                            );
                        }
                        if !report.missing.is_empty() {
                            println!("\nMissing assets:");
                            for missing in &report.missing {
                                println!("  - {missing}");
                            }
                        }
                        if let Some(dest) = extract_root {
                            println!(
                                "\nExtracted {} assets into {}",
                                report.found.len(),
                                dest.display()
                            );
                        }
                    }
                    Err(err) => eprintln!("[grim_engine] asset collection failed: {err:?}"),
                }
            }
            Err(err) => eprintln!(
                "[grim_engine] warning: unable to load LAB archives from {}: {err:?}",
                lab_root_path.display()
            ),
        }
    }

    if !engine_state.queued_scripts.is_empty() {
        println!("\nScripts queued during boot (in order):");
        for event in &engine_state.queued_scripts {
            println!(
                "  - {} <= {}",
                event.name,
                format_reference(&event.triggered_by)
            );
        }
    }

    if !engine_state.queued_movies.is_empty() {
        println!("\nMovies requested during boot (in order):");
        for event in &engine_state.queued_movies {
            println!(
                "  - {} <= {}",
                event.name,
                format_reference(&event.triggered_by)
            );
        }
    }

    print_subsystem_delta_events(&engine_state, verbose);
    print_replayed_subsystem_snapshot(&engine_state.replay_snapshot, verbose);

    if simulate_scheduler_flag {
        simulate_scheduler(&engine_state);
    }

    Ok(())
}

#[derive(Serialize)]
struct TimelineManifest<'a> {
    timeline: &'a BootTimeline,
    engine_state: &'a EngineState,
}

#[derive(Serialize)]
struct SchedulerManifest<'a> {
    scripts: &'a [ScriptEvent],
    movies: &'a [MovieEvent],
}

fn describe_starting_set(set: &SetState, verbose: bool) {
    if let Some(label) = &set.display_name {
        println!("\nStarting set label: {label}");
    }

    println!("Set runtime variable: {}", set.variable_name);

    let limit = if verbose {
        set.hook_applications.len()
    } else {
        set.hook_applications.len().min(6)
    };

    println!("\nBoot-time hooks for {}:", set.set_file);
    for (idx, application) in set.hook_applications.iter().take(limit).enumerate() {
        print_hook_summary(idx, application);
    }

    if !verbose && set.hook_applications.len() > limit {
        println!(
            "  ... +{} additional hooks",
            set.hook_applications.len() - limit
        );
    }

    if !set.actors.is_empty() {
        println!("\nActors staged by boot hooks:");
        for actor in set.actors.values() {
            println!(
                "  - {} (via {})",
                actor.name,
                format_reference(&actor.created_by)
            );
        }
    }

    print_subsystem_states(set, verbose);

    let rollup = build_dependency_rollup(set);
    print_dependency_rollup("Cutscenes", &rollup.cutscenes);
    print_dependency_rollup("Other scripts", &rollup.helper_scripts);
    print_dependency_rollup("Movies", &rollup.movies);
}

fn print_subsystem_delta_events(state: &EngineState, verbose: bool) {
    if state.subsystem_delta_events.is_empty() {
        return;
    }

    println!("\nOrdered subsystem delta events:");
    let event_count = state.subsystem_delta_events.len();
    let display_limit = if verbose {
        event_count
    } else {
        event_count.min(12)
    };

    for event in state.subsystem_delta_events.iter().take(display_limit) {
        let argument_suffix = if event.arguments.is_empty() {
            String::new()
        } else {
            format!("({})", event.arguments.join(", "))
        };
        let method_label = if event.count > 1 && event.arguments.is_empty() {
            format!("{} x{}", event.method, event.count)
        } else {
            format!("{}{}", event.method, argument_suffix)
        };
        println!(
            "  [{subsystem}] {target}: {method} <= {trigger} (hook #{sequence})",
            subsystem = event.subsystem,
            target = event.target,
            method = method_label,
            trigger = format_reference(&event.triggered_by),
            sequence = event.trigger_sequence
        );
    }

    if !verbose && event_count > display_limit {
        println!("  ... +{} more events", event_count - display_limit);
    }
}

fn print_subsystem_states(set: &SetState, verbose: bool) {
    if set.subsystems.is_empty() {
        return;
    }

    println!("\nBoot-time subsystem mutations:");
    print_subsystem_map(&set.subsystems, verbose, "  ");
}

fn print_replayed_subsystem_snapshot(snapshot: &SubsystemReplaySnapshot, verbose: bool) {
    if snapshot.actors.is_empty() && snapshot.subsystems.is_empty() {
        return;
    }

    println!("\nReplayed subsystem snapshot (delta consumer):");

    if !snapshot.actors.is_empty() {
        let actor_total = snapshot.actors.len();
        let display_limit = if verbose {
            actor_total
        } else {
            actor_total.min(4)
        };

        for actor in snapshot.actors.values().take(display_limit) {
            let summary = summarise_method_counts(&actor.method_totals);
            println!(
                "  actor {name}: {summary} (first touched by {hook})",
                name = actor.name,
                hook = format_reference(&actor.created_by)
            );
        }

        if !verbose && actor_total > display_limit {
            println!("  ... +{} more actors", actor_total - display_limit);
        }
    }

    if !snapshot.subsystems.is_empty() {
        if snapshot.actors.is_empty() {
            println!("  subsystems:");
        } else {
            println!("\n  subsystems:");
        }
        print_subsystem_map(&snapshot.subsystems, verbose, "    ");
    }
}

fn print_subsystem_map(
    map: &BTreeMap<StateSubsystem, SubsystemState>,
    verbose: bool,
    indent: &str,
) {
    for (subsystem, state) in map {
        println!(
            "{indent}[{subsystem}]",
            indent = indent,
            subsystem = subsystem
        );
        let target_count = state.targets.len();
        let limit = if verbose {
            target_count
        } else {
            target_count.min(5)
        };

        for target in state.targets.values().take(limit) {
            let summary = summarise_method_counts(&target.method_totals);
            if let Some(invocation) = target.method_history.last() {
                let method_label = if invocation.count > 1 {
                    format!("{} x{}", invocation.method, invocation.count)
                } else {
                    invocation.method.clone()
                };
                println!(
                    "{indent}  {name}: {summary} <= {hook} via {method}",
                    indent = indent,
                    name = target.name,
                    hook = format_reference(&invocation.triggered_by),
                    method = method_label
                );
            } else {
                println!(
                    "{indent}  {name}: {summary}",
                    indent = indent,
                    name = target.name,
                    summary = summary
                );
            }
        }

        if !verbose && target_count > limit {
            println!(
                "{indent}  ... +{} more targets",
                target_count - limit,
                indent = indent
            );
        }
    }
}

fn print_hook_summary(idx: usize, application: &HookApplication) {
    let hook = &application.entry;
    println!(
        "  {:>2}. {} ({})",
        idx + 1,
        hook.hook_name,
        describe_hook_kind(hook.kind)
    );
    if let Some(stage) = &hook.stage {
        println!("      boot stage: #{:>2} {}", stage.index, stage.label);
    }
    print_function_signature(hook);
    print_simulation_details(&hook.simulation);
}

fn describe_hook_kind(kind: HookKind) -> &'static str {
    match kind {
        HookKind::Enter => "enter",
        HookKind::Exit => "exit",
        HookKind::CameraChange => "camera_change",
        HookKind::Setup => "setup",
        HookKind::Other => "hook",
    }
}

fn print_function_signature(hook: &HookTimelineEntry) {
    let params = if hook.parameters.is_empty() {
        String::from("()")
    } else {
        format!("({})", hook.parameters.join(", "))
    };

    if let Some(line) = hook.defined_at_line {
        println!("      defined at {}:{} {}", hook.defined_in, line, params);
    } else {
        println!("      defined in {} {}", hook.defined_in, params);
    }
}

fn print_simulation_details(simulation: &FunctionSimulation) {
    if !simulation.created_actors.is_empty() {
        println!(
            "      creates actors: {}",
            simulation.created_actors.join(", ")
        );
    }

    if !simulation.stateful_calls.is_empty() {
        println!("      state changes:");
        for (subsystem, targets) in simulation.stateful_calls.iter().take(4) {
            println!("        [{}]", subsystem);
            for (target, methods) in targets.iter().take(4) {
                println!("          {target}: {}", summarise_method_counts(methods));
            }
            if targets.len() > 4 {
                println!("          ... +{} more targets", targets.len() - 4);
            }
        }
        if simulation.stateful_calls.len() > 4 {
            println!(
                "        ... +{} more subsystems",
                simulation.stateful_calls.len() - 4
            );
        }
    }

    if !simulation.method_calls.is_empty() {
        println!("      ancillary calls:");
        for (target, methods) in simulation.method_calls.iter().take(4) {
            println!("        {target}: {}", summarise_method_counts(methods));
        }
        if simulation.method_calls.len() > 4 {
            println!(
                "        ... +{} more targets",
                simulation.method_calls.len() - 4
            );
        }
    }

    if !simulation.started_scripts.is_empty() {
        println!(
            "      queued scripts: {}",
            simulation.started_scripts.join(", ")
        );
    }

    if !simulation.movie_calls.is_empty() {
        println!("      movies: {}", simulation.movie_calls.join(", "));
    }

    if !simulation.geometry_calls.is_empty() {
        println!("      geometry calls:");
        for call in simulation.geometry_calls.iter().take(4) {
            println!("        {}({})", call.function, call.arguments.join(", "));
        }
        if simulation.geometry_calls.len() > 4 {
            println!(
                "        ... +{} more calls",
                simulation.geometry_calls.len() - 4
            );
        }
    }
}

fn summarise_method_counts(methods: &BTreeMap<String, usize>) -> String {
    let mut parts: Vec<String> = methods
        .iter()
        .take(5)
        .map(|(method, count)| match *count {
            1 => method.clone(),
            n => format!("{method} x{n}"),
        })
        .collect();
    if methods.len() > 5 {
        parts.push(format!("+{} more", methods.len() - 5));
    }
    parts.join(", ")
}

struct DependencyRollup {
    cutscenes: BTreeMap<String, Vec<String>>,
    helper_scripts: BTreeMap<String, Vec<String>>,
    movies: BTreeMap<String, Vec<String>>,
}

fn build_dependency_rollup(set: &SetState) -> DependencyRollup {
    let mut cutscenes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut helper_scripts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut movies: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for application in &set.hook_applications {
        let hook = &application.entry;
        let reference = format_hook_reference(hook);

        for script in &hook.simulation.started_scripts {
            let target = if is_cutscene_script(script) {
                &mut cutscenes
            } else {
                &mut helper_scripts
            };
            target
                .entry(script.clone())
                .or_default()
                .push(reference.clone());
        }

        for movie in &hook.simulation.movie_calls {
            movies
                .entry(movie.clone())
                .or_default()
                .push(reference.clone());
        }
    }

    DependencyRollup {
        cutscenes,
        helper_scripts,
        movies,
    }
}

fn print_dependency_rollup(title: &str, entries: &BTreeMap<String, Vec<String>>) {
    if entries.is_empty() {
        return;
    }

    println!("\n{title} queued by default set:");
    for (name, hooks) in entries.iter().take(6) {
        println!("  - {name} <= {}", format_hook_refs(hooks));
    }
    if entries.len() > 6 {
        println!("    ... +{} more", entries.len() - 6);
    }
}

fn persist_asset_manifest(path: &Path, report: &AssetReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = report
        .to_json_string()
        .context("serializing asset manifest to JSON")?;
    fs::write(path, json)
        .with_context(|| format!("writing asset manifest to {}", path.display()))?;
    println!("Saved asset manifest to {}", path.display());
    Ok(())
}

fn format_hook_reference(hook: &HookTimelineEntry) -> String {
    let mut label = format!("{}", hook.hook_name);
    if let Some(line) = hook.defined_at_line {
        label.push_str(&format!(" @{}:{}", hook.defined_in, line));
    } else {
        label.push_str(&format!(" @{}", hook.defined_in));
    }
    label
}

fn format_hook_refs(hooks: &[String]) -> String {
    let mut parts: Vec<String> = hooks.iter().take(4).cloned().collect();
    if hooks.len() > 4 {
        parts.push(format!("+{} more", hooks.len() - 4));
    }
    parts.join(", ")
}

fn format_reference(reference: &HookReference) -> String {
    let mut label = format!("{}", reference.name);
    if let Some(line) = reference.defined_at_line {
        label.push_str(&format!(" @{}:{}", reference.defined_in, line));
    } else {
        label.push_str(&format!(" @{}", reference.defined_in));
    }
    if let Some(stage) = &reference.stage {
        label.push_str(&format!(" [stage {}: {}]", stage.index, stage.label));
    }
    match reference.kind {
        HookKind::Enter => label.push_str(" [enter]"),
        HookKind::Exit => label.push_str(" [exit]"),
        HookKind::CameraChange => label.push_str(" [camera_change]"),
        HookKind::Setup => label.push_str(" [setup]"),
        HookKind::Other => {}
    }
    label
}

fn is_cutscene_script(script: &str) -> bool {
    script.to_ascii_lowercase().starts_with("cut_scene.")
}

fn temp_snapshot_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    path.push(format!(
        "grim_engine_geometry_snapshot_{}_{}.json",
        timestamp,
        std::process::id()
    ));
    path
}

fn simulate_scheduler(state: &EngineState) {
    println!("\nSimulated scheduler queue:");
    let mut script_scheduler = ScriptScheduler::from_engine_state(state);
    if script_scheduler.is_empty() {
        println!("  No scripts queued.");
    } else {
        println!("  Script queue:");
        while let Some(event) = script_scheduler.next() {
            println!(
                "    -> {} ({} remaining)",
                event.name,
                script_scheduler.len()
            );
            println!(
                "       triggered by {}",
                format_reference(&event.triggered_by)
            );
        }
    }

    let mut movie_queue = MovieQueue::from_engine_state(state);
    if movie_queue.is_empty() {
        println!("\n  No movies queued.");
    } else {
        println!("\n  Movie queue:");
        while let Some(event) = movie_queue.next() {
            println!("    -> {} ({} remaining)", event.name, movie_queue.len());
            println!(
                "       triggered by {}",
                format_reference(&event.triggered_by)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{fs, path::PathBuf, sync::OnceLock};
    use tempfile::NamedTempFile;

    static BOOT_FIXTURE: OnceLock<(BootTimeline, EngineState)> = OnceLock::new();

    fn boot_fixture() -> &'static (BootTimeline, EngineState) {
        BOOT_FIXTURE.get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let data_root = manifest_dir.join("../extracted/DATA000");
            assert!(
                data_root.exists(),
                "expected extracted data directory at {}",
                data_root.display()
            );

            let mut registry = Registry::default();
            let resources = ResourceGraph::from_data_root(&data_root)
                .expect("failed to load resource graph for test fixture");
            let runtime_model = build_runtime_model(&resources);
            let summary = run_boot_pipeline(
                &mut registry,
                BootRequest { resume_save: false },
                &resources,
            );
            let timeline = build_boot_timeline(&summary, &runtime_model);
            let engine_state = EngineState::from_timeline(&timeline);
            (timeline, engine_state)
        })
    }

    fn dummy_reference(name: &str) -> HookReference {
        HookReference {
            name: name.to_string(),
            kind: HookKind::Other,
            defined_in: "dummy.lua".to_string(),
            defined_at_line: Some(12),
            stage: None,
        }
    }

    #[test]
    fn timeline_manifest_serializes_as_expected() {
        let timeline = BootTimeline {
            stages: Vec::new(),
            default_set: None,
        };
        let engine_state = EngineState::default();
        let manifest = TimelineManifest {
            timeline: &timeline,
            engine_state: &engine_state,
        };

        let value = serde_json::to_value(manifest).expect("manifest serialization");
        let expected = json!({
            "timeline": {
                "stages": [],
                "default_set": null
            },
            "engine_state": {
                "set": null,
                "queued_scripts": [],
                "queued_movies": [],
                "subsystem_deltas": {},
                "subsystem_delta_events": [],
                "replay_snapshot": {
                    "actors": {},
                    "subsystems": {}
                }
            }
        });

        assert_eq!(value, expected);
    }

    #[test]
    fn scheduler_manifest_serializes_as_expected() {
        let reference = dummy_reference("hook");
        let manifest = SchedulerManifest {
            scripts: &[ScriptEvent {
                name: "foo".to_string(),
                triggered_by: reference.clone(),
            }],
            movies: &[MovieEvent {
                name: "intro".to_string(),
                triggered_by: reference.clone(),
            }],
        };

        let value = serde_json::to_value(manifest).expect("scheduler serialization");
        let expected = json!({
            "scripts": [
                {
                    "name": "foo",
                    "triggered_by": {
                        "name": "hook",
                        "kind": "Other",
                        "defined_in": "dummy.lua",
                        "defined_at_line": 12,
                        "stage": null
                    }
                }
            ],
            "movies": [
                {
                    "name": "intro",
                    "triggered_by": {
                        "name": "hook",
                        "kind": "Other",
                        "defined_in": "dummy.lua",
                        "defined_at_line": 12,
                        "stage": null
                    }
                }
            ]
        });

        assert_eq!(value, expected);
    }

    #[test]
    fn scheduler_manifest_matches_fixture() {
        let fixture = boot_fixture();
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_path = manifest_dir.join("tests/fixtures/scheduler_manifest_mo.json");

        let expected: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&fixture_path).expect("failed to read scheduler manifest fixture"),
        )
        .expect("invalid JSON in scheduler manifest fixture");

        let manifest = SchedulerManifest {
            scripts: &fixture.1.queued_scripts,
            movies: &fixture.1.queued_movies,
        };
        let actual =
            serde_json::to_value(manifest).expect("scheduler manifest serialization failed");

        assert_eq!(actual, expected);
    }

    #[test]
    fn verify_geometry_round_trip_matches_static_timeline() -> Result<()> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .expect("workspace root should exist")
            .to_path_buf();

        let data_root = workspace_root.join("extracted").join("DATA000");
        let lab_root = workspace_root.join("dev-install");

        assert!(
            data_root.is_dir(),
            "expected DATA000 at {}",
            data_root.display()
        );
        assert!(
            lab_root.is_dir(),
            "expected dev-install at {}",
            lab_root.display()
        );

        let fixture = boot_fixture();
        let timeline = &fixture.0;
        let engine_state = &fixture.1;

        let snapshot_file = NamedTempFile::new()?;
        let _ = lua_host::run_boot_sequence(
            &data_root,
            Some(lab_root.as_path()),
            false,
            Some(snapshot_file.path()),
            None,
            None,
            None,
        )?;

        let diff_file = NamedTempFile::new()?;
        run_geometry_diff(
            timeline,
            engine_state,
            snapshot_file.path(),
            &data_root,
            Some(diff_file.path()),
        )?;

        let summary_json = fs::read_to_string(diff_file.path())?;
        let summary: Value = serde_json::from_str(&summary_json)?;

        let sectors_are_clean = summary
            .get("sector_mismatches")
            .map(|value| matches!(value, Value::Array(items) if items.is_empty()))
            .unwrap_or(true);
        assert!(
            sectors_are_clean,
            "geometry diff reported sector mismatches: {summary}"
        );

        let issues_are_clean = summary
            .get("issues")
            .and_then(Value::as_object)
            .map(|issues| {
                issues.values().all(|value| match value {
                    Value::Array(items) => items.is_empty(),
                    Value::Null => true,
                    Value::Object(map) => map.is_empty(),
                    _ => false,
                })
            })
            .unwrap_or(true);
        assert!(issues_are_clean, "geometry diff reported issues: {summary}");

        Ok(())
    }
}
