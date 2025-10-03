use crate::{registry::Registry, resources::ResourceGraph};
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct BootRequest {
    pub resume_save: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootSummary {
    pub developer_mode: bool,
    pub pl_mode: bool,
    pub default_set: String,
    pub resume_save_slot: Option<i64>,
    pub time_to_run_intro: bool,
    pub stages: Vec<BootStage>,
    pub resource_counts: ResourceCounts,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ResourceCounts {
    pub years: usize,
    pub menus: usize,
    pub rooms: usize,
}

#[derive(Debug, Clone, Serialize)]
pub enum BootStage {
    InitializeFonts,
    PreloadCursors,
    InitPreferences,
    EnableControls,
    DetermineDefaultSet {
        set: String,
        developer_shortcut_used: bool,
    },
    LoadAchievements,
    ShowLogo,
    ResumeSave {
        slot: i64,
    },
    LoadContent,
    FinalizeBoot {
        set: String,
    },
    StartIntroCutscene,
}

impl BootStage {
    pub fn describe(&self) -> String {
        match self {
            BootStage::InitializeFonts => "Load system fonts".into(),
            BootStage::PreloadCursors => "Preload mouse cursors".into(),
            BootStage::InitPreferences => "Initialize system preferences".into(),
            BootStage::EnableControls => "Enable joystick + mouse controls".into(),
            BootStage::DetermineDefaultSet {
                set,
                developer_shortcut_used,
            } => {
                if *developer_shortcut_used {
                    format!("Jump back to developer set {set}")
                } else {
                    format!("Select default set {set} for new game")
                }
            }
            BootStage::LoadAchievements => "Load achievement tables".into(),
            BootStage::ShowLogo => "Queue SHOWLOGO sequence".into(),
            BootStage::ResumeSave { slot } => format!("Resume from save slot {slot}"),
            BootStage::LoadContent => "Load year, menu, and room scripts".into(),
            BootStage::FinalizeBoot { set } => format!("Finalize boot inside {set}"),
            BootStage::StartIntroCutscene => "Start intro cutscene".into(),
        }
    }
}

pub fn run_boot_pipeline(
    registry: &mut Registry,
    request: BootRequest,
    resources: &ResourceGraph,
) -> BootSummary {
    let developer_flag = registry
        .read_string("good_times")
        .map(|v| v.eq_ignore_ascii_case("pl"))
        .unwrap_or(false);

    let default_set = "mo.set".to_string();
    let developer_shortcut_used = false; // disabled in the shipped script
    let time_to_run_intro = true;

    let resume_slot = if request.resume_save {
        registry.read_int("LastSavedGame")
    } else {
        None
    };

    let mut stages = Vec::new();
    stages.push(BootStage::InitializeFonts);
    stages.push(BootStage::PreloadCursors);
    stages.push(BootStage::InitPreferences);
    stages.push(BootStage::EnableControls);
    stages.push(BootStage::DetermineDefaultSet {
        set: default_set.clone(),
        developer_shortcut_used,
    });
    stages.push(BootStage::LoadAchievements);
    stages.push(BootStage::ShowLogo);

    if let Some(slot) = resume_slot {
        stages.push(BootStage::ResumeSave { slot });
    }

    stages.push(BootStage::LoadContent);
    stages.push(BootStage::FinalizeBoot {
        set: default_set.clone(),
    });

    if resume_slot.is_none() && time_to_run_intro {
        stages.push(BootStage::StartIntroCutscene);
    }

    if resume_slot.is_none() {
        registry.write_string("GrimLastSet", default_set.clone());
    }

    BootSummary {
        developer_mode: developer_flag,
        pl_mode: developer_flag,
        default_set,
        resume_save_slot: resume_slot,
        time_to_run_intro,
        stages,
        resource_counts: ResourceCounts {
            years: resources.year_scripts.len(),
            menus: resources.menu_scripts.len(),
            rooms: resources.room_scripts.len(),
        },
    }
}
