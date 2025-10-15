use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::rc::Rc;

use anyhow::Result;
use mlua::{Lua, Table, Value, Variadic};

use crate::geometry_snapshot::{
    LuaMusicCueSnapshot, LuaMusicSnapshot, LuaSfxInstanceSnapshot, LuaSfxSnapshot,
};

use super::{describe_value, split_self, value_to_f32, value_to_string, EngineContext};

/// Minimal adapter for routing audio events to interested observers.
pub trait AudioCallback {
    fn music_play(&self, _cue: &str, _params: &[String]) {}
    fn music_stop(&self, _mode: Option<&str>) {}
    fn sfx_play(&self, _cue: &str, _params: &[String], _handle: &str) {}
    fn sfx_stop(&self, _target: Option<&str>) {}
}

impl fmt::Debug for dyn AudioCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AudioCallback")
    }
}

#[derive(Debug, Clone)]
pub(super) struct MusicCueSnapshot {
    pub(super) name: String,
    pub(super) parameters: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct MusicState {
    pub(super) current: Option<MusicCueSnapshot>,
    pub(super) queued: Vec<MusicCueSnapshot>,
    pub(super) current_state: Option<String>,
    pub(super) state_stack: Vec<String>,
    pub(super) paused: bool,
    pub(super) muted_groups: BTreeSet<String>,
    pub(super) volume: Option<f32>,
    pub(super) history: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SfxInstance {
    pub(super) handle: String,
    pub(super) numeric: i64,
    pub(super) cue: String,
    pub(super) parameters: Vec<String>,
    pub(super) group: Option<i32>,
    pub(super) volume: i32,
    pub(super) pan: i32,
    pub(super) play_count: u32,
}

#[derive(Debug, Default, Clone)]
pub(super) struct SfxState {
    pub(super) next_handle: u32,
    pub(super) active: BTreeMap<String, SfxInstance>,
    pub(super) active_by_numeric: BTreeMap<i64, String>,
    pub(super) history: Vec<String>,
}

#[derive(Clone, Copy)]
pub(super) struct FootstepProfile {
    pub(super) key: &'static str,
    pub(super) prefix: &'static str,
    pub(super) left_walk: u8,
    pub(super) right_walk: u8,
    pub(super) left_run: Option<u8>,
    pub(super) right_run: Option<u8>,
}

pub(super) const FOOTSTEP_PROFILES: &[FootstepProfile] = &[
    FootstepProfile {
        key: "concrete",
        prefix: "fscon",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "dirt",
        prefix: "fsdrt",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "gravel",
        prefix: "fsgrv",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "creak",
        prefix: "fscrk",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "marble",
        prefix: "fsmar",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "metal",
        prefix: "fsmet",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "pavement",
        prefix: "fspav",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "rug",
        prefix: "fsrug",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "sand",
        prefix: "fssnd",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "snow",
        prefix: "fssno",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "trapdoor",
        prefix: "fstrp",
        left_walk: 1,
        right_walk: 1,
        left_run: Some(1),
        right_run: Some(1),
    },
    FootstepProfile {
        key: "echo",
        prefix: "fseko",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(4),
        right_run: Some(4),
    },
    FootstepProfile {
        key: "reverb",
        prefix: "fsrvb",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "metal2",
        prefix: "fs3mt",
        left_walk: 4,
        right_walk: 4,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "wet",
        prefix: "fswet",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "flowers",
        prefix: "fsflw",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "glottis",
        prefix: "fsglt",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "bone",
        prefix: "fsbon",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "wood",
        prefix: "fswd1",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "wood2",
        prefix: "fswd2",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "wood3",
        prefix: "fswd3",
        left_walk: 3,
        right_walk: 3,
        left_run: Some(3),
        right_run: Some(3),
    },
    FootstepProfile {
        key: "wood4",
        prefix: "fswd4",
        left_walk: 3,
        right_walk: 3,
        left_run: Some(3),
        right_run: Some(3),
    },
    FootstepProfile {
        key: "wood5",
        prefix: "fswd5",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "wood6",
        prefix: "fswd6",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "water",
        prefix: "fswat",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "mud",
        prefix: "fsmud",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "clay",
        prefix: "fscla",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "slime",
        prefix: "fsslm",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "slush",
        prefix: "fsslh",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "velvet",
        prefix: "fsvlv",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "ivy",
        prefix: "fsivy",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "leaves",
        prefix: "fslea",
        left_walk: 3,
        right_walk: 3,
        left_run: Some(3),
        right_run: Some(3),
    },
    FootstepProfile {
        key: "carpet",
        prefix: "fscpt",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "vinyl",
        prefix: "fsvin",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "catwalk",
        prefix: "fscat",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "steam",
        prefix: "fsstm",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "stump",
        prefix: "fsstp",
        left_walk: 1,
        right_walk: 1,
        left_run: Some(1),
        right_run: Some(1),
    },
    FootstepProfile {
        key: "shell",
        prefix: "fsshl",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "guard",
        prefix: "fsgua",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "paper",
        prefix: "fspap",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "cardboard",
        prefix: "fscbx",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "tarp",
        prefix: "fstrp",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "metal3",
        prefix: "fsmt3",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "metal4",
        prefix: "fsmt4",
        left_walk: 2,
        right_walk: 2,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "nick_virago",
        prefix: "fsnic",
        left_walk: 2,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
    FootstepProfile {
        key: "underwater",
        prefix: "fswtr",
        left_walk: 3,
        right_walk: 3,
        left_run: Some(2),
        right_run: Some(2),
    },
    FootstepProfile {
        key: "velasco",
        prefix: "fsbcn",
        left_walk: 3,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
    FootstepProfile {
        key: "jello",
        prefix: "fsjll",
        left_walk: 2,
        right_walk: 2,
        left_run: None,
        right_run: None,
    },
];

pub(super) const IM_SOUND_PLAY_COUNT: i32 = 256;
pub(super) const IM_SOUND_GROUP: i32 = 1024;
pub(super) const IM_SOUND_VOL: i32 = 1536;
pub(super) const IM_SOUND_PAN: i32 = 1792;

pub(super) fn format_music_detail(action: &str, cue: &str, params: &[String]) -> String {
    if params.is_empty() {
        format!("{action} {cue}")
    } else {
        format!("{action} {cue} [{}]", params.join(", "))
    }
}

#[derive(Debug)]
pub(super) struct AudioRuntime {
    callback: Option<Rc<dyn AudioCallback>>,
    music: MusicState,
    sfx: SfxState,
}

impl AudioRuntime {
    pub(super) fn new(callback: Option<Rc<dyn AudioCallback>>) -> Self {
        Self {
            callback,
            music: MusicState::default(),
            sfx: SfxState::default(),
        }
    }

    pub(super) fn music(&self) -> &MusicState {
        &self.music
    }

    pub(super) fn sfx(&self) -> &SfxState {
        &self.sfx
    }

    pub(super) fn sfx_mut(&mut self) -> &mut SfxState {
        &mut self.sfx
    }

    pub(super) fn play_music(&mut self, track: String, params: Vec<String>) -> String {
        let snapshot = MusicCueSnapshot {
            name: track.clone(),
            parameters: params.clone(),
        };
        self.music.current = Some(snapshot);
        let detail = format_music_detail("play", &track, &params);
        self.music.history.push(detail);
        if let Some(callback) = self.callback.as_ref() {
            callback.music_play(&track, &params);
        }
        format!("music.play {}", track)
    }

    pub(super) fn queue_music(&mut self, track: String, params: Vec<String>) -> String {
        let snapshot = MusicCueSnapshot {
            name: track.clone(),
            parameters: params.clone(),
        };
        self.music.queued.push(snapshot);
        let detail = format_music_detail("queue", &track, &params);
        self.music.history.push(detail);
        format!("music.queue {}", track)
    }

    pub(super) fn stop_music(&mut self, mode: Option<String>) -> String {
        self.music.current = None;
        self.music.paused = false;
        let history_entry = match mode.as_deref() {
            Some(value) if !value.is_empty() => format!("stop {}", value),
            _ => "stop".to_string(),
        };
        self.music.history.push(history_entry.clone());
        if let Some(callback) = self.callback.as_ref() {
            callback.music_stop(mode.as_deref());
        }
        match mode.as_deref() {
            Some(value) if !value.is_empty() => format!("music.stop {}", value),
            _ => "music.stop".to_string(),
        }
    }

    pub(super) fn pause_music(&mut self) -> String {
        if !self.music.paused {
            self.music.paused = true;
        }
        self.music.history.push("pause".to_string());
        "music.pause".to_string()
    }

    pub(super) fn resume_music(&mut self) -> String {
        if self.music.paused {
            self.music.paused = false;
        }
        self.music.history.push("resume".to_string());
        "music.resume".to_string()
    }

    pub(super) fn set_music_state(&mut self, state: Option<String>) -> String {
        match state {
            Some(name) => {
                if let Some(current) = self.music.state_stack.last_mut() {
                    *current = name.clone();
                }
                self.music.current_state = Some(name.clone());
                self.music.history.push(format!("state {}", name));
                format!("music.state {}", name)
            }
            None => {
                self.music.current_state = None;
                self.music.history.push("state <nil>".to_string());
                "music.state <nil>".to_string()
            }
        }
    }

    pub(super) fn push_music_state(&mut self, state: Option<String>) -> String {
        match state {
            Some(name) => {
                self.music.state_stack.push(name.clone());
                self.music.current_state = Some(name.clone());
                self.music.history.push(format!("state.push {}", name));
                format!("music.state.push {}", name)
            }
            None => {
                self.music.history.push("state.push <nil>".to_string());
                "music.state.push <nil>".to_string()
            }
        }
    }

    pub(super) fn pop_music_state(&mut self) -> String {
        let popped = self.music.state_stack.pop();
        self.music.current_state = self.music.state_stack.last().cloned();
        let label = popped.as_deref().unwrap_or("<none>");
        self.music.history.push(format!("state.pop {}", label));
        format!("music.state.pop {}", label)
    }

    pub(super) fn mute_music_group(&mut self, group: Option<String>) -> String {
        match group {
            Some(name) => {
                self.music.muted_groups.insert(name.clone());
                self.music.history.push(format!("mute {}", name));
                format!("music.mute {}", name)
            }
            None => {
                self.music.history.push("mute <nil>".to_string());
                "music.mute <nil>".to_string()
            }
        }
    }

    pub(super) fn unmute_music_group(&mut self, group: Option<String>) -> String {
        match group {
            Some(name) => {
                self.music.muted_groups.remove(&name);
                self.music.history.push(format!("unmute {}", name));
                format!("music.unmute {}", name)
            }
            None => {
                self.music.history.push("unmute <nil>".to_string());
                "music.unmute <nil>".to_string()
            }
        }
    }

    pub(super) fn set_music_volume(&mut self, volume: Option<f32>) -> String {
        self.music.volume = volume;
        let detail = match self.music.volume {
            Some(value) => format!("volume {:.3}", value),
            None => "volume <nil>".to_string(),
        };
        self.music.history.push(detail.clone());
        format!("music.{}", detail)
    }

    pub(super) fn play_sound_effect(
        &mut self,
        cue: String,
        params: Vec<String>,
    ) -> (String, String) {
        let numeric = self.sfx.next_handle as i64;
        let handle = format!("sfx_{:04}", self.sfx.next_handle);
        self.sfx.next_handle = self.sfx.next_handle.saturating_add(1);
        let instance = SfxInstance {
            handle: handle.clone(),
            numeric,
            cue: cue.clone(),
            parameters: params.clone(),
            group: None,
            volume: 127,
            pan: 64,
            play_count: 1,
        };
        self.sfx.active_by_numeric.insert(numeric, handle.clone());
        self.sfx.active.insert(handle.clone(), instance);
        let detail = if params.is_empty() {
            format!("sfx.play {} -> {}", cue, handle)
        } else {
            format!("sfx.play {} [{}] -> {}", cue, params.join(", "), handle)
        };
        self.sfx.history.push(detail);
        if let Some(callback) = self.callback.as_ref() {
            callback.sfx_play(&cue, &params, &handle);
        }
        (handle, format!("sfx.play {}", cue))
    }

    pub(super) fn stop_sound_effect(&mut self, target: Option<String>) -> String {
        let requested = target.clone();
        let mut label = String::from("sfx.stop");
        if let Some(spec) = target {
            if let Some(instance) = self.sfx.active.remove(&spec) {
                self.sfx.active_by_numeric.remove(&instance.numeric);
                label = format!("sfx.stop {}", spec);
            } else if let Some((handle, numeric)) = self
                .sfx
                .active
                .iter()
                .find(|(_, instance)| instance.cue.eq_ignore_ascii_case(&spec))
                .map(|(handle, instance)| (handle.clone(), instance.numeric))
            {
                self.sfx.active.remove(&handle);
                self.sfx.active_by_numeric.remove(&numeric);
                label = format!("sfx.stop {}", spec);
            } else {
                label = format!("sfx.stop {}", spec);
            }
        } else {
            self.sfx.active.clear();
            self.sfx.active_by_numeric.clear();
            label.push_str(" all");
        }
        self.sfx.history.push(label.clone());
        if let Some(callback) = self.callback.as_ref() {
            callback.sfx_stop(requested.as_deref());
        }
        label
    }

    pub(super) fn stop_sound_effect_by_numeric(&mut self, numeric: i64) -> String {
        if let Some(handle) = self.sfx.active_by_numeric.get(&numeric).cloned() {
            self.stop_sound_effect(Some(handle))
        } else {
            self.stop_sound_effect(Some(numeric.to_string()))
        }
    }

    pub(super) fn set_sound_param(
        &mut self,
        numeric: i64,
        param: i32,
        value: i32,
    ) -> Option<String> {
        let Some(handle) = self.sfx.active_by_numeric.get(&numeric).cloned() else {
            return None;
        };

        if let Some(instance) = self.sfx.active.get_mut(&handle) {
            let cue_label = instance.cue.clone();
            match param {
                IM_SOUND_VOL => {
                    instance.volume = value;
                    Some(format!("sfx.param {} volume {}", cue_label, value))
                }
                IM_SOUND_PAN => {
                    instance.pan = value;
                    Some(format!("sfx.param {} pan {}", cue_label, value))
                }
                IM_SOUND_PLAY_COUNT => {
                    instance.play_count = value.max(0) as u32;
                    Some(format!(
                        "sfx.param {} play_count {}",
                        cue_label, instance.play_count
                    ))
                }
                IM_SOUND_GROUP => {
                    instance.group = Some(value);
                    Some(format!("sfx.param {} group {}", cue_label, value))
                }
                _ => Some(format!(
                    "sfx.param {} code {} value {}",
                    cue_label, param, value
                )),
            }
        } else {
            None
        }
    }

    pub(super) fn get_sound_param(&self, numeric: i64, param: i32) -> Option<i32> {
        let handle = self.sfx.active_by_numeric.get(&numeric)?;
        let instance = self.sfx.active.get(handle)?;
        let value = match param {
            IM_SOUND_PLAY_COUNT => instance.play_count as i32,
            IM_SOUND_VOL => instance.volume,
            IM_SOUND_PAN => instance.pan,
            IM_SOUND_GROUP => instance.group.unwrap_or(0),
            _ => return None,
        };
        Some(value)
    }
}

impl Default for AudioRuntime {
    fn default() -> Self {
        Self::new(None)
    }
}

pub(super) fn install_music_scaffold(lua: &Lua, context: Rc<RefCell<EngineContext>>) -> Result<()> {
    let globals = lua.globals();
    if matches!(globals.get::<_, Value>("music"), Ok(Value::Table(_))) {
        return Ok(());
    }

    let music = lua.create_table()?;

    let play_context = context.clone();
    music.set(
        "play",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            if values.is_empty() {
                return Ok(());
            }
            let track = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let params = values
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect::<Vec<_>>();
            play_context.borrow_mut().play_music(track, params);
            Ok(())
        })?,
    )?;

    let queue_context = context.clone();
    music.set(
        "queue",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            if values.is_empty() {
                return Ok(());
            }
            let track = values
                .get(0)
                .and_then(value_to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let params = values
                .iter()
                .skip(1)
                .map(|value| describe_value(value))
                .collect::<Vec<_>>();
            queue_context.borrow_mut().queue_music(track, params);
            Ok(())
        })?,
    )?;

    let stop_context = context.clone();
    music.set(
        "stop",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let mode = values.get(0).and_then(|value| value_to_string(value));
            stop_context.borrow_mut().stop_music(mode);
            Ok(())
        })?,
    )?;

    let pause_context = context.clone();
    music.set(
        "pause",
        lua.create_function(move |_, _: Variadic<Value>| {
            pause_context.borrow_mut().pause_music();
            Ok(())
        })?,
    )?;

    let resume_context = context.clone();
    music.set(
        "resume",
        lua.create_function(move |_, _: Variadic<Value>| {
            resume_context.borrow_mut().resume_music();
            Ok(())
        })?,
    )?;

    let set_state_context = context.clone();
    music.set(
        "set_state",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let state = values.get(0).and_then(|value| value_to_string(value));
            set_state_context.borrow_mut().set_music_state(state);
            Ok(())
        })?,
    )?;

    let push_state_context = context.clone();
    music.set(
        "push_state",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let state = values.get(0).and_then(|value| value_to_string(value));
            push_state_context.borrow_mut().push_music_state(state);
            Ok(())
        })?,
    )?;

    let pop_state_context = context.clone();
    music.set(
        "pop_state",
        lua.create_function(move |_, _: Variadic<Value>| {
            pop_state_context.borrow_mut().pop_music_state();
            Ok(())
        })?,
    )?;

    let mute_context = context.clone();
    music.set(
        "mute_group",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let group = values.get(0).and_then(|value| value_to_string(value));
            mute_context.borrow_mut().mute_music_group(group);
            Ok(())
        })?,
    )?;

    let unmute_context = context.clone();
    music.set(
        "unmute_group",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let group = values.get(0).and_then(|value| value_to_string(value));
            unmute_context.borrow_mut().unmute_music_group(group);
            Ok(())
        })?,
    )?;

    let volume_context = context.clone();
    music.set(
        "set_volume",
        lua.create_function(move |_, args: Variadic<Value>| {
            let (_, values) = split_self(args);
            let volume = values.get(0).and_then(|value| value_to_f32(value));
            volume_context.borrow_mut().set_music_volume(volume);
            Ok(())
        })?,
    )?;

    let fallback_context = context.clone();
    let fallback = lua.create_function(move |lua_ctx, (_table, key): (Table, Value)| {
        if let Value::String(method) = key {
            fallback_context
                .borrow_mut()
                .log_event(format!("music.stub {}", method.to_str()?));
        }
        let noop = lua_ctx.create_function(|_, _: Variadic<Value>| Ok(()))?;
        Ok(Value::Function(noop))
    })?;
    let metatable = lua.create_table()?;
    metatable.set("__index", fallback)?;
    music.set_metatable(Some(metatable));

    globals.set("music", music.clone())?;
    if matches!(
        globals.get::<_, Value>("music_state"),
        Ok(Value::Nil) | Err(_)
    ) {
        globals.set("music_state", music)?;
    }
    Ok(())
}

