use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::PathBuf,
};

use anyhow::{Context, Result};
use grim_telemetry_schema::{parse_seq_range, StreamKind};
use serde_json::{Map as JsonMap, Value as JsonValue};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub seq_display: String,
    pub seq_min: u64,
    pub seq_max: u64,
    pub orig_seq_display: String,
    pub orig_seq_min: u64,
    pub orig_seq_max: u64,
    pub has_log_seq: bool,
    pub event: String,
    pub stream: StreamKind,
    pub fields: Vec<Field>,
    pub summary: String,
}

impl LogEntry {
    pub fn display_event(&self) -> &str {
        self.event
            .strip_prefix("semantic_")
            .unwrap_or(self.event.as_str())
    }

    pub fn compute_summary(&self) -> String {
        if matches!(
            self.event.as_str(),
            "set_table_entry" | "semantic_set_table_entry"
        ) {
            if let Some(text) = set_table_entry_summary(self) {
                return text;
            }
        }
        let mut summary = String::new();
        let mut count = 0;
        for field in self.fields.iter() {
            if matches!(
                field.key.as_str(),
                "event"
                    | "seq"
                    | "log_seq"
                    | "stream"
                    | "cause"
                    | "engine"
                    | "component"
                    | "vm_id"
                    | "logger"
                    | "run_id"
                    | "wall_ts"
                    | "pid"
                    | "tid"
                    | "ts"
                    | "source"
            ) {
                continue;
            }
            if !summary.is_empty() {
                summary.push(' ');
            }
            summary.push_str(&field.key);
            summary.push('=');
            summary.push_str(&field.value);
            count += 1;
            if count >= 4 {
                break;
            }
        }
        summary
    }

    pub fn field_value(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.as_str())
    }
}

pub fn load_entries(path: &str) -> Result<Vec<LogEntry>> {
    let reader: Box<dyn BufRead> = if path == "-" {
        Box::new(BufReader::new(io::stdin()))
    } else {
        let file = File::open(PathBuf::from(path)).with_context(|| format!("open {path}"))?;
        Box::new(BufReader::new(file))
    };

    let mut entries = Vec::new();
    for line in reader.lines() {
        let raw_line = line?;
        if let Some(entry) = parse_line(&raw_line) {
            entries.push(entry);
        }
    }
    assign_stream_sequences(&mut entries);
    Ok(entries)
}

pub fn compute_parity(
    left_entries: &[LogEntry],
    right_entries: &[LogEntry],
) -> [Vec<Option<ParityStatus>>; 2] {
    let left_map = semantic_event_map(left_entries);
    let right_map = semantic_event_map(right_entries);

    let mut left_parity = vec![None; left_entries.len()];
    let mut right_parity = vec![None; right_entries.len()];

    for (idx, entry) in left_entries.iter().enumerate() {
        if !matches!(entry.stream, StreamKind::Semantic) {
            continue;
        }
        let status = match right_map.get(&entry.seq_min) {
            Some((_, other_event)) if other_event == entry.display_event() => {
                Some(ParityStatus::Match)
            }
            Some((_, other_event)) => Some(ParityStatus::Mismatch {
                other: other_event.clone(),
            }),
            None => Some(ParityStatus::MissingOther),
        };
        left_parity[idx] = status;
    }

    for (idx, entry) in right_entries.iter().enumerate() {
        if !matches!(entry.stream, StreamKind::Semantic) {
            continue;
        }
        let status = match left_map.get(&entry.seq_min) {
            Some((_, other_event)) if other_event == entry.display_event() => {
                Some(ParityStatus::Match)
            }
            Some((_, other_event)) => Some(ParityStatus::Mismatch {
                other: other_event.clone(),
            }),
            None => Some(ParityStatus::MissingOther),
        };
        right_parity[idx] = status;
    }

    [left_parity, right_parity]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParityStatus {
    Match,
    Mismatch { other: String },
    MissingOther,
}

fn parse_line(raw: &str) -> Option<LogEntry> {
    let value: JsonValue = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;

    let event = object.get("event")?.as_str()?.to_string();
    let stream = stream_from_object(&event, object);
    let seq_display = render_seq(object.get("seq")?)?;
    let seq_range = parse_seq_range(&seq_display)?;
    let log_seq_rendered = object.get("log_seq").and_then(render_seq);
    let has_log_seq = log_seq_rendered.is_some();
    let log_seq_display = log_seq_rendered.unwrap_or_else(|| seq_display.clone());
    let log_seq_range = parse_seq_range(&log_seq_display).unwrap_or(seq_range);

    let mut fields = Vec::with_capacity(object.len());
    let mut keys: Vec<_> = object.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = object.get(key) {
            fields.push(Field {
                key: key.clone(),
                value: render_value(value),
            });
        }
    }

    let mut entry = LogEntry {
        seq_display: seq_display.clone(),
        seq_min: seq_range.min,
        seq_max: seq_range.max,
        orig_seq_display: log_seq_display,
        orig_seq_min: log_seq_range.min,
        orig_seq_max: log_seq_range.max,
        has_log_seq,
        event,
        stream,
        fields,
        summary: String::new(),
    };
    entry.summary = entry.compute_summary();
    Some(entry)
}

