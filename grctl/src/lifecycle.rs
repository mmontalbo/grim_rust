use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::cli::{EngineStart, LogArgs, RetailDebugger, RetailStart, RunSelection};
use crate::components::{
    clear_state, ensure_component_available, load_state, process_alive, write_state, ComponentKind,
    ComponentState, Paths,
};
use crate::gdb;
use crate::log_follow::show_logs;
use crate::retail::{
    extend_env_var, symbol_map_status_for, warn_if_shaders_missing, HookMode, RetailLayout,
    SymbolMapStatus,
};

const RETAIL_STEAM_APP_ID: &str = "345350";
const RETAIL_LUA_PATH: &str = "./?.lua;./?.LUA;./mods/?.lua";
const RUST_SHIM_TARGET: &str = "i686-unknown-linux-gnu";
type EnvVars = Vec<(String, String)>;
type EnvSetup = (EnvVars, Option<String>);

#[derive(Clone, Debug)]
pub(crate) struct LaunchInfo {
    pub(crate) run_id: String,
    pub(crate) log_path: PathBuf,
    #[allow(dead_code)]
    pub(crate) pid: u32,
}

pub(crate) fn start_engine(args: EngineStart, paths: &Paths) -> Result<LaunchInfo> {
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

    let pid = launch_component(
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

    Ok(LaunchInfo {
        run_id,
        log_path,
        pid,
    })
}

pub(crate) fn start_retail(args: RetailStart, paths: &Paths) -> Result<LaunchInfo> {
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
    if args.debugger.is_some() && !args.no_timeout && args.timeout.trim() != "0" {
        println!("[grctl] debugger requested; disabling retail timeout");
    }
    if matches!(mode, HookMode::Instrumented) {
        ensure_rust_shim_ready(paths, &layout)?;
        ensure_symbol_maps_ready(paths, &layout)?;
        let status = layout.instrumentation_status()?;
        if !status.shim_available {
            eprintln!(
                "[grctl] warning: LD_PRELOAD shim missing. Run 'cargo build -p grim_analysis --release' so {} exists; retail hooks will be incomplete until the Rust shim is built.",
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

    let runtime_preloads = gather_runtime_preloads(&layout);
    let (env_pairs, ld_preload) = assemble_retail_env(&layout, mode, &runtime_preloads)?;

    if let Some(RetailDebugger::Gdb) = args.debugger {
        let cmd_path = gdb::write_retail_gdb_script(
            paths,
            &layout,
            &session_id,
            &env_pairs,
            ld_preload.as_deref(),
        )?;
        let mut command = build_retail_gdb_command(&layout, &args, &cmd_path)?;
        command.env("GRCTL_MANAGED", "1");
        command.env("GRCTL_SESSION_ID", &session_id);
        command.env("GRCTL_COMPONENT", ComponentKind::Retail.as_str());
        command.env("GRCTL_LOG_PATH", &log_path);
        command.env("GRCTL_STATE_DIR", &paths.state_dir);
        command.env_remove("LD_PRELOAD");
        command.env_remove("LD_PRELOAD_32");

        println!(
            "[grctl] launching retail under gdb (commands: {}); gdb will start the game from entrypoint",
            cmd_path.display()
        );
        let mut child = command.spawn().context("starting gdb")?;
        let pid = child.id();
        let status = child.wait().context("waiting for gdb to exit")?;
        if !status.success() {
            eprintln!("[grctl] warning: gdb exited with {}", status);
        }
        return Ok(LaunchInfo {
            run_id,
            log_path,
            pid,
        });
    }

    let script_path = write_retail_launcher_script(
        paths,
        &session_id,
        &layout,
        &env_pairs,
        ld_preload.as_deref(),
        None,
    )?;
    let (command, command_line) = build_retail_command(&layout, &args, &script_path)?;

    let pid = launch_component(
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

    Ok(LaunchInfo {
        run_id,
        log_path,
        pid,
    })
}

fn ensure_rust_shim_ready(paths: &Paths, layout: &RetailLayout) -> Result<()> {
    ensure_i686_target_installed()?;
    println!("[grctl] rebuilding grim_analysis --release...");
    let build_cmd = format!(
        "cargo build -p grim_analysis --release --target {}",
        RUST_SHIM_TARGET
    );
    let status = Command::new("nix-shell")
        .current_dir(&paths.repo_root)
        .args(["--run", &build_cmd])
        .status()
        .context("building grim_analysis --release for i686-unknown-linux-gnu")?;
    if !status.success() {
        bail!("grim_analysis build failed with status {}", status);
    }
    if layout.resolved_shim_path().is_some() {
        Ok(())
    } else {
        bail!(
            "grim_analysis build succeeded but the shared object is still missing (expected {})",
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

pub(crate) fn build_retail_command(
    layout: &RetailLayout,
    args: &RetailStart,
    script_path: &Path,
) -> Result<(Command, Vec<String>)> {
    let debugging = args.debugger.is_some();
    let use_timeout = !args.no_timeout && args.timeout.trim() != "0" && !debugging;
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

pub(crate) fn build_ld_library_path(layout: &RetailLayout) -> Option<String> {
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
    extra_env: Option<&[(String, String)]>,
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
    if let Some(extra) = extra_env {
        for (key, value) in extra {
            writeln!(file, "export {}={}", key, shell_quote(value))?;
        }
    }
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

pub(crate) fn build_retail_gdb_command(
    layout: &RetailLayout,
    args: &RetailStart,
    cmd_path: &Path,
) -> Result<Command> {
    let retail_bin = layout.retail_bin();
    if !retail_bin.exists() {
        bail!(
            "retail binary missing at {}; run 'grctl retail copy' first",
            retail_bin.display()
        );
    }

    let cmd_path_str = cmd_path.to_string_lossy().into_owned();
    let mut command = Command::new("steam-run");
    command
        .current_dir(layout.dev_install())
        .args(["gdb", "-q", "-x", &cmd_path_str, "--args"])
        .arg(retail_bin);
    for extra in &args.extra_args {
        command.arg(extra);
    }
    Ok(command)
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

pub(crate) fn launch_component(
    component: ComponentKind,
    paths: &Paths,
    session_id: String,
    run_id: String,
    log_path: PathBuf,
    mut command: Command,
    command_line: Vec<String>,
) -> Result<u32> {
    command.current_dir(&paths.repo_root);
    command.stdin(Stdio::null());
    command.env("GRCTL_MANAGED", "1");
    command.env("GRCTL_SESSION_ID", &session_id);
    command.env("GRCTL_COMPONENT", component.as_str());
    command.env("GRCTL_LOG_PATH", &log_path);
    command.env("GRCTL_STATE_DIR", &paths.state_dir);

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
    Ok(pid)
}

pub(crate) fn spawn_reaper(
    component: ComponentKind,
    paths: Paths,
    log_path: PathBuf,
    mut child: Child,
) {
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

pub(crate) fn handle_component_exit(
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

pub(crate) fn has_intro_timeline_events(events_path: &Path) -> io::Result<bool> {
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

pub(crate) fn stop_component(component: ComponentKind, paths: &Paths, force: bool) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::TempDir;

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &Path)]) -> Self {
            let mut saved = Vec::new();
            for (key, value) in vars {
                saved.push((*key, std::env::var(key).ok()));
                std::env::set_var(key, value);
            }
            EnvGuard { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[cfg(unix)]
    fn build_fake_layout(root: &Path) -> Result<(RetailLayout, EnvGuard)> {
        let dev_install = root.join("dev-install");
        fs::create_dir_all(&dev_install)?;
        fs::write(dev_install.join("GrimFandango"), b"bin")?;
        fs::write(dev_install.join("libLua.so"), b"bin")?;
        fs::write(dev_install.join("GrimFandango.sym"), b"")?;
        fs::write(dev_install.join("libLua.so.sym"), b"")?;

        let shim_path = root
            .join("target")
            .join("i686-unknown-linux-gnu")
            .join("release")
            .join("libgrim_analysis.so");
        if let Some(parent) = shim_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&shim_path, b"shim")?;

        let steam_root = root.join("steam-root");
        fs::create_dir_all(&steam_root)?;
        let steam_install = root.join("steam-install");
        fs::create_dir_all(&steam_install)?;

        let guard = EnvGuard::set(&[
            ("GRIM_STEAM_ROOT", &steam_root),
            ("GRIM_STEAM_INSTALL", &steam_install),
        ]);
        let layout = RetailLayout::new(root)?;
        Ok((layout, guard))
    }

    fn build_fake_paths(root: &Path) -> Result<Paths> {
        let state_dir = root.join("state");
        let log_dir = root.join("logs");
        let launcher_dir = root.join("launchers");
        fs::create_dir_all(&state_dir)?;
        fs::create_dir_all(&log_dir)?;
        fs::create_dir_all(&launcher_dir)?;
        Ok(Paths {
            repo_root: root.to_path_buf(),
            state_dir,
            log_dir,
            launcher_dir,
        })
    }

    #[test]
    fn gdb_command_launches_under_steam_run() -> Result<()> {
        let tmp = TempDir::new()?;
        let (layout, _guard) = build_fake_layout(tmp.path())?;
        let script = tmp.path().join("cmds.gdb");
        fs::write(&script, b"# dummy")?;

        let args = RetailStart {
            timeout: "0".to_string(),
            no_timeout: true,
            vanilla: false,
            attach: false,
            debugger: Some(RetailDebugger::Gdb),
            run_id: None,
            extra_args: vec!["--foo".to_string(), "bar".to_string()],
        };

        let command = build_retail_gdb_command(&layout, &args, &script)?;
        assert_eq!(command.get_program(), Path::new("steam-run"));
        let arg_list: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &arg_list[..6],
            &[
                "gdb".to_string(),
                "-q".to_string(),
                "-x".to_string(),
                script.to_string_lossy().into_owned(),
                "--args".to_string(),
                layout.retail_bin().to_string_lossy().into_owned(),
            ]
        );
        assert!(arg_list.ends_with(&["--foo".to_string(), "bar".to_string()]));
        Ok(())
    }

    #[test]
    fn gdb_script_includes_start_and_symbols() -> Result<()> {
        let tmp = TempDir::new()?;
        let paths = build_fake_paths(tmp.path())?;
        let (layout, _guard) = build_fake_layout(tmp.path())?;
        let env_pairs = vec![
            ("FOO".to_string(), "BAR".to_string()),
            ("LD_PRELOAD".to_string(), "SHOULD_SKIP".to_string()),
            ("LD_PRELOAD_32".to_string(), "SHOULD_SKIP".to_string()),
        ];
        let script = gdb::write_retail_gdb_script(
            &paths,
            &layout,
            "sess-1",
            &env_pairs,
            Some("preload.so"),
        )?;
        let contents = fs::read_to_string(script)?;
        assert!(contents.contains("set breakpoint pending on"));
        assert!(contents.contains("start"));
        assert!(contents.contains("libLua.so"));
        assert!(contents.contains("telemetry shim"));
        assert!(contents.contains("apply_env()"));
        assert!(contents.contains("set environment"));
        assert!(contents.contains("\"FOO\": \"BAR\""));
        assert!(!contents.contains("\"LD_PRELOAD\":"));
        assert!(!contents.contains("\"LD_PRELOAD_32\":"));
        assert!(contents.contains("set_lua_alloc_breaks()"));
        assert!(contents.contains("dump_lua_stack(L, max_slots=5)"));
        assert!(contents.contains("read_u32"));
        assert!(contents.contains("LUA_TYPE_NAMES = {"));
        assert!(contents.contains("POINTER_HINTS = {"));
        assert!(contents.contains("install_lua_breakpoint("));
        assert!(contents.contains("class LuaReturnBreakpoint"));
        assert!(contents.contains("class LuaEntryBreakpoint"));
        assert!(contents.contains("LUA_NEWSTATE_OFF = 0x125f0"));
        assert!(contents.contains("LUA_OPEN_OFF = 0x128a0"));
        assert!(contents.contains("def decode_value("));
        assert!(contents.contains("type_display(tt_signed)} ttype=0x{words[0]:x}"));
        assert!(contents.contains("used={used_slots}"));
        assert!(contents.contains("gdb.Breakpoint"));
        assert!(contents.contains("gdb.FinishBreakpoint"));
        assert!(contents.contains("unset environment LD_PRELOAD"));
        assert!(!contents.contains("set environment LD_PRELOAD "));
        assert!(contents.contains("set environment LD_PRELOAD_32"));
        Ok(())
    }
}
