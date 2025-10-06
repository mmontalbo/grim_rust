# Grim Fandango Rust Study

Research workspace for recreating Grim Fandango's scripting runtime in Rust. We
keep copies of the original game assets locally so we can study the retail boot
flow and prototype modern tooling around it.

## Quick Start
- Set `GRIM_INSTALL_PATH` to your Grim Fandango Remastered install so tooling
  can locate the LAB archives.
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
  - `docs/startup_overview.md` walks through the retail boot sequence.
  - `docs/runtime_smoke_tests.md` documents the Manny hotspot demo and the CLI
    loop we use for runtime smoke tests.
- `tools/` – repo-level utilities such as `tools/wip_summary.py` for the current
  project plan and `tools/sync_assets.sh` for asset preparation.

## Development
- Run workspace checks from inside `nix-shell`: `cargo fmt` and `cargo test` (or
  crate-specific commands) keep the tree tidy.
- Use `tools/wip_summary.py [--workstream SLUG]` to review the current project
  focus before diving into a new task. When you finish a thread, update the
  corresponding section in `tools/wip_summary.py` (usually by pruning the
  completed workstream and nudging the “Active Threads” / “Next Steps”) so the
  next contributor sees the new priority without having to chase context in the
  commit log.
- If the commit subject/Why/What spacing trips you up, run
  `git config commit.template tools/commit_template.txt` once—the template
  pre-populates the blank line and bullet blocks so you only fill in the text.
- Component-specific workflows and deep dives live in each crate's README; start
  there when modifying a particular subsystem.
