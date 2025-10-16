use std::{collections::BTreeMap, fs::File, path::PathBuf};

use anyhow::Result;
use clap::Parser;
use grim_analysis::boot::{run_boot_pipeline, BootRequest};
use grim_analysis::registry::Registry;
use grim_analysis::report::{build_runtime_report, HookTriggerReport, ScriptCategory};
use grim_analysis::resources::{ResourceGraph, SetFunction};
use grim_analysis::runtime::build_runtime_model;
use grim_analysis::simulation::{simulate_set_function, FunctionSimulation, StateSubsystem};
use grim_analysis::state_catalog::build_state_catalog;

#[derive(Parser, Debug)]
#[command(author, version, about = "Rust exploration of Grim Fandango's boot flow", long_about = None)]
struct Args {
    /// Path to the DATA000 extraction root (contains _sets.decompiled.lua)
    #[arg(long, default_value = "../extracted/DATA000")]
    data_root: PathBuf,

    /// Optional JSON file with registry overrides (e.g. LastSavedGame)
    #[arg(long)]
    registry: Option<PathBuf>,

    /// Simulate the engine asking to resume the last save slot
    #[arg(long)]
    resume_save: bool,

    /// Optional path to write a JSON report summarizing parsed hooks
    #[arg(long)]
    json_report: Option<PathBuf>,

    /// Optional path to write the state catalog JSON
    #[arg(long)]
    state_catalog_json: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut registry = Registry::from_json_file(args.registry.as_deref())?;
    let resources = ResourceGraph::from_data_root(&args.data_root)?;
    let runtime_model = build_runtime_model(&resources);

    let summary = run_boot_pipeline(
        &mut registry,
        BootRequest {
            resume_save: args.resume_save,
        },
        &resources,
    );

    let runtime_report = build_runtime_report(&summary, &runtime_model);

    if let Some(path) = args.json_report.as_deref() {
        let file = File::create(path)?;
        serde_json::to_writer_pretty(file, &runtime_report)?;
        println!("[grim_analysis] wrote JSON report to {}", path.display());
    }

    if let Some(path) = args.state_catalog_json.as_deref() {
        let catalog = build_state_catalog(&args.data_root, &resources, &runtime_model);
        let file = File::create(path)?;
        serde_json::to_writer_pretty(file, &catalog)?;
        println!("[grim_analysis] wrote state catalog to {}", path.display());
    }

    registry.save()?;

    println!("Developer mode: {}", summary.developer_mode);
    println!("PL developer flag active: {}", summary.pl_mode);
    println!("Default set: {}", summary.default_set);
    println!("Intro cutscene scheduled: {}", summary.time_to_run_intro);
    match summary.resume_save_slot {
        Some(slot) => println!("Resume slot requested: {slot}"),
        None => println!("Starting a fresh game"),
    }

    if let Some(commentary) = registry.read_bool("DirectorsCommentary") {
        println!("Registry commentary flag: {commentary}");
    }
    println!(
        "Resource counts -> years: {}, menus: {}, rooms: {}",
        summary.resource_counts.years, summary.resource_counts.menus, summary.resource_counts.rooms
    );

    println!("\nBoot stages:");
    for (idx, stage) in summary.stages.iter().enumerate() {
        println!("  {:>2}. {}", idx + 1, stage.describe());
    }

    if !resources.menu_scripts.is_empty() {
        println!("\nFirst few menu scripts:");
        for script in resources.menu_scripts.iter().take(5) {
            println!("  - {script}");
        }
    }

    if !resources.room_scripts.is_empty() {
        println!("\nFirst few room scripts:");
        for script in resources.room_scripts.iter().take(5) {
            println!("  - {script}");
        }
        println!(
            "  ... ({} total rooms parsed)",
            resources.room_scripts.len()
        );
    }

