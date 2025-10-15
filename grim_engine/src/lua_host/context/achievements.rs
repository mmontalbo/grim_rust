use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub(super) struct AchievementState {
    pub(super) eligible: bool,
    pub(super) established: bool,
}

#[derive(Debug, Default)]
pub(super) struct AchievementRuntime {
    states: BTreeMap<String, AchievementState>,
}

/// Couples achievement mutations with the engine event log.
pub(super) struct AchievementRuntimeAdapter<'a> {
    runtime: &'a mut AchievementRuntime,
    events: &'a mut Vec<String>,
}

/// Provides read-only helpers for achievement queries.
pub(super) struct AchievementRuntimeView<'a> {
    runtime: &'a AchievementRuntime,
}

impl AchievementRuntime {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn set_eligibility(&mut self, id: &str, eligible: bool) -> String {
        let entry = self
            .states
            .entry(id.to_string())
            .or_insert_with(AchievementState::default);
        entry.eligible = eligible;
        entry.established = true;
        let state = if eligible { "eligible" } else { "ineligible" };
        format!("achievement.{id} {state}")
    }

    fn is_eligible(&self, id: &str) -> bool {
        self.states
            .get(id)
            .map(|state| state.eligible)
            .unwrap_or(false)
    }

    fn has_been_established(&self, id: &str) -> bool {
        self.states
            .get(id)
            .map(|state| state.established)
            .unwrap_or(false)
    }
}

impl<'a> AchievementRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut AchievementRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn set_eligibility(&mut self, id: &str, eligible: bool) {
        let message = self.runtime.set_eligibility(id, eligible);
        self.events.push(message);
    }
}

impl<'a> AchievementRuntimeView<'a> {
    pub(super) fn new(runtime: &'a AchievementRuntime) -> Self {
        Self { runtime }
    }

    pub(super) fn is_eligible(&self, id: &str) -> bool {
        self.runtime.is_eligible(id)
    }

    pub(super) fn has_been_established(&self, id: &str) -> bool {
        self.runtime.has_been_established(id)
    }
}
