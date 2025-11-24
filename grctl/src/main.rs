use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use humantime::format_duration;
use nix::errno::Errno;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod retail;
use retail::{extend_env_var, warn_if_shaders_missing, HookMode, RetailLayout};

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
    /// Manage grim_viewer instances.
    #[command(subcommand)]
    Viewer(ViewerCommand),
    /// Manage the retail Grim Fandango binary.
    #[command(subcommand)]
    Retail(RetailCommand),
    /// Run or manage scenario harness sessions.
    #[command(subcommand)]
    Scenario(ScenarioCommand),
    /// Watch live parity signals between engine and retail.
    #[command(subcommand)]
    Watch(WatchCommand),
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

#[derive(Args, Debug)]
struct EngineStart {
    /// Run grim_engine with cargo --release.
    #[arg(long)]
    release: bool,
    /// Start the engine in headless mode.
    #[arg(long)]
    headless: bool,
    /// Enable verbose Lua logging.
    #[arg(long)]
    verbose: bool,
    /// Stream the engine log to this terminal until you Ctrl-C.
    #[arg(long)]
    attach: bool,
    /// Additional arguments forwarded directly to grim_engine after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum ViewerCommand {
    Start(ViewerStart),
    Stop,
    Status,
    Logs(LogArgs),
}

#[derive(Args, Debug)]
struct ViewerStart {
    /// Run grim_viewer with cargo --release.
    #[arg(long)]
    release: bool,
    /// Address grim_viewer connects to for engine state updates.
    #[arg(long)]
    engine_stream: Option<String>,
    /// Start grim_viewer without expecting a retail capture feed.
    #[arg(long)]
    no_retail: bool,
    /// Initial window width.
    #[arg(long, default_value_t = 1280)]
    window_width: u32,
    /// Initial window height.
    #[arg(long, default_value_t = 720)]
    window_height: u32,
    /// Additional arguments forwarded to grim_viewer after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum RetailCommand {
    Start(RetailStart),
    Stop,
    Status,
    Logs(LogArgs),
    /// Copy the Steam install into dev-install (defaults to ~/.steam/...).
    Copy(RetailCopy),
}

#[derive(Args, Debug)]
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
    /// Additional arguments passed directly to the retail binary after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Args, Debug)]
struct RetailCopy {
    /// Source directory to copy from (defaults to $GRIM_STEAM_INSTALL or ~/.steam/...).
    #[arg(long)]
    source: Option<PathBuf>,
    /// Overwrite an existing dev-install directory.
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand, Debug)]
enum ScenarioCommand {
    /// Run a managed scenario harness.
    Run(ScenarioArgs),
    /// Stop any scenario-managed components still running under grctl.
    Stop,
}

#[derive(Args, Debug)]
struct ScenarioArgs {
    /// Scenario to execute.
    #[arg(value_enum)]
    scenario: ScenarioKind,
    /// Maximum runtime in seconds before grctl aborts the scenario (0 disables).
    #[arg(long)]
    timeout: Option<u64>,
    /// Extra hold time in seconds after the scenario markers appear.
    #[arg(long, default_value_t = 0.0)]
    hold_seconds: f64,
    /// Launch the scenario components and exit immediately without waiting for completion.
    #[arg(long)]
    detach: bool,
    /// Optional directory for scenario artifacts (forwarded to grim_scenarios).
    #[arg(long)]
    artifacts_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "kebab_case")]
enum ScenarioKind {
    #[clap(name = "intro-to-office-computer")]
    IntroToOfficeComputer,
    #[clap(name = "intro-to-office-tube")]
    IntroToOfficeTube,
}

impl ScenarioKind {
    fn as_cli(self) -> &'static str {
        match self {
            ScenarioKind::IntroToOfficeComputer => "intro-to-office-computer",
            ScenarioKind::IntroToOfficeTube => "intro-to-office-tube",
        }
    }
}

#[derive(Subcommand, Debug)]
enum WatchCommand {
    /// Watch intro.timeline parity between grim_engine logs and retail telemetry.
    IntroTimeline(WatchIntroArgs),
}

