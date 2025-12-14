use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "grctl",
    author,
    version,
    about = "Grim runtime control utility",
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CommandKind,
}

#[derive(Subcommand, Debug)]
pub enum CommandKind {
    /// Manage grim_engine instances.
    #[command(subcommand)]
    Engine(EngineCommand),
    /// Manage the retail Grim Fandango binary.
    #[command(subcommand)]
    Retail(RetailCommand),
    /// Parity-focused helpers for engine vs retail.
    #[command(subcommand)]
    Parity(ParityCommand),
    /// Show component status for the entire stack.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum EngineCommand {
    /// Launch grim_engine under grctl supervision.
    Start(EngineStart),
    /// Stop a grim_engine instance started by grctl.
    Stop,
    /// Inspect the current grim_engine status.
    Status,
    /// Read recent log lines for grim_engine.
    Logs(LogArgs),
}

#[derive(Args, Debug, Clone)]
pub struct EngineStart {
    /// Run grim_engine with cargo --release.
    #[arg(long)]
    pub release: bool,
    /// Start the engine in headless mode.
    #[arg(long)]
    pub headless: bool,
    /// Enable verbose Lua logging.
    #[arg(long, hide = true)]
    pub verbose: bool,
    /// Stream the engine log to this terminal until you Ctrl-C.
    #[arg(long)]
    pub attach: bool,
    /// Set the run_id for this launch.
    #[arg(long, value_parser = parse_run_id)]
    pub run_id: Option<String>,
    /// Additional arguments forwarded directly to grim_engine after '--'.
    #[arg(last = true)]
    pub extra_args: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum RetailCommand {
    Start(RetailStart),
    Stop,
    Status,
    Logs(LogArgs),
    /// Copy the Steam install into dev-install (defaults to ~/.steam/...).
    #[command(hide = true)]
    Copy(RetailCopy),
}

#[derive(Args, Debug, Clone)]
pub struct RetailStart {
    /// Time limit for the retail session (examples: 20s, 5m). Use 0 to disable.
    #[arg(long, default_value = "20s")]
    pub timeout: String,
    /// Disable the timeout entirely (overrides --timeout).
    #[arg(long)]
    pub no_timeout: bool,
    /// Skip the LD_PRELOAD shim for a vanilla retail launch.
    #[arg(long)]
    pub vanilla: bool,
    /// Stream the retail stdout/stderr log to this terminal until you Ctrl-C.
    #[arg(long)]
    pub attach: bool,
    /// Launch retail under gdb with the grctl-managed environment.
    #[arg(long, value_enum)]
    pub debugger: Option<RetailDebugger>,
    /// Set the run_id for this launch.
    #[arg(long, value_parser = parse_run_id)]
    pub run_id: Option<String>,
    /// Additional arguments passed directly to the retail binary after '--'.
    #[arg(last = true)]
    pub extra_args: Vec<String>,
}

#[derive(Args, Debug, Clone)]
pub struct RetailCopy {
    /// Source directory to copy from (defaults to $GRIM_STEAM_INSTALL or ~/.steam/...).
    #[arg(long)]
    pub source: Option<std::path::PathBuf>,
    /// Overwrite an existing dev-install directory.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Number of lines to display from the end of the log (0 prints the entire file).
    #[arg(long, default_value_t = 0)]
    pub tail: usize,
    /// Continuously stream log updates after the initial tail.
    #[arg(long, short = 'f')]
    pub follow: bool,
    /// Open an interactive TUI viewer instead of printing to stdout.
    #[arg(long)]
    pub tui: bool,
    /// Select which run_id segment to display (defaults to latest run).
    #[arg(long, value_parser = parse_run_selection, default_value = "latest")]
    pub run: RunSelection,
}

#[derive(Subcommand, Debug)]
pub enum ParityCommand {
    /// Launch grim_engine and retail with a shared run_id for parity checks.
    Start(ParityStartArgs),
    /// View engine/retail logs for a run_id.
    #[command(alias = "tail")]
    Logs(ParityLogsArgs),
    /// Stop both engine and retail sessions launched by grctl.
    Stop(ParityStopArgs),
    /// Show engine/retail status together.
    Status,
}

#[derive(Args, Debug)]
pub struct ParityStartArgs {
    /// Optional run identifier shared across engine + retail (defaults to a new UUID).
    #[arg(long, value_parser = parse_run_id)]
    pub run_id: Option<String>,
    /// Run grim_engine with cargo --release.
    #[arg(long)]
    pub engine_release: bool,
    /// Start the engine in headless mode.
    #[arg(long)]
    pub engine_headless: bool,
    /// Run retail without the Rust shim (vanilla).
    #[arg(long)]
    pub retail_vanilla: bool,
    /// Time limit for the retail session (examples: 20s, 5m). Use 0 to disable.
    #[arg(long, default_value = "20s")]
    pub timeout: String,
    /// Disable the timeout entirely (overrides --timeout).
    #[arg(long)]
    pub no_timeout: bool,
}

#[derive(Args, Debug)]
pub struct ParityStopArgs {
    /// Force kill if graceful stop times out.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct ParityLogsArgs {
    /// Run selection to stream (defaults to latest).
    #[arg(long, value_parser = parse_run_selection, default_value = "latest")]
    pub run: RunSelection,
    /// Report the first divergence between engine/retail and exit.
    #[arg(long)]
    pub first_diff: bool,
    /// Number of events of context to show around the first divergence.
    #[arg(long, default_value_t = 3)]
    pub window: usize,
    /// Continuously stream log updates after the initial read.
    #[arg(long, short = 'f')]
    pub follow: bool,
    /// Display the raw telemetry stream (default is semantic-only; see grim_telemetry_schema/README.md).
    #[arg(long)]
    pub raw: bool,
    /// Number of recent seqs to print before following (0 to skip).
    #[arg(long, default_value_t = 30)]
    pub backfill: usize,
    /// Start streaming from the beginning of each log (ignores --backfill).
    #[arg(long)]
    pub from_start: bool,
    /// Poll interval in milliseconds when watching for new lines.
    #[arg(long, default_value_t = 300)]
    pub poll_ms: u64,
    /// Open an interactive TUI viewer instead of streaming aligned rows.
    #[arg(long)]
    pub tui: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunSelection {
    Latest,
    Id(String),
}

pub fn parse_run_selection(value: &str) -> std::result::Result<RunSelection, String> {
    if value.eq_ignore_ascii_case("latest") {
        Ok(RunSelection::Latest)
    } else {
        validate_run_id(value)?;
        Ok(RunSelection::Id(value.to_string()))
    }
}

pub fn parse_run_id(value: &str) -> std::result::Result<String, String> {
    validate_run_id(value)?;
    Ok(value.to_string())
}

pub fn validate_run_id(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("run id cannot be empty".to_string());
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if !value.chars().all(allowed) {
        return Err("run id must be alphanumeric with '-' or '_'".to_string());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "kebab_case")]
pub enum RetailDebugger {
    /// Launch retail under gdb with the proper env (no gdbserver).
    Gdb,
}
