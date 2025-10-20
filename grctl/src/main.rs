use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
use uuid::Uuid;

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
    /// Run higher-level scenarios composed of multiple components.
    #[command(subcommand)]
    Scenario(ScenarioCommand),
    /// Execute test/validation scripts with standard timeouts.
    #[command(subcommand)]
    Check(CheckCommand),
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
    /// Address grim_engine should bind for the viewer handshake.
    #[arg(long, default_value = "127.0.0.1:17500")]
    stream_bind: String,
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
    #[arg(long, default_value = "127.0.0.1:17500")]
    engine_stream: String,
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
}

#[derive(Args, Debug)]
struct RetailStart {
    /// Time limit for the retail session (examples: 20s, 5m). Use 0 to disable.
    #[arg(long, default_value = "20s")]
    timeout: String,
    /// Disable the timeout entirely (overrides --timeout).
    #[arg(long)]
    no_timeout: bool,
    /// Additional arguments passed through to tools/run_dev_install.sh.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum ScenarioCommand {
    /// Launch the live-preview stack (retail capture + engine + viewer).
    LivePreview(ScenarioArgs),
    /// Launch grim_engine + grim_viewer for movie debugging.
    MovieDebug(ScenarioArgs),
}

#[derive(Args, Debug)]
struct ScenarioArgs {
    /// Maximum runtime in seconds before the scenario is terminated (0 disables).
    #[arg(long, default_value_t = 180)]
    timeout: u64,
    /// Extra arguments forwarded to the underlying scenario script after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum CheckCommand {
    /// Ensure Manny's office resumes after the intro cutscene.
    IntroResume(CheckArgs),
    /// Validate that the viewer debug overlay renders engine events.
    EngineOverlay(CheckArgs),
}

#[derive(Args, Debug)]
struct CheckArgs {
    /// Maximum runtime in seconds before grctl aborts the check (0 disables).
    #[arg(long, default_value_t = 90)]
    timeout: u64,
    /// Additional arguments forwarded to the check script after '--'.
    #[arg(last = true)]
    extra_args: Vec<String>,
}

#[derive(Args, Debug)]
struct LogArgs {
    /// Number of lines to display from the end of the log (0 prints the entire file).
    #[arg(long, default_value_t = 80)]
    tail: usize,
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
        fs::create_dir_all(&state_dir).context("creating grctl state directory")?;
        fs::create_dir_all(&log_dir).context("creating grctl logs directory")?;
        Ok(Self {
            repo_root,
            state_dir,
            log_dir,
        })
    }

    fn state_path(&self, component: ComponentKind) -> PathBuf {
        self.state_dir.join(format!("{}.json", component.as_str()))
    }

    fn log_path(&self, component: ComponentKind) -> PathBuf {
        self.log_dir.join(format!("{}.log", component.as_str()))
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
        CommandKind::Check(cmd) => handle_check(cmd, &paths),
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
        EngineCommand::Logs(args) => show_logs(paths, ComponentKind::Engine, args.tail),
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
        ViewerCommand::Logs(args) => show_logs(paths, ComponentKind::Viewer, args.tail),
    }
}

fn handle_retail(cmd: RetailCommand, paths: &Paths) -> Result<()> {
    match cmd {
        RetailCommand::Start(args) => start_retail(args, paths),
        RetailCommand::Stop => stop_component(ComponentKind::Retail, paths, true),
        RetailCommand::Status => {
            print_component_status(paths, ComponentKind::Retail)?;
            Ok(())
        }
        RetailCommand::Logs(args) => show_logs(paths, ComponentKind::Retail, args.tail),
    }
}

fn handle_scenario(cmd: ScenarioCommand, paths: &Paths) -> Result<()> {
    match cmd {
        ScenarioCommand::LivePreview(args) => run_tool_script(
            paths,
            "python3",
            "tools/run_live_preview.py",
            &args.extra_args,
            args.timeout,
            "scenario:live_preview",
        ),
        ScenarioCommand::MovieDebug(args) => run_tool_script(
            paths,
            "python3",
            "tools/run_movie_debug.py",
            &args.extra_args,
            args.timeout,
            "scenario:movie_debug",
        ),
    }
}

