use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use grim_telemetry_common::{
    normalize_seq_for_filter, parse_seq_field, stream_kind_from_line, SeqRange, StreamFilter,
    StreamKind,
};
use serde_json::Value as JsonValue;

use crate::cli::{ParityLogsArgs, RunSelection};
use crate::components::{ComponentKind, Paths};
use crate::log_follow;

type AlignedRow = (u64, Option<String>, Option<String>);

#[derive(Debug)]
struct EventLine {
    seq: u64,
    event: String,
}

#[derive(Debug)]
struct FirstDiff {
    position: usize,
    engine: EventLine,
    retail: EventLine,
    context: Vec<String>,
}

fn classify_line(line: &str) -> Option<(StreamKind, SeqRange)> {
    let stream = stream_kind_from_line(line);
    let seq = parse_seq_field(line)?;
    Some((stream, seq))
}

pub fn parity_logs(args: ParityLogsArgs, paths: &Paths) -> Result<()> {
    let run_id = resolve_parity_run_id(paths, &args.run)?;
    let engine_log = paths.run_log_path(ComponentKind::Engine, &run_id)?;
    let retail_log = paths.run_log_path(ComponentKind::Retail, &run_id)?;
    let stream_filter = if args.raw {
        StreamFilter::Raw
    } else {
        StreamFilter::Semantic
    };
    let semantic_only = matches!(stream_filter, StreamFilter::Semantic);
    if !engine_log.exists() {
        bail!(
            "engine log missing for run {} at {}",
            run_id,
            engine_log.display()
        );
    }
    if !retail_log.exists() {
        bail!(
            "retail log missing for run {} at {}",
            run_id,
            retail_log.display()
        );
    }

    println!("[grctl] parity logs for run {run_id}");
    println!("  engine log: {}", engine_log.display());
    println!("  retail log: {}", retail_log.display());
    println!(
        "  stream: {}",
        if semantic_only { "semantic" } else { "raw" }
    );

    if args.first_diff {
        if args.follow {
            bail!("--first-diff is incompatible with --follow");
        }
        let report = first_diff_report(&engine_log, &retail_log, stream_filter, args.window)?;
        match report {
            None => {
                println!("[grctl] no divergences found");
            }
            Some(report) => {
                println!("[grctl] first divergence at position {}", report.position);
                println!(
                    "  engine: seq={:06} event={}",
                    report.engine.seq, report.engine.event
                );
                println!(
                    "  retail: seq={:06} event={}",
                    report.retail.seq, report.retail.event
                );
                println!();
                println!("[grctl] context:");
                for entry in report.context {
                    println!("{entry}");
                }
            }
        }
        return Ok(());
    }
    if args.tui {
        if args.from_start {
            println!("[grctl] --from-start is ignored with --tui (viewer reads full logs)");
        } else if args.backfill > 0 {
            println!("[grctl] --backfill is ignored with --tui (viewer reads full logs)");
        }
        println!();
        return log_follow::launch_parity_tui(paths, &engine_log, &retail_log);
    }
    if !args.follow {
        if args.from_start {
            println!("[grctl] --from-start has no effect without --follow; showing full logs");
        }
        if args.backfill > 0 {
            println!("[grctl] --backfill has no effect without --follow; showing full logs");
        }
        println!();
        return print_full_parity_log(&engine_log, &retail_log, stream_filter);
    }
    println!("  poll: {}ms", args.poll_ms);
    if args.from_start {
        println!("  starting from beginning of both logs");
    } else if args.backfill > 0 {
        println!("  backfill last {} seqs before following", args.backfill);
    } else {
        println!("  starting from end (no backfill)");
    }
    println!();

    let mut printed: HashMap<u64, (Option<String>, Option<String>)> = HashMap::new();
    if !args.from_start && args.backfill > 0 {
        let pairs = backfill_pairs(&engine_log, &retail_log, args.backfill, stream_filter)?;
        for (seq, engine_line, retail_line) in pairs {
            print_aligned_row(seq, engine_line.as_deref(), retail_line.as_deref());
            printed.insert(seq, (engine_line, retail_line));
        }
    }

    let mut engine_pos = if args.from_start {
        0
    } else {
        fs::metadata(&engine_log).map(|m| m.len()).unwrap_or(0)
    };
    let mut retail_pos = if args.from_start {
        0
    } else {
        fs::metadata(&retail_log).map(|m| m.len()).unwrap_or(0)
    };
    let mut engine_sem_seq = if semantic_only && !args.from_start {
        collect_lines(&engine_log, StreamFilter::Semantic)?.len() as u64
    } else {
        0
    };
    let mut retail_sem_seq = if semantic_only && !args.from_start {
        collect_lines(&retail_log, StreamFilter::Semantic)?.len() as u64
    } else {
        0
    };

    let poll = Duration::from_millis(args.poll_ms);
    loop {
        let mut new_seqs: BTreeSet<u64> = BTreeSet::new();

        let engine_lines = match read_new_lines(&engine_log, &mut engine_pos) {
            Ok(lines) => lines,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                println!(
                    "[grctl] waiting for engine log to appear at {}; retrying...",
                    engine_log.display()
                );
                thread::sleep(poll);
                continue;
            }
            Err(err) => return Err(err).context(format!("reading {}", engine_log.display())),
        };
        let retail_lines = match read_new_lines(&retail_log, &mut retail_pos) {
            Ok(lines) => lines,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                println!(
                    "[grctl] waiting for retail log to appear at {}; retrying...",
                    retail_log.display()
                );
                thread::sleep(poll);
                continue;
            }
            Err(err) => return Err(err).context(format!("reading {}", retail_log.display())),
        };

        let engine_events = normalize_new_events(engine_lines, stream_filter, &mut engine_sem_seq);
        let retail_events = normalize_new_events(retail_lines, stream_filter, &mut retail_sem_seq);

        for (seq, line) in engine_events {
            let entry = printed.entry(seq).or_insert((None, None));
            if entry.0.is_none() {
                entry.0 = Some(line);
            }
            new_seqs.insert(seq);
        }
        for (seq, line) in retail_events {
            let entry = printed.entry(seq).or_insert((None, None));
            if entry.1.is_none() {
                entry.1 = Some(line);
            }
            new_seqs.insert(seq);
        }

        for seq in new_seqs {
            if let Some((engine_line, retail_line)) = printed.get(&seq) {
                print_aligned_row(seq, engine_line.as_deref(), retail_line.as_deref());
            }
        }

        thread::sleep(poll);
    }
}

