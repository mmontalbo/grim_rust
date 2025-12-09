# Grim Fandango Rust Study

Research workspace for recreating Grim Fandango's scripting runtime in Rust. We
keep copies of the original game assets locally so we can study the retail boot
flow and prototype modern tooling around it.

## Quick Start
- Use `grctl retail copy` to copy your Grim Fandango Remastered install into the
  local `dev-install/` directory (or copy it manually if you prefer). The
  development shell exports `GRIM_INSTALL_PATH=dev-install` for tools that still
  read that variable.
- Enter the development shell with `nix-shell` (Rust toolchain, Lua, ripgrep,
  etc. are provisioned there).
- Populate extracted assets with `tools/sync_assets.sh [dest] [-- lab_extract
  flags...]`. The default destination is `extracted/`, which downstream crates
  read from automatically.
- Treat `dev-install/` and `extracted/` as read-only reference data; do not edit
  or commit changes to those directories.

## Repository Layout
- `grim_analysis/` – retail telemetry shim (`grim_analysis`); see
  `grim_analysis/README.rust_shim.md`.
- `grim_engine/` – prototype runtime host; see `grim_engine/README.md`.
- `grim_formats/` – asset format helpers and CLIs; see `grim_formats/README.md`.
- `docs/` – reference notes for current tooling.
- `tools/` – repo-level utilities such as `tools/sync_assets.sh` for asset
  preparation and commit helpers.

## Development
- Run workspace checks from inside `nix-shell`: `cargo fmt` and `cargo test` (or
  crate-specific commands) keep the tree tidy.
- Run `tools/install_git_hooks.sh` once to install the shared `commit-msg` hook;
  it calls `tools/lint_commit.py` so commits without Why/What bullets are
  rejected instead of slipping into history.
- When using `tools/format_commit.py`, run `git commit -F .git/COMMIT_EDITMSG`
  (or set `GIT_EDITOR=true`) so Git skips launching an interactive editor inside
  the CLI harness.
- If the commit subject/Why/What spacing trips you up, run
  `git config commit.template tools/commit_template.txt` once—the template
  pre-populates the blank line and bullet blocks so you only fill in the text.
- Component-specific workflows and deep dives live in each crate's README; start
  there when modifying a particular subsystem.

## License
- Distributed under the terms of the GNU General Public License, version 2.0 or
  (at your option) any later version. See `COPYING` for the full text and
  retain upstream notices when importing reference implementations.