impl MusicState {
    pub(super) fn to_snapshot(&self) -> LuaMusicSnapshot {
        let current = self.current.as_ref().map(|cue| cue.to_snapshot());
        let queued = self
            .queued
            .iter()
            .map(|cue| cue.to_snapshot())
            .collect::<Vec<_>>();
        let muted_groups = self.muted_groups.iter().cloned().collect::<Vec<_>>();
        LuaMusicSnapshot {
            current,
            queued,
            current_state: self.current_state.clone(),
            state_stack: self.state_stack.clone(),
            paused: self.paused,
            muted_groups,
            volume: self.volume,
            history: self.history.clone(),
        }
    }
}

/// Couples the audio runtime with the engine event log so call sites stay lean.
pub(super) struct AudioRuntimeAdapter<'a> {
    runtime: &'a mut AudioRuntime,
    events: &'a mut Vec<String>,
}

/// Read-only access to the audio runtime, used to surface state without exposing internals.
pub(super) struct AudioRuntimeView<'a> {
    runtime: &'a AudioRuntime,
}

impl<'a> AudioRuntimeView<'a> {
    pub(super) fn new(runtime: &'a AudioRuntime) -> Self {
        Self { runtime }
    }

    pub(super) fn music(&self) -> &MusicState {
        self.runtime.music()
    }

