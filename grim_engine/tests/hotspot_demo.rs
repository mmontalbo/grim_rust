use std::{fs, path::PathBuf, process::Command};

use anyhow::{Context, Result};
use serde::Deserialize;
use tempfile::tempdir;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AudioEvent {
    SfxPlay {
        cue: String,
    },
    SfxStop {
        #[allow(dead_code)]
        target: Option<String>,
    },
    MusicPlay {
        #[allow(dead_code)]
        cue: String,
        #[allow(dead_code)]
        params: Vec<String>,
    },
    MusicStop {
        #[allow(dead_code)]
        mode: Option<String>,
    },
}

#[test]
fn hotspot_demo_logs_hotspot_markers() -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("workspace root should exist")
        .to_path_buf();

    let data_root = workspace_root.join("extracted").join("DATA000");
    let lab_root = workspace_root.join("dev-install");

    assert!(
        data_root.is_dir(),
        "expected DATA000 at {}",
        data_root.display()
    );
    assert!(
        lab_root.is_dir(),
        "expected dev-install at {}",
        lab_root.display()
    );

    let temp_dir = tempdir().context("creating temporary directory for audio log")?;
    let audio_log_path = temp_dir.path().join("audio_log.json");
    let audio_log_str = audio_log_path
        .to_str()
        .context("audio log path is not valid UTF-8")?;

    let output = Command::new(env!("CARGO_BIN_EXE_grim_engine"))
        .current_dir(&workspace_root)
        .args([
            "--run-lua",
            "--hotspot-demo",
            "computer",
            "--audio-log-json",
            audio_log_str,
        ])
        .output()
        .context("executing grim_engine hotspot demo")?;

    assert!(
        output.status.success(),
        "grim_engine exited with {:?}",
        output.status
    );
    assert!(
        audio_log_path.is_file(),
        "grim_engine did not produce an audio log"
    );

    let mut transcript = String::from_utf8_lossy(&output.stdout).to_string();
    transcript.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        transcript.contains("hotspot.demo.start computer"),
        "hotspot start marker missing from output: {transcript}"
    );
    assert!(
        transcript.contains("hotspot.demo.end computer"),
        "hotspot end marker missing from output: {transcript}"
    );

    assert!(
        transcript.contains("dialog.begin manny /moma112/"),
        "computer dialogue missing from output: {transcript}"
    );

    let audio_log = fs::read_to_string(&audio_log_path)
        .with_context(|| format!("reading audio log from {}", audio_log_path.display()))?;
    let events: Vec<AudioEvent> =
        serde_json::from_str(&audio_log).context("parsing hotspot demo audio log")?;
    assert!(!events.is_empty(), "audio log did not record any events");

    let cues: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AudioEvent::SfxPlay { cue } => Some(cue.clone()),
            _ => None,
        })
        .collect();

    for expected in ["keyboard.imu", "txtScrl3.WAV", "compbeep.wav"] {
        assert!(
            cues.iter().any(|cue| cue.eq_ignore_ascii_case(expected)),
            "expected SFX cue {expected} missing from audio log: {:?}",
            cues
        );
    }

    Ok(())
}
