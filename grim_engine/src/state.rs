use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

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
    pub subsystem_deltas: BTreeMap<StateSubsystem, Vec<SubsystemMutation>>,
    pub subsystem_delta_events: Vec<SubsystemDeltaEvent>,
    pub replay_snapshot: SubsystemReplaySnapshot,
}

impl EngineState {
    pub fn from_timeline(timeline: &BootTimeline) -> Self {
        let mut state = EngineState::default();

        if let Some(set_timeline) = timeline.default_set.as_ref() {
            let set_state = SetState::from_timeline(set_timeline);
            let mut scripts = Vec::new();
            let mut movies = Vec::new();
            let mut deltas: BTreeMap<StateSubsystem, Vec<SubsystemMutation>> = BTreeMap::new();
            let mut delta_events: Vec<SubsystemDeltaEvent> = Vec::new();

            for application in &set_state.hook_applications {
                for mutation in &application.stateful_mutations {
                    deltas
                        .entry(mutation.subsystem)
                        .or_default()
                        .push(mutation.clone());
                    if mutation.call_details.is_empty() {
                        for (method, count) in &mutation.methods {
                            delta_events.push(SubsystemDeltaEvent {
                                subsystem: mutation.subsystem,
                                target: mutation.target.clone(),
                                method: method.clone(),
                                arguments: Vec::new(),
                                count: *count,
                                trigger_sequence: application.sequence_index + 1,
                                triggered_by: mutation.triggered_by.clone(),
                                call_index: None,
                            });
                        }
                    } else {
                        for detail in &mutation.call_details {
                            delta_events.push(SubsystemDeltaEvent {
                                subsystem: mutation.subsystem,
                                target: mutation.target.clone(),
                                method: detail.method.clone(),
                                arguments: detail.arguments.clone(),
                                count: 1,
                                trigger_sequence: application.sequence_index + 1,
                                triggered_by: mutation.triggered_by.clone(),
                                call_index: Some(detail.occurrence_index),
                            });
                        }
                    }
                }
                scripts.extend(application.queued_scripts.iter().cloned());
                movies.extend(application.queued_movies.iter().cloned());
            }

            delta_events.sort_by(|a, b| {
                a.trigger_sequence
                    .cmp(&b.trigger_sequence)
                    .then_with(|| a.subsystem.cmp(&b.subsystem))
                    .then_with(|| a.target.cmp(&b.target))
                    .then_with(|| a.call_index.cmp(&b.call_index))
                    .then_with(|| a.method.cmp(&b.method))
            });

            state.queued_scripts = scripts;
            state.queued_movies = movies;
            state.set = Some(set_state);
            state.subsystem_deltas = deltas;
            state.subsystem_delta_events = delta_events;
            state.replay_snapshot =
                SubsystemReplaySnapshot::from_events(&state.subsystem_delta_events);
        }

        state
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SubsystemReplaySnapshot {
    pub actors: BTreeMap<String, ActorState>,
    pub subsystems: BTreeMap<StateSubsystem, SubsystemState>,
}

impl SubsystemReplaySnapshot {
    pub fn from_events(events: &[SubsystemDeltaEvent]) -> Self {
        let mut snapshot = SubsystemReplaySnapshot::default();
        for event in events {
            snapshot.apply_event(event);
        }
        snapshot
    }

    pub fn apply_event(&mut self, event: &SubsystemDeltaEvent) {
        if event.subsystem == StateSubsystem::Actors {
            self.apply_actor_event(event);
        } else {
            self.apply_subsystem_event(event);
        }
    }

    fn apply_actor_event(&mut self, event: &SubsystemDeltaEvent) {
        let entry = self
            .actors
            .entry(event.target.clone())
            .or_insert_with(|| ActorState {
                name: event.target.clone(),
                created_by: event.triggered_by.clone(),
                method_history: Vec::new(),
                method_totals: BTreeMap::new(),
                transform: ActorTransform::default(),
                chore_state: ActorChoreState::default(),
            });

        if entry.method_history.is_empty() {
            entry.created_by = event.triggered_by.clone();
        }

        apply_method_invocation(
            &mut entry.method_totals,
            &mut entry.method_history,
            &event.method,
            event.count,
            &event.triggered_by,
        );

        apply_actor_replay_metadata(entry, event);
    }

    fn apply_subsystem_event(&mut self, event: &SubsystemDeltaEvent) {
        let subsystem_state = self
            .subsystems
            .entry(event.subsystem)
            .or_insert_with(SubsystemState::default);

        let target_state = subsystem_state
            .targets
            .entry(event.target.clone())
            .or_insert_with(|| TargetState {
                name: event.target.clone(),
                method_totals: BTreeMap::new(),
                method_history: Vec::new(),
                first_touched_by: Some(event.triggered_by.clone()),
            });

        if target_state.first_touched_by.is_none() {
            target_state.first_touched_by = Some(event.triggered_by.clone());
        }

        apply_method_invocation(
            &mut target_state.method_totals,
            &mut target_state.method_history,
            &event.method,
            event.count,
            &event.triggered_by,
        );
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

        for (sequence_index, hook) in timeline.hooks.iter().enumerate() {
            let application = HookApplication::from_entry(sequence_index, hook.clone());

            for actor in &application.created_actors {
                if seen_actors.insert(actor.clone()) {
                    actors.insert(
                        actor.clone(),
                        ActorState {
                            name: actor.clone(),
                            created_by: application.reference.clone(),
                            method_history: Vec::new(),
                            method_totals: BTreeMap::new(),
                            transform: ActorTransform::default(),
                            chore_state: ActorChoreState::default(),
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
    pub transform: ActorTransform,
    pub chore_state: ActorChoreState,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ActorTransform {
    pub position: Option<Vector3>,
    pub rotation: Option<Vector3>,
    pub facing_target: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ActorChoreState {
    pub last_played: Option<String>,
    pub last_looping: Option<String>,
    pub last_completed: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub history: Vec<ChoreEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChoreEvent {
    pub name: String,
    pub method: String,
    pub triggered_by: HookReference,
    pub trigger_sequence: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookApplication {
    pub sequence_index: usize,
    pub entry: HookTimelineEntry,
    pub reference: HookReference,
    pub created_actors: Vec<String>,
    pub stateful_mutations: Vec<SubsystemMutation>,
    #[allow(dead_code)]
    pub ancillary_calls: Vec<AncillaryCall>,
    pub queued_scripts: Vec<ScriptEvent>,
    pub queued_movies: Vec<MovieEvent>,
}

impl HookApplication {
    fn from_entry(sequence_index: usize, entry: HookTimelineEntry) -> Self {
        let reference = HookReference::from_entry(&entry);
        let simulation = entry.simulation.clone();

        let stateful_mutations = collect_stateful_mutations(&simulation, &reference);
        let ancillary_calls = collect_ancillary_calls(&simulation, &reference);
        let queued_scripts = collect_script_events(&simulation, &reference);
        let queued_movies = collect_movie_events(&simulation, &reference);
        let created_actors = simulation.created_actors.clone();

        HookApplication {
            sequence_index,
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
    pub call_details: Vec<StatefulCallDetail>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatefulCallDetail {
    pub method: String,
    pub arguments: Vec<String>,
    pub occurrence_index: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubsystemDeltaEvent {
    pub subsystem: StateSubsystem,
    pub target: String,
    pub method: String,
    pub arguments: Vec<String>,
    pub count: usize,
    pub trigger_sequence: usize,
    pub triggered_by: HookReference,
    pub call_index: Option<usize>,
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
    let mut detail_groups: BTreeMap<(StateSubsystem, String), Vec<StatefulCallDetail>> =
        BTreeMap::new();

    for (occurrence_index, event) in simulation.stateful_call_events.iter().enumerate() {
        detail_groups
            .entry((event.subsystem, event.target.clone()))
            .or_default()
            .push(StatefulCallDetail {
                method: event.method.clone(),
                arguments: event.arguments.clone(),
                occurrence_index,
            });
    }

    for (subsystem, targets) in &simulation.stateful_calls {
        for (target, methods) in targets {
            let details = detail_groups
                .remove(&(*subsystem, target.clone()))
                .unwrap_or_default();
            mutations.push(SubsystemMutation {
                subsystem: *subsystem,
                target: target.clone(),
                methods: methods.clone(),
                triggered_by: reference.clone(),
                call_details: details,
            });
        }
    }

    for ((subsystem, target), details) in detail_groups {
        mutations.push(SubsystemMutation {
            subsystem,
            target,
            methods: BTreeMap::new(),
            triggered_by: reference.clone(),
            call_details: details,
        });
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

fn apply_actor_replay_metadata(actor: &mut ActorState, event: &SubsystemDeltaEvent) {
    process_actor_detail(
        actor,
        &event.method,
        &event.arguments,
        &event.triggered_by,
        Some(event.trigger_sequence),
        true,
    );
}

fn apply_actor_call_detail(
    actor_state: &mut ActorState,
    detail: &StatefulCallDetail,
    triggered_by: &HookReference,
) {
    process_actor_detail(
        actor_state,
        &detail.method,
        &detail.arguments,
        triggered_by,
        None,
        false,
    );
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
        transform: ActorTransform::default(),
        chore_state: ActorChoreState::default(),
    });

    for detail in &mutation.call_details {
        apply_actor_call_detail(actor_state, detail, triggered_by);
    }

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

fn process_actor_detail(
    actor: &mut ActorState,
    method: &str,
    args: &[String],
    triggered_by: &HookReference,
    trigger_sequence: Option<usize>,
    record_history: bool,
) {
    let method_lower = method.to_ascii_lowercase();

    match method_lower.as_str() {
        "setpos" | "set_pos" | "set_position" => {
            if let Some(position) = vector3_from_arguments(args) {
                actor.transform.position = Some(position);
            }
        }
        "setrot" | "set_rot" | "set_rotation" => {
            if let Some(rotation) = vector3_from_arguments(args) {
                actor.transform.rotation = Some(rotation);
            }
        }
        "set_face_target" | "set_facing" | "look_at" => {
            if let Some(target) = args.first() {
                let target = target.trim();
                if !target.is_empty() {
                    actor.transform.facing_target = Some(target.to_string());
                }
            }
        }
        "play_chore" => {
            if let Some(name) = args.first() {
                actor.chore_state.last_played = Some(name.clone());
                record_chore_event(
                    actor,
                    method,
                    name,
                    triggered_by,
                    trigger_sequence,
                    record_history,
                );
            }
        }
        "play_chore_looping" => {
            if let Some(name) = args.first() {
                actor.chore_state.last_looping = Some(name.clone());
                actor.chore_state.last_played = Some(name.clone());
                record_chore_event(
                    actor,
                    method,
                    name,
                    triggered_by,
                    trigger_sequence,
                    record_history,
                );
            }
        }
        "complete_chore" => {
            if let Some(name) = args.first() {
                actor.chore_state.last_completed = Some(name.clone());
                record_chore_event(
                    actor,
                    method,
                    name,
                    triggered_by,
                    trigger_sequence,
                    record_history,
                );
            }
        }
        _ => {}
    }
}

fn record_chore_event(
    actor: &mut ActorState,
    method: &str,
    name: &str,
    triggered_by: &HookReference,
    trigger_sequence: Option<usize>,
    record_history: bool,
) {
    if !record_history {
        return;
    }

    actor.chore_state.history.push(ChoreEvent {
        name: name.to_string(),
        method: method.to_string(),
        triggered_by: triggered_by.clone(),
        trigger_sequence: trigger_sequence.unwrap_or(0),
    });
}

fn vector3_from_arguments(args: &[String]) -> Option<Vector3> {
    if args.len() < 3 {
        return None;
    }

    let x = parse_f32_arg(&args[0])?;
    let y = parse_f32_arg(&args[1])?;
    let z = parse_f32_arg(&args[2])?;
    Some(Vector3 { x, y, z })
}

fn parse_f32_arg(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    f32::from_str(trimmed).ok()
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
