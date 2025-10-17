# Grim Fandango Rust Study

Research workspace for recreating Grim Fandango's scripting runtime in Rust. We
keep copies of the original game assets locally so we can study the retail boot
flow and prototype modern tooling around it.

## Quick Start
- Copy or symlink your Grim Fandango Remastered install into the local
  `dev-install/` directory. The development shell exports
  `GRIM_INSTALL_PATH=dev-install` for tools that still read that variable.
- Enter the development shell with `nix-shell` (Rust toolchain, Lua, ripgrep,
  etc. are provisioned there).
- Populate extracted assets with `tools/sync_assets.sh [dest] [-- lab_extract
  flags...]`. The default destination is `extracted/`, which downstream crates
  read from automatically.

## Repository Layout
- `grim_analysis/` – static boot-flow analysis; details in
  `grim_analysis/README.md`.
- `grim_engine/` – prototype runtime host; see `grim_engine/README.md`.
- `grim_formats/` – asset format helpers and CLIs; see `grim_formats/README.md`.
- `grim_viewer/` – visual tooling built on the extracted data; see
  `grim_viewer/README.md`.
- `docs/` – reference material:
  - `docs/runtime_smoke_tests.md` now tracks the trimmed intro playback scope
    and links to historical runtime demo notes.
  - `docs/live_streaming_pipeline.md` records the current live-preview stack and
    the pieces that were intentionally removed.
- `tools/` – repo-level utilities such as `tools/wip_summary.py` for the current
  project plan and `tools/sync_assets.sh` for asset preparation.

## Development
- Run workspace checks from inside `nix-shell`: `cargo fmt` and `cargo test` (or
  crate-specific commands) keep the tree tidy.
- Run `tools/wip_summary.py` to review the current project focus before diving
  into a new task. When priorities shift, update the milestone sections in
  `tools/wip_summary.py` so the next contributor sees the Manny office goal
  without chasing context in the commit log.
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
