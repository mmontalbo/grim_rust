use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
