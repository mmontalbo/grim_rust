use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};

use crate::cli::{LogArgs, RunSelection};
use crate::components::{ComponentKind, Paths};
use crate::trace_tui;

#[derive(Debug, Clone)]
struct EngineExitEvent {
    status: String,
    note: Option<String>,
}

#[derive(Debug, Clone)]
struct ExitSummary {
    summary: String,
    code: Option<i32>,
}

pub fn show_logs(paths: &Paths, component: ComponentKind, args: &LogArgs) -> Result<()> {
    let (run_id, log_path) = resolve_run_path(paths, component, &args.run)?;
    println!("# {} (run {})", log_path.display(), run_id);

    if args.tui {
        if args.follow {
            bail!("--tui currently does not support --follow (live tail)");
        }
        if args.tail > 0 {
            println!("[grctl] --tail is ignored when --tui is set (viewer reads full log)");
        }
        return launch_trace_tui(paths, &log_path);
    }

    if args.follow {
        follow_logs(component, &log_path, args.tail)
    } else {
        let lines = tail_file(&log_path, args.tail)?;
        for line in lines {
            println!("{line}");
        }
        Ok(())
    }
}

pub fn resolve_run_path(
    paths: &Paths,
    component: ComponentKind,
    selection: &RunSelection,
) -> Result<(String, PathBuf)> {
    match selection {
        RunSelection::Latest => {
            let run_dir = paths.component_log_dir(component)?;
            let mut runs = paths.list_run_logs(component)?;
            if runs.is_empty() {
                bail!(
                    "no runs recorded yet for {} under {}",
                    component.display(),
                    run_dir.display()
                );
            }
            runs.sort_by_key(|(_, path)| {
                path.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            });
            let (run_id, path) = runs.pop().expect("non-empty after sort ensured above");
            Ok((run_id, path))
        }
        RunSelection::Id(run_id) => {
            let path = paths.run_log_path(component, run_id)?;
            if !path.exists() {
                bail!(
                    "no log for run {} at {} (use --run latest to pick the newest run)",
                    run_id,
                    path.display()
                );
            }
            Ok((run_id.clone(), path))
        }
    }
}

pub fn launch_trace_tui(paths: &Paths, log_path: &Path) -> Result<()> {
    println!("[grctl] launching trace_tui for {}", log_path.display());
    let args = vec![log_path.as_os_str().to_owned()];
    launch_trace_tui_with_args(paths, &args)
}

pub fn launch_parity_tui(paths: &Paths, engine_log: &Path, retail_log: &Path) -> Result<()> {
    println!(
        "[grctl] launching trace_tui for {} (engine) and {} (retail)",
        engine_log.display(),
        retail_log.display()
    );
    let args = vec![
        engine_log.as_os_str().to_owned(),
        retail_log.as_os_str().to_owned(),
        OsString::from("--left-label"),
        OsString::from("engine"),
        OsString::from("--right-label"),
        OsString::from("retail"),
    ];
    launch_trace_tui_with_args(paths, &args)
}

fn launch_trace_tui_with_args(_paths: &Paths, args: &[OsString]) -> Result<()> {
    trace_tui::run_with_args(args)
}