fn handle_check(cmd: CheckCommand, paths: &Paths) -> Result<()> {
    match cmd {
        CheckCommand::IntroResume(args) => run_tool_script(
            paths,
            "python3",
            "tools/check_intro_resume.py",
            &args.extra_args,
            args.timeout,
            "check:intro_resume",
        ),
        CheckCommand::EngineOverlay(args) => run_tool_script(
            paths,
            "python3",
            "tools/check_engine_event_overlay.py",
            &args.extra_args,
            args.timeout,
            "check:engine_overlay",
        ),
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
    command.arg("--stream-bind");
    command.arg(&args.stream_bind);
    command_line.push("--stream-bind".to_string());
    command_line.push(args.stream_bind.clone());
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
    )
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
    command.arg("--engine-stream");
    command.arg(&args.engine_stream);
    command_line.push("--engine-stream".to_string());
    command_line.push(args.engine_stream.clone());
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

    let mut command = Command::new("tools/run_dev_install.sh");
    let mut command_line = vec!["tools/run_dev_install.sh".to_string()];
    if args.no_timeout {
        command.arg("--no-timeout");
        command_line.push("--no-timeout".to_string());
    } else {
        if args.timeout.trim() != "0" {
            command.arg("--timeout");
            command.arg(&args.timeout);
            command_line.push("--timeout".to_string());
            command_line.push(args.timeout.clone());
        } else {
            command.arg("--no-timeout");
            command_line.push("--no-timeout".to_string());
        }
    }
    for extra in &args.extra_args {
        command.arg(extra);
        command_line.push(extra.clone());
    }

    launch_component(
        ComponentKind::Retail,
        paths,
        session_id,
        log_path,
        command,
        command_line,
    )
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
        let summary = match status {
            Ok(code) => format!("exited with {}", code),
            Err(err) => format!("wait error: {err}"),
        };
        if let Ok(mut log) = OpenOptions::new().append(true).open(&log_path) {
            let _ = writeln!(log, "[grctl] {} {}", component.display(), summary);
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

fn show_logs(paths: &Paths, component: ComponentKind, tail: usize) -> Result<()> {
    let log_path = paths.log_path(component);
    if !log_path.exists() {
        bail!(
            "no log file found for {} at {}",
            component.display(),
            log_path.display()
        );
    }
    let lines = tail_file(&log_path, tail)?;
    println!("# {}", log_path.display());
    for line in lines {
        println!("{line}");
    }
    Ok(())
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

fn run_tool_script(
    paths: &Paths,
    program: &str,
    script: &str,
    extra_args: &[String],
    timeout_secs: u64,
    session_label: &str,
) -> Result<()> {
    let script_path = paths.repo_root.join(script);
    if !script_path.exists() {
        bail!("tool script not found: {}", script_path.display());
    }
    let mut command = Command::new(program);
    command.current_dir(&paths.repo_root);
    command.arg(script);
    for arg in extra_args {
        command.arg(arg);
    }
    let session_id = Uuid::new_v4().to_string();
    command.env("GRCTL_MANAGED", "1");
    command.env("GRCTL_SESSION_ID", &session_id);
    command.env("GRCTL_COMPONENT", session_label);
    command.env("GRCTL_STATE_DIR", &paths.state_dir);

    println!("[grctl] launching {script} (session {session_id})");

    let mut child = command
        .spawn()
        .with_context(|| format!("spawning {script} via {program}"))?;
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
                println!("[grctl] {script} completed successfully (session {session_id})");
                Ok(())
            } else {
                Err(anyhow!("{script} exited with status {status}",))
            }
        }
        None => {
            println!("[grctl] timeout ({timeout_secs}s) reached for {script}; sending SIGTERM");
            child.kill().context("terminating timed-out script")?;
            let _ = child.wait();
            Err(anyhow!("{script} timed out after {}s", timeout_secs))
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
