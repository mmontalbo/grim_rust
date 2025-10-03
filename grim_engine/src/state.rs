use std::collections::{BTreeMap, BTreeSet};

use grim_analysis::simulation::{FunctionSimulation, StateSubsystem};
use grim_analysis::timeline::{
    BootTimeline, HookKind, HookStageContext, HookTimelineEntry, SetTimeline,
};
use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct EngineState {
    pub set: Option<SetState>,
    pub queued_scripts: Vec<ScriptEvent>,
    pub queued_movies: Vec<MovieEvent>,
}

impl EngineState {
    pub fn from_timeline(timeline: &BootTimeline) -> Self {
        let mut state = EngineState::default();

        if let Some(set_timeline) = timeline.default_set.as_ref() {
            let set_state = SetState::from_timeline(set_timeline);
            let mut scripts = Vec::new();
            let mut movies = Vec::new();

            for application in &set_state.hook_applications {
                scripts.extend(application.queued_scripts.iter().cloned());
                movies.extend(application.queued_movies.iter().cloned());
            }

            state.queued_scripts = scripts;
            state.queued_movies = movies;
            state.set = Some(set_state);
        }

        state
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SetState {
    pub variable_name: String,
    pub set_file: String,
    pub display_name: Option<String>,
    pub actors: BTreeMap<String, ActorState>,
    pub subsystems: BTreeMap<StateSubsystem, SubsystemState>,
    pub hook_applications: Vec<HookApplication>,
}

impl SetState {
    pub fn from_timeline(timeline: &SetTimeline) -> Self {
        let mut actors: BTreeMap<String, ActorState> = BTreeMap::new();
        let mut subsystems: BTreeMap<StateSubsystem, SubsystemState> = BTreeMap::new();
        let mut applications: Vec<HookApplication> = Vec::new();
        let mut seen_actors: BTreeSet<String> = BTreeSet::new();

        for hook in &timeline.hooks {
            let application = HookApplication::from_entry(hook.clone());

            for actor in &application.created_actors {
                if seen_actors.insert(actor.clone()) {
                    actors.insert(
                        actor.clone(),
                        ActorState {
                            name: actor.clone(),
                            created_by: application.reference.clone(),
                            method_history: Vec::new(),
                            method_totals: BTreeMap::new(),
                        },
                    );
                }
            }

            apply_stateful_mutations(
                &mut actors,
                &mut subsystems,
                &application.stateful_mutations,
            );

            applications.push(application);
        }

        SetState {
            variable_name: timeline.variable_name.clone(),
            set_file: timeline.set_file.clone(),
            display_name: timeline.display_name.clone(),
            actors,
            subsystems,
            hook_applications: applications,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorState {
    pub name: String,
    pub created_by: HookReference,
    pub method_history: Vec<MethodInvocation>,
    pub method_totals: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookApplication {
    pub entry: HookTimelineEntry,
    pub reference: HookReference,
    pub created_actors: Vec<String>,
    #[allow(dead_code)]
    pub stateful_mutations: Vec<SubsystemMutation>,
    #[allow(dead_code)]
    pub ancillary_calls: Vec<AncillaryCall>,
    pub queued_scripts: Vec<ScriptEvent>,
    pub queued_movies: Vec<MovieEvent>,
}

impl HookApplication {
    fn from_entry(entry: HookTimelineEntry) -> Self {
        let reference = HookReference::from_entry(&entry);
        let simulation = entry.simulation.clone();

        let stateful_mutations = collect_stateful_mutations(&simulation, &reference);
        let ancillary_calls = collect_ancillary_calls(&simulation, &reference);
        let queued_scripts = collect_script_events(&simulation, &reference);
        let queued_movies = collect_movie_events(&simulation, &reference);
        let created_actors = simulation.created_actors.clone();

        HookApplication {
            entry,
            reference,
            created_actors,
            stateful_mutations,
            ancillary_calls,
            queued_scripts,
            queued_movies,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HookReference {
    pub name: String,
    pub kind: HookKind,
    pub defined_in: String,
    pub defined_at_line: Option<usize>,
    pub stage: Option<HookStageContext>,
}

impl HookReference {
    pub fn from_entry(entry: &HookTimelineEntry) -> Self {
        HookReference {
            name: entry.hook_name.clone(),
            kind: entry.kind,
            defined_in: entry.defined_in.clone(),
            defined_at_line: entry.defined_at_line,
            stage: entry.stage.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SubsystemMutation {
    pub subsystem: StateSubsystem,
    pub target: String,
    pub methods: BTreeMap<String, usize>,
    pub triggered_by: HookReference,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct AncillaryCall {
    pub target: String,
    pub methods: BTreeMap<String, usize>,
    pub triggered_by: HookReference,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScriptEvent {
    pub name: String,
    pub triggered_by: HookReference,
}

#[derive(Debug, Clone, Serialize)]
pub struct MovieEvent {
    pub name: String,
    pub triggered_by: HookReference,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodInvocation {
    pub method: String,
    pub count: usize,
    pub triggered_by: HookReference,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SubsystemState {
    pub targets: BTreeMap<String, TargetState>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct TargetState {
    pub name: String,
    pub method_totals: BTreeMap<String, usize>,
    pub method_history: Vec<MethodInvocation>,
    pub first_touched_by: Option<HookReference>,
}

fn collect_stateful_mutations(
    simulation: &FunctionSimulation,
    reference: &HookReference,
) -> Vec<SubsystemMutation> {
    let mut mutations = Vec::new();
    for (subsystem, targets) in &simulation.stateful_calls {
        for (target, methods) in targets {
            mutations.push(SubsystemMutation {
                subsystem: *subsystem,
                target: target.clone(),
                methods: methods.clone(),
                triggered_by: reference.clone(),
            });
        }
    }
    mutations
}

fn collect_ancillary_calls(
    simulation: &FunctionSimulation,
    reference: &HookReference,
) -> Vec<AncillaryCall> {
    let mut calls = Vec::new();
    for (target, methods) in &simulation.method_calls {
        calls.push(AncillaryCall {
            target: target.clone(),
            methods: methods.clone(),
            triggered_by: reference.clone(),
        });
    }
    calls
}

fn collect_script_events(
    simulation: &FunctionSimulation,
    reference: &HookReference,
) -> Vec<ScriptEvent> {
    simulation
        .started_scripts
        .iter()
        .map(|script| ScriptEvent {
            name: script.clone(),
            triggered_by: reference.clone(),
        })
        .collect()
}

fn collect_movie_events(
    simulation: &FunctionSimulation,
    reference: &HookReference,
) -> Vec<MovieEvent> {
    simulation
        .movie_calls
        .iter()
        .map(|movie| MovieEvent {
            name: movie.clone(),
            triggered_by: reference.clone(),
        })
        .collect()
}

fn apply_stateful_mutations(
    actors: &mut BTreeMap<String, ActorState>,
    subsystems: &mut BTreeMap<StateSubsystem, SubsystemState>,
    mutations: &[SubsystemMutation],
) {
    for mutation in mutations {
        if mutation.subsystem == StateSubsystem::Actors {
            apply_actor_mutation(actors, mutation);
        } else {
            let subsystem_state = subsystems
                .entry(mutation.subsystem)
                .or_insert_with(SubsystemState::default);
            apply_subsystem_mutation(subsystem_state, mutation);
        }
    }
}

fn apply_actor_mutation(actors: &mut BTreeMap<String, ActorState>, mutation: &SubsystemMutation) {
    let SubsystemMutation {
        target,
        methods,
        triggered_by,
        ..
    } = mutation;

    let actor_state = actors.entry(target.clone()).or_insert_with(|| ActorState {
        name: target.clone(),
        created_by: triggered_by.clone(),
        method_history: Vec::new(),
        method_totals: BTreeMap::new(),
    });

    for (method, count) in methods {
        apply_method_invocation(
            &mut actor_state.method_totals,
            &mut actor_state.method_history,
            method,
            *count,
            triggered_by,
        );
    }
}

fn apply_subsystem_mutation(state: &mut SubsystemState, mutation: &SubsystemMutation) {
    let SubsystemMutation {
        target,
        methods,
        triggered_by,
        ..
    } = mutation;

    let target_state = state
        .targets
        .entry(target.clone())
        .or_insert_with(|| TargetState {
            name: target.clone(),
            method_totals: BTreeMap::new(),
            method_history: Vec::new(),
            first_touched_by: None,
        });

    if target_state.first_touched_by.is_none() {
        target_state.first_touched_by = Some(triggered_by.clone());
    }

    for (method, count) in methods {
        apply_method_invocation(
            &mut target_state.method_totals,
            &mut target_state.method_history,
            method,
            *count,
            triggered_by,
        );
    }
}

fn apply_method_invocation(
    totals: &mut BTreeMap<String, usize>,
    history: &mut Vec<MethodInvocation>,
    method: &str,
    count: usize,
    triggered_by: &HookReference,
) {
    *totals.entry(method.to_string()).or_insert(0) += count;
    history.push(MethodInvocation {
        method: method.to_string(),
        count,
        triggered_by: triggered_by.clone(),
    });
}
