use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;
use tempfile::tempdir;

#[derive(Debug, Deserialize, Clone)]
struct MovementSample {
    frame: u32,
    position: [f32; 3],
    yaw: Option<f32>,
    sector: Option<String>,
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

    let actual_movement = read_movement(&movement_path)?;
    assert!(
        !actual_movement.is_empty(),
        "movement demo produced an empty log"
    );
    if let Some(first) = actual_movement.first() {
        assert_eq!(
            first.frame, 1,
            "movement demo should start at frame 1 (got frame {})",
            first.frame
        );
        for component in first.position {
            assert!(
                component.is_finite(),
                "movement position contains non-finite component {component}"
            );
        }
        if let Some(yaw) = first.yaw {
            assert!(
                yaw.is_finite(),
                "movement demo reported non-finite yaw for first frame"
            );
        }
        assert!(
            first.sector.is_some(),
            "movement demo missing sector information for first frame"
        );
    }

    let audio_bytes = fs::read(&audio_path)
        .with_context(|| format!("reading audio log from {}", audio_path.display()))?;
    assert!(!audio_bytes.is_empty(), "audio log is empty");

    let depth_bytes = fs::read(&depth_path)
        .with_context(|| format!("reading depth stats from {}", depth_path.display()))?;
    assert!(!depth_bytes.is_empty(), "depth stats artefact is empty");

    let timeline_bytes = fs::read(&timeline_path)
        .with_context(|| format!("reading timeline manifest from {}", timeline_path.display()))?;
    assert!(
        !timeline_bytes.is_empty(),
        "timeline manifest artefact is empty"
    );

    let event_log = fs::read_to_string(&event_log_path)
        .with_context(|| format!("reading event log from {}", event_log_path.display()))?;
    assert!(
        event_log.contains("hotspot.demo.start computer"),
        "event log missing hotspot start marker"
    );
    assert!(
        event_log.contains("hotspot.demo.end computer"),
        "event log missing hotspot end marker"
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
