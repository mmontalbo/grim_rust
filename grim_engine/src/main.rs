use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Parser;
use grim_analysis::boot::{run_boot_pipeline, BootRequest};
use grim_analysis::registry::Registry;
use grim_analysis::resources::ResourceGraph;
use grim_analysis::runtime::build_runtime_model;
use grim_analysis::simulation::FunctionSimulation;
use grim_analysis::timeline::{build_boot_timeline, BootTimeline, HookKind, HookTimelineEntry};
use serde::Serialize;

mod assets;
mod lab_collection;
mod state;
use assets::MANNY_OFFICE_ASSETS;
use lab_collection::{collect_assets, AssetReport, LabCollection};
use state::{EngineState, HookApplication, HookReference, SetState};

#[derive(Serialize)]
struct TimelineManifest<'a> {
    timeline: &'a BootTimeline,
    engine_state: &'a EngineState,
}

/// Minimal host prototype that leans on the shared analysis layer.
#[derive(Parser, Debug)]
#[command(
    about = "Prototype host that inspects the new-game boot sequence",
    version
)]
struct Args {
    /// Path to the extracted DATA000 directory
    #[arg(long, default_value = "extracted/DATA000")]
    data_root: PathBuf,

    /// Optional JSON registry file to read/write while simulating the boot
    #[arg(long)]
    registry: Option<PathBuf>,

    /// Print all hook summaries instead of the compact view
    #[arg(long)]
    verbose: bool,

    /// Directory containing LAB archives (default: dev-install)
    #[arg(long)]
    lab_root: Option<PathBuf>,

    /// Optional directory to extract Manny's office assets into
    #[arg(long)]
    extract_assets: Option<PathBuf>,

    /// Path to write the boot timeline JSON report
    #[arg(long)]
    timeline_json: Option<PathBuf>,

    /// Path to write the Manny's Office asset scan JSON manifest
    #[arg(long)]
    asset_manifest: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut registry =
        Registry::from_json_file(args.registry.as_deref()).context("loading registry snapshot")?;
    let resources = ResourceGraph::from_data_root(&args.data_root)
        .context("loading extracted lua resources")?;
    let runtime_model = build_runtime_model(&resources);

    let summary = run_boot_pipeline(
        &mut registry,
        BootRequest { resume_save: false },
        &resources,
    );
    let timeline = build_boot_timeline(&summary, &runtime_model);
    let engine_state = EngineState::from_timeline(&timeline);
    if let Some(path) = args.timeline_json.as_ref() {
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
        Some(set) => describe_starting_set(set, args.verbose),
        None => println!(
            "!! could not locate set entry for {} within parsed runtime metadata",
            summary.default_set
        ),
    }

    if args.lab_root.is_some() || args.extract_assets.is_some() || args.asset_manifest.is_some() {
        let lab_root = args
            .lab_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("dev-install"));
        match LabCollection::load_from_dir(&lab_root) {
            Ok(collection) => {
                let extract_root = args.extract_assets.as_deref();
                match collect_assets(&collection, MANNY_OFFICE_ASSETS, extract_root) {
                    Ok(report) => {
                        if let Some(path) = args.asset_manifest.as_ref() {
                            if let Err(err) = persist_asset_manifest(path, &report) {
                                eprintln!("[grim_engine] asset manifest error: {err:?}");
                            }
                        }
                        println!("\nManny's office asset scan ({})", lab_root.display());
                        for entry in &report.found {
                            println!(
                                "  - {name:<24} {size:>8} bytes @ 0x{offset:08X} <= {archive}",
                                name = entry.asset_name,
                                size = entry.size,
                                offset = entry.offset,
                                archive = entry.archive_path.display()
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
                lab_root.display()
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

    Ok(())
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
fn print_subsystem_states(set: &SetState, verbose: bool) {
    if set.subsystems.is_empty() {
        return;
    }

    println!("\nBoot-time subsystem mutations:");
    for (subsystem, state) in &set.subsystems {
        println!("  [{}]", subsystem);
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
                    "    {name}: {summary} <= {hook} via {method}",
                    name = target.name,
                    hook = format_reference(&invocation.triggered_by),
                    method = method_label
                );
            } else {
                println!("    {}: {}", target.name, summary);
            }
        }
        if !verbose && target_count > limit {
            println!("    ... +{} more targets", target_count - limit);
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
