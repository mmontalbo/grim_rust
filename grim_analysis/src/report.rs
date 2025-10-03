use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    boot::BootSummary,
    resources::SetFunction,
    runtime::{BootRuntimeModel, RuntimeSet},
    simulation::{simulate_set_function, FunctionSimulation},
};

pub fn build_runtime_report(summary: &BootSummary, model: &BootRuntimeModel) -> RuntimeReport {
    let mut sets = Vec::new();
    let mut unclassified: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut script_dependencies: BTreeMap<String, Vec<HookTriggerReport>> = BTreeMap::new();
    let mut movie_dependencies: BTreeMap<String, Vec<HookTriggerReport>> = BTreeMap::new();

    for runtime_set in &model.sets {
        let mut hooks = HookCollectionReport::default();

        if let Some(function) = runtime_set.hooks.enter.as_ref() {
            let simulation = simulate_set_function(function);
            record_unclassified(&mut unclassified, &simulation.method_calls);
            record_dependencies(
                &mut script_dependencies,
                &mut movie_dependencies,
                runtime_set,
                "enter",
                function,
                &simulation,
            );
            hooks.enter = Some(FunctionReport::from(function, &simulation));
        }

        if let Some(function) = runtime_set.hooks.exit.as_ref() {
            let simulation = simulate_set_function(function);
            record_unclassified(&mut unclassified, &simulation.method_calls);
            record_dependencies(
                &mut script_dependencies,
                &mut movie_dependencies,
                runtime_set,
                "exit",
                function,
                &simulation,
            );
            hooks.exit = Some(FunctionReport::from(function, &simulation));
        }

        if let Some(function) = runtime_set.hooks.camera_change.as_ref() {
            let simulation = simulate_set_function(function);
            record_unclassified(&mut unclassified, &simulation.method_calls);
            record_dependencies(
                &mut script_dependencies,
                &mut movie_dependencies,
                runtime_set,
                "camera_change",
                function,
                &simulation,
            );
            hooks.camera_change = Some(FunctionReport::from(function, &simulation));
        }

        let mut setup_reports = Vec::new();
        for function in &runtime_set.hooks.setup_functions {
            let simulation = simulate_set_function(function);
            record_unclassified(&mut unclassified, &simulation.method_calls);
            record_dependencies(
                &mut script_dependencies,
                &mut movie_dependencies,
                runtime_set,
                &function.name,
                function,
                &simulation,
            );
            setup_reports.push(FunctionReport::from(function, &simulation));
        }
        hooks.setup = setup_reports;

        sets.push(SetReport {
            variable_name: runtime_set.variable_name.clone(),
            set_file: runtime_set.set_file.clone(),
            display_name: runtime_set.display_name.clone(),
            hooks,
        });
    }

    RuntimeReport {
        metadata: ReportMetadata {
            developer_mode: summary.developer_mode,
            pl_mode: summary.pl_mode,
            default_set: summary.default_set.clone(),
            resume_save_slot: summary.resume_save_slot,
            time_to_run_intro: summary.time_to_run_intro,
            resource_counts: ReportResourceCounts {
                years: summary.resource_counts.years,
                menus: summary.resource_counts.menus,
                rooms: summary.resource_counts.rooms,
            },
            boot_stages: summary
                .stages
                .iter()
                .map(|stage| stage.describe())
                .collect(),
        },
        sets,
        unclassified_methods: aggregate_unclassified(unclassified),
        script_dependencies: aggregate_script_dependencies(script_dependencies),
        movie_dependencies: aggregate_movie_dependencies(movie_dependencies),
    }
}

fn record_dependencies(
    script_dependencies: &mut BTreeMap<String, Vec<HookTriggerReport>>,
    movie_dependencies: &mut BTreeMap<String, Vec<HookTriggerReport>>,
    runtime_set: &RuntimeSet,
    hook_label: &str,
    function: &SetFunction,
    simulation: &FunctionSimulation,
) {
    if simulation.started_scripts.is_empty() && simulation.movie_calls.is_empty() {
        return;
    }

    let trigger = HookTriggerReport::from(runtime_set, hook_label, function);

    for script in &simulation.started_scripts {
        script_dependencies
            .entry(script.clone())
            .or_default()
            .push(trigger.clone());
    }

    for movie in &simulation.movie_calls {
        movie_dependencies
            .entry(movie.clone())
            .or_default()
            .push(trigger.clone());
    }
}

fn record_unclassified(
    accumulator: &mut BTreeMap<String, BTreeMap<String, usize>>,
    method_calls: &BTreeMap<String, BTreeMap<String, usize>>,
) {
    for (target, methods) in method_calls {
        let entry = accumulator.entry(target.clone()).or_default();
        for (method, count) in methods {
            *entry.entry(method.clone()).or_insert(0) += count;
        }
    }
}

