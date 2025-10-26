use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

#[cfg(unix)]
use std::os::unix::fs as unix_fs;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookMode {
    Instrumented,
    Vanilla,
}

#[derive(Clone, Debug)]
pub struct RetailLayout {
    dev_install: PathBuf,
    steam_install: PathBuf,
    steam_root: Option<PathBuf>,
    telemetry_source: PathBuf,
    telemetry_dest: PathBuf,
    telemetry_backup: PathBuf,
    rust_shim_workspace_release: PathBuf,
    rust_shim_workspace_debug: PathBuf,
    rust_shim_local_release: PathBuf,
    rust_shim_local_debug: PathBuf,
}

#[derive(Clone, Debug)]
pub struct InstrumentationStatus {
    pub telemetry_exists: bool,
    pub telemetry_linked: bool,
    pub telemetry_backup_exists: bool,
    pub shim_available: bool,
}

impl RetailLayout {
    pub fn new(repo_root: &Path) -> Result<Self> {
        let dev_install = repo_root.join("dev-install");
        let steam_root = detect_steam_root_path().ok();
        let telemetry_source = repo_root
            .join("grim_analysis")
            .join("retail_capture")
            .join("telemetry.lua");
        let telemetry_dest = dev_install.join("mods").join("telemetry.lua");
        let telemetry_backup = dev_install.join("mods").join("telemetry.lua.retail");
        let shim_name = "libgrim_telemetry_shim.so";
        let rust_workspace_target = repo_root.join("target").join("i686-unknown-linux-gnu");
        let rust_local_target = repo_root
            .join("grim_analysis")
            .join("retail_capture")
            .join("rust_shim")
            .join("target")
            .join("i686-unknown-linux-gnu");
        let rust_shim_workspace_release = rust_workspace_target.join("release").join(shim_name);
        let rust_shim_workspace_debug = rust_workspace_target.join("debug").join(shim_name);
        let rust_shim_local_release = rust_local_target.join("release").join(shim_name);
        let rust_shim_local_debug = rust_local_target.join("debug").join(shim_name);
        let steam_install = detect_steam_install_path()?;
        Ok(Self {
            dev_install,
            steam_install,
            steam_root,
            telemetry_source,
            telemetry_dest,
            telemetry_backup,
            rust_shim_workspace_release,
            rust_shim_workspace_debug,
            rust_shim_local_release,
            rust_shim_local_debug,
        })
    }

    pub fn dev_install(&self) -> &Path {
        &self.dev_install
    }

    pub fn steam_root(&self) -> Option<&Path> {
        self.steam_root.as_deref()
    }

    pub fn resolved_shim_path(&self) -> Option<PathBuf> {
        let candidates = [
            &self.rust_shim_workspace_release,
            &self.rust_shim_local_release,
            &self.rust_shim_workspace_debug,
            &self.rust_shim_local_debug,
        ];
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate.clone());
            }
        }
        None
    }

    pub fn preferred_shim_path(&self) -> &Path {
        &self.rust_shim_workspace_release
    }

    pub fn ensure_dev_install_exists(&self) -> Result<()> {
        if self.dev_install.exists() {
            return Ok(());
        }
        bail!(
            "dev-install directory missing at {}; run 'grctl retail copy' first",
            self.dev_install.display()
        );
    }

    pub fn instrumentation_status(&self) -> Result<InstrumentationStatus> {
        let telemetry_exists = self.telemetry_dest.exists();
        let telemetry_linked = read_link_normalized(&self.telemetry_dest)?
            .map(|target| target == self.telemetry_source)
            .unwrap_or(false);
        let telemetry_backup_exists = self.telemetry_backup.exists();
        let shim_available = self.resolved_shim_path().is_some();
        Ok(InstrumentationStatus {
            telemetry_exists,
            telemetry_linked,
            telemetry_backup_exists,
            shim_available,
        })
    }

    pub fn apply_mode(&self, mode: HookMode) -> Result<InstrumentationStatus> {
        match mode {
            HookMode::Instrumented => self.install_hooks()?,
            HookMode::Vanilla => self.remove_hooks()?,
        }
        self.instrumentation_status()
    }

    pub fn sync_from(&self, source_override: Option<&Path>, force: bool) -> Result<PathBuf> {
        let source = match source_override {
            Some(path) => path.to_path_buf(),
            None => self.steam_install.clone(),
        };
        if !source.exists() {
            bail!("steam install not found at {}", source.display());
        }
        if source == self.dev_install {
            bail!("source and destination paths match; aborting");
        }

        if self.dev_install.exists() {
            if !force {
                bail!(
                    "{} already exists; re-run with --force to overwrite",
                    self.dev_install.display()
                );
            }
            fs::remove_dir_all(&self.dev_install)
                .with_context(|| format!("removing {}", self.dev_install.display()))?;
        }
        copy_tree(&source, &self.dev_install)?;
        Ok(self.dev_install.clone())
    }

    pub fn telemetry_dest(&self) -> &Path {
        &self.telemetry_dest
    }

    pub fn steam_ld_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let Some(root) = &self.steam_root else {
            return paths;
        };
        let ubuntu32 = root.join("ubuntu12_32");
        push_unique_if_exists(&mut paths, ubuntu32.clone());
        push_unique_if_exists(&mut paths, ubuntu32.join("pinned_libs_32"));
        push_unique_if_exists(&mut paths, root.join("linux32"));
        if let Some(runtime) = self.steam_runtime_dir() {
            let candidates = [
                runtime.clone(),
                runtime.join("pinned_libs_32"),
                runtime.join("lib"),
                runtime.join("lib/i386-linux-gnu"),
                runtime.join("usr/lib"),
                runtime.join("usr/lib/i386-linux-gnu"),
            ];
            for candidate in candidates {
                push_unique_if_exists(&mut paths, candidate);
            }
        }
        paths
    }

    pub fn steam_runtime_dir(&self) -> Option<PathBuf> {
        let root = self.steam_root.as_ref()?;
        let runtime = root.join("ubuntu12_32").join("steam-runtime");
        if runtime.exists() {
            Some(runtime)
        } else {
            None
        }
    }

    pub fn steamclient32_path(&self) -> Option<PathBuf> {
        let root = self.steam_root.as_ref()?;
        let candidates = [
            root.join("ubuntu12_32").join("steamclient.so"),
            root.join("linux32").join("steamclient.so"),
        ];
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    fn install_hooks(&self) -> Result<()> {
        if !self.telemetry_source.exists() {
            bail!(
                "telemetry source missing at {}; build grim_analysis first?",
                self.telemetry_source.display()
            );
        }
        let mods_dir = self.telemetry_dest.parent().unwrap();
        fs::create_dir_all(mods_dir)
            .with_context(|| format!("creating mods dir {}", mods_dir.display()))?;

        if self.telemetry_dest.exists()
            && !self.telemetry_dest.is_symlink()
            && !self.telemetry_backup.exists()
        {
            fs::copy(&self.telemetry_dest, &self.telemetry_backup).with_context(|| {
                format!(
                    "backing up telemetry script to {}",
                    self.telemetry_backup.display()
                )
            })?;
        }

        replace_with_symlink(&self.telemetry_source, &self.telemetry_dest)?;
        Ok(())
    }

    fn remove_hooks(&self) -> Result<()> {
        let mods_dir = self.telemetry_dest.parent().unwrap();
        fs::create_dir_all(mods_dir)
            .with_context(|| format!("ensuring mods dir {}", mods_dir.display()))?;
        if self.telemetry_backup.exists() {
            fs::copy(&self.telemetry_backup, &self.telemetry_dest).with_context(|| {
                format!(
                    "restoring telemetry script from {}",
                    self.telemetry_backup.display()
                )
            })?;
        } else if self.telemetry_dest.is_symlink() || self.telemetry_dest.exists() {
            fs::remove_file(&self.telemetry_dest)
                .with_context(|| format!("removing {}", self.telemetry_dest.display()))?;
        }
        Ok(())
    }
}