pub fn follow_logs(component: ComponentKind, path: &Path, tail: usize) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut exit_event: Option<EngineExitEvent> = None;
    let mut exit_summary: Option<ExitSummary> = None;
    let mut exit_seen_at: Option<Instant> = None;

    if tail == 0 {
        for line in reader.by_ref().lines() {
            let line = line?;
            process_log_line(
                component,
                &line,
                &mut exit_event,
                &mut exit_summary,
                &mut exit_seen_at,
            );
        }
    } else {
        let mut buffer: VecDeque<String> = VecDeque::with_capacity(tail.max(1));
        for line in reader.by_ref().lines() {
            let line = line?;
            if buffer.len() == tail {
                buffer.pop_front();
            }
            buffer.push_back(line);
        }
        for line in buffer {
            process_log_line(
                component,
                &line,
                &mut exit_event,
                &mut exit_summary,
                &mut exit_seen_at,
            );
        }
    }

    if should_finish(
        component,
        exit_event.as_ref(),
        exit_summary.as_ref(),
        exit_seen_at,
        false,
    ) {
        print_exit_footer(exit_event.as_ref(), exit_summary.as_ref());
        return Ok(());
    }
    if should_finish(
        component,
        exit_event.as_ref(),
        exit_summary.as_ref(),
        exit_seen_at,
        true,
    ) {
        print_exit_footer(exit_event.as_ref(), exit_summary.as_ref());
        return Ok(());
    }

    let mut file = reader.into_inner();
    let mut pending: Vec<u8> = Vec::new();

    loop {
        let mut chunk = [0u8; 4096];
        let read = loop {
            match file.read(&mut chunk) {
                Ok(count) => break count,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context(format!("reading {}", path.display())),
            }
        };
        if read == 0 {
            let current_pos = file.stream_position()?;
            let file_len = file.metadata()?.len();
            if file_len < current_pos {
                println!(
                    "[grctl] {} log truncated; restarting stream",
                    path.display()
                );
                file.seek(SeekFrom::Start(0))?;
                pending.clear();
                exit_event = None;
                exit_summary = None;
                exit_seen_at = None;
            }
            if should_finish(
                component,
                exit_event.as_ref(),
                exit_summary.as_ref(),
                exit_seen_at,
                true,
            ) {
                print_exit_footer(exit_event.as_ref(), exit_summary.as_ref());
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
            continue;
        }

        pending.extend_from_slice(&chunk[..read]);

        loop {
            let newline_pos = pending.iter().position(|&b| b == b'\n');
            let Some(pos) = newline_pos else { break };
            let line_bytes: Vec<u8> = pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            process_log_line(
                component,
                line,
                &mut exit_event,
                &mut exit_summary,
                &mut exit_seen_at,
            );
        }
        if should_finish(
            component,
            exit_event.as_ref(),
            exit_summary.as_ref(),
            exit_seen_at,
            false,
        ) {
            print_exit_footer(exit_event.as_ref(), exit_summary.as_ref());
            return Ok(());
        }
    }
}

fn process_log_line(
    component: ComponentKind,
    line: &str,
    exit_event: &mut Option<EngineExitEvent>,
    exit_summary: &mut Option<ExitSummary>,
    exit_seen_at: &mut Option<Instant>,
) {
    if component == ComponentKind::Engine {
        if exit_event.is_none() {
            if let Some(event) = parse_engine_exit_event(line) {
                *exit_seen_at = Some(Instant::now());
                *exit_event = Some(event);
            }
        }
        if exit_summary.is_none() {
            if let Some(summary) = parse_exit_summary(line, component) {
                *exit_summary = Some(summary);
            }
        }
    }
    println!("{line}");
}

fn parse_engine_exit_event(line: &str) -> Option<EngineExitEvent> {
    let content = strip_log_metadata(line);
    let fields = tokenize_kv_fields(content);
    if fields.get("event").map(|v| v.as_str()) != Some("engine_exit") {
        return None;
    }
    let status = fields.get("status")?.clone();
    let note = fields.get("note").cloned();
    Some(EngineExitEvent { status, note })
}

fn tokenize_kv_fields(line: &str) -> std::collections::HashMap<String, String> {
    let mut fields = std::collections::HashMap::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if in_quotes => {
                if let Some('"') = chars.peek().copied() {
                    chars.next();
                    current.push('"');
                } else {
                    current.push('\\');
                }
            }
            '"' => in_quotes = !in_quotes,
            ch if ch.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    if let Some((key, value)) = current.split_once('=') {
                        fields.insert(key.to_string(), value.to_string());
                    }
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        if let Some((key, value)) = current.split_once('=') {
            fields.insert(key.to_string(), value.to_string());
        }
    }
    fields
}