    pub(super) fn sfx(&self) -> &SfxState {
        self.runtime.sfx()
    }

    pub(super) fn get_sound_param(&self, numeric: i64, param: i32) -> Option<i32> {
        self.runtime.get_sound_param(numeric, param)
    }
}

impl<'a> AudioRuntimeAdapter<'a> {
    pub(super) fn new(runtime: &'a mut AudioRuntime, events: &'a mut Vec<String>) -> Self {
        Self { runtime, events }
    }

    pub(super) fn play_music(&mut self, track: String, params: Vec<String>) {
        let event = self.runtime.play_music(track, params);
        self.events.push(event);
    }

    pub(super) fn queue_music(&mut self, track: String, params: Vec<String>) {
        let event = self.runtime.queue_music(track, params);
        self.events.push(event);
    }

    pub(super) fn stop_music(&mut self, mode: Option<String>) {
        let event = self.runtime.stop_music(mode);
        self.events.push(event);
    }

    pub(super) fn pause_music(&mut self) {
        let event = self.runtime.pause_music();
        self.events.push(event);
    }

    pub(super) fn resume_music(&mut self) {
        let event = self.runtime.resume_music();
        self.events.push(event);
    }

    pub(super) fn set_music_state(&mut self, state: Option<String>) {
        let event = self.runtime.set_music_state(state);
        self.events.push(event);
    }

