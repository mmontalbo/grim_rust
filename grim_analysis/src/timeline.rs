use crate::{
    boot::{BootStage, BootSummary},
    resources::SetFunction,
    runtime::{BootRuntimeModel, RuntimeSet},
    simulation::{simulate_set_function, FunctionSimulation},
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BootTimeline {
    pub stages: Vec<StageTimelineEntry>,
    pub default_set: Option<SetTimeline>,
}

impl BootTimeline {
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("boot timeline serialization should succeed")
    }

    pub fn to_json_string(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StageTimelineEntry {
    pub index: usize,
    pub label: String,
    pub stage: BootStage,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetTimeline {
    pub variable_name: String,
    pub set_file: String,
    pub display_name: Option<String>,
    pub hooks: Vec<HookTimelineEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookTimelineEntry {
    pub hook_name: String,
    pub kind: HookKind,
    pub defined_in: String,
    pub defined_at_line: Option<usize>,
    pub parameters: Vec<String>,
    pub stage: Option<HookStageContext>,
    pub simulation: FunctionSimulation,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookStageContext {
    pub index: usize,
    pub label: String,
    pub stage: BootStage,
    pub prerequisites: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HookKind {
    Enter,
    Exit,
    CameraChange,
    Setup,
    Other,
}

impl HookKind {
    pub fn label(self) -> &'static str {
        match self {
            HookKind::Enter => "enter",
            HookKind::Exit => "exit",
            HookKind::CameraChange => "camera_change",
            HookKind::Setup => "setup",
            HookKind::Other => "other",
        }
    }
}

pub fn build_boot_timeline(summary: &BootSummary, model: &BootRuntimeModel) -> BootTimeline {
    let stages = build_stage_entries(summary);
    let finalize_stage_index = determine_finalize_boot_stage(summary);

    BootTimeline {
        stages: stages.clone(),
        default_set: locate_default_set_timeline(summary, model, &stages, finalize_stage_index),
    }
}

fn build_stage_entries(summary: &BootSummary) -> Vec<StageTimelineEntry> {
    summary
        .stages
        .iter()
        .enumerate()
        .map(|(idx, stage)| StageTimelineEntry {
            index: idx + 1,
            label: stage.describe(),
            stage: stage.clone(),
        })
        .collect()
}

fn determine_finalize_boot_stage(summary: &BootSummary) -> Option<usize> {
    let default_set = summary.default_set.to_ascii_lowercase();
    summary
        .stages
        .iter()
        .enumerate()
        .find_map(|(idx, stage)| match stage {
            BootStage::FinalizeBoot { set } if set.to_ascii_lowercase() == default_set => {
                Some(idx + 1)
            }
            _ => None,
        })
}

fn locate_default_set_timeline(
    summary: &BootSummary,
    model: &BootRuntimeModel,
    stages: &[StageTimelineEntry],
    default_stage_index: Option<usize>,
) -> Option<SetTimeline> {
    let default_set = summary.default_set.to_ascii_lowercase();
    let runtime_set = model
        .sets
        .iter()
        .find(|set| set.set_file.to_ascii_lowercase() == default_set)?;

    Some(build_set_timeline(runtime_set, stages, default_stage_index))
}

fn build_set_timeline(
    runtime_set: &RuntimeSet,
    stages: &[StageTimelineEntry],
    default_stage_index: Option<usize>,
) -> SetTimeline {
    let stage_context = stage_context_for_index(stages, default_stage_index);
    let mut hooks = Vec::new();

    if let Some(function) = runtime_set.hooks.enter.as_ref() {
        hooks.push(build_hook_entry(
            function,
            HookKind::Enter,
            stage_context.as_ref(),
        ));
    }

    for setup in &runtime_set.hooks.setup_functions {
        hooks.push(build_hook_entry(
            setup,
            HookKind::Setup,
            stage_context.as_ref(),
        ));
    }

    if let Some(function) = runtime_set.hooks.camera_change.as_ref() {
        hooks.push(build_hook_entry(
            function,
            HookKind::CameraChange,
            stage_context.as_ref(),
        ));
    }

    SetTimeline {
        variable_name: runtime_set.variable_name.clone(),
        set_file: runtime_set.set_file.clone(),
        display_name: runtime_set.display_name.clone(),
        hooks,
    }
}

fn build_hook_entry(
    function: &SetFunction,
    kind: HookKind,
    stage: Option<&HookStageContext>,
) -> HookTimelineEntry {
    let simulation = simulate_set_function(function);
    HookTimelineEntry {
        hook_name: function.name.clone(),
        kind,
        defined_in: function.defined_in.clone(),
        defined_at_line: function.defined_at_line,
        parameters: function.parameters.clone(),
        stage: stage.cloned(),
        simulation,
    }
}

fn stage_context_for_index(
    stages: &[StageTimelineEntry],
    preferred_index: Option<usize>,
) -> Option<HookStageContext> {
    let entry = preferred_index
        .and_then(|idx| stages.iter().find(|stage| stage.index == idx))
        .or_else(|| stages.last())?;

    let prerequisites = stages
        .iter()
        .filter(|stage| stage.index < entry.index)
        .map(|stage| stage.label.clone())
        .collect();

    Some(HookStageContext {
        index: entry.index,
        label: entry.label.clone(),
        stage: entry.stage.clone(),
        prerequisites,
    })
}
