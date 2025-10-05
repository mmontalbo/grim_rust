use std::{cell::RefCell, rc::Rc};

use serde::Serialize;

use crate::lua_host::AudioCallback;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioEvent {
    MusicPlay {
        cue: String,
        params: Vec<String>,
    },
    MusicStop {
        mode: Option<String>,
    },
    SfxPlay {
        cue: String,
        params: Vec<String>,
        handle: String,
    },
    SfxStop {
        target: Option<String>,
    },
}

#[derive(Clone, Default)]
pub struct RecordingAudioCallback {
    events: Rc<RefCell<Vec<AudioEvent>>>,
}

impl RecordingAudioCallback {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<AudioEvent> {
        self.events.borrow().clone()
    }
}

impl AudioCallback for RecordingAudioCallback {
    fn music_play(&self, cue: &str, params: &[String]) {
        self.events.borrow_mut().push(AudioEvent::MusicPlay {
            cue: cue.to_string(),
            params: params.to_vec(),
        });
    }

    fn music_stop(&self, mode: Option<&str>) {
        self.events.borrow_mut().push(AudioEvent::MusicStop {
            mode: mode.map(|value| value.to_string()),
        });
    }

    fn sfx_play(&self, cue: &str, params: &[String], handle: &str) {
        self.events.borrow_mut().push(AudioEvent::SfxPlay {
            cue: cue.to_string(),
            params: params.to_vec(),
            handle: handle.to_string(),
        });
    }

    fn sfx_stop(&self, target: Option<&str>) {
        self.events.borrow_mut().push(AudioEvent::SfxStop {
            target: target.map(|value| value.to_string()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_callback_tracks_audio_events() {
        let callback = RecordingAudioCallback::new();
        callback.music_play("intro", &vec!["loop".into()]);
        callback.music_stop(Some("fade"));
        callback.sfx_play("door", &Vec::new(), "sfx_0001");
        callback.sfx_stop(None);

        let events = callback.events();
        assert_eq!(
            events,
            vec![
                AudioEvent::MusicPlay {
                    cue: "intro".to_string(),
                    params: vec!["loop".to_string()],
                },
                AudioEvent::MusicStop {
                    mode: Some("fade".to_string()),
                },
                AudioEvent::SfxPlay {
                    cue: "door".to_string(),
                    params: Vec::new(),
                    handle: "sfx_0001".to_string(),
                },
                AudioEvent::SfxStop { target: None },
            ]
        );
    }
}
