use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use anyhow::Result;
use grim_stream::{CoverageCounter, StateUpdate};

use super::context::{EngineContext, EngineContextHandle};

/// Tracks incremental state so we can emit compact `StateUpdate` payloads.
pub struct StateUpdateBuilder {
    context_handle: EngineContextHandle,
    event_cursor: usize,
    prev_coverage: BTreeMap<String, u64>,
    last_position: Option<[f32; 3]>,
    last_yaw: Option<f32>,
    last_setup: Option<String>,
    last_hotspot: Option<String>,
    last_movie: Option<String>,
    manny_handle: Option<u32>,
    manny_actor_id: Option<String>,
    sent_initial: bool,
}

impl StateUpdateBuilder {
    pub fn new(
        context_handle: EngineContextHandle,
        initial_event_cursor: usize,
        initial_coverage: BTreeMap<String, u64>,
    ) -> Self {
        Self {
            context_handle,
            event_cursor: initial_event_cursor,
            prev_coverage: initial_coverage,
            last_position: None,
            last_yaw: None,
            last_setup: None,
            last_hotspot: None,
            last_movie: None,
            manny_handle: None,
            manny_actor_id: None,
            sent_initial: false,
        }
    }

    pub fn build(
        &mut self,
        frame: u32,
        context: &Rc<RefCell<EngineContext>>,
    ) -> Result<Option<StateUpdate>> {
        self.ensure_manny_handle();

        let (
            position_opt,
            yaw_opt,
            active_setup_opt,
            active_hotspot_opt,
            events_len,
            mut new_events,
            coverage_samples,
            active_movie_opt,
        ) = {
            let ctx = context.borrow();

            let position_opt = self
                .manny_handle
                .and_then(|handle| ctx.actor_position_by_handle(handle))
                .map(|vec| [vec.x, vec.y, vec.z]);

            let yaw_opt = self
                .manny_handle
                .and_then(|handle| ctx.actor_rotation_by_handle(handle))
                .map(|rot| rot.y);

            let active_setup_opt = ctx.active_setup_label();

            let active_hotspot_opt = self.manny_actor_id.as_ref().and_then(|actor_id| {
                ctx.geometry_sector_name(actor_id, "hot")
                    .or_else(|| ctx.geometry_sector_name(actor_id, "walk"))
            });

            let events = ctx.events();
            let events_len = events.len();
            let new_events = if self.event_cursor < events_len {
                events[self.event_cursor..].to_vec()
            } else {
                Vec::new()
            };

            let coverage_samples: Vec<(String, u64)> = ctx
                .coverage_counts()
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect();

            let active_movie_opt = ctx.active_fullscreen_movie();

            (
                position_opt,
                yaw_opt,
                active_setup_opt,
                active_hotspot_opt,
                events_len,
                new_events,
                coverage_samples,
                active_movie_opt,
            )
        };

        self.event_cursor = events_len;

        let mut coverage_updates = Vec::new();
        for (key, value) in coverage_samples {
            let previous = self.prev_coverage.insert(key.clone(), value);
            if !self.sent_initial || previous != Some(value) {
                coverage_updates.push(CoverageCounter { key, value });
            }
        }

        let mut changed = !self.sent_initial;

        if let Some(pos) = position_opt {
            if self.last_position != Some(pos) {
                self.last_position = Some(pos);
                changed = true;
            }
        }

        if let Some(yaw) = yaw_opt {
            if self.last_yaw != Some(yaw) {
                self.last_yaw = Some(yaw);
                changed = true;
            }
        }

        if let Some(setup) = active_setup_opt.as_ref() {
            if self.last_setup.as_deref() != Some(setup.as_str()) {
                self.last_setup = Some(setup.clone());
                changed = true;
            }
        }

        if let Some(hotspot) = active_hotspot_opt.as_ref() {
            if self.last_hotspot.as_deref() != Some(hotspot.as_str()) {
                self.last_hotspot = Some(hotspot.clone());
                changed = true;
            }
        }

        if !coverage_updates.is_empty() {
            changed = true;
        }

        let mut movie_state_changed = false;
        if active_movie_opt.as_deref() != self.last_movie.as_deref() {
            changed = true;
            movie_state_changed = true;
        }

        if movie_state_changed {
            if let Some(name) = active_movie_opt.as_ref() {
                if !new_events
                    .iter()
                    .any(|event| event.starts_with("cut_scene.fullscreen.start "))
                {
                    new_events.push(format!("cut_scene.fullscreen.start {name}"));
                }
            } else if let Some(previous) = self.last_movie.as_ref() {
                if !new_events
                    .iter()
                    .any(|event| event.starts_with("cut_scene.fullscreen.end "))
                {
                    new_events.push(format!("cut_scene.fullscreen.end {previous}"));
                }
            }
        }

        if active_movie_opt.is_some() {
            changed = true;
        }

        if new_events.is_empty() && !changed {
            return Ok(None);
        }

        self.sent_initial = true;
        self.last_movie = active_movie_opt.clone();

        let update = StateUpdate {
            seq: 0,
            host_time_ns: 0,
            frame: Some(frame),
            position: self.last_position,
            yaw: self.last_yaw,
            active_setup: self.last_setup.clone(),
            active_hotspot: self.last_hotspot.clone(),
            coverage: coverage_updates,
            events: new_events,
            active_movie: self.last_movie.clone(),
        };

        Ok(Some(update))
    }

    fn ensure_manny_handle(&mut self) {
        if self.manny_handle.is_some() {
            return;
        }
        if let Some((handle, id)) = self
            .context_handle
            .resolve_actor_handle(&["manny", "Manny"])
        {
            self.manny_handle = Some(handle);
            self.manny_actor_id = Some(id);
        }
    }
}
