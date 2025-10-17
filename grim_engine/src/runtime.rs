use std::{fs, path::PathBuf, rc::Rc};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::audio_bridge::RecordingAudioCallback;
use crate::cli::RunLuaArgs;
use crate::codec3_depth::write_manny_office_depth_stats;
use crate::lua_host::{self, run_boot_sequence, HotspotOptions, MovementOptions};
use crate::stream::StreamServer;

pub fn execute(args: RunLuaArgs) -> Result<()> {
    let RunLuaArgs {
        data_root,
        verbose,
        lab_root,
        lua_geometry_json,
        audio_log_json,
        event_log_json,
        coverage_json,
        movement_demo,
        movement_log_json,
        hotspot_demo,
        depth_stats_json,
        verify_geometry,
        geometry_diff,
        geometry_diff_json,
        stream_bind,
        stream_ready_file,
    } = args;

    if verify_geometry {
        bail!("--verify-geometry cannot be combined with --run-lua");
    }

    if let Some(path) = geometry_diff.as_ref() {
        eprintln!(
            "[grim_engine] warning: --geometry-diff={} ignored with --run-lua",
            path.display()
        );
    }
    if let Some(path) = geometry_diff_json.as_ref() {
        eprintln!(
            "[grim_engine] warning: --geometry-diff-json={} ignored with --run-lua",
            path.display()
        );
    }
    if let Some(path) = audio_log_json.as_ref() {
        eprintln!(
            "[grim_engine] info: capturing audio events to {}",
            path.display()
        );
    }
    if let Some(path) = event_log_json.as_ref() {
        eprintln!(
            "[grim_engine] info: capturing hotspot events to {}",
            path.display()
        );
    }
    if movement_log_json.is_some() && !movement_demo {
        eprintln!("[grim_engine] warning: --movement-log-json is ignored without --movement-demo");
    }

    let audio_recorder = audio_log_json
        .as_ref()
        .map(|_| Rc::new(RecordingAudioCallback::new()));
    let audio_callback = audio_recorder
        .as_ref()
        .map(|recorder| recorder.clone() as Rc<dyn lua_host::AudioCallback>);

    let movement = if movement_demo {
        Some(MovementOptions::demo(movement_log_json.clone()))
    } else {
        None
    };

    let hotspot = match hotspot_demo.as_ref() {
        Some(slug) => Some(HotspotOptions::parse(slug)?),
        None => None,
    };

    let lab_root_path = lab_root
        .clone()
        .unwrap_or_else(|| PathBuf::from("dev-install"));
    let stream = if let Some(addr) = stream_bind.as_ref() {
        Some(StreamServer::bind(
            addr,
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )?)
    } else {
        None
    };
    let (run_summary, runtime) = run_boot_sequence(
        &data_root,
        lab_root.as_deref(),
        verbose,
        lua_geometry_json.as_deref(),
        audio_callback,
        movement,
        hotspot,
        stream,
        stream_ready_file,
    )?;

    if let Some(path) = event_log_json.as_ref() {
        let log = build_hotspot_event_log(run_summary.events());
        let json =
            serde_json::to_string_pretty(&log).context("serializing hotspot event log to JSON")?;
        fs::write(path, &json)
            .with_context(|| format!("writing hotspot event log to {}", path.display()))?;
        println!("Saved hotspot event log to {}", path.display());
    }

    if let Some(path) = coverage_json.as_ref() {
        let json = serde_json::to_string_pretty(run_summary.coverage())
            .context("serializing coverage counts to JSON")?;
        fs::write(path, &json)
            .with_context(|| format!("writing coverage counts to {}", path.display()))?;
        println!("Saved coverage counts to {}", path.display());
    }

    if let (Some(path), Some(recorder)) = (audio_log_json.as_ref(), audio_recorder) {
        let events = recorder.events();
        let json =
            serde_json::to_string_pretty(&events).context("serializing audio event log to JSON")?;
        fs::write(path, &json)
            .with_context(|| format!("writing audio event log to {}", path.display()))?;
    }

    if let Some(path) = depth_stats_json.as_ref() {
        write_manny_office_depth_stats(&lab_root_path, path)?;
    }

    if let Some(runtime) = runtime {
        runtime.run()?;
    }

    Ok(())
}

#[derive(Serialize)]
struct HotspotEventLogEntry {
    sequence: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame: Option<u32>,
    label: String,
}

#[derive(Serialize)]
struct HotspotEventLog {
    events: Vec<HotspotEventLogEntry>,
}

