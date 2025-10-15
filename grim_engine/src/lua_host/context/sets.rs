use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::geometry::{ParsedSetGeometry, SectorHit, SetDescriptor, SetSnapshot, SetupInfo};
use crate::lab_collection::LabCollection;
use grim_analysis::resources::ResourceGraph;
use grim_formats::{SectorKind as SetSectorKind, SetFile as SetFileData};

#[derive(Debug)]
pub(crate) struct SetRuntime {
    verbose: bool,
    available_sets: BTreeMap<String, SetDescriptor>,
    loaded_sets: BTreeSet<String>,
    current_setups: BTreeMap<String, i32>,
    current_set: Option<SetSnapshot>,
    set_geometry: BTreeMap<String, ParsedSetGeometry>,
    sector_states: BTreeMap<String, BTreeMap<String, bool>>,
    lab_collection: Option<Rc<LabCollection>>,
}

/// Couples set runtime mutations with the engine event log.
pub(super) struct SetRuntimeAdapter<'a> {
    runtime: &'a mut SetRuntime,
    events: &'a mut Vec<String>,
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

    pub(super) fn mark_set_loaded(&mut self, set_file: &str) {
        let newly_loaded = self.runtime.mark_set_loaded(set_file);
        if newly_loaded {
            self.events.push(format!("set.load {set_file}"));
        }
        if let Some(message) = self.runtime.ensure_geometry_cached(set_file) {
            self.events.push(message);
        }
    }

    pub(super) fn ensure_sector_state_map(&mut self, set_file: &str) -> bool {
        let (has_geometry, geometry_message) = self.runtime.ensure_sector_state_map(set_file);
        if let Some(message) = geometry_message {
            self.events.push(message);
        }
        has_geometry
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
            SectorToggleResult::Applied { set_file, sector, .. } => {
                self.events
                    .push(format!("sector.active {set_file}:{sector} {state}"));
            }
            SectorToggleResult::NoChange { set_file, sector, .. } => {
                self.events.push(format!(
                    "sector.active {set_file}:{sector} already {state}"
                ));
            }
            SectorToggleResult::NoSet => {}
        }

        result
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SetRuntimeSnapshot {
    pub(crate) current_set: Option<SetSnapshot>,
    pub(crate) loaded_sets: BTreeSet<String>,
    pub(crate) current_setups: BTreeMap<String, i32>,
    pub(crate) available_sets: BTreeMap<String, SetDescriptor>,
    pub(crate) set_geometry: BTreeMap<String, ParsedSetGeometry>,
    pub(crate) sector_states: BTreeMap<String, BTreeMap<String, bool>>,
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
    pub(crate) fn new(
        resources: Rc<ResourceGraph>,
        verbose: bool,
        lab_collection: Option<Rc<LabCollection>>,
    ) -> Self {
        let mut available_sets = BTreeMap::new();
        for meta in &resources.sets {
            let setups = meta
                .setup_slots
                .iter()
                .map(|slot| SetupInfo {
                    label: slot.label.clone(),
                    index: slot.index as i32,
                })
                .collect();
            available_sets.insert(
                meta.set_file.clone(),
                SetDescriptor {
                    variable_name: meta.variable_name.clone(),
                    display_name: meta.display_name.clone(),
                    setups,
                },
            );
        }

        Self {
            verbose,
            available_sets,
            loaded_sets: BTreeSet::new(),
            current_setups: BTreeMap::new(),
            current_set: None,
            set_geometry: BTreeMap::new(),
            sector_states: BTreeMap::new(),
            lab_collection,
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
        if !self.sector_states.contains_key(set_file) {
            let mut map = BTreeMap::new();
            if let Some(geometry) = self.set_geometry.get(set_file) {
                for sector in &geometry.sectors {
                    map.insert(sector.name.clone(), sector.default_active);
                }
            }
            self.sector_states.insert(set_file.to_string(), map);
        } else if let Some(geometry) = self.set_geometry.get(set_file) {
            if let Some(states) = self.sector_states.get_mut(set_file) {
                for sector in &geometry.sectors {
                    states
                        .entry(sector.name.clone())
                        .or_insert(sector.default_active);
                }
            }
        }
        (self.set_geometry.contains_key(set_file), geometry_message)
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
        let known_sector = if has_geometry {
            self.set_geometry
                .get(&set_file)
                .map(|geometry| {
                    geometry
                        .sectors
                        .iter()
                        .any(|poly| poly.name.eq_ignore_ascii_case(&canonical))
                })
                .unwrap_or(false)
        } else {
            false
        };

        let states = self
            .sector_states
            .get_mut(&set_file)
            .expect("sector state map missing after ensure");
        let previous = states.insert(canonical.clone(), active);
        let result = match previous {
            Some(prev) if prev == active => {
                SectorToggleResult::NoChange {
                    set_file: set_file.clone(),
                    sector: canonical.clone(),
                    known_sector,
                }
            }
            _ => {
                SectorToggleResult::Applied {
                    set_file: set_file.clone(),
                    sector: canonical.clone(),
                    known_sector,
                }
            }
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

    pub(crate) fn available_sets(&self) -> &BTreeMap<String, SetDescriptor> {
        &self.available_sets
    }

    pub(crate) fn set_geometry(&self) -> &BTreeMap<String, ParsedSetGeometry> {
        &self.set_geometry
    }

    pub(crate) fn snapshot(&self) -> SetRuntimeSnapshot {
        SetRuntimeSnapshot {
            current_set: self.current_set.clone(),
            loaded_sets: self.loaded_sets.clone(),
            current_setups: self.current_setups.clone(),
            available_sets: self.available_sets.clone(),
            set_geometry: self.set_geometry.clone(),
            sector_states: self.sector_states.clone(),
        }
    }

    pub(crate) fn point_in_active_walk(&self, set_file: &str, point: (f32, f32)) -> bool {
        if let Some(geometry) = self.set_geometry.get(set_file) {
            for sector in geometry
                .sectors
                .iter()
                .filter(|sector| matches!(sector.kind, SetSectorKind::Walk))
            {
                if sector.contains(point) && self.is_sector_active(set_file, &sector.name) {
                    return true;
                }
            }
            return false;
        }
        true
    }

    pub(crate) fn geometry_sector_hit(
        &self,
        raw_kind: &str,
        point: (f32, f32),
    ) -> Option<SectorHit> {
        let current = self.current_set.as_ref()?;
        let geometry = self.set_geometry.get(&current.set_file)?;
        match raw_kind {
            "camera" | "2" | "hot" | "1" => {
                let request = if matches!(raw_kind, "hot" | "1") {
                    "hot"
                } else {
                    "camera"
                };
                if let Some(setup) = geometry.best_setup_for_point(point) {
                    return self.sector_hit_from_setup(&current.set_file, &setup.name, request);
                }
            }
            "walk" | "0" => {
                if let Some(polygon) = geometry.find_polygon(SetSectorKind::Walk, point) {
                    if self.is_sector_active(&current.set_file, &polygon.name) {
                        return Some(SectorHit::new(polygon.id, polygon.name.clone(), "WALK"));
                    }
                }
            }
            _ => {
                if let Some(kind) = match raw_kind {
                    "camera" | "2" => Some(SetSectorKind::Camera),
                    "walk" | "0" => Some(SetSectorKind::Walk),
                    _ => None,
                } {
                    if let Some(polygon) = geometry.find_polygon(kind, point) {
                        if self.is_sector_active(&current.set_file, &polygon.name) {
                            return Some(SectorHit::new(
                                polygon.id,
                                polygon.name.clone(),
                                raw_kind.to_ascii_uppercase(),
                            ));
                        }
                    }
                }
            }
        }
        None
    }

    pub(crate) fn sector_hit_from_setup(
        &self,
        set_file: &str,
        label: &str,
        kind: &str,
    ) -> Option<SectorHit> {
        let descriptor = self.available_sets.get(set_file)?;
        let index = descriptor.setup_index(label)?;
        let kind_upper = match kind {
            "2" => "CAMERA".to_string(),
            "1" => "HOT".to_string(),
            "0" => "WALK".to_string(),
            other => other.to_ascii_uppercase(),
        };
        Some(SectorHit::new(index, label.to_string(), kind_upper))
    }

    pub(crate) fn canonical_sector_name(&self, set_file: &str, sector: &str) -> Option<String> {
        let lower = sector.to_ascii_lowercase();
        if let Some(geometry) = self.set_geometry.get(set_file) {
            if let Some(poly) = geometry
                .sectors
                .iter()
                .find(|poly| poly.name.to_ascii_lowercase() == lower)
            {
                return Some(poly.name.clone());
            }
        }
        self.sector_states.get(set_file).and_then(|map| {
            map.keys()
                .find(|name| name.to_ascii_lowercase() == lower)
                .cloned()
        })
    }

    #[cfg(test)]
    pub(crate) fn insert_geometry_for_tests(
        &mut self,
        set_file: &str,
        geometry: ParsedSetGeometry,
    ) {
        self.set_geometry.insert(set_file.to_string(), geometry);
    }

    pub(crate) fn ensure_geometry_cached(&mut self, set_file: &str) -> Option<String> {
        if self.set_geometry.contains_key(set_file) {
            return None;
        }
        let Some(collection) = &self.lab_collection else {
            return None;
        };
        match collection.find_entry(set_file) {
            Some((archive, entry)) => {
                let bytes = archive.read_entry_bytes(entry);
                match SetFileData::parse(&bytes) {
                    Ok(file) => {
                        let geometry = ParsedSetGeometry::from_set_file(file);
                        if geometry.has_geometry() {
                            let sector_count = geometry.sectors.len();
                            let setup_count = geometry.setups.len();
                            self.sector_states
                                .entry(set_file.to_string())
                                .or_insert_with(|| {
                                    let mut map = BTreeMap::new();
                                    for sector in &geometry.sectors {
                                        map.insert(sector.name.clone(), sector.default_active);
                                    }
                                    map
                                });
                            self.set_geometry.insert(set_file.to_string(), geometry);
                            if self.verbose {
                                return Some(format!(
                                    "set.geometry {set_file} sectors={} setups={}",
                                    sector_count, setup_count
                                ));
                            }
                        } else if self.verbose {
                            eprintln!(
                                "[grim_engine] info: {} contained no geometry data",
                                set_file
                            );
                        }
                    }
                    Err(err) => {
                        if self.verbose {
                            eprintln!(
                                "[grim_engine] warning: failed to parse {}: {:?}",
                                set_file, err
                            );
                        }
                    }
                }
            }
            None => {
                if self.verbose {
                    eprintln!(
                        "[grim_engine] info: no LAB entry for {} when loading geometry",
                        set_file
                    );
                }
            }
        }
        None
    }
}
