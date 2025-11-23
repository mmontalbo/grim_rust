//! Intro timeline comparison helpers shared by the scenario harness.
#![allow(dead_code)]
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::scenario::ScenarioContext;

/// Full diff between the intro timeline emitted by grim_engine and the retail capture.
#[derive(Debug, Serialize)]
pub struct IntroTimelineReport {
    pub engine_events: Vec<String>,
    pub retail_events: Vec<String>,
    pub missing_in_engine: Vec<String>,
    pub missing_in_retail: Vec<String>,
    pub order_matches: bool,
}

impl IntroTimelineReport {
    fn new(engine_events: Vec<String>, retail_events: Vec<String>) -> Self {
        let missing_in_engine = missing_events(&retail_events, &engine_events);
        let missing_in_retail = missing_events(&engine_events, &retail_events);
        let order_matches = engine_events == retail_events;
        Self {
            engine_events,
            retail_events,
            missing_in_engine,
            missing_in_retail,
            order_matches,
        }
    }
}

/// Analyze the intro timeline output from both binaries and produce a structured diff.
pub fn analyze_intro_timeline(
    ctx: &ScenarioContext,
    engine_log: &Path,
) -> Result<Option<IntroTimelineReport>> {
    let engine_events = collect_engine_intro_timeline(engine_log)?;
    let telemetry_path = ctx.telemetry_events_path();
    if !telemetry_path.exists() {
        eprintln!(
            "[grim_scenarios] telemetry file {} missing; intro timeline comparison unavailable",
            telemetry_path.display()
        );
        return Ok(None);
    }
    let retail_events = collect_retail_intro_timeline(&telemetry_path)?;
    if engine_events.is_empty() && retail_events.is_empty() {
        eprintln!("[grim_scenarios] intro timeline telemetry empty in both engine and retail logs");
        return Ok(None);
    }
    Ok(Some(IntroTimelineReport::new(engine_events, retail_events)))
}

/// Pull intro timeline labels from the engine log.
fn collect_engine_intro_timeline(log_path: &Path) -> Result<Vec<String>> {
    collect_intro_timeline(log_path, "engine log")
}

/// Extract intro timeline labels from the retail telemetry JSONL file.
fn collect_retail_intro_timeline(path: &Path) -> Result<Vec<String>> {
    collect_intro_timeline(path, "telemetry")
}

fn collect_intro_timeline(path: &Path, label: &str) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("opening {label} {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(event) = parse_intro_timeline_event(&line) {
            events.push(event);
        }
    }
    Ok(events)
}

/// Lightweight JSON helper for intro timeline telemetry lines.
pub fn parse_intro_timeline_event(line: &str) -> Option<String> {
    let start = line.find('{')?;
    let value: Value = serde_json::from_str(&line[start..]).ok()?;
    if value.get("label").and_then(|label| label.as_str()) != Some("intro.timeline") {
        return None;
    }
    value
        .get("data")
        .and_then(|data| data.get("event"))
        .and_then(|event| event.as_str())
        .map(|event| event.to_string())
}

fn missing_events(expected: &[String], actual: &[String]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for event in actual {
        *counts.entry(event).or_insert(0) += 1;
    }
    let mut missing = Vec::new();
    for event in expected {
        match counts.get_mut(event.as_str()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing.push(event.clone()),
        }
    }
    missing
}

/// Print the high-level comparison summary to stdout so scenario runs stay informative.
pub fn summarize_intro_timeline(report: &IntroTimelineReport) {
    let total = report.engine_events.len().max(report.retail_events.len());
    if report.missing_in_engine.is_empty() && report.missing_in_retail.is_empty() {
        if report.order_matches {
            println!(
                "[grim_scenarios] intro timeline matches across engine and retail ({} events)",
                total
            );
        } else {
            println!(
                "[grim_scenarios] intro timeline events match, but ordering differs; see scenario artifacts for details"
            );
        }
        return;
    }
    if !report.missing_in_engine.is_empty() {
        println!(
            "[grim_scenarios] intro timeline missing in engine: {}",
            report.missing_in_engine.join(", ")
        );
    }
    if !report.missing_in_retail.is_empty() {
        println!(
            "[grim_scenarios] intro timeline missing in retail: {}",
            report.missing_in_retail.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_intro_timeline_event_extracts_event_name() {
        let line = r#"{"label":"intro.timeline","data":{"event":"movie.intro.start"}}"#;
        assert_eq!(
            parse_intro_timeline_event(line).as_deref(),
            Some("movie.intro.start")
        );
    }

    #[test]
    fn parse_intro_timeline_event_skips_unrelated_lines() {
        let line = r#"{"label":"other.timeline","data":{"event":"movie.intro.start"}}"#;
        assert!(parse_intro_timeline_event(line).is_none());
        assert!(parse_intro_timeline_event("not json").is_none());
    }

    #[test]
    fn parse_intro_timeline_event_ignores_prefixes() {
        let line =
            r#"[grim_engine] {"label":"intro.timeline","data":{"event":"movie.logos.start"}}"#;
        assert_eq!(
            parse_intro_timeline_event(line).as_deref(),
            Some("movie.logos.start")
        );
    }

    #[test]
    fn missing_events_counts_duplicates_and_order() {
        let expected = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        let actual = vec!["a".to_string()];
        assert_eq!(
            missing_events(&expected, &actual),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn intro_timeline_report_handles_order_mismatch_without_missing_events() {
        let report = IntroTimelineReport::new(
            vec!["movie.logos.start".into(), "movie.logos.end".into()],
            vec!["movie.logos.end".into(), "movie.logos.start".into()],
        );
        assert!(!report.order_matches);
        assert!(report.missing_in_engine.is_empty());
        assert!(report.missing_in_retail.is_empty());
    }
}