fn build_hotspot_event_log(events: &[String]) -> HotspotEventLog {
    const RELEVANT_ACTOR_PREFIXES: &[&str] = &[
        "head_target",
        "head_rate",
        "ignore_boxes",
        "at_interest",
        "enter",
        "chore",
        "walk_chore",
        "push_costume",
        "pop_costume",
        "base_costume",
        "costume",
    ];

    let mut filtered: Vec<HotspotEventLogEntry> = Vec::new();
    let mut last_frame: Option<u32> = None;
    let mut pending_without_frame: Vec<usize> = Vec::new();
    let mut last_emitted: Option<String> = None;

    for (index, entry) in events.iter().enumerate() {
        let line = entry.trim();
        if let Some(frame) = parse_movement_frame(line) {
            last_frame = Some(frame);
            for pending_index in pending_without_frame.drain(..) {
                if let Some(slot) = filtered.get_mut(pending_index) {
                    slot.frame = Some(frame);
                }
            }
            continue;
        }

        if !is_relevant_event(line, RELEVANT_ACTOR_PREFIXES) {
            continue;
        }

        if last_emitted.as_deref() == Some(line) {
            continue;
        }

        let mut frame = last_frame;
        let force_backfill = should_anchor_to_next_frame(line);
        if force_backfill {
            frame = None;
        }

        let entry = HotspotEventLogEntry {
            sequence: index as u32,
            frame,
            label: line.to_string(),
        };
        let needs_backfill = force_backfill || entry.frame.is_none();
        filtered.push(entry);
        if needs_backfill {
            pending_without_frame.push(filtered.len() - 1);
        }
        last_emitted = Some(line.to_string());
    }

    if !pending_without_frame.is_empty() {
        if let Some(frame) = last_frame.or(Some(0)) {
            for index in pending_without_frame {
                if let Some(slot) = filtered.get_mut(index) {
                    slot.frame = Some(frame);
                }
            }
        }
    }

    HotspotEventLog { events: filtered }
}

fn parse_movement_frame(line: &str) -> Option<u32> {
    let remainder = line.strip_prefix("movement.frame ")?;
    let mut parts = remainder.split_whitespace();
    let frame_str = parts.next()?;
    frame_str.parse().ok()
}

fn should_anchor_to_next_frame(line: &str) -> bool {
    line.starts_with("hotspot.demo.approach ")
}

fn is_relevant_event(line: &str, actor_prefixes: &[&str]) -> bool {
    if line.starts_with("set.setup.make")
        || line.starts_with("set.setup.get")
        || line.starts_with("set.switch")
    {
        return true;
    }

    if line.starts_with("actor.select") {
        return true;
    }

    if line.starts_with("hotspot.") {
        return true;
    }

    if line.starts_with("actor.manny.") {
        let suffix = &line["actor.manny.".len()..];
        return actor_prefixes
            .iter()
            .any(|prefix| suffix.starts_with(prefix));
    }

    if line.starts_with("dialog.") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::build_hotspot_event_log;

    #[test]
    fn hotspot_event_log_backfills_initial_frames() {
        let events = vec![
            "actor.select manny".to_string(),
            "set.switch mo.set".to_string(),
            "movement.frame 12 0.000,0.000".to_string(),
            "hotspot.demo.start computer".to_string(),
        ];

        let log = build_hotspot_event_log(&events);
        let frames: Vec<Option<u32>> = log.events.iter().map(|event| event.frame).collect();

        assert_eq!(frames, vec![Some(12), Some(12), Some(12)]);
    }

    #[test]
    fn hotspot_event_log_defaults_to_zero_when_no_frames() {
        let events = vec![
            "actor.select manny".to_string(),
            "hotspot.demo.start computer".to_string(),
        ];

        let log = build_hotspot_event_log(&events);

        assert!(
            log.events.iter().all(|event| event.frame == Some(0)),
            "expected fallback frame of 0"
        );
    }

    #[test]
    fn hotspot_event_log_anchors_approach_to_next_frame() {
        let events = vec![
            "movement.frame 114 0.607,2.021".to_string(),
            "hotspot.demo.approach computer".to_string(),
            "movement.frame 1 0.500,1.975".to_string(),
            "movement.frame 2 0.501,1.970".to_string(),
            "hotspot.demo.start computer".to_string(),
        ];

        let log = build_hotspot_event_log(&events);

        let approach = log
            .events
            .iter()
            .find(|event| event.label == "hotspot.demo.approach computer")
            .expect("approach event missing");
        assert_eq!(approach.frame, Some(1));

        let start = log
            .events
            .iter()
            .find(|event| event.label == "hotspot.demo.start computer")
            .expect("start event missing");
        assert_eq!(start.frame, Some(2));
    }
}
