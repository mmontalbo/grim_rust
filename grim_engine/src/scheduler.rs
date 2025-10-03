use std::collections::VecDeque;

use serde::Serialize;

use crate::state::{EngineState, MovieEvent, ScriptEvent};

/// Maintains the boot-time script queue in execution order so the future Rust
/// runtime can drive the original story hooks without Lua.
#[derive(Debug, Clone, Serialize)]
pub struct ScriptScheduler {
    pending: VecDeque<ScriptEvent>,
    history: Vec<ScriptEvent>,
}

impl ScriptScheduler {
    pub fn from_engine_state(state: &EngineState) -> Self {
        Self::new(state.queued_scripts.clone())
    }

    pub fn new<S>(events: S) -> Self
    where
        S: IntoIterator<Item = ScriptEvent>,
    {
        let pending: VecDeque<ScriptEvent> = events.into_iter().collect();
        ScriptScheduler {
            pending,
            history: Vec::new(),
        }
    }

    pub fn next(&mut self) -> Option<ScriptEvent> {
        let event = self.pending.pop_front()?;
        self.history.push(event.clone());
        Some(event)
    }

    #[allow(dead_code)]
    pub fn peek(&self) -> Option<&ScriptEvent> {
        self.pending.front()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[allow(dead_code)]
    pub fn pending(&self) -> impl ExactSizeIterator<Item = &ScriptEvent> {
        self.pending.iter()
    }

    #[allow(dead_code)]
    pub fn history(&self) -> &[ScriptEvent] {
        &self.history
    }
}

/// Mirrors the script scheduler but tracks fullscreen movie requests so the
/// engine can hand them off to a renderer/player.
#[derive(Debug, Clone, Serialize)]
pub struct MovieQueue {
    pending: VecDeque<MovieEvent>,
    history: Vec<MovieEvent>,
}

impl MovieQueue {
    pub fn from_engine_state(state: &EngineState) -> Self {
        Self::new(state.queued_movies.clone())
    }

    pub fn new<M>(events: M) -> Self
    where
        M: IntoIterator<Item = MovieEvent>,
    {
        let pending: VecDeque<MovieEvent> = events.into_iter().collect();
        MovieQueue {
            pending,
            history: Vec::new(),
        }
    }

    pub fn next(&mut self) -> Option<MovieEvent> {
        let event = self.pending.pop_front()?;
        self.history.push(event.clone());
        Some(event)
    }

    #[allow(dead_code)]
    pub fn peek(&self) -> Option<&MovieEvent> {
        self.pending.front()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[allow(dead_code)]
    pub fn pending(&self) -> impl ExactSizeIterator<Item = &MovieEvent> {
        self.pending.iter()
    }

    #[allow(dead_code)]
    pub fn history(&self) -> &[MovieEvent] {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::{MovieQueue, ScriptScheduler};
    use crate::state::{HookReference, MovieEvent, ScriptEvent};
    use grim_analysis::timeline::HookKind;

    fn dummy_reference(name: &str) -> HookReference {
        HookReference {
            name: name.to_string(),
            kind: HookKind::Other,
            defined_in: "dummy.lua".to_string(),
            defined_at_line: Some(42),
            stage: None,
        }
    }

    fn script(name: &str) -> ScriptEvent {
        ScriptEvent {
            name: name.to_string(),
            triggered_by: dummy_reference("hook"),
        }
    }

    fn movie(name: &str) -> MovieEvent {
        MovieEvent {
            name: name.to_string(),
            triggered_by: dummy_reference("hook"),
        }
    }

    #[test]
    fn script_scheduler_preserves_order() {
        let scripts = vec![script("a"), script("b"), script("c")];
        let mut scheduler = ScriptScheduler::new(scripts.clone());
        assert_eq!(scheduler.len(), 3);
        assert_eq!(scheduler.peek().map(|s| s.name.as_str()), Some("a"));

        let mut drained = Vec::new();
        while let Some(event) = scheduler.next() {
            drained.push(event.name.clone());
        }

        assert!(scheduler.is_empty());
        assert_eq!(drained, vec!["a", "b", "c"]);
        assert_eq!(scheduler.history().len(), 3);
    }

    #[test]
    fn movie_queue_tracks_history() {
        let movies = vec![movie("intro"), movie("logos")];
        let mut queue = MovieQueue::new(movies);
        assert_eq!(queue.peek().map(|m| m.name.as_str()), Some("intro"));
        queue.next().expect("first movie present");
        assert_eq!(queue.len(), 1);
        queue.next().expect("second movie present");
        assert!(queue.is_empty());
        assert_eq!(queue.history()[0].name, "intro");
        assert_eq!(queue.history()[1].name, "logos");
    }
}
