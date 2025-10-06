use std::{
    collections::{BTreeMap, BTreeSet},
    convert::TryFrom,
};

use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HookKey {
    pub name: String,
    pub defined_in: Option<String>,
    pub defined_at_line: Option<u32>,
}

impl HookKey {
    pub fn new(name: String, defined_in: Option<String>, defined_at_line: Option<u32>) -> Self {
        Self {
            name,
            defined_in,
            defined_at_line,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HookReference {
    pub key: HookKey,
    pub stage_index: Option<u32>,
    pub stage_label: Option<String>,
}

impl HookReference {
    pub fn name(&self) -> &str {
        &self.key.name
    }

    pub fn defined_in(&self) -> Option<&str> {
        self.key.defined_in.as_deref()
    }

    pub fn defined_at_line(&self) -> Option<u32> {
        self.key.defined_at_line
    }
}

#[derive(Debug, Clone)]
pub struct TimelineStage {
    pub index: u32,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct TimelineHook {
    pub key: HookKey,
    pub kind: Option<String>,
    pub stage_index: Option<u32>,
    pub stage_label: Option<String>,
    pub prerequisites: Vec<String>,
    pub defined_in: Option<String>,
    pub defined_at_line: Option<u32>,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TimelineSummary {
    pub stages: Vec<TimelineStage>,
    pub hooks: Vec<TimelineHook>,
}

#[derive(Debug, Clone, Default)]
pub struct HookLookup {
    map: BTreeMap<HookKey, usize>,
}

impl HookLookup {
    pub fn new(summary: Option<&TimelineSummary>) -> Self {
        let mut map = BTreeMap::new();
        if let Some(summary) = summary {
            for (idx, hook) in summary.hooks.iter().enumerate() {
                map.insert(hook.key.clone(), idx);
            }
        }
        Self { map }
    }

    pub fn find(&self, reference: &HookReference) -> Option<usize> {
        self.map.get(&reference.key).copied()
    }
}

fn value_as_u32(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|raw| u32::try_from(raw).ok())
}

fn extract_prerequisites(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_hook_reference(value: &Value) -> Option<HookReference> {
    let name = value.get("name")?.as_str()?.to_string();
    let defined_in = value
        .get("defined_in")
        .and_then(|field| field.as_str().map(str::to_string));
    let defined_at_line = value.get("defined_at_line").and_then(value_as_u32);
    let stage = value.get("stage");
    let stage_index = stage
        .and_then(|field| field.get("index"))
        .and_then(value_as_u32);
    let stage_label = stage
        .and_then(|field| field.get("label"))
        .and_then(|field| field.as_str().map(str::to_string));

    Some(HookReference {
        key: HookKey::new(name, defined_in, defined_at_line),
        stage_index,
        stage_label,
    })
}

pub fn build_timeline_summary(manifest: &Value) -> Result<Option<TimelineSummary>> {
    let timeline = match manifest.get("timeline") {
        Some(node) => node,
        None => return Ok(None),
    };

    let stages_value = timeline.get("stages");
    let mut stages: Vec<TimelineStage> = stages_value
        .and_then(|field| field.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|entry| {
                    let index = entry.get("index").and_then(value_as_u32)?;
                    let label = entry.get("label")?.as_str()?.to_string();
                    Some(TimelineStage { index, label })
                })
                .collect()
        })
        .unwrap_or_default();
    stages.sort_by(|a, b| a.index.cmp(&b.index));

    let default_set = match timeline.get("default_set") {
        Some(node) => node,
        None => {
            if stages.is_empty() {
                return Ok(None);
            }
            return Ok(Some(TimelineSummary {
                stages,
                hooks: Vec::new(),
            }));
        }
    };

    let hooks_value = default_set.get("hooks");
    let hooks: Vec<TimelineHook> = hooks_value
        .and_then(|field| field.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|entry| {
                    let name = entry.get("hook_name")?.as_str()?.to_string();
                    let defined_in = entry
                        .get("defined_in")
                        .and_then(|field| field.as_str().map(str::to_string));
                    let defined_at_line = entry.get("defined_at_line").and_then(value_as_u32);
                    let kind = entry
                        .get("kind")
                        .and_then(|field| field.as_str().map(str::to_string));
                    let stage = entry.get("stage");
                    let stage_index = stage
                        .and_then(|field| field.get("index"))
                        .and_then(value_as_u32);
                    let stage_label = stage
                        .and_then(|field| field.get("label"))
                        .and_then(|field| field.as_str().map(str::to_string));
                    let prerequisites = stage
                        .and_then(|field| field.get("prerequisites"))
                        .map(extract_prerequisites)
                        .unwrap_or_default();

                    let mut targets: BTreeSet<String> = BTreeSet::new();
                    if let Some(created) = entry
                        .get("simulation")
                        .and_then(|sim| sim.get("created_actors"))
                        .and_then(|list| list.as_array())
                    {
                        for actor in created {
                            if let Some(actor_name) = actor.as_str() {
                                targets.insert(actor_name.to_string());
                            }
                        }
                    }
                    if let Some(events) = entry
                        .get("simulation")
                        .and_then(|sim| sim.get("stateful_call_events"))
                        .and_then(|list| list.as_array())
                    {
                        for event in events {
                            if let Some(target) = event.get("target").and_then(|f| f.as_str()) {
                                targets.insert(target.to_string());
                            }
                        }
                    }

                    let key = HookKey::new(name, defined_in.clone(), defined_at_line);

                    Some(TimelineHook {
                        key,
                        kind,
                        stage_index,
                        stage_label,
                        prerequisites,
                        defined_in,
                        defined_at_line,
                        targets: targets.into_iter().collect(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if hooks.is_empty() && stages.is_empty() {
        return Ok(None);
    }

    Ok(Some(TimelineSummary { stages, hooks }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_extracts_hook_targets() {
        let data: Value = serde_json::from_str(include_str!(
            "../../grim_engine/tests/fixtures/timeline_manifest_mo.json"
        ))
        .expect("fixture parses");
        let summary = build_timeline_summary(&data)
            .expect("summary builds")
            .expect("timeline present");
        assert_eq!(summary.hooks.len(), 3);

        let enter = summary
            .hooks
            .iter()
            .find(|hook| hook.key.name == "enter")
            .expect("enter hook");
        assert_eq!(enter.stage_index, Some(9));
        assert!(enter.targets.iter().any(|target| target == "mo.tube"));
        assert!(
            enter
                .targets
                .iter()
                .any(|target| target == "canister_actor")
        );
    }

    #[test]
    fn hook_lookup_matches_actor_created_by() {
        let data: Value = serde_json::from_str(include_str!(
            "../../grim_engine/tests/fixtures/timeline_manifest_mo.json"
        ))
        .expect("fixture parses");
        let summary = build_timeline_summary(&data)
            .expect("summary builds")
            .expect("timeline present");
        let lookup = HookLookup::new(Some(&summary));

        let created_by = data["engine_state"]["replay_snapshot"]["actors"]["Actor"]
            .get("created_by")
            .expect("created_by exists");
        let reference = parse_hook_reference(created_by).expect("hook reference");
        assert_eq!(reference.stage_index, Some(9));
        let index = lookup.find(&reference).expect("hook found");
        assert_eq!(summary.hooks[index].key.name, reference.key.name);
    }
}