fn detect_steam_install_path() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("GRIM_STEAM_INSTALL") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Ok(path);
        }
    }
    let home = std::env::var("HOME").context("HOME not set; cannot derive steam path")?;
    let default = Path::new(&home)
        .join(".steam")
        .join("steam")
        .join("steamapps")
        .join("common")
        .join("Grim Fandango Remastered");
    Ok(default)
}

fn detect_steam_root_path() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("GRIM_STEAM_ROOT") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Ok(path);
        }
    }
    let home = std::env::var("HOME").context("HOME not set; cannot derive steam root")?;
    let home_dir = Path::new(&home);
    let candidates = [
        home_dir.join(".local/share/Steam"),
        home_dir.join(".steam/steam"),
        home_dir.join(".steam/root"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("steam root not found (set $GRIM_STEAM_ROOT to override)");
}

fn push_unique_if_exists(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if candidate.exists() && !paths.iter().any(|existing| existing == &candidate) {
        paths.push(candidate);
    }
}

fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let rel = match entry.path().strip_prefix(source) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("creating directory {}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
        if entry.file_type().is_symlink() {
            let link_target = fs::read_link(entry.path())
                .with_context(|| format!("reading symlink {}", entry.path().display()))?;
            replace_with_symlink(&link_target, &target)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| {
                format!("copying {} to {}", entry.path().display(), target.display())
            })?;
        }
    }
    Ok(())
}

fn replace_with_symlink(source: &Path, dest: &Path) -> Result<()> {
    if dest.exists() || dest.is_symlink() {
        fs::remove_file(dest).with_context(|| format!("removing {}", dest.display()))?;
    }
    #[cfg(unix)]
    {
        unix_fs::symlink(source, dest)
            .with_context(|| format!("linking {} -> {}", dest.display(), source.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::copy(source, dest)
            .with_context(|| format!("copying {} to {}", source.display(), dest.display()))?;
        Ok(())
    }
}

fn read_link_normalized(path: &Path) -> Result<Option<PathBuf>> {
    match fs::read_link(path) {
        Ok(target) => {
            if target.is_absolute() {
                Ok(Some(target))
            } else {
                let resolved = path
                    .parent()
                    .map(|parent| parent.join(&target))
                    .unwrap_or(target);
                Ok(Some(resolved))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading link {}", path.display())),
    }
}

pub fn extend_env_var(current: Option<OsString>, prefix: &str) -> OsString {
    let mut value = OsString::from(prefix);
    if let Some(existing) = current {
        if !existing.is_empty() {
            value.push(":");
            value.push(existing);
        }
    }
    value
}

pub fn warn_if_shaders_missing(layout: &RetailLayout) {
    let compiled_dir = layout
        .dev_install()
        .join("x86")
        .join("shaders")
        .join("compiled");
    let sentinel = compiled_dir.join("deferred_light_v.glsl");
    if sentinel.exists() {
        return;
    }
    eprintln!(
        "[grctl] warning: compiled shaders missing under {}; run the shader pack script before launching retail",
        compiled_dir.display()
    );
}