#[derive(Args, Debug)]
struct WatchIntroArgs {
    /// Path to the grim_engine log to watch.
    #[arg(long, default_value = "target/grctl/logs/grim_engine.log")]
    engine_log: PathBuf,
    /// Path to the retail telemetry JSONL file.
    #[arg(long, default_value = "dev-install/mods/telemetry_events.jsonl")]
    retail_events: PathBuf,
    /// Poll interval (ms) for reading new lines.
    #[arg(long, default_value_t = 500)]
    poll_interval_ms: u64,
    /// Skip existing content and start watching from the end of each file.
    #[arg(long)]
    from_end: bool,
    /// Launch grim_engine and the retail capture before watching.
    #[arg(long)]
    launch: bool,
    /// Run grim_engine/grim_viewer with --release when launching the session.
    #[arg(long, requires = "launch")]
    engine_release: bool,
}

#[derive(Args, Debug)]
struct LogArgs {
    /// Number of lines to display from the end of the log (0 prints the entire file).
    #[arg(long, default_value_t = 80)]
    tail: usize,
    /// Continuously stream log updates after the initial tail.
    #[arg(long, short = 'f')]
    follow: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[clap(rename_all = "kebab_case")]
enum ComponentKind {
    Engine,
    Viewer,
    Retail,
}

impl ComponentKind {
    fn as_str(self) -> &'static str {
        match self {
            ComponentKind::Engine => "grim_engine",
            ComponentKind::Viewer => "grim_viewer",
            ComponentKind::Retail => "retail_game",
        }
    }

    fn display(self) -> &'static str {
        match self {
            ComponentKind::Engine => "grim_engine",
            ComponentKind::Viewer => "grim_viewer",
            ComponentKind::Retail => "retail game",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ComponentState {
    pid: u32,
    session_id: String,
    command: Vec<String>,
    started_at: DateTime<Utc>,
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
        CommandKind::Viewer(cmd) => handle_viewer(cmd, &paths),
        CommandKind::Retail(cmd) => handle_retail(cmd, &paths),
        CommandKind::Scenario(cmd) => handle_scenario(cmd, &paths),
        CommandKind::Watch(cmd) => handle_watch(cmd, &paths),
        CommandKind::Status => {
            for component in [
                ComponentKind::Engine,
                ComponentKind::Viewer,
                ComponentKind::Retail,
            ] {
                print_component_status(&paths, component)?;
            }
            Ok(())
        }
    }
}

fn handle_engine(cmd: EngineCommand, paths: &Paths) -> Result<()> {
    match cmd {
        EngineCommand::Start(args) => start_engine(args, paths),
        EngineCommand::Stop => stop_component(ComponentKind::Engine, paths, false),
        EngineCommand::Status => {
            print_component_status(paths, ComponentKind::Engine)?;
            Ok(())
        }
        EngineCommand::Logs(args) => {
            show_logs(paths, ComponentKind::Engine, args.tail, args.follow)
        }
    }
}

fn handle_viewer(cmd: ViewerCommand, paths: &Paths) -> Result<()> {
    match cmd {
        ViewerCommand::Start(args) => start_viewer(args, paths),
        ViewerCommand::Stop => stop_component(ComponentKind::Viewer, paths, false),
        ViewerCommand::Status => {
            print_component_status(paths, ComponentKind::Viewer)?;
            Ok(())
        }
        ViewerCommand::Logs(args) => {
            show_logs(paths, ComponentKind::Viewer, args.tail, args.follow)
        }
    }
}

fn handle_retail(cmd: RetailCommand, paths: &Paths) -> Result<()> {
    match cmd {
        RetailCommand::Start(args) => start_retail(args, paths),
        RetailCommand::Stop => stop_component(ComponentKind::Retail, paths, true),
        RetailCommand::Status => {
            print_component_status(paths, ComponentKind::Retail)?;
            print_retail_instrumentation(paths)?;
            Ok(())
        }
        RetailCommand::Logs(args) => {
            show_logs(paths, ComponentKind::Retail, args.tail, args.follow)
        }
        RetailCommand::Copy(args) => copy_retail(args, paths),
    }
}

fn handle_scenario(cmd: ScenarioCommand, paths: &Paths) -> Result<()> {
    match cmd {
        ScenarioCommand::Run(args) => run_scenario(paths, args),
        ScenarioCommand::Stop => stop_scenario(paths),
    }
}

fn handle_watch(cmd: WatchCommand, paths: &Paths) -> Result<()> {
    match cmd {
        WatchCommand::IntroTimeline(args) => watch_intro_timeline(args, paths),
    }
}

fn start_engine(args: EngineStart, paths: &Paths) -> Result<()> {
    ensure_component_available(ComponentKind::Engine, paths)?;

    let session_id = Uuid::new_v4().to_string();
    let log_path = paths.log_path(ComponentKind::Engine);

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
        session_id,
        log_path,
        command,
        command_line,
    )?;

