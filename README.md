# Grim Fandango Rust Study

Research workspace for a Rust reimplementation of Grim Fandango's scripting
runtime. We keep the original assets (LAB archives, Lua sources, movies, audio)
on disk so we can study how the shipped engine boots and then recreate that
behaviour in Rust step by step.

## Repository Layout
- `docs/startup_overview.md` – notes on the retail boot/new-game flow.
- `extracted/` – decompiled Lua scripts and supporting metadata pulled from
  `DATA000.LAB` / `IMAGES.LAB` for inspection.
- `dev-install/` – optional reference copy of the shipping game; useful for
  comparing behaviour but no longer managed by scripts in this repo.
- `grim_analysis/` – Rust crate that parses the decompiled Lua, normalises
  legacy syntax, and reports on the boot flow.
- `grim_formats/` – low-level format helpers (currently includes the LAB
  archive reader and utilities).

## Development Environment
Enter the Nix shell to get the required tooling (Rust, scummvm-tools, Lua 5.1,
Python, ripgrep, etc.):

```bash
nix-shell
```

If you still keep a local install of the retail game around for comparison, set
`GRIM_INSTALL_PATH` before launching `nix-shell`; the shell will echo the path
so it is obvious which install is being referenced.

## Boot Prototype CLI
`grim_analysis` focuses on modelling the Lua boot flow. Run it with Cargo (paths
are relative to the crate directory, so we point at `../extracted/DATA000` by
default):

```bash
cargo run --manifest-path grim_analysis/Cargo.toml
```

Key flags:
- `--data-root <path>` – override the path to the extracted Lua bundle.
- `--registry <file>` – provide a JSON snippet with registry keys such as
  `LastSavedGame` or `good_times` to explore alternate boot paths. When this
  flag is present the tool persists mutations (e.g. `GrimLastSet` on fresh
  boots) back into the same JSON file so repeated runs observe the same state the
  real engine would record.
- `--resume-save` – simulate the engine instructing Lua to resume
  `LastSavedGame` instead of starting fresh.
- `--json-report <file>` – write a machine-readable summary of every parsed set
  hook, including their static simulations and an aggregate of unclassified
  method targets.

The CLI prints the derived boot stages, selected default set, and tallies of the
Lua scripts that `source_all_set_files()` would load. It surfaces the first few
`Set:create` and `Actor:create` sites by parsing the decompiled Lua with
`full_moon`, giving a quick view of where rooms live and which global actors
register during boot. To cope with the engine’s legacy Lua 3 syntax, files are
normalised on the fly (e.g., stripping `%` upvalue markers, renaming the legacy
helper named `in`) before being fed to the parser so metadata stays AST-backed
rather than relying on brittle string parsing.

For each set we assemble a "runtime hook" view that categorises `enter`, `exit`,
`camerachange`, and every `set_up_*` function. We run a lightweight static
simulation that notes which actors the hooks spawn via `Actor:create` and calls
out stateful interactions grouped by subsystem—object state toggles,
interest-actor chore loops, inventory mutations, actor pose adjustments,
ambient-audio triggers, and achievement/progression writes show up in their own
buckets. This creates a reproducible summary of room setup behaviour without
launching the original executable.

The crate exposes its metadata, the JSON report builder, and a `BootTimeline`
helper as public APIs so future host binaries can reuse the analysis without
depending on this CLI. The timeline assembles boot stages together with the
default set's hook order and their simulated side-effects, giving downstream
tools a predictable structure instead of raw terminal output. The JSON report
also records any method calls that do not yet match a known subsystem so we can
expand classification coverage over time.

Registry snapshots are simple JSON documents; keeping them in a single format
avoids extra dependencies and makes diffs easy to read in version control.

## LAB Format Helpers
`grim_formats` supplies a reusable `LabArchive` reader plus a tiny CLI for quick
inspection. Open a LAB and print its table of contents with:

```bash
cargo run -p grim_formats --bin lab_dump -- dev-install/DATA000.LAB | head
```

The `LabArchive` API lets other tools map entries, stream individual assets, or
extract them to disk. Unit tests cover the parser with synthetic archives so we
notice regressions before pointing at the real game data.

## Testing
Unit coverage lives in the analysis crate. Run the suite with:

```bash
cargo test --manifest-path grim_analysis/Cargo.toml
```

## Current Focus
1. Harden the legacy-Lua normaliser so additional constructs keep parsing under
   `full_moon`.
2. Expand the static simulator with more subsystem coverage and regression
   tests.
3. Outline the services (cutscene player, script scheduler, registry) that a
   future Rust runtime must provide so the Lua boot sequence can execute without
   the original binary.