fn aggregate_unclassified(
    sources: BTreeMap<String, BTreeMap<String, usize>>,
) -> Vec<UnclassifiedMethodReport> {
    let mut entries: Vec<UnclassifiedMethodReport> = sources
        .into_iter()
        .map(|(target, methods)| {
            let total_calls = methods.values().copied().sum();
            UnclassifiedMethodReport {
                target,
                total_calls,
                methods,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.total_calls
            .cmp(&a.total_calls)
            .then_with(|| a.target.cmp(&b.target))
    });
    entries
}

#[derive(Debug, Serialize)]
pub struct RuntimeReport {
    pub metadata: ReportMetadata,
    pub sets: Vec<SetReport>,
    pub unclassified_methods: Vec<UnclassifiedMethodReport>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub script_dependencies: Vec<ScriptDependencyReport>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub movie_dependencies: Vec<MovieDependencyReport>,
}

#[derive(Debug, Serialize)]
pub struct ReportMetadata {
    pub developer_mode: bool,
    pub pl_mode: bool,
    pub default_set: String,
    pub resume_save_slot: Option<i64>,
    pub time_to_run_intro: bool,
    pub resource_counts: ReportResourceCounts,
    pub boot_stages: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ReportResourceCounts {
    pub years: usize,
    pub menus: usize,
    pub rooms: usize,
}

#[derive(Debug, Serialize)]
pub struct SetReport {
    pub variable_name: String,
    pub set_file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub hooks: HookCollectionReport,
}

#[derive(Debug, Default, Serialize)]
pub struct HookCollectionReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enter: Option<FunctionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<FunctionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_change: Option<FunctionReport>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub setup: Vec<FunctionReport>,
}

#[derive(Debug, Serialize)]
pub struct FunctionReport {
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parameters: Vec<String>,
    pub defined_in: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_at_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SerializableFunctionSimulation>,
}

impl FunctionReport {
    fn from(function: &SetFunction, simulation: &FunctionSimulation) -> Self {
        Self {
            name: function.name.clone(),
            parameters: function.parameters.clone(),
            defined_in: function.defined_in.clone(),
            defined_at_line: function.defined_at_line,
            simulation: SerializableFunctionSimulation::from_simulation(simulation),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SerializableFunctionSimulation {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub created_actors: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub stateful_calls: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub method_calls: BTreeMap<String, BTreeMap<String, usize>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub started_scripts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub movie_calls: Vec<String>,
}

impl SerializableFunctionSimulation {
    fn from_simulation(simulation: &FunctionSimulation) -> Option<Self> {
        if simulation.created_actors.is_empty()
            && simulation.stateful_calls.is_empty()
            && simulation.method_calls.is_empty()
            && simulation.started_scripts.is_empty()
            && simulation.movie_calls.is_empty()
        {
            return None;
        }

        let stateful = simulation
            .stateful_calls
            .iter()
            .map(|(subsystem, targets)| (subsystem.to_string(), targets.clone()))
            .collect();

        Some(Self {
            created_actors: simulation.created_actors.clone(),
            stateful_calls: stateful,
            method_calls: simulation.method_calls.clone(),
            started_scripts: simulation.started_scripts.clone(),
            movie_calls: simulation.movie_calls.clone(),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct UnclassifiedMethodReport {
    pub target: String,
    pub total_calls: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub methods: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookTriggerReport {
    pub set_variable: String,
    pub set_file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_label: Option<String>,
    pub hook: String,
    pub defined_in: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_at_line: Option<usize>,
}

impl HookTriggerReport {
    fn from(runtime_set: &RuntimeSet, hook_label: &str, function: &SetFunction) -> Self {
        Self {
            set_variable: runtime_set.variable_name.clone(),
            set_file: runtime_set.set_file.clone(),
            set_label: runtime_set.display_name.clone(),
            hook: hook_label.to_string(),
            defined_in: function.defined_in.clone(),
            defined_at_line: function.defined_at_line,
        }
    }
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScriptCategory {
    Cutscene,
    General,
}

impl ScriptCategory {
    fn classify(name: &str) -> Self {
        if name.to_ascii_lowercase().starts_with("cut_scene.") {
            ScriptCategory::Cutscene
        } else {
            ScriptCategory::General
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ScriptDependencyReport {
    pub script: String,
    pub category: ScriptCategory,
    pub triggered_by: Vec<HookTriggerReport>,
}

#[derive(Debug, Serialize)]
pub struct MovieDependencyReport {
    pub movie: String,
    pub triggered_by: Vec<HookTriggerReport>,
}

fn aggregate_script_dependencies(
    sources: BTreeMap<String, Vec<HookTriggerReport>>,
) -> Vec<ScriptDependencyReport> {
    let mut entries: Vec<ScriptDependencyReport> = sources
        .into_iter()
        .map(|(script, mut triggers)| {
            triggers.sort_by(|a, b| {
                a.set_file
                    .cmp(&b.set_file)
                    .then_with(|| a.hook.cmp(&b.hook))
                    .then_with(|| a.defined_in.cmp(&b.defined_in))
            });
            ScriptDependencyReport {
                category: ScriptCategory::classify(&script),
                script,
                triggered_by: triggers,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.script.cmp(&b.script))
    });
    entries
}

fn aggregate_movie_dependencies(
    sources: BTreeMap<String, Vec<HookTriggerReport>>,
) -> Vec<MovieDependencyReport> {
    let mut entries: Vec<MovieDependencyReport> = sources
        .into_iter()
        .map(|(movie, mut triggers)| {
            triggers.sort_by(|a, b| {
                a.set_file
                    .cmp(&b.set_file)
                    .then_with(|| a.hook.cmp(&b.hook))
                    .then_with(|| a.defined_in.cmp(&b.defined_in))
            });
            MovieDependencyReport {
                movie,
                triggered_by: triggers,
            }
        })
        .collect();

    entries.sort_by(|a, b| a.movie.cmp(&b.movie));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn runtime_report_serializes() {
        let summary = BootSummary {
            developer_mode: true,
            pl_mode: true,
            default_set: "mo.set".to_string(),
            resume_save_slot: Some(2),
            time_to_run_intro: false,
            stages: vec![],
            resource_counts: crate::boot::ResourceCounts {
                years: 1,
                menus: 2,
                rooms: 3,
            },
        };

        let model = BootRuntimeModel { sets: Vec::new() };
        let report = build_runtime_report(&summary, &model);

        let serialized = serde_json::to_string(&report).expect("serialize report");
        let parsed: Value = serde_json::from_str(&serialized).expect("parse report");
        assert!(parsed.get("metadata").is_some());
    }
}