    if !resources.sets.is_empty() {
        println!("\nSample sets:");
        for set in resources.sets.iter().take(5) {
            let label = set.display_name.as_deref().unwrap_or("<unnamed>");
            let slot_preview = set
                .setup_slots
                .iter()
                .take(3)
                .map(|slot| format!("{}={}", slot.label, slot.index))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  - {} => {} [var {}] ({} setup slots) from {}{}",
                set.set_file,
                label,
                set.variable_name,
                set.setup_slots.len(),
                set.lua_file,
                if slot_preview.is_empty() {
                    String::new()
                } else {
                    format!(" :: {slot_preview}")
                }
            );
        }
        println!("  ... ({} total sets parsed)", resources.sets.len());
    }

    if !runtime_model.sets.is_empty() {
        println!("\nSet runtime hooks:");
        for runtime_set in runtime_model.sets.iter().take(3) {
            println!(
                "  - {} [{}]",
                runtime_set.variable_name, runtime_set.set_file
            );
            if let Some(title) = &runtime_set.display_name {
                println!("      title: {}", title);
            }
            println!(
                "      enter: {}",
                format_function_reference(runtime_set.hooks.enter.as_ref())
            );
            println!(
                "      exit: {}",
                format_function_reference(runtime_set.hooks.exit.as_ref())
            );
            if let Some(camera) = runtime_set.hooks.camera_change.as_ref() {
                println!(
                    "      camera change: {}",
                    format_function_reference(Some(camera))
                );
            }
            if !runtime_set.hooks.setup_functions.is_empty() {
                let names: Vec<String> = runtime_set
                    .hooks
                    .setup_functions
                    .iter()
                    .take(4)
                    .map(|f| f.name.clone())
                    .collect();
                let remainder = runtime_set
                    .hooks
                    .setup_functions
                    .len()
                    .saturating_sub(names.len());
                if remainder > 0 {
                    println!(
                        "      setup fns: {} (+{} more)",
                        names.join(", "),
                        remainder
                    );
                } else {
                    println!("      setup fns: {}", names.join(", "));
                }
            }
            if !runtime_set.hooks.other_methods.is_empty() {
                println!(
                    "      other methods: {}",
                    runtime_set.hooks.other_methods.len()
                );
            }

            if let Some(enter_function) = runtime_set.hooks.enter.as_ref() {
                let simulation = simulate_set_function(enter_function);
                print_simulation_summary("enter", &simulation);
            }

            if !runtime_set.hooks.setup_functions.is_empty() {
                let setup_limit = 3usize;
                let total_setups = runtime_set.hooks.setup_functions.len();
                for setup_fn in runtime_set.hooks.setup_functions.iter().take(setup_limit) {
                    let simulation = simulate_set_function(setup_fn);
                    let label = format!("{} (setup)", setup_fn.name);
                    print_simulation_summary(&label, &simulation);
                }
                if total_setups > setup_limit {
                    println!(
                        "      ... +{} more setup functions",
                        total_setups - setup_limit
                    );
                }
            }
        }
    }

    if !resources.actors.is_empty() {
        println!("\nSample actors:");
        for actor in resources.actors.iter().take(5) {
            println!(
                "  - {} => {} (from {})",
                actor.variable_name, actor.label, actor.lua_file
            );
        }
        println!("  ... ({} total actors parsed)", resources.actors.len());
    }

    print_dependency_summary(&runtime_report);

    if !runtime_report.unclassified_methods.is_empty() {
        println!("\nTop unclassified method targets:");
        for entry in runtime_report.unclassified_methods.iter().take(5) {
            println!(
                "  - {}: {}",
                entry.target,
                format_method_counts(&entry.methods)
            );
        }
        if runtime_report.unclassified_methods.len() > 5 {
            println!(
                "    ... +{} more targets",
                runtime_report.unclassified_methods.len() - 5
            );
        }
    }

    Ok(())
}

fn format_function_reference(func: Option<&SetFunction>) -> String {
    match func {
        Some(info) => {
            let location = info
                .defined_at_line
                .map(|line| format!("{}:{}", info.defined_in, line))
                .unwrap_or_else(|| info.defined_in.clone());
            let params = if info.parameters.is_empty() {
                String::from("()")
            } else {
                format!("({})", info.parameters.join(", "))
            };
            format!("defined {} {}", location, params)
        }
        None => "missing".to_string(),
    }
}

fn print_simulation_summary(label: &str, simulation: &FunctionSimulation) {
    let has_details = !simulation.created_actors.is_empty()
        || !simulation.stateful_calls.is_empty()
        || !simulation.method_calls.is_empty()
        || !simulation.started_scripts.is_empty()
        || !simulation.movie_calls.is_empty();
    if !has_details {
        return;
    }

    println!("      {label} summary:");
    if !simulation.created_actors.is_empty() {
        println!(
            "        creates actors: {}",
            simulation.created_actors.join(", ")
        );
    }

    if !simulation.stateful_calls.is_empty() {
        println!("        state ops:");
        print_stateful_call_groups(&simulation.stateful_calls, "          ", 4, 5);
    }

    if !simulation.method_calls.is_empty() {
        println!("        other calls:");
        let entries: Vec<(String, BTreeMap<String, usize>)> = simulation
            .method_calls
            .iter()
            .map(|(target, methods)| (target.clone(), methods.clone()))
            .collect();
        print_method_call_entries(&entries, 5, "          ");
    }

    if !simulation.started_scripts.is_empty() {
        println!(
            "        queued scripts: {}",
            simulation.started_scripts.join(", ")
        );
    }

    if !simulation.movie_calls.is_empty() {
        println!("        movies: {}", simulation.movie_calls.join(", "));
    }
}