fn stream_from_object(event: &str, object: &JsonMap<String, JsonValue>) -> StreamKind {
    if let Some(stream) = object.get("stream").and_then(|v| v.as_str()) {
        return StreamKind::from_field(stream);
    }
    if event.starts_with("semantic_") || event == "engine_exit" {
        return StreamKind::Semantic;
    }
    StreamKind::Other
}

fn render_seq(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => Some(text.clone()),
        JsonValue::Number(num) => num.as_u64().map(|n| format!("{n:06}")),
        _ => None,
    }
}

fn render_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
    }
}

fn assign_stream_sequences(entries: &mut [LogEntry]) {
    if entries.iter().all(|entry| entry.has_log_seq) {
        return;
    }

    let mut raw_seq = 0u64;
    let mut semantic_seq = 0u64;

    for entry in entries.iter_mut() {
        match entry.stream {
            StreamKind::Raw | StreamKind::Other => {
                raw_seq = raw_seq.saturating_add(1);
                entry.seq_display = format!("{raw_seq:06}");
                entry.seq_min = raw_seq;
                entry.seq_max = raw_seq;
            }
            StreamKind::Semantic => {
                semantic_seq = semantic_seq.saturating_add(1);
                entry.seq_display = format!("{semantic_seq:06}");
                entry.seq_min = semantic_seq;
                entry.seq_max = semantic_seq;
            }
        }
    }
}

fn semantic_event_map(entries: &[LogEntry]) -> std::collections::HashMap<u64, (usize, String)> {
    let mut map = std::collections::HashMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        if matches!(entry.stream, StreamKind::Semantic) {
            map.insert(entry.seq_min, (idx, entry.display_event().to_string()));
        }
    }
    map
}

fn set_table_entry_summary(entry: &LogEntry) -> Option<String> {
    let table = entry
        .field_value("table_handle_label")
        .or_else(|| entry.field_value("table_handle"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "table".to_string());
    let key = value_preview(entry, "key");
    let value = value_preview(entry, "value");
    Some(format!("{table}[{key}] = {value}"))
}

fn render_preview(raw: &str) -> Option<String> {
    let json: JsonValue = serde_json::from_str(raw).ok()?;
    let obj = json.as_object()?;
    if let Some(preview) = obj.get("preview").and_then(|v| v.as_str()) {
        return Some(preview.to_string());
    }
    if let Some(value) = obj.get("value").and_then(|v| v.as_str()) {
        return Some(value.to_string());
    }
    if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
        return Some(kind.to_string());
    }
    None
}

fn value_preview(entry: &LogEntry, key: &str) -> String {
    entry
        .field_value(key)
        .and_then(render_preview)
        .or_else(|| entry.field_value(key).map(|s| s.to_string()))
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_exit_as_semantic() {
        let line = r#"{"seq":"000010","event":"engine_exit","status":"exit_code","code":1,"note":"boom","stream":"semantic"}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.stream, StreamKind::Semantic);
        assert_eq!(entry.display_event(), "engine_exit");
        assert_eq!(entry.seq_min, 10);
        assert!(entry.summary.contains("status=exit_code"));
    }

    #[test]
    fn omits_cause_from_summary() {
        let line = r#"{"seq":"000011","event":"engine_exit","status":"unknown","note":"oops","cause":"Caused by: runtime error","stream":"semantic"}"#;
        let entry = parse_line(line).expect("should parse");
        assert!(!entry.summary.contains("cause="));
    }

    #[test]
    fn keeps_cause_with_pipes_when_metadata_present() {
        let line = r#"{"seq":"000012","event":"engine_exit","status":"exit_code","cause":"Caused by: | runtime error: boom | stack traceback:","stream":"semantic","wall_ts":"123","pid":1,"tid":2}"#;
        let entry = parse_line(line).expect("should parse");
        let cause = entry.field_value("cause").expect("cause");
        assert!(cause.contains("runtime error: boom"));
        assert!(cause.contains("stack traceback:"));
    }
}
