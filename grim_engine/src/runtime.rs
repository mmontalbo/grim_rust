use anyhow::{Error, Result};

use crate::cli::EngineArgs;
use crate::lua_host::{log_engine_exit, run_boot_sequence};

const EXIT_MARKERS: [&str; 4] = ["thread '", "stack traceback", "runtime error", "panic"];

/// Execute the minimal intro boot sequence and record a structured exit event.
pub fn run_intro(args: EngineArgs) -> Result<()> {
    let EngineArgs {
        data_root,
        headless,
        verbose,
    } = args;

    let result = run_boot_sequence(&data_root, verbose, headless);

    match result {
        Ok(()) => {
            log_engine_exit("ok", None, Some(0), None, None);
            Ok(())
        }
        Err(err) => {
            let cause = format_exit_cause(&err);
            let note = err.to_string();
            log_engine_exit("exit_code", Some(&note), Some(1), None, cause.as_deref());
            Err(err)
        }
    }
}

/// Surface a concise cause string from an `anyhow` chain for telemetry.
fn format_exit_cause(err: &Error) -> Option<String> {
    let dump = format!("{err:?}");
    let best = dump.rfind("Caused by:").or_else(|| {
        EXIT_MARKERS
            .iter()
            .filter_map(|marker| dump.rfind(marker))
            .max()
    });
    let snippet = best.map(|idx| &dump[idx..]).unwrap_or(&dump);
    let lines: Vec<String> = snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let joined = lines.join(" | ");
    let max_len = 900;
    if joined.len() > max_len {
        Some(format!("{}…", &joined[..max_len]))
    } else {
        Some(joined)
    }
}