fn resolve_parity_run_id(paths: &Paths, selection: &RunSelection) -> Result<String> {
    match selection {
        RunSelection::Id(run_id) => Ok(run_id.clone()),
        RunSelection::Latest => {
            let mut runs = paths.list_run_logs(ComponentKind::Engine)?;
            runs.sort_by_key(|(_, path)| {
                path.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            });
            let Some((run_id, _)) = runs.pop() else {
                bail!("no engine runs recorded yet");
            };
            Ok(run_id)
        }
    }
}

fn collect_lines(path: &Path, filter: StreamFilter) -> Result<Vec<(u64, String)>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    let mut semantic_seq = 0u64;
    for line in reader.lines() {
        let line = line?;
        let Some((stream, seq)) = classify_line(&line) else {
            continue;
        };
        if let Some(display_seq) = normalize_seq_for_filter(stream, seq, filter, &mut semantic_seq)
        {
            entries.push((display_seq.min, line));
        }
    }
    Ok(entries)
}

fn tail_lines_by_seq(
    path: &Path,
    limit: usize,
    filter: StreamFilter,
) -> Result<Vec<(u64, String)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut entries = collect_lines(path, filter)?;
    if entries.len() > limit {
        let start = entries.len().saturating_sub(limit);
        entries = entries.split_off(start);
    }
    Ok(entries)
}

fn backfill_pairs(
    engine_log: &Path,
    retail_log: &Path,
    limit: usize,
    filter: StreamFilter,
) -> Result<Vec<AlignedRow>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let engine_events = tail_lines_by_seq(engine_log, limit, filter)?;
    let retail_events = tail_lines_by_seq(retail_log, limit, filter)?;
    let mut engine_map = HashMap::new();
    let mut retail_map = HashMap::new();
    for (seq, line) in engine_events {
        engine_map.insert(seq, line);
    }
    for (seq, line) in retail_events {
        retail_map.insert(seq, line);
    }
    let mut seqs: BTreeSet<u64> = engine_map
        .keys()
        .copied()
        .chain(retail_map.keys().copied())
        .collect();
    while seqs.len() > limit {
        seqs.pop_first();
    }
    let mut rows = Vec::new();
    for seq in seqs {
        rows.push((seq, engine_map.remove(&seq), retail_map.remove(&seq)));
    }
    Ok(rows)
}

fn first_diff_report(
    engine_log: &Path,
    retail_log: &Path,
    filter: StreamFilter,
    window: usize,
) -> Result<Option<FirstDiff>> {
    let engine_entries = collect_lines(engine_log, filter)?;
    let retail_entries = collect_lines(retail_log, filter)?;
    if engine_entries.is_empty() && retail_entries.is_empty() {
        return Ok(None);
    }

    let mut idx = 0;
    while idx < engine_entries.len() && idx < retail_entries.len() {
        let engine_event = extract_event(&engine_entries[idx].1);
        let retail_event = extract_event(&retail_entries[idx].1);
        if engine_event != retail_event {
            return Ok(Some(build_first_diff(
                idx,
                &engine_entries,
                &retail_entries,
                window,
                engine_event.unwrap_or_else(|| "<unknown>".to_string()),
                retail_event.unwrap_or_else(|| "<unknown>".to_string()),
            )));
        }
        idx += 1;
    }

    if engine_entries.len() != retail_entries.len() {
        let position = idx + 1;
        let engine_entry = engine_entries.get(idx).cloned();
        let retail_entry = retail_entries.get(idx).cloned();
        let engine = engine_entry
            .as_ref()
            .map(|(seq, line)| EventLine {
                seq: *seq,
                event: extract_event(line).unwrap_or_else(|| "<unknown>".to_string()),
            })
            .unwrap_or(EventLine {
                seq: 0,
                event: "(missing)".to_string(),
            });
        let retail = retail_entry
            .as_ref()
            .map(|(seq, line)| EventLine {
                seq: *seq,
                event: extract_event(line).unwrap_or_else(|| "<unknown>".to_string()),
            })
            .unwrap_or(EventLine {
                seq: 0,
                event: "(missing)".to_string(),
            });
        let context = gather_context(idx, &engine_entries, &retail_entries, window);
        return Ok(Some(FirstDiff {
            position,
            engine,
            retail,
            context,
        }));
    }

    Ok(None)
}

