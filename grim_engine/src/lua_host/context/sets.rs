use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::geometry::{ParsedSetGeometry, SetDescriptor, SetSnapshot};
use grim_analysis::resources::ResourceGraph;

#[derive(Debug)]
pub(crate) struct SetRuntime {
    available_sets: BTreeMap<String, SetDescriptor>,
    loaded_sets: BTreeSet<String>,
    current_setups: BTreeMap<String, i32>,
    current_set: Option<SetSnapshot>,
    set_geometry: BTreeMap<String, ParsedSetGeometry>,
    sector_states: BTreeMap<String, BTreeMap<String, bool>>,
}

/// Couples set runtime mutations with the engine event log.
pub(super) struct SetRuntimeAdapter<'a> {
    runtime: &'a mut SetRuntime,
    events: &'a mut Vec<String>,
}

/// Exposes read-only queries on the set runtime.
pub(super) struct SetRuntimeView<'a> {
    runtime: &'a SetRuntime,
}

impl<'a> SetRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut SetRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn switch_to_set(&mut self, set_file: &str) -> &SetSnapshot {
        let snapshot = self.runtime.switch_to_set(set_file);
        self.events.push(format!("set.switch {set_file}"));
        snapshot
    }

    pub(super) fn mark_set_loaded(&mut self, set_file: &str) -> bool {
        let newly_loaded = self.runtime.mark_set_loaded(set_file);
        if newly_loaded {
            self.events.push(format!("set.load {set_file}"));
        }
        if let Some(message) = self.runtime.ensure_geometry_cached(set_file) {
            self.events.push(message);
        }
        newly_loaded
    }

    pub(super) fn set_sector_active(
        &mut self,
        set_file_hint: Option<&str>,
        sector_name: &str,
        active: bool,
    ) -> SectorToggleResult {
        if let Some(candidate) = set_file_hint.filter(|value| !value.is_empty()) {
            if let Some(message) = self.runtime.ensure_geometry_cached(candidate) {
                self.events.push(message);
            }
        } else if let Some(current) = self.runtime.current_set().map(|set| set.set_file.clone()) {
            if let Some(message) = self.runtime.ensure_geometry_cached(&current) {
                self.events.push(message);
            }
        }

        let result = self
            .runtime
            .set_sector_active(set_file_hint, sector_name, active);

        let state = if active { "on" } else { "off" };
        match &result {
            SectorToggleResult::Applied {
                set_file, sector, ..
            } => {
                self.events
                    .push(format!("sector.active {set_file}:{sector} {state}"));
            }
            SectorToggleResult::NoChange {
                set_file, sector, ..
            } => {
                self.events
                    .push(format!("sector.active {set_file}:{sector} already {state}"));
            }
            SectorToggleResult::NoSet => {}
        }

        result
    }

    pub(super) fn record_current_setup(&mut self, set_file: &str, setup: i32) {
        self.runtime.record_current_setup(set_file, setup);
    }
}

impl<'a> SetRuntimeView<'a> {
    pub(super) fn new(runtime: &'a SetRuntime) -> Self {
        Self { runtime }
    }

    pub(super) fn current_set(&self) -> Option<&SetSnapshot> {
        self.runtime.current_set()
    }

    pub(super) fn is_sector_active(&self, set_file: &str, sector_name: &str) -> bool {
        self.runtime.is_sector_active(set_file, sector_name)
    }

    pub(super) fn current_setup_for(&self, set_file: &str) -> Option<i32> {
        self.runtime.current_setup_for(set_file)
    }
}

#[derive(Debug)]
pub(crate) enum SectorToggleResult {
    Applied {
        set_file: String,
        sector: String,
        known_sector: bool,
    },
    NoChange {
        set_file: String,
        sector: String,
        known_sector: bool,
    },
    NoSet,
}

impl SetRuntime {
    pub(crate) fn new(resources: Rc<ResourceGraph>) -> Self {
        let mut available_sets = BTreeMap::new();
        for meta in &resources.sets {
            available_sets.insert(
                meta.set_file.clone(),
                SetDescriptor {
                    variable_name: meta.variable_name.clone(),
                    display_name: meta.display_name.clone(),
                },
            );
        }

        Self {
            available_sets,
            loaded_sets: BTreeSet::new(),
            current_setups: BTreeMap::new(),
            current_set: None,
            set_geometry: BTreeMap::new(),
            sector_states: BTreeMap::new(),
        }
    }

