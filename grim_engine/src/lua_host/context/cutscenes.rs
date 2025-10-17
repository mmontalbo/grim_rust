#[derive(Debug, Clone)]
pub(super) struct CutSceneRecord {
    pub(super) label: Option<String>,
    pub(super) flags: Vec<String>,
    pub(super) set_file: Option<String>,
    pub(super) sector: Option<String>,
    pub(super) suppressed: bool,
}

impl CutSceneRecord {
    pub(super) fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or("<unnamed>")
    }
}

#[derive(Debug, Clone)]
pub(super) struct CommentaryRecord {
    pub(super) label: Option<String>,
    pub(super) object_handle: Option<i64>,
    pub(super) active: bool,
    pub(super) suppressed_reason: Option<String>,
}

impl CommentaryRecord {
    pub(super) fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or("<none>")
    }
}

#[derive(Debug, Clone)]
pub(super) struct OverrideRecord {
    pub(super) description: String,
}

#[derive(Debug, Clone)]
pub(super) struct FullscreenMovieState {
    pub(super) name: String,
    pub(super) play_yields_remaining: u32,
}

const DEFAULT_FULLSCREEN_YIELDS: u32 = 6;

#[derive(Debug, Clone)]
pub(super) struct DialogState {
    pub(super) actor_id: String,
    pub(super) actor_label: String,
    pub(super) line: String,
}

#[derive(Debug, Default, Clone)]
pub(super) struct CutsceneRuntime {
    cut_scene_stack: Vec<CutSceneRecord>,
    override_stack: Vec<OverrideRecord>,
    commentary: Option<CommentaryRecord>,
    active_dialog: Option<DialogState>,
    speaking_actor: Option<String>,
    message_active: bool,
    fullscreen_movie: Option<FullscreenMovieState>,
}

impl CutsceneRuntime {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn push_cut_scene(
        &mut self,
        label: Option<String>,
        flags: Vec<String>,
        set_file: Option<String>,
        sector: Option<String>,
        suppressed: bool,
    ) -> String {
        let display = label
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("<unnamed>")
            .to_string();
        let flag_list = if flags.is_empty() {
            None
        } else {
            Some(flags.join(", "))
        };
        let mut message = if let Some(flags) = flag_list.as_ref() {
            format!("cut_scene.start {} [{}]", display, flags)
        } else {
            format!("cut_scene.start {}", display)
        };
        if suppressed {
            let sector_name = sector
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("<unknown>");
            message.push_str(&format!(" (sector {} inactive)", sector_name));
        }
        self.cut_scene_stack.push(CutSceneRecord {
            label,
            flags,
            set_file,
            sector,
            suppressed,
        });
        message
    }

    pub(super) fn pop_cut_scene(&mut self) -> Option<String> {
        let record = self.cut_scene_stack.pop()?;
        let display = record.display_label().to_string();
        let message = if record.suppressed {
            format!("cut_scene.end {} (suppressed)", display)
        } else {
            format!("cut_scene.end {}", display)
        };
        Some(message)
    }

    pub(super) fn handle_sector_activation(
        &mut self,
        set_file: &str,
        sector: &str,
        active: bool,
    ) -> Vec<String> {
        let mut messages = Vec::new();
        for record in &mut self.cut_scene_stack {
            let matches_set = record
                .set_file
                .as_ref()
                .map(|file| file.eq_ignore_ascii_case(set_file))
                .unwrap_or(false);
            if !matches_set {
                continue;
            }
            if let Some(record_sector) = record.sector.as_ref() {
                if record_sector.eq_ignore_ascii_case(sector) {
                    if active && record.suppressed {
                        record.suppressed = false;
                        messages.push(format!("cut_scene.unblock {}", record.display_label()));
                    } else if !active && !record.suppressed {
                        record.suppressed = true;
                        messages.push(format!("cut_scene.block {}", record.display_label()));
                    }
                }
            }
        }
        messages
    }

    pub(super) fn push_override(&mut self, description: String) -> String {
        self.override_stack.push(OverrideRecord {
            description: description.clone(),
        });
        format!("cut_scene.override.push {}", description)
    }

    pub(super) fn pop_override(&mut self) -> Option<String> {
        let record = self.override_stack.pop()?;
        Some(format!("cut_scene.override.pop {}", record.description))
    }

    pub(super) fn take_all_overrides(&mut self) -> Vec<String> {
        let mut messages = Vec::new();
        while let Some(record) = self.override_stack.pop() {
            messages.push(format!("cut_scene.override.pop {}", record.description));
        }
        messages
    }

    pub(super) fn set_commentary(&mut self, record: CommentaryRecord) -> Option<String> {
        let log_needed = match self.commentary.as_ref() {
            Some(existing) => {
                existing.label != record.label
                    || existing.object_handle != record.object_handle
                    || existing.active != record.active
                    || existing.suppressed_reason != record.suppressed_reason
            }
            None => true,
        };
        let display = record.display_label().to_string();
        let message = if record.active {
            format!("commentary.active {}", display)
        } else {
            format!("commentary.suppressed {}", display)
        };
        self.commentary = Some(record);
        log_needed.then_some(message)
    }

    pub(super) fn disable_commentary(&mut self) -> String {
        match self.commentary.take() {
            Some(record) => {
                let display = record.display_label().to_string();
                format!("commentary.active off ({display})")
            }
            None => "commentary.active off".to_string(),
        }
    }