fn build_first_diff(
    idx: usize,
    engine_entries: &[(u64, String)],
    retail_entries: &[(u64, String)],
    window: usize,
    engine_event: String,
    retail_event: String,
) -> FirstDiff {
    let engine_line = &engine_entries[idx];
    let retail_line = &retail_entries[idx];
    FirstDiff {
        position: idx + 1,
        engine: EventLine {
            seq: engine_line.0,
            event: engine_event,
        },
        retail: EventLine {
            seq: retail_line.0,
            event: retail_event,
        },
        context: gather_context(idx, engine_entries, retail_entries, window),
    }
}

fn gather_context(
    center_idx: usize,
    engine_entries: &[(u64, String)],
    retail_entries: &[(u64, String)],
    window: usize,
) -> Vec<String> {
    let start = center_idx.saturating_sub(window);
    let end = (center_idx + window + 1).min(engine_entries.len().max(retail_entries.len()));
    let mut rows = Vec::new();
    for i in start..end {
        let engine = engine_entries
            .get(i)
            .map(|(seq, line)| format!("E {:06}: {}", seq, line))
            .unwrap_or_else(|| "E ------: (missing)".to_string());
        let retail = retail_entries
            .get(i)
            .map(|(seq, line)| format!("R {:06}: {}", seq, line))
            .unwrap_or_else(|| "R ------: (missing)".to_string());
        rows.push(engine);
        rows.push(retail);
        rows.push(String::from("---"));
    }
    rows
}

fn extract_event(line: &str) -> Option<String> {
    let value: JsonValue = serde_json::from_str(line).ok()?;
    value
        .get("event")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn print_aligned_row(seq: u64, engine: Option<&str>, retail: Option<&str>) {
    let engine_text = engine.unwrap_or("(missing in engine)");
    let retail_text = retail.unwrap_or("(missing in retail)");
    println!("seq={seq:06}");
    println!("  engine: {engine_text}");
    println!("  retail: {retail_text}");
}

fn print_full_parity_log(engine_log: &Path, retail_log: &Path, filter: StreamFilter) -> Result<()> {
    let semantic_only = matches!(filter, StreamFilter::Semantic);
    let engine_entries = collect_lines(engine_log, filter)?;
    let retail_entries = collect_lines(retail_log, filter)?;
    let mut engine_map: HashMap<u64, String> = engine_entries.into_iter().collect();
    let mut retail_map: HashMap<u64, String> = retail_entries.into_iter().collect();

    let mut rows: BTreeMap<u64, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut seqs: BTreeSet<u64> = engine_map
        .keys()
        .copied()
        .chain(retail_map.keys().copied())
        .collect();
    if semantic_only && seqs.is_empty() {
        return Ok(());
    }
    if semantic_only {
        if let Some(max) = seqs.iter().copied().max() {
            seqs = (1..=max).collect();
        }
    }

    for seq in seqs {
        let entry = rows.entry(seq).or_insert((None, None));
        if let Some(line) = engine_map.remove(&seq) {
            entry.0 = Some(line);
        }
        if let Some(line) = retail_map.remove(&seq) {
            entry.1 = Some(line);
        }
    }

    for (seq, (engine, retail)) in rows {
        print_aligned_row(seq, engine.as_deref(), retail.as_deref());
    }

    Ok(())
}

fn read_new_lines(path: &Path, position: &mut u64) -> io::Result<Vec<String>> {
    let mut reader = BufReader::new(File::open(path)?);
    let len = reader.get_ref().metadata()?.len();
    if len < *position {
        *position = 0;
    }
    reader.seek(SeekFrom::Start(*position))?;
    let mut lines = Vec::new();
    let mut new_pos = *position;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        new_pos += bytes as u64;
        lines.push(line.trim_end_matches(&['\n', '\r'][..]).to_string());
    }
    *position = new_pos;
    Ok(lines)
}

fn normalize_new_events(
    lines: Vec<String>,
    filter: StreamFilter,
    semantic_counter: &mut u64,
) -> Vec<(u64, String)> {
    let mut events = Vec::new();
    for line in lines {
        let Some((stream, seq_range)) = classify_line(&line) else {
            continue;
        };
        if let Some(seq) = normalize_seq_for_filter(stream, seq_range, filter, semantic_counter) {
            events.push((seq.min, line));
        }
    }
    events
}
