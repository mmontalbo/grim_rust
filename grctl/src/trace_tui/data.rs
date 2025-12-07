use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::PathBuf,
};

use anyhow::{Context, Result};
use grim_telemetry_common::{parse_seq_range, stream_kind_from_line, StreamKind};
use serde_json::Value as JsonValue;

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
        let mut parts = Vec::new();
        for field in self.fields.iter() {
            if field.key == "event" || field.key == "seq" || field.key == "stream" {
                continue;
            }
            parts.push(format!("{}={}", field.key, field.value));
            if parts.len() >= 4 {
                break;
            }
        }
        parts.join(" ")
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
    let without_metadata = raw
        .split(" | ")
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string());

    let content = if let Some(rest) = without_metadata.strip_prefix('[') {
        if let Some(idx) = rest.find("] ") {
            rest[idx + 2..].to_string()
        } else {
            without_metadata
        }
    } else {
        without_metadata
    };

    let tokens = split_fields(&content);
    if tokens.is_empty() {
        return None;
    }

    let mut fields = Vec::new();
    let mut seq_display = None;
    let mut event = None;
    let mut stream = StreamKind::Other;

    for token in tokens {
        if let Some((key, value)) = token.split_once('=') {
            let key = key.to_string();
            let value = value.to_string();
            if key == "seq" {
                seq_display = Some(value.clone());
            } else if key == "event" {
                event = Some(value.clone());
            } else if key == "stream" {
                stream = StreamKind::from_field(&value);
            }
            fields.push(Field { key, value });
        }
    }

    if matches!(stream, StreamKind::Other) {
        stream = stream_kind_from_line(&content);
    }

    let seq_display = seq_display?;
    let event = event?;
    let seq_range = parse_seq_range(&seq_display)?;
    let seq_min = seq_range.min;
    let seq_max = seq_range.max;

    let mut entry = LogEntry {
        seq_display: seq_display.clone(),
        seq_min,
        seq_max,
        orig_seq_display: seq_display,
        orig_seq_min: seq_min,
        orig_seq_max: seq_max,
        event,
        stream,
        fields,
        summary: String::new(),
    };
    entry.summary = entry.compute_summary();
    Some(entry)
}

fn assign_stream_sequences(entries: &mut [LogEntry]) {
    let mut semantic_seq = 0u64;

    for entry in entries.iter_mut() {
        match entry.stream {
            StreamKind::Semantic => {
                semantic_seq = semantic_seq.saturating_add(1);
                entry.seq_display = format!("{semantic_seq:06}");
                entry.seq_min = semantic_seq;
                entry.seq_max = semantic_seq;
            }
            _ => {
                entry.seq_display = entry.orig_seq_display.clone();
                entry.seq_min = entry.orig_seq_min;
                entry.seq_max = entry.orig_seq_max;
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

fn split_fields(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            '\\' => {
                if let Some('"') = chars.peek() {
                    current.push('"');
                    chars.next();
                } else {
                    current.push('\\');
                }
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn set_table_entry_summary(entry: &LogEntry) -> Option<String> {
    let table = field_value(entry, "table_handle_label")
        .or_else(|| field_value(entry, "table_handle"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "table".to_string());
    let key = value_preview(entry, "key");
    let value = value_preview(entry, "value");
    Some(format!("{table}[{key}] = {value}"))
}

fn field_value<'a>(entry: &'a LogEntry, key: &str) -> Option<&'a str> {
    entry
        .fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.as_str())
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
    field_value(entry, key)
        .and_then(render_preview)
        .or_else(|| field_value(entry, key).map(|s| s.to_string()))
        .unwrap_or_else(|| "?".to_string())
}