    pub(super) fn update_commentary_visibility(
        &mut self,
        visible: bool,
        suppressed_reason: &str,
    ) -> Option<String> {
        let record = self.commentary.as_mut()?;
        match (record.active, visible) {
            (true, false) => {
                record.active = false;
                record.suppressed_reason = Some(suppressed_reason.to_string());
                let display = record.display_label().to_string();
                Some(format!("commentary.suspend {}", display))
            }
            (false, true) => {
                record.active = true;
                record.suppressed_reason = None;
                let display = record.display_label().to_string();
                Some(format!("commentary.resume {}", display))
            }
            _ => None,
        }
    }

    pub(super) fn commentary(&self) -> Option<&CommentaryRecord> {
        self.commentary.as_ref()
    }

    pub(super) fn cut_scene_stack(&self) -> &[CutSceneRecord] {
        &self.cut_scene_stack
    }

    pub(super) fn set_dialog_state(&mut self, state: DialogState) {
        self.speaking_actor = Some(state.actor_id.clone());
        self.message_active = true;
        self.active_dialog = Some(state);
    }

    pub(super) fn active_dialog(&self) -> Option<&DialogState> {
        self.active_dialog.as_ref()
    }

    pub(super) fn take_active_dialog(&mut self) -> Option<DialogState> {
        self.active_dialog.take()
    }

    pub(super) fn clear_dialog_flags(&mut self) {
        self.speaking_actor = None;
        self.message_active = false;
    }

    pub(super) fn is_message_active(&self) -> bool {
        self.message_active
    }

    pub(super) fn speaking_actor(&self) -> Option<&str> {
        self.speaking_actor.as_deref()
    }

    pub(super) fn start_fullscreen_movie(&mut self, movie: String, yields: Option<u32>) -> String {
        let play_yields = yields.unwrap_or(DEFAULT_FULLSCREEN_YIELDS).max(1);
        self.fullscreen_movie = Some(FullscreenMovieState {
            name: movie.clone(),
            play_yields_remaining: play_yields,
        });
        format!("cut_scene.fullscreen.start {movie}")
    }

    pub(super) fn poll_fullscreen_movie(&mut self) -> (bool, Option<String>) {
        let Some(state) = self.fullscreen_movie.as_mut() else {
            return (false, None);
        };

        if state.play_yields_remaining > 1 {
            state.play_yields_remaining -= 1;
            return (true, None);
        }

        let movie = state.name.clone();
        self.fullscreen_movie = None;
        (false, Some(format!("cut_scene.fullscreen.end {movie}")))
    }
}

/// Couples cutscene runtime state with the engine event log.
pub(super) struct CutsceneRuntimeAdapter<'a> {
    runtime: &'a mut CutsceneRuntime,
    events: &'a mut Vec<String>,
}

/// Provides read-only accessors for cutscene state.
pub(super) struct CutsceneRuntimeView<'a> {
    runtime: &'a CutsceneRuntime,
}

impl<'a> CutsceneRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut CutsceneRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn push_cut_scene(
        &mut self,
        label: Option<String>,
        flags: Vec<String>,
        set_file: Option<String>,
        sector: Option<String>,
        suppressed: bool,
    ) {
        let message = self
            .runtime
            .push_cut_scene(label, flags, set_file, sector, suppressed);
        self.events.push(message);
    }

    pub(super) fn pop_cut_scene(&mut self) {
        if let Some(message) = self.runtime.pop_cut_scene() {
            self.events.push(message);
        }
    }

    pub(super) fn push_override(&mut self, description: String) {
        let message = self.runtime.push_override(description);
        self.events.push(message);
    }

    pub(super) fn pop_override(&mut self) -> bool {
        if let Some(message) = self.runtime.pop_override() {
            self.events.push(message);
            true
        } else {
            false
        }
    }

    pub(super) fn clear_overrides(&mut self) {
        for message in self.runtime.take_all_overrides() {
            self.events.push(message);
        }
    }

    pub(super) fn handle_sector_activation(&mut self, set_file: &str, sector: &str, active: bool) {
        let messages = self
            .runtime
            .handle_sector_activation(set_file, sector, active);
        for message in messages {
            self.events.push(message);
        }
    }

    pub(super) fn set_commentary(&mut self, record: CommentaryRecord) {
        if let Some(message) = self.runtime.set_commentary(record) {
            self.events.push(message);
        }
    }

    pub(super) fn disable_commentary(&mut self) {
        let message = self.runtime.disable_commentary();
        self.events.push(message);
    }

    pub(super) fn update_commentary_visibility(&mut self, visible: bool, suppressed_reason: &str) {
        if let Some(message) = self
            .runtime
            .update_commentary_visibility(visible, suppressed_reason)
        {
            self.events.push(message);
        }
    }

    pub(super) fn start_fullscreen_movie(&mut self, movie: String, yields: Option<u32>) -> bool {
        let message = self.runtime.start_fullscreen_movie(movie, yields);
        self.events.push(message);
        true
    }

    pub(super) fn poll_fullscreen_movie(&mut self) -> bool {
        let (active, maybe_message) = self.runtime.poll_fullscreen_movie();
        if let Some(message) = maybe_message {
            self.events.push(message);
        }
        active
    }
}

impl<'a> CutsceneRuntimeView<'a> {
    pub(super) fn new(runtime: &'a CutsceneRuntime) -> Self {
        Self { runtime }
    }

    pub(super) fn active_dialog(&self) -> Option<&DialogState> {
        self.runtime.active_dialog()
    }

    pub(super) fn is_message_active(&self) -> bool {
        self.runtime.is_message_active()
    }

    pub(super) fn speaking_actor(&self) -> Option<&str> {
        self.runtime.speaking_actor()
    }

    pub(super) fn commentary(&self) -> Option<&CommentaryRecord> {
        self.runtime.commentary()
    }

    pub(super) fn cut_scene_stack(&self) -> &[CutSceneRecord] {
        self.runtime.cut_scene_stack()
    }
}
