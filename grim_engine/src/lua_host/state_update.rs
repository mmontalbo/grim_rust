use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use grim_stream::{CommentaryState, StateUpdate, TubeState};

use super::context::{EngineContext, EngineContextHandle, TubeStateSnapshot};

/// Tracks incremental state so we can emit compact `StateUpdate` payloads.
pub struct StateUpdateBuilder {
    context_handle: EngineContextHandle,
    event_cursor: usize,
    last_position: Option<[f32; 3]>,
    last_yaw: Option<f32>,
    last_setup: Option<String>,
    last_hotspot: Option<String>,
    last_commentary: Option<CommentaryState>,
    last_tube_state: Option<TubeState>,
    last_movie: Option<String>,
    manny_handle: Option<u32>,
    manny_actor_id: Option<String>,
    sent_initial: bool,
}

impl StateUpdateBuilder {
    pub fn new(context_handle: EngineContextHandle, initial_event_cursor: usize) -> Self {
        Self {
            context_handle,
            event_cursor: initial_event_cursor,
            last_position: None,
            last_yaw: None,
            last_setup: None,
            last_hotspot: None,
            last_commentary: None,
            last_tube_state: None,
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
            commentary_state,
            tube_state,
            events_len,
            mut new_events,
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

            let active_movie_opt = ctx.active_fullscreen_movie();
            let commentary_state = ctx.commentary_snapshot().map(|snapshot| CommentaryState {
                label: snapshot.label,
                active: snapshot.active,
                suppressed_reason: snapshot.suppressed_reason,
            });
            let TubeStateSnapshot { pose, contains } = ctx.tube_state_snapshot();
            let tube_state = if pose.is_some() || contains.is_some() {
                Some(TubeState { pose, contains })
            } else {
                None
            };

            (
                position_opt,
                yaw_opt,
                active_setup_opt,
                active_hotspot_opt,
                commentary_state,
                tube_state,
                events_len,
                new_events,
                active_movie_opt,
            )
        };

        self.event_cursor = events_len;

        let mut changed = !self.sent_initial;

        if commentary_state != self.last_commentary {
            self.last_commentary = commentary_state.clone();
            changed = true;
        }

        if tube_state != self.last_tube_state {
            self.last_tube_state = tube_state.clone();
            changed = true;
        }

        if self.last_position != position_opt {
            self.last_position = position_opt;
            changed = true;
        }

        if self.last_yaw != yaw_opt {
            self.last_yaw = yaw_opt;
            changed = true;
        }

        if self.last_setup.as_deref() != active_setup_opt.as_deref() {
            self.last_setup = active_setup_opt.clone();
            changed = true;
        }

        if self.last_hotspot.as_deref() != active_hotspot_opt.as_deref() {
            self.last_hotspot = active_hotspot_opt.clone();
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
            commentary: self.last_commentary.clone(),
            tube: self.last_tube_state.clone(),
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
