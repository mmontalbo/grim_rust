use anyhow::{Error, Result};

use crate::cli::RunLuaArgs;
use crate::lua_host::{log_engine_exit, run_boot_sequence};

pub fn execute(args: RunLuaArgs) -> Result<()> {
    let RunLuaArgs {
        data_root,
        headless,
        verbose,
    } = args;

    let result = (|| -> Result<()> {
        run_boot_sequence(&data_root, verbose, headless)
    })();

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

fn format_exit_cause(err: &Error) -> Option<String> {
    let dump = format!("{err:?}");
    let markers = ["thread '", "stack traceback", "runtime error", "panic"];
    let best: Option<usize> = if let Some(idx) = dump.rfind("Caused by:") {
        Some(idx)
    } else {
        let mut fallback: Option<usize> = None;
        for marker in markers {
            if let Some(idx) = dump.rfind(marker) {
                fallback = Some(fallback.map_or(idx, |prev| prev.max(idx)));
            }
        }
        fallback
    };
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
