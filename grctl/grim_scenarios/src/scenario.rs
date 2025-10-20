use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Shared context for scenario harnesses that need to launch grctl-managed components.
pub struct ScenarioContext {
    repo_root: PathBuf,
    grctl_bin: PathBuf,
    log_dir: PathBuf,
}

impl ScenarioContext {
    pub fn new() -> Result<Self> {
        let repo_root = locate_repo_root()?;
        let grctl_bin = find_grctl_bin(&repo_root)?;
        let log_dir = repo_root.join("target/grctl/logs");
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("creating log directory {}", log_dir.display()))?;
        Ok(Self {
            repo_root,
            grctl_bin,
            log_dir,
        })
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn grctl_bin(&self) -> &Path {
        &self.grctl_bin
    }

    pub fn log_path(&self, component: &str) -> PathBuf {
        self.log_dir.join(format!("{component}.log"))
    }

    pub fn reset_log(&self, component: &str) -> Result<()> {
        let path = self.log_path(component);
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("removing prior log before scenario: {}", path.display())
            })?;
        }
        Ok(())
    }
}

pub struct ManagedComponent {
    grctl_bin: PathBuf,
    repo_root: PathBuf,
    kind: &'static str,
    stopped: bool,
}

impl ManagedComponent {
    pub fn start(ctx: &ScenarioContext, kind: &'static str, args: &[String]) -> Result<Self> {
        let mut command = Command::new(ctx.grctl_bin());
        command.current_dir(ctx.repo_root());
        command.arg(kind);
        command.arg("start");
        for arg in args {
            command.arg(arg);
        }

        let status = command
            .status()
            .with_context(|| format!("launching grctl {kind} start"))?;
        if !status.success() {
            bail!("grctl {kind} start exited with status {status}");
        }

        Ok(Self {
            grctl_bin: ctx.grctl_bin().to_path_buf(),
            repo_root: ctx.repo_root().to_path_buf(),
            kind,
            stopped: false,
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let mut command = Command::new(&self.grctl_bin);
        command.current_dir(&self.repo_root);
        command.arg(self.kind);
        command.arg("stop");
        let status = command
            .status()
            .with_context(|| format!("stopping grctl {}", self.kind))?;
        if !status.success() {
            bail!("grctl {} stop exited with status {}", self.kind, status);
        }
        self.stopped = true;
        Ok(())
    }
}

impl Drop for ManagedComponent {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let _ = self.stop();
    }
}

pub struct LogTailer {
    reader: BufReader<File>,
}

impl LogTailer {
    pub fn open(path: &Path, deadline: Option<Instant>) -> Result<Self> {
        loop {
            match File::open(path) {
                Ok(file) => {
                    return Ok(Self {
                        reader: BufReader::new(file),
                    });
                }
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    if let Some(deadline) = deadline {
                        if Instant::now() >= deadline {
                            return Err(err).with_context(|| {
                                format!(
                                    "timed out waiting for log file to appear: {}",
                                    path.display()
                                )
                            });
                        }
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("opening log {}", path.display()));
                }
            }
        }
    }

    pub fn read_line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        match self.reader.read_line(&mut line)? {
            0 => Ok(None),
            _ => Ok(Some(line.trim_end_matches(&['\n', '\r'][..]).to_string())),
        }
    }
}

fn locate_repo_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir().context("determining current directory")?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("unable to locate repository root containing Cargo.toml");
        }
    }
}

fn find_grctl_bin(repo_root: &Path) -> Result<PathBuf> {
    if let Ok(path) = std::env::var("GRCTL_BIN") {
        let bin = PathBuf::from(path);
        if bin.exists() {
            return Ok(bin);
        }
    }

    let candidate = repo_root.join("target/debug/grctl");
    if candidate.exists() {
        return Ok(candidate);
    }

    bail!("GRCTL_BIN not set and target/debug/grctl not found; build grctl first")
}