    if args.attach {
        println!(
            "[grctl] attaching to engine log (Ctrl-C to detach): {}",
            paths.log_path(ComponentKind::Engine).display()
        );
        show_logs(paths, ComponentKind::Engine, 200, true)?;
    } else {
        println!(
            "[grctl] engine log: {} (use 'grctl engine logs -f' to follow)",
            paths.log_path(ComponentKind::Engine).display()
        );
    }

    Ok(())
}

fn start_viewer(args: ViewerStart, paths: &Paths) -> Result<()> {
    ensure_component_available(ComponentKind::Viewer, paths)?;

    let session_id = Uuid::new_v4().to_string();
    let log_path = paths.log_path(ComponentKind::Viewer);

    let mut command_line = vec!["cargo".to_string(), "run".to_string()];
    let mut command = Command::new("cargo");
    command.arg("run");
    if args.release {
        command.arg("--release");
        command_line.push("--release".to_string());
    }
    command.args(["-p", "grim_viewer", "--"]);
    command_line.push("-p".to_string());
    command_line.push("grim_viewer".to_string());
    command_line.push("--".to_string());
    if let Some(stream) = &args.engine_stream {
        command.arg("--engine-stream");
        command.arg(stream);
        command_line.push("--engine-stream".to_string());
        command_line.push(stream.clone());
    }
    command.arg("--window-width");
    command.arg(args.window_width.to_string());
    command_line.push("--window-width".to_string());
    command_line.push(args.window_width.to_string());
    command.arg("--window-height");
    command.arg(args.window_height.to_string());
    command_line.push("--window-height".to_string());
    command_line.push(args.window_height.to_string());
    if args.no_retail {
        command.arg("--no-retail");
        command_line.push("--no-retail".to_string());
    }
    for extra in &args.extra_args {
        command.arg(extra);
        command_line.push(extra.clone());
    }

    launch_component(
        ComponentKind::Viewer,
        paths,
        session_id,
        log_path,
        command,
        command_line,
    )
}