    pub(crate) fn switch_to_set<'a>(&'a mut self, set_file: &str) -> &'a SetSnapshot {
        let set_key = set_file.to_string();
        let (variable_name, display_name) = match self.available_sets.get(&set_key) {
            Some(descriptor) => (
                descriptor.variable_name.clone(),
                descriptor.display_name.clone(),
            ),
            None => (set_key.clone(), None),
        };
        self.current_set = Some(SetSnapshot {
            set_file: set_key.clone(),
            variable_name,
            display_name,
        });
        self.current_setups.entry(set_key).or_insert(0);
        self.current_set
            .as_ref()
            .expect("current set just assigned")
    }

    pub(crate) fn current_set(&self) -> Option<&SetSnapshot> {
        self.current_set.as_ref()
    }

    pub(crate) fn mark_set_loaded(&mut self, set_file: &str) -> bool {
        self.loaded_sets.insert(set_file.to_string())
    }

    pub(crate) fn ensure_sector_state_map(&mut self, set_file: &str) -> (bool, Option<String>) {
        let geometry_message = self.ensure_geometry_cached(set_file);
        self.sector_states
            .entry(set_file.to_string())
            .or_insert_with(BTreeMap::new);
        (false, geometry_message)
    }

    pub(crate) fn set_sector_active(
        &mut self,
        set_file_hint: Option<&str>,
        sector_name: &str,
        active: bool,
    ) -> SectorToggleResult {
        let set_file = match set_file_hint {
            Some(file) if !file.is_empty() => file.to_string(),
            _ => match self.current_set.as_ref() {
                Some(snapshot) => snapshot.set_file.clone(),
                None => return SectorToggleResult::NoSet,
            },
        };

        let (has_geometry, _) = self.ensure_sector_state_map(&set_file);
        let canonical = self
            .canonical_sector_name(&set_file, sector_name)
            .unwrap_or_else(|| sector_name.to_string());
        let known_sector = has_geometry
            || self
                .sector_states
                .get(&set_file)
                .map(|map| map.contains_key(&canonical))
                .unwrap_or(false);

        let states = self
            .sector_states
            .get_mut(&set_file)
            .expect("sector state map missing after ensure");
        let previous = states.insert(canonical.clone(), active);
        let result = match previous {
            Some(prev) if prev == active => SectorToggleResult::NoChange {
                set_file: set_file.clone(),
                sector: canonical.clone(),
                known_sector,
            },
            _ => SectorToggleResult::Applied {
                set_file: set_file.clone(),
                sector: canonical.clone(),
                known_sector,
            },
        };

        result
    }

    pub(crate) fn is_sector_active(&self, set_file: &str, sector_name: &str) -> bool {
        let key = self
            .canonical_sector_name(set_file, sector_name)
            .unwrap_or_else(|| sector_name.to_string());
        self.sector_states
            .get(set_file)
            .and_then(|map| map.get(&key))
            .copied()
            .unwrap_or(true)
    }

    pub(crate) fn record_current_setup(&mut self, set_file: &str, setup: i32) {
        self.current_setups.insert(set_file.to_string(), setup);
    }

    pub(crate) fn current_setup_for(&self, set_file: &str) -> Option<i32> {
        self.current_setups.get(set_file).copied()
    }

    pub(crate) fn canonical_sector_name(&self, set_file: &str, sector: &str) -> Option<String> {
        let lower = sector.to_ascii_lowercase();
        self.sector_states.get(set_file).and_then(|map| {
            map.keys()
                .find(|name| name.to_ascii_lowercase() == lower)
                .cloned()
        })
    }

    pub(crate) fn ensure_geometry_cached(&mut self, set_file: &str) -> Option<String> {
        if !self.set_geometry.contains_key(set_file) {
            self.set_geometry
                .insert(set_file.to_string(), ParsedSetGeometry::default());
        }
        None
    }
}