fn print_dependency_summary(report: &grim_analysis::report::RuntimeReport) {
    let cutscene_scripts: Vec<_> = report
        .script_dependencies
        .iter()
        .filter(|entry| entry.category == ScriptCategory::Cutscene)
        .collect();

    if !cutscene_scripts.is_empty() {
        println!("\nCutscene script triggers:");
        for entry in cutscene_scripts.iter().take(8) {
            println!(
                "  - {} <= {}",
                entry.script,
                format_triggers(&entry.triggered_by, 4)
            );
        }
        if cutscene_scripts.len() > 8 {
            println!(
                "    ... +{} more cutscene scripts",
                cutscene_scripts.len() - 8
            );
        }
    }

    let helper_scripts: Vec<_> = report
        .script_dependencies
        .iter()
        .filter(|entry| entry.category == ScriptCategory::General)
        .collect();

    if !helper_scripts.is_empty() {
        println!("\nOther queued scripts:");
        for entry in helper_scripts.iter().take(6) {
            println!(
                "  - {} <= {}",
                entry.script,
                format_triggers(&entry.triggered_by, 4)
            );
        }
        if helper_scripts.len() > 6 {
            println!("    ... +{} more helper scripts", helper_scripts.len() - 6);
        }
    }

    if !report.movie_dependencies.is_empty() {
        println!("\nMovie playback triggers:");
        for entry in report.movie_dependencies.iter().take(6) {
            println!(
                "  - {} <= {}",
                entry.movie,
                format_triggers(&entry.triggered_by, 4)
            );
        }
        if report.movie_dependencies.len() > 6 {
            println!(
                "    ... +{} more movies",
                report.movie_dependencies.len() - 6
            );
        }
    }
}

fn format_triggers(triggers: &[HookTriggerReport], limit: usize) -> String {
    let mut parts: Vec<String> = triggers
        .iter()
        .take(limit)
        .map(format_single_trigger)
        .collect();
    if triggers.len() > limit {
        parts.push(format!("+{} more", triggers.len() - limit));
    }
    parts.join(", ")
}

fn format_single_trigger(trigger: &HookTriggerReport) -> String {
    let set_label = trigger
        .set_label
        .as_deref()
        .unwrap_or(&trigger.set_variable);
    let mut label = format!("{} ({})::{}", trigger.set_file, set_label, trigger.hook);
    if let Some(line) = trigger.defined_at_line {
        label.push_str(&format!(" @{}:{}", trigger.defined_in, line));
    } else {
        label.push_str(&format!(" @{}", trigger.defined_in));
    }
    label
}

fn format_method_counts(methods: &BTreeMap<String, usize>) -> String {
    let mut parts: Vec<String> = methods
        .iter()
        .take(6)
        .map(|(name, count)| {
            if *count == 1 {
                name.clone()
            } else {
                format!("{name} x{count}")
            }
        })
        .collect();
    let remainder = methods.len().saturating_sub(parts.len());
    if remainder > 0 {
        parts.push(format!("+{remainder} more"));
    }
    parts.join(", ")
}

fn print_method_call_entries(
    entries: &[(String, BTreeMap<String, usize>)],
    limit: usize,
    indent: &str,
) {
    for (target, methods) in entries.iter().take(limit) {
        println!("{indent}{target}: {}", format_method_counts(methods));
    }
    if entries.len() > limit {
        println!("{indent}... +{} more targets", entries.len() - limit);
    }
}

fn print_stateful_call_groups(
    calls: &BTreeMap<StateSubsystem, BTreeMap<String, BTreeMap<String, usize>>>,
    indent: &str,
    subsystem_limit: usize,
    target_limit: usize,
) {
    for (subsystem_idx, (subsystem, targets)) in calls.iter().enumerate() {
        if subsystem_idx >= subsystem_limit {
            println!(
                "{indent}... +{} more subsystems",
                calls.len() - subsystem_limit
            );
            break;
        }

        println!("{indent}[{subsystem}]");
        let entries: Vec<(String, BTreeMap<String, usize>)> = targets
            .iter()
            .map(|(target, methods)| (target.clone(), methods.clone()))
            .collect();
        print_method_call_entries(&entries, target_limit, &format!("{indent}  "));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use anyhow::Result;
    use serde_json::Value;
    use std::path::PathBuf;

    #[test]
    fn runtime_report_serializes_without_unclassified_calls() -> Result<()> {
        let data_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../extracted/DATA000");
        assert!(
            data_root.exists(),
            "expected extracted data at {}",
            data_root.display()
        );

        let mut registry = Registry::default();
        let resources = ResourceGraph::from_data_root(&data_root)?;
        let runtime_model = build_runtime_model(&resources);

        let summary = run_boot_pipeline(
            &mut registry,
            BootRequest { resume_save: false },
            &resources,
        );

        let report = build_runtime_report(&summary, &runtime_model);
        assert!(
            report.unclassified_methods.is_empty(),
            "expected no unclassified methods, found {:?}",
            report.unclassified_methods
        );

        let serialized = serde_json::to_string(&report)?;
        let parsed: Value = serde_json::from_str(&serialized)?;
        assert!(parsed.get("metadata").is_some(), "metadata missing");
        assert!(parsed.get("sets").is_some(), "sets missing");
        assert!(
            parsed.get("unclassified_methods").is_some(),
            "unclassified_methods missing"
        );

        assert_eq!(
            registry.read_string("GrimLastSet"),
            Some("mo.set"),
            "boot pipeline should persist default set"
        );

        Ok(())
    }
}