fn start_retail(args: RetailStart, paths: &Paths) -> Result<()> {
    ensure_component_available(ComponentKind::Retail, paths)?;

    let session_id = Uuid::new_v4().to_string();
    let log_path = paths.log_path(ComponentKind::Retail);

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
        let status = layout.instrumentation_status()?;
        if !status.shim_available {
            eprintln!(
                "[grctl] warning: LD_PRELOAD shim missing. Run 'cargo build -p grim_telemetry_shim --release' so {} exists; retail hooks will be incomplete until the Rust shim is built.",
                layout.preferred_shim_path().display(),
            );
        }
    }

    let (command, command_line) = build_retail_command(&layout, &args, mode, paths, &session_id)?;

    launch_component(
        ComponentKind::Retail,
        paths,
        session_id,
        log_path,
        command,
        command_line,
    )?;

    if args.attach {
        println!(
            "[grctl] attaching to retail log (Ctrl-C to detach): {}",
            paths.log_path(ComponentKind::Retail).display()
        );
        show_logs(paths, ComponentKind::Retail, 200, true)?;
    } else {
        println!(
            "[grctl] retail log: {} (use 'grctl retail logs -f' to follow)",
            paths.log_path(ComponentKind::Retail).display()
        );
    }

    Ok(())
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
    if status.shim_available {
        "instrumented (shim available)".to_string()
    } else {
        "vanilla (shim missing; build grim_telemetry_shim)".to_string()
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
    let retail_bin = layout.dev_install().join("GrimFandango");
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
) -> Result<(Vec<(String, String)>, Option<String>)> {
    let mut envs = Vec::new();
    if let Some(value) = build_ld_library_path(layout) {
        envs.push(("LD_LIBRARY_PATH".to_string(), value));
    }
    envs.push(("LUA_PATH".to_string(), RETAIL_LUA_PATH.to_string()));
    if let Some(audio) = default_audio_driver() {
        envs.push(("SDL_AUDIODRIVER".to_string(), audio));
    }
    envs.extend(build_steam_env(layout));
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

    let mut log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let timestamp = Utc::now();
    writeln!(
        log_file,
        "\n===== [{}] launching {} =====",
        timestamp.to_rfc3339(),
        component.display()
    )
    .ok();

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
        command: command_line,
        started_at: timestamp,
        log_path: log_path.clone(),
    };
    write_state(component, paths, &state)?;
    spawn_reaper(component, paths.clone(), log_path, child);

    println!(
        "[grctl] started {} (pid {}, session {})",
        component.display(),
        pid,
        session_id
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
                let uptime = match (Utc::now() - state.started_at).to_std() {
                    Ok(duration) => format_duration(duration).to_string(),
                    Err(_) => "unknown".to_string(),
                };
                println!(
                    "[grctl] {:<12} status: running (pid {}, session {}, uptime {})",
                    component.as_str(),
                    state.pid,
                    state.session_id,
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

fn show_logs(paths: &Paths, component: ComponentKind, tail: usize, follow: bool) -> Result<()> {
    let log_path = paths.log_path(component);
    if !log_path.exists() {
        bail!(
            "no log file found for {} at {}",
            component.display(),
            log_path.display()
        );
    }
    println!("# {}", log_path.display());
    if follow {
        follow_logs(&log_path, tail)?;
    } else {
        let lines = tail_file(&log_path, tail)?;
        for line in lines {
            println!("{line}");
        }
    }
    Ok(())
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

fn watch_intro_timeline(args: WatchIntroArgs, paths: &Paths) -> Result<()> {
    let WatchIntroArgs {
        engine_log,
        retail_events,
        poll_interval_ms,
        from_end,
        launch,
        engine_release,
    } = args;

    let shutdown = Arc::new(AtomicBool::new(false));
    install_watch_shutdown_handler(shutdown.clone())?;

    let poll_interval = Duration::from_millis(poll_interval_ms);
    let mut guard = LaunchGuard::default();
    let headless_engine = true;

    let (engine_path, retail_path) = if launch {
        ensure_component_available(ComponentKind::Engine, paths)?;
        ensure_component_available(ComponentKind::Retail, paths)?;
        prepare_intro_watch_sources(paths)?;
        start_intro_watch_components(paths, &mut guard, headless_engine, engine_release)?;
        (
            paths.log_path(ComponentKind::Engine),
            paths.retail_telemetry_path(),
        )
    } else {
        (
            resolve_repo_path(paths, &engine_log),
            resolve_repo_path(paths, &retail_events),
        )
    };

    println!("[grctl] watching intro.timeline parity");
    println!("  engine log: {}", engine_path.display());
    println!("  retail telemetry: {}", retail_path.display());
    println!("  poll interval: {}ms", poll_interval_ms);
    if from_end {
        println!("  starting from end of both files");
    }
    if launch {
        println!("  launched: grim_engine (headless, --verbose) and retail capture");
        println!("[grctl] press Ctrl-C to stop the watch and shut down launched components");
    }

    let status_paths = if launch { Some(paths) } else { None };
    let result = run_intro_timeline_loop(
        &engine_path,
        &retail_path,
        from_end,
        poll_interval,
        &shutdown,
        status_paths,
    );

    if launch {
        println!("[grctl] stopping launched components...");
        guard.stop_all(paths);
    }

    result
}

fn install_watch_shutdown_handler(flag: Arc<AtomicBool>) -> Result<()> {
    ctrlc::set_handler(move || {
        flag.store(true, Ordering::SeqCst);
    })
    .context("installing Ctrl-C handler for intro timeline watch")
}

fn start_intro_watch_components(
    paths: &Paths,
    guard: &mut LaunchGuard,
    headless_engine: bool,
    engine_release: bool,
) -> Result<()> {
    let engine_args = EngineStart {
        release: engine_release,
        headless: headless_engine,
        verbose: true,
        attach: false,
        extra_args: Vec::new(),
    };
    start_engine(engine_args, paths)?;
    guard.push(ComponentKind::Engine);

    let retail_args = RetailStart {
        timeout: "0".to_string(),
        no_timeout: true,
        vanilla: false,
        attach: false,
        extra_args: Vec::new(),
    };
    start_retail(retail_args, paths)?;
    guard.push(ComponentKind::Retail);

    Ok(())
}

fn prepare_intro_watch_sources(paths: &Paths) -> Result<()> {
    let engine_path = paths.log_path(ComponentKind::Engine);
    let retail_path = paths.retail_telemetry_path();
    reset_file(&engine_path)?;
    reset_file(&retail_path)?;
    Ok(())
}

fn reset_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory for {}", path.display()))?;
    }
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("clearing {}", path.display()))?;
    }
    File::create(path).with_context(|| format!("initializing {}", path.display()))?;
    Ok(())
}

