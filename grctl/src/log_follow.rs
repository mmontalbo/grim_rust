use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use serde_json::Value as JsonValue;

use crate::cli::{LogArgs, RunSelection};
use crate::components::{ComponentKind, Paths};
use crate::trace_tui;

#[derive(Debug, Clone)]
struct ExitEvent {
    kind: ExitKind,
    status: Option<String>,
    note: Option<String>,
    code: Option<i32>,
    signal: Option<i32>,
    cause: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ExitKind {
    Engine,
    Component,
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
    let mut engine_exit: Option<ExitEvent> = None;
    let mut component_exit: Option<ExitEvent> = None;
    let mut exit_seen_at: Option<Instant> = None;

    if tail == 0 {
        for line in reader.by_ref().lines() {
            let line = line?;
            process_log_line(
                component,
                &line,
                &mut engine_exit,
                &mut component_exit,
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
                &mut engine_exit,
                &mut component_exit,
                &mut exit_seen_at,
            );
        }
    }

    if should_finish(
        component,
        component_exit.as_ref(),
        engine_exit.as_ref(),
        exit_seen_at,
        false,
    ) {
        print_exit_footer(engine_exit.as_ref(), component_exit.as_ref());
        return Ok(());
    }
    if should_finish(
        component,
        component_exit.as_ref(),
        engine_exit.as_ref(),
        exit_seen_at,
        true,
    ) {
        print_exit_footer(engine_exit.as_ref(), component_exit.as_ref());
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
                engine_exit = None;
                component_exit = None;
                exit_seen_at = None;
            }
            if should_finish(
                component,
                component_exit.as_ref(),
                engine_exit.as_ref(),
                exit_seen_at,
                true,
            ) {
                print_exit_footer(engine_exit.as_ref(), component_exit.as_ref());
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
                &mut engine_exit,
                &mut component_exit,
                &mut exit_seen_at,
            );
        }
        if should_finish(
            component,
            component_exit.as_ref(),
            engine_exit.as_ref(),
            exit_seen_at,
            false,
        ) {
            print_exit_footer(engine_exit.as_ref(), component_exit.as_ref());
            return Ok(());
        }
    }
}

fn process_log_line(
    component: ComponentKind,
    line: &str,
    engine_exit: &mut Option<ExitEvent>,
    component_exit: &mut Option<ExitEvent>,
    exit_seen_at: &mut Option<Instant>,
) {
    if component == ComponentKind::Engine {
        if let Some(event) = parse_exit_event(line) {
            match event.kind {
                ExitKind::Engine => {
                    *exit_seen_at = Some(Instant::now());
                    *engine_exit = Some(event);
                }
                ExitKind::Component => {
                    *component_exit = Some(event);
                    *exit_seen_at = Some(Instant::now());
                }
            }
        }
    }
    println!("{line}");
}

fn parse_exit_event(line: &str) -> Option<ExitEvent> {
    let value: JsonValue = serde_json::from_str(line).ok()?;
    let obj = value.as_object()?;
    let event = obj.get("event")?.as_str()?;
    let status = obj
        .get("status")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let note = obj.get("note").and_then(|v| v.as_str()).map(str::to_string);
    let code = obj.get("code").and_then(|v| v.as_i64()).map(|v| v as i32);
    let signal = obj.get("signal").and_then(|v| v.as_i64()).map(|v| v as i32);
    let cause = obj
        .get("cause")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match event {
        "engine_exit" => Some(ExitEvent {
            kind: ExitKind::Engine,
            status,
            note,
            code,
            signal,
            cause,
        }),
        "component_exit" => Some(ExitEvent {
            kind: ExitKind::Component,
            status,
            note,
            code,
            signal,
            cause,
        }),
        _ => None,
    }
}

fn should_finish(
    component: ComponentKind,
    component_exit: Option<&ExitEvent>,
    engine_exit: Option<&ExitEvent>,
    exit_seen_at: Option<Instant>,
    idle: bool,
) -> bool {
    if component != ComponentKind::Engine {
        return false;
    }
    if component_exit.is_some() {
        return true;
    }
    if idle {
        if let Some(seen) = exit_seen_at {
            if seen.elapsed() >= Duration::from_secs(1) && engine_exit.is_some() {
                return true;
            }
        }
    }
    false
}

fn print_exit_footer(engine_exit: Option<&ExitEvent>, component_exit: Option<&ExitEvent>) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(event) = engine_exit {
        let mut status = String::from("engine_exit:");
        if let Some(label) = &event.status {
            status.push_str(&format!(" status={}", label));
        }
        if let Some(note) = &event.note {
            status.push(' ');
            status.push_str(&format!("note=\"{}\"", note.replace('"', "\\\"")));
        }
        parts.push(status);
    }

    if let Some(exit) = component_exit {
        let mut status = String::from("component_exit:");
        if let Some(label) = &exit.status {
            status.push_str(&format!(" status={}", label));
        }
        if let Some(code) = exit.code {
            status.push_str(&format!(" code={}", code));
        }
        if let Some(signal) = exit.signal {
            status.push_str(&format!(" signal={}", signal));
        }
        if let Some(note) = &exit.note {
            status.push(' ');
            status.push_str(&format!("note=\"{}\"", note.replace('"', "\\\"")));
        }
        if let Some(cause) = &exit.cause {
            status.push(' ');
            status.push_str(&format!("cause=\"{}\"", cause.replace('"', "\\\"")));
        }
        parts.push(status);
    }

    if parts.is_empty() {
        parts.push("exit status unknown".to_string());
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
    fn parses_engine_exit_event_from_json() {
        let line = r#"{"seq":"000001","event":"engine_exit","status":"ok","note":"done","stream":"semantic"}"#;
        let event = parse_exit_event(line).expect("event");
        assert_eq!(event.kind, ExitKind::Engine);
        assert_eq!(event.status.as_deref(), Some("ok"));
        assert_eq!(event.note.as_deref(), Some("done"));
    }

    #[test]
    fn parses_component_exit_with_codes_and_cause() {
        let line = r#"{"seq":"000000","event":"component_exit","status":"exit_code","code":1,"note":"boom","cause":"runtime error","stream":"semantic"}"#;
        let event = parse_exit_event(line).expect("event");
        assert_eq!(event.kind, ExitKind::Component);
        assert_eq!(event.status.as_deref(), Some("exit_code"));
        assert_eq!(event.code, Some(1));
        assert_eq!(event.note.as_deref(), Some("boom"));
        assert_eq!(event.cause.as_deref(), Some("runtime error"));
    }
}
