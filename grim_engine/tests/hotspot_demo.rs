use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::tempdir;

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

    Ok(())
}