fn run_intro_timeline_loop(
    engine_path: &Path,
    retail_path: &Path,
    from_end: bool,
    poll_interval: Duration,
    shutdown: &Arc<AtomicBool>,
    status_paths: Option<&Paths>,
) -> Result<()> {
    let mut engine_pos = start_position(engine_path, from_end);
    let mut retail_pos = start_position(retail_path, from_end);
    let mut engine_events: Vec<String> = Vec::new();
    let mut retail_events: Vec<String> = Vec::new();
    let mut last_snapshot = String::new();
    let mut first_snapshot = true;
    let mut waiting_for_engine = false;
    let mut waiting_for_retail = false;
    let mut reported_engine_exit = false;
    let mut reported_retail_exit = false;

    while !shutdown.load(Ordering::SeqCst) {
        let mut changed = false;

        match read_new_lines(engine_path, &mut engine_pos, from_end) {
            Ok((lines, reset)) => {
                if reset {
                    engine_events.clear();
                    changed = true;
                    println!(
                        "[grctl] {} truncated; restarting {}",
                        engine_path.display(),
                        if from_end { "at end" } else { "from start" }
                    );
                }
                for line in lines {
                    if let Some(event) = parse_intro_timeline_line(&line) {
                        engine_events.push(event);
                        changed = true;
                    }
                }
                if waiting_for_engine {
                    println!(
                        "[grctl] engine log available; resuming watch at {}",
                        engine_path.display()
                    );
                    waiting_for_engine = false;
                    changed = true;
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                if !waiting_for_engine {
                    println!(
                        "[grctl] waiting for engine log {}; retrying...",
                        engine_path.display()
                    );
                    waiting_for_engine = true;
                }
            }
            Err(err) => return Err(err).context(format!("reading {}", engine_path.display())),
        };

        match read_new_lines(retail_path, &mut retail_pos, from_end) {
            Ok((lines, reset)) => {
                if reset {
                    retail_events.clear();
                    changed = true;
                    println!(
                        "[grctl] {} truncated; restarting {}",
                        retail_path.display(),
                        if from_end { "at end" } else { "from start" }
                    );
                }
                for line in lines {
                    if let Some(event) = parse_intro_timeline_line(&line) {
                        retail_events.push(event);
                        changed = true;
                    }
                }
                if waiting_for_retail {
                    println!(
                        "[grctl] retail telemetry available; resuming watch at {}",
                        retail_path.display()
                    );
                    waiting_for_retail = false;
                    changed = true;
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                if !waiting_for_retail {
                    println!(
                        "[grctl] waiting for retail telemetry {}; retrying...",
                        retail_path.display()
                    );
                    waiting_for_retail = true;
                }
            }
            Err(err) => return Err(err).context(format!("reading {}", retail_path.display())),
        };

        if changed || first_snapshot {
            let snapshot = render_intro_snapshot(&engine_events, &retail_events);
            if snapshot != last_snapshot {
                println!("{snapshot}");
                last_snapshot = snapshot;
            }
            first_snapshot = false;
        }

        if let Some(paths) = status_paths {
            let engine_log = paths.log_path(ComponentKind::Engine);
            let retail_log = paths.log_path(ComponentKind::Retail);
            if !reported_engine_exit {
                if let Some(state) = load_state(ComponentKind::Engine, paths)? {
                    if !process_alive(state.pid) {
                        println!(
                            "[grctl] grim_engine exited (pid {}, session {}); recent log:",
                            state.pid, state.session_id
                        );
                        print_log_tail(&engine_log, 20);
                        reported_engine_exit = true;
                    }
                }
            }
            if !reported_retail_exit {
                if let Some(state) = load_state(ComponentKind::Retail, paths)? {
                    if !process_alive(state.pid) {
                        println!(
                            "[grctl] retail_game exited (pid {}, session {}); recent log:",
                            state.pid, state.session_id
                        );
                        print_log_tail(&retail_log, 20);
                        reported_retail_exit = true;
                    }
                }
            }
        }

        thread::sleep(poll_interval);
    }

    if let Some(paths) = status_paths {
        let engine_log = paths.log_path(ComponentKind::Engine);
        let retail_log = paths.log_path(ComponentKind::Retail);
        if !reported_engine_exit {
            if let Some(state) = load_state(ComponentKind::Engine, paths)? {
                if !process_alive(state.pid) {
                    println!(
                        "[grctl] grim_engine exited (pid {}, session {}); recent log:",
                        state.pid, state.session_id
                    );
                    print_log_tail(&engine_log, 20);
                }
            }
        }
        if !reported_retail_exit {
            if let Some(state) = load_state(ComponentKind::Retail, paths)? {
                if !process_alive(state.pid) {
                    println!(
                        "[grctl] retail_game exited (pid {}, session {}); recent log:",
                        state.pid, state.session_id
                    );
                    print_log_tail(&retail_log, 20);
                }
            }
        }
    }

    Ok(())
}

fn start_position(path: &Path, from_end: bool) -> Option<u64> {
    if from_end {
        fs::metadata(path).map(|meta| meta.len()).ok()
    } else {
        Some(0)
    }
}

fn read_new_lines(
    path: &Path,
    position: &mut Option<u64>,
    from_end_on_reset: bool,
) -> io::Result<(Vec<String>, bool)> {
    let mut reader = BufReader::new(File::open(path)?);
    let len = reader.get_ref().metadata()?.len();
    let mut reset = false;
    let mut pos = position.unwrap_or_else(|| if from_end_on_reset { len } else { 0 });
    if len < pos {
        pos = if from_end_on_reset { len } else { 0 };
        reset = true;
    }
    reader.seek(SeekFrom::Start(pos))?;
    let mut lines: Vec<String> = Vec::new();
    let mut new_pos = pos;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        new_pos += bytes as u64;
        let trimmed = line.trim_end_matches(&['\n', '\r'][..]).to_string();
        lines.push(trimmed);
    }
    *position = Some(new_pos);
    Ok((lines, reset))
}

fn print_log_tail(path: &Path, lines: usize) {
    if let Ok(file) = File::open(path) {
        let reader = BufReader::new(file);
        let mut buffer: Vec<String> = Vec::with_capacity(lines);
        for line in reader.lines().flatten() {
            if buffer.len() == lines {
                buffer.remove(0);
            }
            buffer.push(line);
        }
        for line in buffer {
            println!("  {line}");
        }
    } else {
        println!("  (log {} not readable)", path.display());
    }
}

fn parse_intro_timeline_line(line: &str) -> Option<String> {
    let start = line.find('{')?;
    let value: Value = serde_json::from_str(&line[start..]).ok()?;
    if value.get("label").and_then(|label| label.as_str()) != Some("intro.timeline") {
        return None;
    }
    value
        .get("data")
        .and_then(|data| data.get("event"))
        .and_then(|event| event.as_str())
        .map(|event| event.to_string())
}

fn render_intro_snapshot(engine_events: &[String], retail_events: &[String]) -> String {
    let missing_in_engine = missing_events(retail_events, engine_events);
    let missing_in_retail = missing_events(engine_events, retail_events);
    let order_matches = engine_events == retail_events;
    format!(
        "[grctl] intro.timeline: order_match={} missing_in_engine=[{}] missing_in_retail=[{}]\n  engine ({}): {}\n  retail ({}): {}",
        order_matches,
        missing_in_engine.join(", "),
        missing_in_retail.join(", "),
        engine_events.len(),
        render_event_list(engine_events),
        retail_events.len(),
        render_event_list(retail_events),
    )
}

fn render_event_list(events: &[String]) -> String {
    if events.is_empty() {
        "none".to_string()
    } else {
        events.join(", ")
    }
}

fn missing_events(expected: &[String], actual: &[String]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for event in actual {
        *counts.entry(event).or_insert(0) += 1;
    }
    let mut missing = Vec::new();
    for event in expected {
        match counts.get_mut(event.as_str()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing.push(event.clone()),
        }
    }
    missing
}

fn resolve_repo_path(paths: &Paths, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        paths.repo_root.join(path)
    }
}