pub fn strip_log_metadata(line: &str) -> &str {
    let without_metadata = line.split(" | ").next().unwrap_or(line);
    if let Some(rest) = without_metadata.strip_prefix('[') {
        if let Some(idx) = rest.find("] ") {
            return &rest[idx + 2..];
        }
    }
    without_metadata
}

fn parse_exit_summary(line: &str, component: ComponentKind) -> Option<ExitSummary> {
    let needle = format!("{} exited with ", component.display());
    let start = line.find(&needle)?;
    let summary = line[start + needle.len()..].trim();
    Some(ExitSummary {
        code: parse_exit_code(summary),
        summary: summary.to_string(),
    })
}

fn parse_exit_code(summary: &str) -> Option<i32> {
    if let Some(rest) = summary.strip_prefix("exit status: ") {
        return rest
            .split_whitespace()
            .next()
            .and_then(|num| num.parse().ok());
    }
    if let Some(rest) = summary.strip_prefix("signal: ") {
        return rest
            .split_whitespace()
            .next()
            .and_then(|num| num.parse().ok());
    }
    None
}

fn should_finish(
    component: ComponentKind,
    exit_event: Option<&EngineExitEvent>,
    exit_summary: Option<&ExitSummary>,
    exit_seen_at: Option<Instant>,
    idle: bool,
) -> bool {
    if component != ComponentKind::Engine {
        return false;
    }
    if exit_summary.is_some() {
        return true;
    }
    if idle {
        if let Some(seen) = exit_seen_at {
            if seen.elapsed() >= Duration::from_secs(1) && exit_event.is_some() {
                return true;
            }
        }
    }
    false
}

fn print_exit_footer(exit_event: Option<&EngineExitEvent>, exit_summary: Option<&ExitSummary>) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(event) = exit_event {
        let mut status = format!("engine_exit: status={}", event.status);
        if let Some(note) = &event.note {
            status.push(' ');
            status.push_str(&format!("note=\"{}\"", note.replace('"', "\\\"")));
        }
        parts.push(status);
    } else {
        parts.push("engine_exit: missing".to_string());
    }

    match exit_summary {
        Some(summary) => {
            if let Some(code) = summary.code {
                parts.push(format!("exit code {}", code));
            } else {
                parts.push(format!("exit {}", summary.summary));
            }
        }
        None => parts.push("exit status unknown".to_string()),
    }

    println!("[grctl] {}", parts.join(" "));
}

pub fn tail_file(path: &Path, tail: usize) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    if tail == 0 {
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line in reader.lines() {
            lines.push(line?);
        }
        return Ok(lines);
    }

    let reader = BufReader::new(file);
    let mut buffer: VecDeque<String> = VecDeque::with_capacity(tail);
    for line in reader.lines() {
        let line = line?;
        if buffer.len() == tail {
            buffer.pop_front();
        }
        buffer.push_back(line);
    }
    Ok(buffer.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_log_metadata_and_annotations() {
        assert_eq!(strip_log_metadata("[INFO] foo"), "foo");
        assert_eq!(strip_log_metadata("[INFO] foo | metadata"), "foo");
    }

    #[test]
    fn parses_engine_exit_with_quoted_note() {
        let line = r#"[INFO] event=engine_exit status=ok note="engine done""#;
        let event = parse_engine_exit_event(line).expect("should parse");
        assert_eq!(event.status, "ok");
        assert_eq!(event.note.as_deref(), Some("engine done"));
    }

    #[test]
    fn parses_exit_codes() {
        assert_eq!(parse_exit_code("exit status: 42"), Some(42));
        assert_eq!(parse_exit_code("signal: 9 (SIGKILL)"), Some(9));
        assert_eq!(parse_exit_code("unknown"), None);
    }
}
