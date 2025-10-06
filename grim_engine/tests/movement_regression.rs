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
fn movement_demo_matches_fixture() -> Result<()> {
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

    let temp_dir = tempdir().context("creating temporary directory for movement log")?;
    let log_path = temp_dir.path().join("movement_log.json");
    let log_path_str = log_path
        .to_str()
        .context("movement log path is not valid UTF-8")?;

    let status = Command::new(env!("CARGO_BIN_EXE_grim_engine"))
        .current_dir(&workspace_root)
        .args([
            "--run-lua",
            "--movement-demo",
            "--movement-log-json",
            log_path_str,
        ])
        .status()
        .context("executing grim_engine movement demo")?;

    assert!(status.success(), "grim_engine exited with {status:?}");
    assert!(
        log_path.is_file(),
        "grim_engine did not produce a movement log"
    );

    let expected = read_samples(manifest_dir.join("tests/fixtures/movement_demo_log.json"))?;
    let actual = read_samples(&log_path)?;

    assert_eq!(
        actual.len(),
        expected.len(),
        "movement sample count changed (expected {}, got {})",
        expected.len(),
        actual.len()
    );

    for (idx, (exp, act)) in expected.iter().zip(actual.iter()).enumerate() {
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

    Ok(())
}

fn read_samples(path: impl AsRef<Path>) -> Result<Vec<MovementSample>> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref)
        .with_context(|| format!("reading movement log from {}", path_ref.display()))?;
    let samples: Vec<MovementSample> = serde_json::from_str(&data)
        .with_context(|| format!("parsing movement log from {}", path_ref.display()))?;
    Ok(samples)
}

fn approx(expected: f32, actual: f32, tolerance: f32) -> bool {
    (expected - actual).abs() <= tolerance
}