fn stop_scenario(paths: &Paths) -> Result<()> {
    stop_component(ComponentKind::Viewer, paths, false)?;
    stop_component(ComponentKind::Engine, paths, false)
}

fn run_scenario(paths: &Paths, args: ScenarioArgs) -> Result<()> {
    const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
    let scenario_timeout = args.timeout.unwrap_or(DEFAULT_TIMEOUT_SECONDS);

    let mut command = Command::new("cargo");
    command.arg("run");
    command.arg("-p");
    command.arg("grim_scenarios");
    command.arg("--");
    command.arg("run");
    command.arg(args.scenario.as_cli());
    command.arg("--timeout");
    command.arg(scenario_timeout.to_string());
    if args.hold_seconds > 0.0 {
        command.arg("--hold-seconds");
        command.arg(args.hold_seconds.to_string());
    }
    if args.detach {
        command.arg("--detach");
    }
    if let Some(dir) = &args.artifacts_dir {
        command.arg("--artifacts-dir");
        command.arg(dir);
    }
    run_managed_command(
        command,
        paths,
        scenario_timeout,
        &format!("scenario:{}", args.scenario.as_cli()),
        &format!("grim_scenarios {}", args.scenario.as_cli()),
    )
}

fn run_managed_command(
    mut command: Command,
    paths: &Paths,
    timeout_secs: u64,
    session_label: &str,
    description: &str,
) -> Result<()> {
    command.current_dir(&paths.repo_root);
    let session_id = Uuid::new_v4().to_string();
    command.env("GRCTL_MANAGED", "1");
    command.env("GRCTL_SESSION_ID", &session_id);
    command.env("GRCTL_COMPONENT", session_label);
    command.env("GRCTL_STATE_DIR", &paths.state_dir);
    if let Ok(bin_path) = std::env::current_exe() {
        command.env("GRCTL_BIN", bin_path);
    }

    println!("[grctl] launching {description} (session {session_id})");

    let mut child = command
        .spawn()
        .with_context(|| format!("spawning {description}"))?;
    let timeout = if timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(timeout_secs))
    };

    let result = match timeout {
        Some(limit) => wait_with_timeout(&mut child, limit),
        None => child.wait().map(Some).map_err(Into::into),
    }?;

    match result {
        Some(status) => {
            if status.success() {
                println!("[grctl] {description} completed successfully (session {session_id})");
                Ok(())
            } else {
                Err(anyhow!("{description} exited with status {status}"))
            }
        }
        None => {
            println!(
                "[grctl] timeout ({timeout_secs}s) reached for {description}; sending SIGTERM"
            );
            child.kill().context("terminating timed-out process")?;
            let _ = child.wait();
            Err(anyhow!("{description} timed out after {}s", timeout_secs))
        }
    }
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    limit: Duration,
) -> Result<Option<std::process::ExitStatus>> {
    let mut elapsed = Duration::from_millis(0);
    let poll = Duration::from_millis(200);
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if elapsed >= limit {
            return Ok(None);
        }
        thread::sleep(poll);
        elapsed = start.elapsed();
    }
}