    pub(super) fn push_music_state(&mut self, state: Option<String>) {
        let event = self.runtime.push_music_state(state);
        self.events.push(event);
    }

    pub(super) fn pop_music_state(&mut self) {
        let event = self.runtime.pop_music_state();
        self.events.push(event);
    }

    pub(super) fn mute_music_group(&mut self, group: Option<String>) {
        let event = self.runtime.mute_music_group(group);
        self.events.push(event);
    }

    pub(super) fn unmute_music_group(&mut self, group: Option<String>) {
        let event = self.runtime.unmute_music_group(group);
        self.events.push(event);
    }

    pub(super) fn set_music_volume(&mut self, volume: Option<f32>) {
        let event = self.runtime.set_music_volume(volume);
        self.events.push(event);
    }

    pub(super) fn play_sound_effect(&mut self, cue: String, params: Vec<String>) -> String {
        let (handle, event) = self.runtime.play_sound_effect(cue, params);
        self.events.push(event);
        handle
    }

    pub(super) fn stop_sound_effect(&mut self, target: Option<String>) {
        let event = self.runtime.stop_sound_effect(target);
        self.events.push(event);
    }

    pub(super) fn stop_sound_effect_by_numeric(&mut self, numeric: i64) {
        let event = self.runtime.stop_sound_effect_by_numeric(numeric);
        self.events.push(event);
    }

    pub(super) fn set_sound_param(&mut self, numeric: i64, param: i32, value: i32) {
        if let Some(event) = self.runtime.set_sound_param(numeric, param, value) {
            self.events.push(event);
        }
    }

    pub(super) fn sfx_mut(&mut self) -> &mut SfxState {
        self.runtime.sfx_mut()
    }
}

impl MusicCueSnapshot {
    fn to_snapshot(&self) -> LuaMusicCueSnapshot {
        LuaMusicCueSnapshot {
            name: self.name.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

impl SfxState {
    pub(super) fn to_snapshot(&self) -> LuaSfxSnapshot {
        let active = self
            .active
            .values()
            .map(|instance| instance.to_snapshot())
            .collect::<Vec<_>>();
        LuaSfxSnapshot {
            active,
            history: self.history.clone(),
        }
    }
}

impl SfxInstance {
    fn to_snapshot(&self) -> LuaSfxInstanceSnapshot {
        LuaSfxInstanceSnapshot {
            handle: self.handle.clone(),
            cue: self.cue.clone(),
            parameters: self.parameters.clone(),
        }
    }
}
