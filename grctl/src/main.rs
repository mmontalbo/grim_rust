use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use humantime::format_duration;
use nix::errno::Errno;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod retail;
use retail::{
    extend_env_var, symbol_map_status_for, warn_if_shaders_missing, HookMode, RetailLayout,
    SymbolMapStatus,
};

const RETAIL_STEAM_APP_ID: &str = "345350";
const RETAIL_LUA_PATH: &str = "./?.lua;./?.LUA;./mods/?.lua";
const RUST_SHIM_TARGET: &str = "i686-unknown-linux-gnu";
#[derive(Parser, Debug)]
#[command(
    name = "grctl",
    author,
    version,
    about = "Grim runtime control utility",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
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
enum EngineCommand {
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
struct EngineStart {
    /// Run grim_engine with cargo --release.
    #[arg(long)]
    release: bool,
    /// Start the engine in headless mode.
    #[arg(long)]
    headless: bool,
    /// Enable verbose Lua logging.
    #[arg(long, hide = true)]
    verbose: bool,
    /// Stream the engine log to this terminal until you Ctrl-C.
    #[arg(long)]
    attach: bool,
    /// Override the GRIM_TRACE_RUN_ID for this launch.
    #[arg(long, value_parser = parse_run_id)]
    run_id: Option<String>,
    /// Additional arguments forwarded directly to grim_engine after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
enum RetailCommand {
    Start(RetailStart),
    Stop,
    Status,
    Logs(LogArgs),
    /// Copy the Steam install into dev-install (defaults to ~/.steam/...).
    #[command(hide = true)]
    Copy(RetailCopy),
}

#[derive(Args, Debug, Clone)]
struct RetailStart {
    /// Time limit for the retail session (examples: 20s, 5m). Use 0 to disable.
    #[arg(long, default_value = "20s")]
    timeout: String,
    /// Disable the timeout entirely (overrides --timeout).
    #[arg(long)]
    no_timeout: bool,
    /// Skip the LD_PRELOAD shim for a vanilla retail launch.
    #[arg(long)]
    vanilla: bool,
    /// Stream the retail stdout/stderr log to this terminal until you Ctrl-C.
    #[arg(long)]
    attach: bool,
    /// Override the GRIM_TRACE_RUN_ID for this launch.
    #[arg(long, value_parser = parse_run_id)]
    run_id: Option<String>,
    /// Additional arguments passed directly to the retail binary after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Args, Debug, Clone)]
struct RetailCopy {
    /// Source directory to copy from (defaults to $GRIM_STEAM_INSTALL or ~/.steam/...).
    #[arg(long)]
    source: Option<PathBuf>,
    /// Overwrite an existing dev-install directory.
    #[arg(long)]
    force: bool,
}

#[derive(Args, Debug, Clone)]
struct LogArgs {
    /// Number of lines to display from the end of the log (0 prints the entire file).
    #[arg(long, default_value_t = 0)]
    tail: usize,
    /// Continuously stream log updates after the initial tail.
    #[arg(long, short = 'f')]
    follow: bool,
    /// Open an interactive TUI viewer instead of printing to stdout.
    #[arg(long)]
    tui: bool,
    /// Select which run_id segment to display (defaults to latest run).
    #[arg(long, value_parser = parse_run_selection, default_value = "latest")]
    run: RunSelection,
}

#[derive(Subcommand, Debug)]
enum ParityCommand {
    /// Launch grim_engine and retail with a shared run_id for parity checks.
    Start(ParityStartArgs),
    /// Tail engine/retail logs aligned by seq for a given run_id.
    Tail(ParityTailArgs),
    /// Stop both engine and retail sessions launched by grctl.
    Stop(ParityStopArgs),
    /// Show engine/retail status together.
    Status,
}

#[derive(Args, Debug)]
struct ParityStartArgs {
    /// Optional run identifier shared across engine + retail (defaults to a new UUID).
    #[arg(long, value_parser = parse_run_id)]
    run_id: Option<String>,
    /// Run grim_engine with cargo --release.
    #[arg(long)]
    engine_release: bool,
    /// Start the engine in headless mode.
    #[arg(long)]
    engine_headless: bool,
    /// Run retail without the Rust shim (vanilla).
    #[arg(long)]
    retail_vanilla: bool,
    /// Disable the retail timeout (defaults to 20s).
    #[arg(long)]
    retail_no_timeout: bool,
}

#[derive(Args, Debug)]
struct ParityStopArgs {
    /// Force kill if graceful stop times out.
    #[arg(long)]
    force: bool,
}

#[derive(Args, Debug)]
struct ParityTailArgs {
    /// Run selection to stream (defaults to latest).
    #[arg(long, value_parser = parse_run_selection, default_value = "latest")]
    run: RunSelection,
    /// Number of recent seqs to print before following (0 to skip).
    #[arg(long, default_value_t = 30)]
    backfill: usize,
    /// Start streaming from the beginning of each log (ignores --backfill).
    #[arg(long)]
    from_start: bool,
    /// Poll interval in milliseconds when watching for new lines.
    #[arg(long, default_value_t = 300)]
    poll_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RunSelection {
    Latest,
    Id(String),
}

type AlignedRow = (u64, Option<String>, Option<String>);
type EnvVars = Vec<(String, String)>;
type EnvSetup = (EnvVars, Option<String>);

fn parse_run_selection(value: &str) -> std::result::Result<RunSelection, String> {
    if value.eq_ignore_ascii_case("latest") {
        Ok(RunSelection::Latest)
    } else {
        validate_run_id(value)?;
        Ok(RunSelection::Id(value.to_string()))
    }
}

fn parse_run_id(value: &str) -> std::result::Result<String, String> {
    validate_run_id(value)?;
    Ok(value.to_string())
}

fn validate_run_id(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("run id cannot be empty".to_string());
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if !value.chars().all(allowed) {
        return Err("run id must be alphanumeric with '-' or '_'".to_string());
    }
    Ok(())
}

fn parity_tail(args: ParityTailArgs, paths: &Paths) -> Result<()> {
    let run_id = resolve_parity_run_id(paths, &args.run)?;
    let engine_log = paths.run_log_path(ComponentKind::Engine, &run_id)?;
    let retail_log = paths.run_log_path(ComponentKind::Retail, &run_id)?;
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

    println!("[grctl] parity tail for run {run_id}");
    println!("  engine log: {}", engine_log.display());
    println!("  retail log: {}", retail_log.display());
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
        let pairs = backfill_pairs(&engine_log, &retail_log, args.backfill)?;
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

        for line in engine_lines {
            if let Some(seq) = parse_seq_from_line(&line) {
                let entry = printed.entry(seq).or_insert((None, None));
                if entry.0.is_none() {
                    entry.0 = Some(line);
                }
                new_seqs.insert(seq);
            }
        }
        for line in retail_lines {
            if let Some(seq) = parse_seq_from_line(&line) {
                let entry = printed.entry(seq).or_insert((None, None));
                if entry.1.is_none() {
                    entry.1 = Some(line);
                }
                new_seqs.insert(seq);
            }
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

fn parse_seq_from_line(line: &str) -> Option<u64> {
    let idx = line.find(" seq=")?;
    let rest = &line[idx + " seq=".len()..];
    let mut digits = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn tail_lines_by_seq(path: &Path, limit: usize) -> Result<Vec<(u64, String)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut buffer: VecDeque<(u64, String)> = VecDeque::with_capacity(limit.max(1));
    for line in reader.lines() {
        let line = line?;
        let Some(seq) = parse_seq_from_line(&line) else {
            continue;
        };
        if buffer.len() == limit {
            buffer.pop_front();
        }
        buffer.push_back((seq, line));
    }
    Ok(buffer.into_iter().collect())
}

fn backfill_pairs(
    engine_log: &Path,
    retail_log: &Path,
    limit: usize,
) -> Result<Vec<AlignedRow>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let engine_events = tail_lines_by_seq(engine_log, limit)?;
    let retail_events = tail_lines_by_seq(retail_log, limit)?;
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

fn print_aligned_row(seq: u64, engine: Option<&str>, retail: Option<&str>) {
    let engine_text = engine.unwrap_or("(missing in engine)");
    let retail_text = retail.unwrap_or("(missing in retail)");
    println!("seq={seq:06}");
    println!("  engine: {engine_text}");
    println!("  retail: {retail_text}");
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[clap(rename_all = "kebab_case")]
enum ComponentKind {
    Engine,
    Retail,
}

impl ComponentKind {
    fn as_str(self) -> &'static str {
        match self {
            ComponentKind::Engine => "grim_engine",
            ComponentKind::Retail => "retail_game",
        }
    }

    fn display(self) -> &'static str {
        match self {
            ComponentKind::Engine => "grim_engine",
            ComponentKind::Retail => "retail game",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ComponentState {
    pid: u32,
    session_id: String,
    #[serde(default)]
    run_id: Option<String>,
    command: Vec<String>,
    started_at: DateTime<Utc>,
    log_path: PathBuf,
}

impl ComponentState {
    fn effective_run_id(&self) -> &str {
        self.run_id.as_deref().unwrap_or(&self.session_id)
    }
}

#[derive(Clone, Debug)]
struct LaunchInfo {
    run_id: String,
    log_path: PathBuf,
}

#[derive(Clone, Debug)]
struct Paths {
    repo_root: PathBuf,
    state_dir: PathBuf,
    log_dir: PathBuf,
    launcher_dir: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self> {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let Some(repo_root) = crate_dir.parent() else {
            bail!(
                "failed to determine repository root from {}",
                crate_dir.display()
            );
        };
        let repo_root = repo_root.to_path_buf();
        let state_dir = repo_root.join("target/grctl/state");
        let log_dir = repo_root.join("target/grctl/logs");
        let launcher_dir = repo_root.join("target/grctl/launchers");
        fs::create_dir_all(&state_dir).context("creating grctl state directory")?;
        fs::create_dir_all(&log_dir).context("creating grctl logs directory")?;
        fs::create_dir_all(&launcher_dir).context("creating grctl launcher directory")?;
        Ok(Self {
            repo_root,
            state_dir,
            log_dir,
            launcher_dir,
        })
    }

    fn state_path(&self, component: ComponentKind) -> PathBuf {
        self.state_dir.join(format!("{}.json", component.as_str()))
    }

    fn log_path(&self, component: ComponentKind) -> PathBuf {
        self.log_dir.join(format!("{}.log", component.as_str()))
    }

    fn component_log_dir(&self, component: ComponentKind) -> Result<PathBuf> {
        let dir = self.log_dir.join(component.as_str());
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating log directory {}", dir.display()))?;
        Ok(dir)
    }

    fn run_log_path(&self, component: ComponentKind, run_id: &str) -> Result<PathBuf> {
        let dir = self.component_log_dir(component)?;
        Ok(dir.join(format!("{run_id}.log")))
    }

    fn update_latest_log_alias(&self, component: ComponentKind, target: &Path) -> Result<()> {
        let alias = self.log_path(component);
        if let Err(err) = fs::remove_file(&alias) {
            if err.kind() != io::ErrorKind::NotFound {
                return Err(err).with_context(|| format!("clearing {}", alias.display()));
            }
        }
        #[cfg(unix)]
        {
            unix_fs::symlink(target, &alias)
                .with_context(|| format!("linking {} -> {}", alias.display(), target.display()))?;
        }
        #[cfg(not(unix))]
        {
            fs::hard_link(target, &alias).with_context(|| {
                format!(
                    "linking {} to {} (hard link fallback on this platform)",
                    alias.display(),
                    target.display()
                )
            })?;
        }
        Ok(())
    }

    fn list_run_logs(&self, component: ComponentKind) -> Result<Vec<(String, PathBuf)>> {
        let dir = self.component_log_dir(component)?;
        let mut runs = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("reading log directory {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("log") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            runs.push((stem.to_string(), path));
        }
        Ok(runs)
    }

    fn launcher_script(&self, session_id: &str) -> PathBuf {
        self.launcher_dir.join(format!("retail_{session_id}.sh"))
    }

    fn retail_telemetry_path(&self) -> PathBuf {
        self.repo_root
            .join("dev-install")
            .join("mods")
            .join("telemetry_events.jsonl")
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;

    match cli.command {
        CommandKind::Engine(cmd) => handle_engine(cmd, &paths),
        CommandKind::Retail(cmd) => handle_retail(cmd, &paths),
        CommandKind::Parity(cmd) => handle_parity(cmd, &paths),
        CommandKind::Status => {
            for component in [ComponentKind::Engine, ComponentKind::Retail] {
                print_component_status(&paths, component)?;
            }
            Ok(())
        }
    }
}

fn handle_engine(cmd: EngineCommand, paths: &Paths) -> Result<()> {
    match cmd {
        EngineCommand::Start(args) => start_engine(args, paths).map(|_| ()),
        EngineCommand::Stop => stop_component(ComponentKind::Engine, paths, false),
        EngineCommand::Status => {
            print_component_status(paths, ComponentKind::Engine)?;
            Ok(())
        }
        EngineCommand::Logs(args) => show_logs(paths, ComponentKind::Engine, &args),
    }
}

fn handle_retail(cmd: RetailCommand, paths: &Paths) -> Result<()> {
    match cmd {
        RetailCommand::Start(args) => start_retail(args, paths).map(|_| ()),
        RetailCommand::Stop => stop_component(ComponentKind::Retail, paths, true),
        RetailCommand::Status => {
            print_component_status(paths, ComponentKind::Retail)?;
            print_retail_instrumentation(paths)?;
            Ok(())
        }
        RetailCommand::Logs(args) => show_logs(paths, ComponentKind::Retail, &args),
        RetailCommand::Copy(args) => copy_retail(args, paths),
    }
}

fn handle_parity(cmd: ParityCommand, paths: &Paths) -> Result<()> {
    match cmd {
        ParityCommand::Start(args) => parity_start(args, paths),
        ParityCommand::Tail(args) => parity_tail(args, paths),
        ParityCommand::Stop(args) => parity_stop(args, paths),
        ParityCommand::Status => {
            print_component_status(paths, ComponentKind::Engine)?;
            print_component_status(paths, ComponentKind::Retail)?;
            Ok(())
        }
    }
}

fn parity_start(args: ParityStartArgs, paths: &Paths) -> Result<()> {
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut guard = LaunchGuard::default();

    let engine_args = EngineStart {
        release: args.engine_release,
        headless: args.engine_headless,
        verbose: false,
        attach: false,
        run_id: Some(run_id.clone()),
        extra_args: Vec::new(),
    };
    let engine_launch = start_engine(engine_args, paths)?;
    guard.push(ComponentKind::Engine);

    let retail_args = RetailStart {
        timeout: if args.retail_no_timeout {
            "0".to_string()
        } else {
            "20s".to_string()
        },
        no_timeout: args.retail_no_timeout,
        vanilla: args.retail_vanilla,
        attach: false,
        run_id: Some(run_id.clone()),
        extra_args: Vec::new(),
    };
    let retail_launch = match start_retail(retail_args, paths) {
        Ok(info) => info,
        Err(err) => {
            guard.stop_all(paths);
            return Err(err);
        }
    };

    println!("[grctl] parity run {}", engine_launch.run_id);
    println!("  engine log:   {}", engine_launch.log_path.display());
    println!("  retail log:   {}", retail_launch.log_path.display());
    println!(
        "  telemetry:    {}",
        paths.retail_telemetry_path().display()
    );
    println!(
        "Next: grctl engine logs --run {} -f | grctl retail logs --run {} -f",
        engine_launch.run_id, retail_launch.run_id
    );

    Ok(())
}

fn parity_stop(args: ParityStopArgs, paths: &Paths) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for component in [ComponentKind::Engine, ComponentKind::Retail] {
        let force = args.force || matches!(component, ComponentKind::Retail);
        if let Err(err) = stop_component(component, paths, force) {
            eprintln!(
                "[grctl] warning: failed to stop {}: {err:?}",
                component.display()
            );
            last_err = Some(err);
        }
    }
    if let Some(err) = last_err {
        Err(err)
    } else {
        Ok(())
    }
}

fn start_engine(args: EngineStart, paths: &Paths) -> Result<LaunchInfo> {
    ensure_component_available(ComponentKind::Engine, paths)?;

    let session_id = Uuid::new_v4().to_string();
    let run_id = args.run_id.clone().unwrap_or_else(|| session_id.clone());
    let log_path = paths.run_log_path(ComponentKind::Engine, &run_id)?;

    let mut command_line = vec!["cargo".to_string(), "run".to_string()];
    let mut command = Command::new("cargo");
    command.arg("run");
    if args.release {
        command.arg("--release");
        command_line.push("--release".to_string());
    }
    command.args(["-p", "grim_engine", "--"]);
    command_line.push("-p".to_string());
    command_line.push("grim_engine".to_string());
    command_line.push("--".to_string());
    if args.headless {
        command.arg("--headless");
        command_line.push("--headless".to_string());
    }
    if args.verbose {
        command.arg("--verbose");
        command_line.push("--verbose".to_string());
    }
    for extra in &args.extra_args {
        command.arg(extra);
        command_line.push(extra.clone());
    }

    launch_component(
        ComponentKind::Engine,
        paths,
        session_id.clone(),
        run_id.clone(),
        log_path.clone(),
        command,
        command_line,
    )?;

    if args.attach {
        println!(
            "[grctl] attaching to engine log (Ctrl-C to detach): {}",
            log_path.display()
        );
        let log_args = LogArgs {
            tail: 200,
            follow: true,
            tui: false,
            run: RunSelection::Id(run_id.clone()),
        };
        show_logs(paths, ComponentKind::Engine, &log_args)?;
    } else {
        println!(
            "[grctl] engine log (run {}): {}",
            run_id,
            log_path.display()
        );
    }

    Ok(LaunchInfo { run_id, log_path })
}

fn start_retail(args: RetailStart, paths: &Paths) -> Result<LaunchInfo> {
    ensure_component_available(ComponentKind::Retail, paths)?;

    let session_id = Uuid::new_v4().to_string();
    let run_id = args.run_id.clone().unwrap_or_else(|| session_id.clone());
    let log_path = paths.run_log_path(ComponentKind::Retail, &run_id)?;

    let layout = RetailLayout::new(&paths.repo_root)?;
    layout.ensure_dev_install_exists()?;
    warn_if_shaders_missing(&layout);
    let mode = if args.vanilla {
        HookMode::Vanilla
    } else {
        HookMode::Instrumented
    };
    if matches!(mode, HookMode::Instrumented) {
        ensure_rust_shim_ready(paths, &layout)?;
        ensure_symbol_maps_ready(paths, &layout)?;
        let status = layout.instrumentation_status()?;
        if !status.shim_available {
            eprintln!(
                "[grctl] warning: LD_PRELOAD shim missing. Run 'cargo build -p grim_telemetry_shim --release' so {} exists; retail hooks will be incomplete until the Rust shim is built.",
                layout.preferred_shim_path().display(),
            );
        }
        if status.symbol_map != SymbolMapStatus::Fresh {
            eprintln!(
                "[grctl] warning: retail symbol map missing or stale at {}; labels may show raw addresses",
                layout.symbol_map_path().display(),
            );
        }
        if status.liblua_symbol_map != SymbolMapStatus::Fresh {
            eprintln!(
                "[grctl] warning: libLua symbol map missing or stale at {}; Lua closures may show raw addresses",
                layout.liblua_symbol_map_path().display(),
            );
        }
    }

    let (command, command_line) = build_retail_command(&layout, &args, mode, paths, &session_id)?;

    launch_component(
        ComponentKind::Retail,
        paths,
        session_id.clone(),
        run_id.clone(),
        log_path.clone(),
        command,
        command_line,
    )?;

    if args.attach {
        println!(
            "[grctl] attaching to retail log (Ctrl-C to detach): {}",
            log_path.display()
        );
        let log_args = LogArgs {
            tail: 200,
            follow: true,
            tui: false,
            run: RunSelection::Id(run_id.clone()),
        };
        show_logs(paths, ComponentKind::Retail, &log_args)?;
    } else {
        println!(
            "[grctl] retail log (run {}): {}",
            run_id,
            log_path.display()
        );
    }

    Ok(LaunchInfo { run_id, log_path })
}

fn ensure_rust_shim_ready(paths: &Paths, layout: &RetailLayout) -> Result<()> {
    ensure_i686_target_installed()?;
    println!("[grctl] rebuilding grim_telemetry_shim --release...");
    let build_cmd = format!(
        "cargo build -p grim_telemetry_shim --release --target {}",
        RUST_SHIM_TARGET
    );
    let status = Command::new("nix-shell")
        .current_dir(&paths.repo_root)
        .args(["--run", &build_cmd])
        .status()
        .context("building grim_telemetry_shim --release for i686-unknown-linux-gnu")?;
    if !status.success() {
        bail!("grim_telemetry_shim build failed with status {}", status);
    }
    if layout.resolved_shim_path().is_some() {
        Ok(())
    } else {
        bail!(
            "grim_telemetry_shim build succeeded but the shared object is still missing (expected {})",
            layout.preferred_shim_path().display()
        );
    }
}

fn ensure_symbol_maps_ready(paths: &Paths, layout: &RetailLayout) -> Result<()> {
    ensure_symbol_map_for_binary(
        paths,
        layout,
        "retail",
        layout.retail_bin(),
        layout.symbol_map_path(),
    )?;
    ensure_symbol_map_for_binary(
        paths,
        layout,
        "libLua",
        layout.liblua_bin(),
        layout.liblua_symbol_map_path(),
    )?;
    Ok(())
}

fn ensure_symbol_map_for_binary(
    paths: &Paths,
    layout: &RetailLayout,
    label: &str,
    binary: &Path,
    map_path: &Path,
) -> Result<()> {
    match symbol_map_status_for(map_path, binary)? {
        SymbolMapStatus::Fresh => return Ok(()),
        SymbolMapStatus::Stale | SymbolMapStatus::Missing => {}
    }

    println!("[grctl] rebuilding {label} symbol map...");
    let dev_install = layout.dev_install().to_string_lossy().into_owned();
    let binary_name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("unable to determine filename for {}", binary.display()))?;
    let map_dest = map_path.to_string_lossy().into_owned();
    let command = format!(
        "cd {} && nm -n --demangle {} | awk '$2 ~ /^[tT]$/ {{print $1, $3}}' > {}",
        shell_quote(&dev_install),
        shell_quote(binary_name),
        shell_quote(&map_dest)
    );
    let status = Command::new("nix-shell")
        .current_dir(&paths.repo_root)
        .args(["--run", &command])
        .status()
        .with_context(|| format!("building {label} symbol map with nm"))?;
    if !status.success() {
        bail!(
            "{label} symbol map generation failed with status {}",
            status
        );
    }

    match symbol_map_status_for(map_path, binary)? {
        SymbolMapStatus::Fresh => Ok(()),
        SymbolMapStatus::Stale => bail!(
            "{label} symbol map at {} is stale after regeneration",
            map_path.display()
        ),
        SymbolMapStatus::Missing => bail!(
            "{label} symbol map missing after regeneration attempt at {}",
            map_path.display()
        ),
    }
}

fn ensure_i686_target_installed() -> Result<()> {
    let status = Command::new("rustup")
        .args(["target", "add", RUST_SHIM_TARGET])
        .status();
    match status {
        Ok(result) if result.success() => Ok(()),
        Ok(result) => bail!(
            "failed to add Rust target {} (rustup exited with {})",
            RUST_SHIM_TARGET,
            result
        ),
        Err(err) => bail!(
            "rustup not available while ensuring target {}; install rustup or add the target manually (error: {err})",
            RUST_SHIM_TARGET
        ),
    }
}

fn copy_retail(args: RetailCopy, paths: &Paths) -> Result<()> {
    let layout = RetailLayout::new(&paths.repo_root)?;
    let destination = layout.sync_from(args.source.as_deref(), args.force)?;
    println!("[grctl] copied retail install to {}", destination.display());
    Ok(())
}

fn print_retail_instrumentation(paths: &Paths) -> Result<()> {
    let layout = RetailLayout::new(&paths.repo_root)?;
    if !layout.dev_install().exists() {
        println!(
            "[grctl] {:<12} instrumentation: dev-install missing (run 'grctl retail copy')",
            ComponentKind::Retail.as_str()
        );
        return Ok(());
    }
    let status = layout.instrumentation_status()?;
    println!(
        "[grctl] {:<12} instrumentation: {}",
        ComponentKind::Retail.as_str(),
        describe_instrumentation(&status)
    );
    Ok(())
}

fn describe_instrumentation(status: &retail::InstrumentationStatus) -> String {
    if !status.shim_available {
        return "vanilla (shim missing; build grim_telemetry_shim)".to_string();
    }
    if status.symbol_map == SymbolMapStatus::Fresh
        && status.liblua_symbol_map == SymbolMapStatus::Fresh
    {
        return "instrumented (shim + symbol maps ready)".to_string();
    }
    format!(
        "instrumented (shim ready, symbol maps: retail={}, libLua={})",
        describe_map_status(status.symbol_map),
        describe_map_status(status.liblua_symbol_map),
    )
}

fn describe_map_status(status: SymbolMapStatus) -> &'static str {
    match status {
        SymbolMapStatus::Fresh => "fresh",
        SymbolMapStatus::Stale => "stale",
        SymbolMapStatus::Missing => "missing",
    }
}

fn build_retail_command(
    layout: &RetailLayout,
    args: &RetailStart,
    mode: HookMode,
    paths: &Paths,
    session_id: &str,
) -> Result<(Command, Vec<String>)> {
    let use_timeout = !args.no_timeout && args.timeout.trim() != "0";
    let mut command_line = Vec::new();
    let mut command = if use_timeout {
        let mut cmd = Command::new("timeout");
        cmd.arg(&args.timeout);
        cmd.arg("steam-run");
        command_line.push("timeout".to_string());
        command_line.push(args.timeout.clone());
        command_line.push("steam-run".to_string());
        cmd
    } else {
        command_line.push("steam-run".to_string());
        Command::new("steam-run")
    };
    let retail_bin = layout.retail_bin();
    if !retail_bin.exists() {
        bail!(
            "retail binary missing at {}; run 'grctl retail copy' first",
            retail_bin.display()
        );
    }
    let runtime_preloads = gather_runtime_preloads(layout);
    let (env_pairs, ld_preload) = assemble_retail_env(layout, mode, &runtime_preloads)?;
    let script_path =
        write_retail_launcher_script(paths, session_id, layout, &env_pairs, ld_preload.as_deref())?;
    let script_str = script_path.to_string_lossy().into_owned();
    command.arg(&script_str);
    command_line.push(script_str);
    for extra in &args.extra_args {
        command.arg(extra);
        command_line.push(extra.clone());
    }
    Ok((command, command_line))
}

fn build_ld_preload(
    mode: HookMode,
    layout: &RetailLayout,
    extra_preloads: &[PathBuf],
) -> Result<Option<String>> {
    let mut libs: Vec<PathBuf> = Vec::new();
    match mode {
        HookMode::Instrumented => {
            if let Some(shim) = layout.resolved_shim_path() {
                libs.push(shim);
            }
        }
        HookMode::Vanilla => {}
    }
    libs.extend(extra_preloads.iter().cloned());

    if libs.is_empty() {
        if matches!(mode, HookMode::Vanilla) {
            return Ok(None);
        }
        if let Some(existing) = std::env::var_os("LD_PRELOAD") {
            return Ok(Some(existing.to_string_lossy().into_owned()));
        }
        return Ok(None);
    }

    let mut value = std::env::var_os("LD_PRELOAD");
    for lib in libs.into_iter().rev() {
        let lib_value = lib.to_string_lossy().into_owned();
        value = Some(extend_env_var(value, &lib_value));
    }
    Ok(value.map(|v| v.to_string_lossy().into_owned()))
}

fn assemble_retail_env(
    layout: &RetailLayout,
    mode: HookMode,
    extra_preloads: &[PathBuf],
) -> Result<EnvSetup> {
    let mut envs: EnvVars = Vec::new();
    if let Some(value) = build_ld_library_path(layout) {
        envs.push(("LD_LIBRARY_PATH".to_string(), value));
    }
    envs.push(("LUA_PATH".to_string(), RETAIL_LUA_PATH.to_string()));
    if let Some(audio) = default_audio_driver() {
        envs.push(("SDL_AUDIODRIVER".to_string(), audio));
    }
    envs.extend(build_steam_env(layout));
    if matches!(mode, HookMode::Instrumented) {
        if let SymbolMapStatus::Fresh = layout.symbol_map_status()? {
            envs.push((
                "GRIM_SHIM_SYMBOL_MAP".to_string(),
                layout.symbol_map_path().to_string_lossy().into_owned(),
            ));
            envs.push((
                "GRIM_SHIM_SYMBOL_MAP_MODULE".to_string(),
                "GrimFandango".to_string(),
            ));
        }
        if let SymbolMapStatus::Fresh = layout.liblua_symbol_map_status()? {
            envs.push((
                "GRIM_SHIM_SYMBOL_MAP_LUALIB".to_string(),
                layout
                    .liblua_symbol_map_path()
                    .to_string_lossy()
                    .into_owned(),
            ));
            let module_name = layout
                .liblua_bin()
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("libLua.so")
                .to_string();
            envs.push((
                "GRIM_SHIM_SYMBOL_MAP_LUALIB_MODULE".to_string(),
                module_name,
            ));
        }
    }
    let preload = build_ld_preload(mode, layout, extra_preloads)?;
    Ok((envs, preload))
}

fn build_ld_library_path(layout: &RetailLayout) -> Option<String> {
    let mut prefixes: Vec<String> = Vec::new();
    prefixes.push(layout.dev_install().to_string_lossy().into_owned());
    prefixes.extend(
        layout
            .steam_ld_paths()
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );

    let mut value = std::env::var_os("LD_LIBRARY_PATH");
    for prefix in prefixes.into_iter().rev() {
        value = Some(extend_env_var(value, &prefix));
    }
    value.map(|v| v.to_string_lossy().into_owned())
}

fn default_audio_driver() -> Option<String> {
    if std::env::var_os("SDL_AUDIODRIVER").is_none() {
        Some("pulse".to_string())
    } else {
        None
    }
}

fn build_steam_env(layout: &RetailLayout) -> Vec<(String, String)> {
    let mut vars = vec![
        ("SteamAppId".to_string(), RETAIL_STEAM_APP_ID.to_string()),
        ("SteamGameId".to_string(), RETAIL_STEAM_APP_ID.to_string()),
        (
            "SteamOverlayGameId".to_string(),
            RETAIL_STEAM_APP_ID.to_string(),
        ),
        ("SteamClientLaunch".to_string(), "1".to_string()),
        ("SteamEnv".to_string(), "1".to_string()),
    ];

    if let Some(root) = layout.steam_root() {
        let root_str = root.to_string_lossy().into_owned();
        vars.push(("SteamPath".to_string(), root_str.clone()));
        vars.push((
            "STEAM_COMPAT_CLIENT_INSTALL_PATH".to_string(),
            root_str.clone(),
        ));
        if let Some(runtime) = layout.steam_runtime_dir() {
            let runtime_str = runtime.to_string_lossy().into_owned();
            vars.push(("SteamRuntime".to_string(), runtime_str.clone()));
            vars.push(("STEAM_RUNTIME".to_string(), runtime_str));
        } else {
            eprintln!(
                "[grctl] warning: steam-runtime directory missing under {}; consider running Steam once to populate it",
                root.display()
            );
        }
    } else {
        eprintln!(
            "[grctl] warning: unable to detect Steam root; set $GRIM_STEAM_ROOT if Steam is installed elsewhere"
        );
    }

    vars
}

fn write_retail_launcher_script(
    paths: &Paths,
    session_id: &str,
    layout: &RetailLayout,
    env_pairs: &[(String, String)],
    ld_preload: Option<&str>,
) -> Result<PathBuf> {
    let script_path = paths.launcher_script(session_id);
    let mut file = File::create(&script_path).with_context(|| {
        format!(
            "creating retail launcher script at {}",
            script_path.display()
        )
    })?;
    writeln!(file, "#!/bin/sh")?;
    writeln!(file, "# Auto-generated by grctl")?;
    writeln!(file, "set -euo pipefail")?;
    writeln!(file)?;
    for (key, value) in env_pairs {
        writeln!(file, "export {}={}", key, shell_quote(value))?;
    }
    writeln!(file, "unset LD_PRELOAD")?;
    if let Some(preload) = ld_preload {
        let quoted = shell_quote(preload);
        writeln!(file, "export LD_PRELOAD_32={}", quoted)?;
        writeln!(file, "export LD_PRELOAD={}", quoted)?;
    }
    let dev_install = layout.dev_install().to_string_lossy().into_owned();
    writeln!(file, "cd {}", shell_quote(&dev_install))?;
    writeln!(file, "exec ./GrimFandango \"$@\"")?;
    drop(file);
    #[cfg(unix)]
    {
        let perms = PermissionsExt::from_mode(0o755);
        fs::set_permissions(&script_path, perms)
            .with_context(|| format!("setting permissions on {}", script_path.display()))?;
    }
    Ok(script_path)
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        "''".to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn gather_runtime_preloads(layout: &RetailLayout) -> Vec<PathBuf> {
    let mut preloads = Vec::new();
    if let Some(path) = layout.steamclient32_path() {
        preloads.push(path);
    } else if let Some(root) = layout.steam_root() {
        eprintln!(
            "[grctl] warning: steamclient.so not found under {}; SteamAPI may still fail",
            root.display()
        );
    } else {
        eprintln!("[grctl] warning: steamclient.so preload skipped (Steam root unknown)");
    }
    preloads
}

fn launch_component(
    component: ComponentKind,
    paths: &Paths,
    session_id: String,
    run_id: String,
    log_path: PathBuf,
    mut command: Command,
    command_line: Vec<String>,
) -> Result<()> {
    command.current_dir(&paths.repo_root);
    command.stdin(Stdio::null());
    command.env("GRCTL_MANAGED", "1");
    command.env("GRCTL_SESSION_ID", &session_id);
    command.env("GRCTL_COMPONENT", component.as_str());
    command.env("GRCTL_LOG_PATH", &log_path);
    command.env("GRCTL_STATE_DIR", &paths.state_dir);
    command.env("GRIM_TRACE_RUN_ID", &run_id);

    if log_path.exists() {
        fs::remove_file(&log_path).with_context(|| format!("clearing {}", log_path.display()))?;
    }
    let mut log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let timestamp = Utc::now();
    writeln!(
        log_file,
        "\n===== launching {} session={} run_id={} at {} =====",
        component.display(),
        session_id,
        run_id,
        timestamp.to_rfc3339()
    )
    .ok();

    paths
        .update_latest_log_alias(component, &log_path)
        .with_context(|| format!("updating latest log alias for {}", component.as_str()))?;

    let stdout = log_file
        .try_clone()
        .context("cloning log file for stdout")?;
    let stderr = log_file
        .try_clone()
        .context("cloning log file for stderr")?;
    command.stdout(Stdio::from(stdout));
    command.stderr(Stdio::from(stderr));

    let child = command.spawn().with_context(|| {
        format!(
            "spawning {} ({})",
            component.display(),
            command_line.join(" ")
        )
    })?;

    let pid = child.id();
    let state = ComponentState {
        pid,
        session_id: session_id.clone(),
        run_id: Some(run_id.clone()),
        command: command_line,
        started_at: timestamp,
        log_path: log_path.clone(),
    };
    write_state(component, paths, &state)?;
    spawn_reaper(component, paths.clone(), log_path, child);

    println!(
        "[grctl] started {} (pid {}, session {}, run {})",
        component.display(),
        pid,
        session_id,
        run_id
    );
    Ok(())
}

fn ensure_component_available(component: ComponentKind, paths: &Paths) -> Result<()> {
    if let Some(state) = load_state(component, paths)? {
        if process_alive(state.pid) {
            bail!(
                "{} already running (pid {}, session {})",
                component.display(),
                state.pid,
                state.session_id
            );
        } else {
            println!(
                "[grctl] removing stale state for {} (pid {} no longer alive)",
                component.display(),
                state.pid
            );
            clear_state(component, paths)?;
        }
    }
    Ok(())
}

fn spawn_reaper(component: ComponentKind, paths: Paths, log_path: PathBuf, mut child: Child) {
    thread::spawn(move || {
        let status = child.wait();
        let summary = match &status {
            Ok(code) => format!("exited with {}", code),
            Err(err) => format!("wait error: {err}"),
        };
        if let Ok(mut log) = OpenOptions::new().append(true).open(&log_path) {
            let _ = writeln!(log, "[grctl] {} {}", component.display(), summary);
        }
        if let Ok(exit_status) = &status {
            handle_component_exit(component, &paths, exit_status);
        }
        if let Err(err) = clear_state(component, &paths) {
            if err
                .downcast_ref::<io::Error>()
                .map(|ioe| ioe.kind() == io::ErrorKind::NotFound)
                .unwrap_or(false)
            {
                return;
            }
            eprintln!(
                "[grctl] warning: failed to clear state for {}: {err:?}",
                component.display()
            );
        }
    });
}

fn handle_component_exit(
    component: ComponentKind,
    paths: &Paths,
    status: &std::process::ExitStatus,
) {
    if component == ComponentKind::Retail {
        handle_retail_exit(paths, status);
    }
}

fn handle_retail_exit(paths: &Paths, status: &std::process::ExitStatus) {
    if status.code() != Some(124) {
        return;
    }
    let layout = match RetailLayout::new(&paths.repo_root) {
        Ok(layout) => layout,
        Err(err) => {
            eprintln!(
                "[grctl] retail timeout triage skipped: unable to inspect dev-install ({err:?})"
            );
            return;
        }
    };
    let events_path = layout
        .dev_install()
        .join("mods")
        .join("telemetry_events.jsonl");
    if !events_path.exists() {
        println!(
            "[grctl] retail timeout: {} missing (telemetry hooks inactive or shim disabled)",
            events_path.display()
        );
        return;
    }
    match has_intro_timeline_events(&events_path) {
        Ok(true) => {}
        Ok(false) => {
            println!(
                "[grctl] retail timeout: no intro.timeline events recorded in {}; retail likely stalled before the logos/intro (black screen).",
                events_path.display()
            );
        }
        Err(err) => {
            eprintln!(
                "[grctl] retail timeout triage failed while reading {}: {err:?}",
                events_path.display()
            );
        }
    }
}

fn has_intro_timeline_events(events_path: &Path) -> io::Result<bool> {
    let file = File::open(events_path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.contains("intro.timeline") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn stop_component(component: ComponentKind, paths: &Paths, force: bool) -> Result<()> {
    let Some(state) = load_state(component, paths)? else {
        println!("[grctl] {} is not running", component.display());
        return Ok(());
    };

    if !process_alive(state.pid) {
        println!(
            "[grctl] {} already stopped (pid {} not active)",
            component.display(),
            state.pid
        );
        clear_state(component, paths)?;
        return Ok(());
    }

    if let Ok(mut log) = OpenOptions::new().append(true).open(&state.log_path) {
        let _ = writeln!(
            log,
            "[grctl] stop requested for {} (session {})",
            component.display(),
            state.session_id
        );
    }

    let pid = Pid::from_raw(state.pid as i32);
    kill(pid, Signal::SIGTERM).with_context(|| {
        format!(
            "sending SIGTERM to {} (pid {})",
            component.display(),
            state.pid
        )
    })?;

    let mut waited = Duration::from_millis(0);
    let wait_step = Duration::from_millis(200);
    let wait_limit = Duration::from_secs(10);
    while process_alive(state.pid) && waited < wait_limit {
        thread::sleep(wait_step);
        waited += wait_step;
    }

    if process_alive(state.pid) {
        if force {
            println!(
                "[grctl] {} still running after SIGTERM; escalating to SIGKILL",
                component.display()
            );
            kill(pid, Signal::SIGKILL).with_context(|| {
                format!(
                    "sending SIGKILL to {} (pid {})",
                    component.display(),
                    state.pid
                )
            })?;
        } else {
            bail!(
                "{} did not exit within {}s; retry with --force if appropriate",
                component.display(),
                wait_limit.as_secs()
            );
        }
    }

    // Give the reaper thread a brief moment to clean up.
    thread::sleep(Duration::from_millis(200));
    clear_state(component, paths).ok();

    println!(
        "[grctl] stopped {} (session {})",
        component.display(),
        state.session_id
    );
    Ok(())
}

fn process_alive(pid: u32) -> bool {
    let pid = Pid::from_raw(pid as i32);
    match kill(pid, None) {
        Ok(_) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}

fn load_state(component: ComponentKind, paths: &Paths) -> Result<Option<ComponentState>> {
    let path = paths.state_path(component);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let state: ComponentState =
        serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(state))
}

fn write_state(component: ComponentKind, paths: &Paths, state: &ComponentState) -> Result<()> {
    let path = paths.state_path(component);
    let temp_path = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(state)?;
    fs::write(&temp_path, data).with_context(|| format!("writing {}", temp_path.display()))?;
    fs::rename(&temp_path, &path).with_context(|| {
        format!(
            "committing state file {} -> {}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn clear_state(component: ComponentKind, paths: &Paths) -> Result<()> {
    let path = paths.state_path(component);
    match fs::remove_file(&path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

fn print_component_status(paths: &Paths, component: ComponentKind) -> Result<()> {
    match load_state(component, paths)? {
        None => {
            println!("[grctl] {:<12} status: stopped", component.as_str());
        }
        Some(state) => {
            if process_alive(state.pid) {
                let run_id = state.effective_run_id();
                let uptime = match (Utc::now() - state.started_at).to_std() {
                    Ok(duration) => format_duration(duration).to_string(),
                    Err(_) => "unknown".to_string(),
                };
                println!(
                    "[grctl] {:<12} status: running (pid {}, session {}, run {}, uptime {})",
                    component.as_str(),
                    state.pid,
                    state.session_id,
                    run_id,
                    uptime
                );
            } else {
                println!(
                    "[grctl] {:<12} status: stale (pid {} not active)",
                    component.as_str(),
                    state.pid
                );
            }
        }
    }
    Ok(())
}

fn show_logs(paths: &Paths, component: ComponentKind, args: &LogArgs) -> Result<()> {
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
        follow_logs(&log_path, args.tail)
    } else {
        let lines = tail_file(&log_path, args.tail)?;
        for line in lines {
            println!("{line}");
        }
        Ok(())
    }
}

fn launch_trace_tui(paths: &Paths, log_path: &Path) -> Result<()> {
    println!("[grctl] launching trace_tui for {}", log_path.display());
    let status = Command::new("cargo")
        .arg("run")
        .arg("-p")
        .arg("trace_tui")
        .arg("--")
        .arg(log_path)
        .current_dir(&paths.repo_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("spawning trace_tui")?;
    if !status.success() {
        bail!("trace_tui exited with status {}", status);
    }
    Ok(())
}

fn resolve_run_path(
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

fn follow_logs(path: &Path, tail: usize) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);

    if tail == 0 {
        for line in reader.by_ref().lines() {
            println!("{}", line?);
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
            println!("{line}");
        }
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
            println!("{line}");
        }
    }
}

fn tail_file(path: &Path, tail: usize) -> Result<Vec<String>> {
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

#[derive(Default)]
struct LaunchGuard {
    components: Vec<ComponentKind>,
}

impl LaunchGuard {
    fn push(&mut self, component: ComponentKind) {
        self.components.push(component);
    }

    fn stop_all(&mut self, paths: &Paths) {
        for component in self.components.iter().rev() {
            if let Err(err) = stop_component(*component, paths, false) {
                eprintln!(
                    "[grctl] warning: failed to stop {}: {err:?}",
                    component.display()
                );
            }
        }
        self.components.clear();
    }
}
