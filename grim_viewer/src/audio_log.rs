use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicStatus {
    pub cue: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfxStatus {
    pub cue: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioAggregation {
    pub current_music: Option<MusicStatus>,
    pub last_music_stop_mode: Option<String>,
    pub active_sfx: BTreeMap<String, SfxStatus>,
}

impl AudioAggregation {
    pub fn apply(&mut self, event: &AudioEvent) {
        match event {
            AudioEvent::MusicPlay { cue, params } => {
                self.current_music = Some(MusicStatus {
                    cue: cue.clone(),
                    params: params.clone(),
                });
                self.last_music_stop_mode = None;
            }
            AudioEvent::MusicStop { mode } => {
                self.current_music = None;
                self.last_music_stop_mode = mode.clone();
            }
            AudioEvent::SfxPlay {
                cue,
                params,
                handle,
            } => {
                self.active_sfx.insert(
                    handle.clone(),
                    SfxStatus {
                        cue: cue.clone(),
                        params: params.clone(),
                    },
                );
            }
            AudioEvent::SfxStop { target } => {
                if let Some(handle) = target {
                    self.active_sfx.remove(handle);
                } else {
                    self.active_sfx.clear();
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioLogFormat {
    Array,
    Ndjson,
}

#[derive(Debug, Default)]
pub struct AudioLogParser {
    mode: Option<AudioLogFormat>,
    seen: usize,
}

impl AudioLogParser {
    pub fn parse_new_events(
        &mut self,
        contents: &str,
    ) -> Result<Vec<AudioEvent>, serde_json::Error> {
        let trimmed = contents.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let mode = match self.mode {
            Some(mode) => mode,
            None => {
                let detected = if trimmed.as_bytes().first() == Some(&b'[') {
                    AudioLogFormat::Array
                } else {
                    AudioLogFormat::Ndjson
                };
                self.mode = Some(detected);
                detected
            }
        };

        match mode {
            AudioLogFormat::Array => self.parse_array(trimmed),
            AudioLogFormat::Ndjson => self.parse_ndjson(trimmed),
        }
    }

    fn parse_array(&mut self, contents: &str) -> Result<Vec<AudioEvent>, serde_json::Error> {
        let events: Vec<AudioEvent> = match serde_json::from_str(contents) {
            Ok(events) => events,
            Err(err) => {
                if err.is_eof() {
                    return Ok(Vec::new());
                }
                return Err(err);
            }
        };
        if events.len() < self.seen {
            self.seen = 0;
        }
        let new_events = events[self.seen..].to_vec();
        self.seen = events.len();
        Ok(new_events)
    }

    fn parse_ndjson(&mut self, contents: &str) -> Result<Vec<AudioEvent>, serde_json::Error> {
        let lines: Vec<&str> = contents
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();

        if lines.len() < self.seen {
            self.seen = 0;
        }

        let mut new_events = Vec::new();
        for line in lines.iter().skip(self.seen) {
            match serde_json::from_str::<AudioEvent>(line) {
                Ok(event) => new_events.push(event),
                Err(err) => {
                    if err.is_eof() {
                        break;
                    }
                    return Err(err);
                }
            }
        }
        self.seen = lines.len();
        Ok(new_events)
    }
}

#[derive(Debug, Default)]
pub struct AudioLogTracker {
    parser: AudioLogParser,
    pub state: AudioAggregation,
}

impl AudioLogTracker {
    pub fn ingest(&mut self, contents: &str) -> Result<bool, serde_json::Error> {
        let events = self.parser.parse_new_events(contents)?;
        if events.is_empty() {
            return Ok(false);
        }
        for event in &events {
            self.state.apply(event);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_tracks_json_array_incrementally() {
        let mut parser = AudioLogParser::default();
        let first = r#"[
            {"kind":"music_play","cue":"intro","params":[]}
        ]"#;
        let events = parser.parse_new_events(first).expect("first parse");
        assert_eq!(events.len(), 1);

        let second = r#"[
            {"kind":"music_play","cue":"intro","params":[]},
            {"kind":"sfx_play","cue":"door","params":[],"handle":"sfx_001"}
        ]"#;
        let events = parser.parse_new_events(second).expect("second parse");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AudioEvent::SfxPlay { handle, .. } if handle == "sfx_001"
        ));
    }

    #[test]
    fn parser_handles_array_until_closure() {
        let mut parser = AudioLogParser::default();
        let partial = "[\n  {\"kind\":\"music_play\",\"cue\":\"intro\",\"params\":[]}";
        let events = parser.parse_new_events(partial).expect("partial parse");
        assert!(events.is_empty(), "should ignore incomplete arrays");
    }

    #[test]
    fn parser_tracks_ndjson() {
        let mut parser = AudioLogParser::default();
        let chunk = "{\"kind\":\"music_play\",\"cue\":\"intro\",\"params\":[]}\n";
        let first = parser.parse_new_events(chunk).expect("first chunk");
        assert_eq!(first.len(), 1);

        let chunk_two = format!("{}{{\"kind\":\"music_stop\",\"mode\":\"fade\"}}\n", chunk);
        let second = parser.parse_new_events(&chunk_two).expect("second chunk");
        assert_eq!(second.len(), 1);
        assert!(
            matches!(&second[0], AudioEvent::MusicStop { mode } if mode.as_deref() == Some("fade"))
        );
    }

    #[test]
    fn tracker_applies_events_to_state() {
        let mut tracker = AudioLogTracker::default();
        let payload = r#"[
            {"kind":"music_play","cue":"intro","params":["loop"]},
            {"kind":"sfx_play","cue":"door","params":[],"handle":"sfx_1"},
            {"kind":"sfx_play","cue":"phone","params":["volume=0.5"],"handle":"sfx_2"},
            {"kind":"sfx_stop","target":"sfx_1"},
            {"kind":"music_stop","mode":"fade"},
            {"kind":"sfx_stop","target":null}
        ]"#;

        let changed = tracker.ingest(payload).expect("ingest payload");
        assert!(changed);

        assert_eq!(tracker.state.current_music, None);
        assert_eq!(tracker.state.last_music_stop_mode.as_deref(), Some("fade"));
        assert!(tracker.state.active_sfx.is_empty());
    }
}
