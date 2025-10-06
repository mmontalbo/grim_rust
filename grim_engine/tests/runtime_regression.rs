use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tempfile::tempdir;

#[derive(Debug, Deserialize, Clone)]
struct MovementSample {
    frame: u32,
    position: [f32; 3],
    yaw: Option<f32>,
    sector: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AudioEvent {
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

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct HotspotEventLog {
    events: Vec<HotspotEventLogEntry>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct HotspotEventLogEntry {
    sequence: u32,
    frame: Option<u32>,
    label: String,
}

#[test]
fn manny_office_runtime_regression() -> Result<()> {
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

    let temp_dir = tempdir().context("creating temporary directory for regression artefacts")?;
    let movement_path = temp_dir.path().join("movement_log.json");
    let audio_path = temp_dir.path().join("hotspot_audio.json");
    let depth_path = temp_dir.path().join("manny_office_depth_stats.json");
    let timeline_path = temp_dir.path().join("manny_office_timeline.json");
    let event_log_path = temp_dir.path().join("hotspot_events.json");

    let movement_path_str = movement_path
        .to_str()
        .context("movement log path is not valid UTF-8")?;
    let audio_path_str = audio_path
        .to_str()
        .context("audio log path is not valid UTF-8")?;
    let depth_path_str = depth_path
        .to_str()
        .context("depth stats path is not valid UTF-8")?;
    let timeline_path_str = timeline_path
        .to_str()
        .context("timeline path is not valid UTF-8")?;
    let event_log_path_str = event_log_path
        .to_str()
        .context("event log path is not valid UTF-8")?;

    let timeline_output = Command::new(env!("CARGO_BIN_EXE_grim_engine"))
        .current_dir(&workspace_root)
        .args(["--timeline-json", timeline_path_str])
        .output()
        .context("capturing Manny timeline manifest via grim_engine analysis run")?;
    if !timeline_output.status.success() {
        let mut transcript = String::from_utf8_lossy(&timeline_output.stdout).to_string();
        transcript.push_str(&String::from_utf8_lossy(&timeline_output.stderr));
        panic!(
            "grim_engine timeline capture exited with {:?}: {}",
            timeline_output.status, transcript
        );
    }
    assert!(
        timeline_path.is_file(),
        "grim_engine did not produce a timeline manifest"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_grim_engine"))
        .current_dir(&workspace_root)
        .args([
            "--run-lua",
            "--movement-demo",
            "--movement-log-json",
            movement_path_str,
            "--hotspot-demo",
            "computer",
            "--audio-log-json",
            audio_path_str,
            "--depth-stats-json",
            depth_path_str,
            "--event-log-json",
            event_log_path_str,
        ])
        .output()
        .context("executing grim_engine runtime regression harness")?;

    assert!(
        output.status.success(),
        "grim_engine exited with {:?}",
        output.status
    );
    assert!(
        movement_path.is_file(),
        "grim_engine did not produce a movement log"
    );
    assert!(
        audio_path.is_file(),
        "grim_engine did not produce an audio log"
    );
    assert!(
        depth_path.is_file(),
        "grim_engine did not produce a depth stats artefact"
    );
    assert!(
        event_log_path.is_file(),
        "grim_engine did not produce an event log artefact"
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

    let expected_movement = read_movement(workspace_root.join("tools/tests/movement_log.json"))?;
    let actual_movement = read_movement(&movement_path)?;

    assert_eq!(
        actual_movement.len(),
        expected_movement.len(),
        "movement sample count changed (expected {}, got {})",
        expected_movement.len(),
        actual_movement.len()
    );

    for (idx, (exp, act)) in expected_movement
        .iter()
        .zip(actual_movement.iter())
        .enumerate()
    {
        assert_eq!(
            act.frame, exp.frame,
            "frame mismatch at index {idx} (expected {}, got {})",
            exp.frame, act.frame
        );

        match (exp.sector.as_ref(), act.sector.as_ref()) {
            (Some(expected_sector), Some(actual_sector)) => {
                assert_eq!(
                    actual_sector, expected_sector,
                    "sector mismatch at frame {} (expected {}, got {})",
                    exp.frame, expected_sector, actual_sector
                );
            }
            (None, None) => {}
            _ => panic!("sector presence mismatch at frame {}", exp.frame),
        }

        match (exp.yaw, act.yaw) {
            (Some(expected_yaw), Some(actual_yaw)) => {
                assert!(
                    approx(expected_yaw, actual_yaw, 0.05),
                    "yaw mismatch at frame {} (expected {expected_yaw}, got {actual_yaw})",
                    exp.frame
                );
            }
            (None, None) => {}
            _ => panic!("yaw presence mismatch at frame {}", exp.frame),
        }

        for axis in 0..3 {
            let expected_component = exp.position[axis];
            let actual_component = act.position[axis];
            assert!(
                approx(expected_component, actual_component, 0.001),
                "position mismatch at frame {} axis {} (expected {}, got {})",
                exp.frame,
                axis,
                expected_component,
                actual_component
            );
        }
    }

    let expected_audio = read_audio(workspace_root.join("tools/tests/hotspot_audio.json"))?;
    let actual_audio = read_audio(&audio_path)?;

    assert_eq!(
        actual_audio, expected_audio,
        "audio events diverged from baseline"
    );

    let expected_depth =
        read_depth_stats(workspace_root.join("tools/tests/manny_office_depth_stats.json"))?;
    let actual_depth = read_depth_stats(&depth_path)?;

    assert_eq!(
        actual_depth, expected_depth,
        "depth stats diverged from baseline"
    );

    let expected_timeline =
        read_timeline_manifest(workspace_root.join("tools/tests/manny_office_timeline.json"))?;
    let actual_timeline = read_timeline_manifest(&timeline_path)?;

    assert_eq!(
        actual_timeline, expected_timeline,
        "timeline manifest diverged from baseline"
    );

    let expected_event_log = read_event_log(workspace_root.join("tools/tests/hotspot_events.json"))?;
    let actual_event_log = read_event_log(&event_log_path)?;

    assert_eq!(
        actual_event_log, expected_event_log,
        "event log diverged from baseline"
    );

    Ok(())
}

fn read_movement(path: impl AsRef<Path>) -> Result<Vec<MovementSample>> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref)
        .with_context(|| format!("reading movement log from {}", path_ref.display()))?;
    let samples: Vec<MovementSample> = serde_json::from_str(&data)
        .with_context(|| format!("parsing movement log from {}", path_ref.display()))?;
    Ok(samples)
}

fn read_audio(path: impl AsRef<Path>) -> Result<Vec<AudioEvent>> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref)
        .with_context(|| format!("reading audio log from {}", path_ref.display()))?;
    let events: Vec<AudioEvent> = serde_json::from_str(&data)
        .with_context(|| format!("parsing audio log from {}", path_ref.display()))?;
    Ok(events)
}

fn read_depth_stats(path: impl AsRef<Path>) -> Result<Value> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref)
        .with_context(|| format!("reading depth stats from {}", path_ref.display()))?;
    let value: Value = serde_json::from_str(&data)
        .with_context(|| format!("parsing depth stats from {}", path_ref.display()))?;
    Ok(value)
}

fn read_timeline_manifest(path: impl AsRef<Path>) -> Result<Value> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref)
        .with_context(|| format!("reading timeline manifest from {}", path_ref.display()))?;
    let value: Value = serde_json::from_str(&data)
        .with_context(|| format!("parsing timeline manifest from {}", path_ref.display()))?;
    Ok(value)
}

fn read_event_log(path: impl AsRef<Path>) -> Result<HotspotEventLog> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref)
        .with_context(|| format!("reading event log from {}", path_ref.display()))?;
    let log: HotspotEventLog = serde_json::from_str(&data)
        .with_context(|| format!("parsing event log from {}", path_ref.display()))?;
    Ok(log)
}

fn approx(expected: f32, actual: f32, tolerance: f32) -> bool {
    (expected - actual).abs() <= tolerance
}
