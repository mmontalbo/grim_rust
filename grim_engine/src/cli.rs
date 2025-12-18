//! Command-line surface for the minimal intro runner.
//!
//! The flags intentionally mirror the narrow runtime we still support: point
//! at extracted data, optionally run headless, and toggle verbose logging from
//! the Lua host. Anything more belongs in a resurrected tool, not this binary.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Minimal host for driving the Grim intro sequence", version)]
struct Args {
    /// Path to the extracted DATA000 directory
    #[arg(long, default_value = "extracted/DATA000")]
    data_root: PathBuf,

    /// Run without a viewer and print emitted engine events to stdout
    #[arg(long)]
    headless: bool,

    /// Print additional logging from the Lua host
    #[arg(long)]
    verbose: bool,
}

/// Parsed CLI arguments forwarded to the runtime layer.
#[derive(Debug, Clone)]
pub struct EngineArgs {
    /// Path to the extracted DATA000 directory.
    pub data_root: PathBuf,
    /// Print emitted events instead of talking to a viewer.
    pub headless: bool,
    /// Enable extra logging from the Lua host.
    pub verbose: bool,
}

/// Parse CLI flags into the stable argument struct used by `runtime`.
pub fn parse_args() -> EngineArgs {
    let args = Args::parse();
    EngineArgs {
        data_root: args.data_root,
        headless: args.headless,
        verbose: args.verbose,
    }
}
