use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use humantime::format_duration;
use serde::{Deserialize, Serialize};
use serde_json;

use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[clap(rename_all = "kebab_case")]
pub enum ComponentKind {
    Engine,
    Retail,
}

impl ComponentKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ComponentKind::Engine => "grim_engine",
            ComponentKind::Retail => "retail_game",
        }
    }

    pub(crate) fn display(self) -> &'static str {
        match self {
            ComponentKind::Engine => "grim_engine",
            ComponentKind::Retail => "retail game",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComponentState {
    pub(crate) pid: u32,
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) run_id: Option<String>,
    pub(crate) command: Vec<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) log_path: PathBuf,
}

impl ComponentState {
    pub(crate) fn effective_run_id(&self) -> &str {
        self.run_id.as_deref().unwrap_or(&self.session_id)
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub(crate) repo_root: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) log_dir: PathBuf,
    pub(crate) launcher_dir: PathBuf,
}

impl Paths {
    pub(crate) fn discover() -> Result<Self> {
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

    pub(crate) fn state_path(&self, component: ComponentKind) -> PathBuf {
        self.state_dir.join(format!("{}.json", component.as_str()))
    }

    pub(crate) fn log_path(&self, component: ComponentKind) -> PathBuf {
        self.log_dir.join(format!("{}.log", component.as_str()))
    }

    pub(crate) fn component_log_dir(&self, component: ComponentKind) -> Result<PathBuf> {
        let dir = self.log_dir.join(component.as_str());
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating log directory {}", dir.display()))?;
        Ok(dir)
    }

    pub(crate) fn run_log_path(&self, component: ComponentKind, run_id: &str) -> Result<PathBuf> {
        let dir = self.component_log_dir(component)?;
        Ok(dir.join(format!("{run_id}.log")))
    }

    pub(crate) fn update_latest_log_alias(
        &self,
        component: ComponentKind,
        target: &Path,
    ) -> Result<()> {
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

    pub(crate) fn list_run_logs(&self, component: ComponentKind) -> Result<Vec<(String, PathBuf)>> {
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

    pub(crate) fn launcher_script(&self, session_id: &str) -> PathBuf {
        self.launcher_dir.join(format!("retail_{session_id}.sh"))
    }

    pub(crate) fn gdb_commands_path(&self, session_id: &str) -> PathBuf {
        self.launcher_dir
            .join(format!("retail_gdb_{session_id}.cmds"))
    }

    pub(crate) fn retail_telemetry_path(&self) -> PathBuf {
        self.repo_root
            .join("dev-install")
            .join("mods")
            .join("telemetry_events.jsonl")
    }
}

pub(crate) fn ensure_component_available(component: ComponentKind, paths: &Paths) -> Result<()> {
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

pub(crate) fn process_alive(pid: u32) -> bool {
    let pid = Pid::from_raw(pid as i32);
    match kill(pid, None) {
        Ok(_) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}

pub(crate) fn load_state(
    component: ComponentKind,
    paths: &Paths,
) -> Result<Option<ComponentState>> {
    let path = paths.state_path(component);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let state: ComponentState =
        serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(state))
}

pub(crate) fn write_state(
    component: ComponentKind,
    paths: &Paths,
    state: &ComponentState,
) -> Result<()> {
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

pub(crate) fn clear_state(component: ComponentKind, paths: &Paths) -> Result<()> {
    let path = paths.state_path(component);
    match fs::remove_file(&path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

pub(crate) fn print_component_status(paths: &Paths, component: ComponentKind) -> Result<()> {
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
