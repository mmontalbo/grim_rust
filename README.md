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
- `grim_engine/` – prototype host that consumes the shared analysis APIs.
- `grim_viewer/` – wgpu/winit spike that previews assets via manifests.

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

## Host Prototype
`grim_engine` exercises the exported analysis APIs without going through the CLI.
It loads the extracted data, runs the boot simulation, and uses the stage-aware
`BootTimeline` to print a concise summary of the opening set. Each hook reports
its boot-stage index, the actors it spawns, subsystems it touches, and any
cutscenes/scripts/movies it queues. Try it with:

```bash
cargo run -p grim_engine -- --data-root extracted/DATA000 --verbose
```

Use `--verbose` to emit every hook in Manny's office; omit it for a compact
overview that lists the first few entries and a roll-up of queued cutscenes,
helper scripts, and fullscreen movies. Pass `--timeline-json timeline.json` to
persist a prettified export that bundles both the boot stages and the resulting
`EngineState`, giving downstream tooling a single manifest with hook
simulations, prerequisites, and the post-boot snapshot (including per-subsystem
mutation deltas for actors, objects, inventory, etc.). Ordered
`subsystem_delta_events` record each method invocation alongside the hook index,
making it trivial for upcoming Rust runtime services to replay boot-time
mutations in sequence.

When pointed at a retail install (`--lab-root`), the host probes the LAB
archives and reports whether the Manny's Office assets we rely on are present.
`--asset-manifest <file>` writes that scan as JSON so other tools can reuse
offsets/sizes without rerunning lookups; bitmap entries now include codec,
frame count, and dimensions extracted by the shared decoder. Use
`--extract-assets <dir>` to copy the matching binaries into a workspace folder
for manual inspection.

Pass `--simulate-scheduler` to replay the boot-time script and movie queues in
order. The prototype scheduler reports the trigger hook for each entry, giving a
preview of the execution cadence the eventual Rust runtime must support. Pair it
with `--scheduler-json <file>` to persist the queues verbatim so downstream
tooling can consume the same ordering without scraping stdout. Snapshot fixtures
under `grim_engine/tests/fixtures` exercise both exports using the real Manny
bootstrap data, so schema changes trip a failing test before the CLI output
drifts out of sync with downstream consumers.

Experimental Lua execution is available via `--run-lua`. This spins up an
embedded `mlua` interpreter, wires a minimal `EngineContext` through the
existing boot analysis APIs, and records actor/set mutations as the scripts run.
Verbose runs (`--run-lua --verbose`) echo every loaded script plus the
engine-side events (actor selection, Manny's position/costume changes, inventory
mutations). The host currently skips legacy scaffolding such as
`setfallback.lua` and `_actors.lua` until the matching Rust services land, so
expect the mode to bail out once the menu boot scripts request functionality we
have not reimplemented yet. The summary still captures the state we do manage to
mutate—active set, queued scripts, Manny's transforms—so we can diff real Lua
behaviour against the static analysis when filling in the missing services.

## Viewer Spike
`grim_viewer` boots a wgpu surface on top of winit, consumes the JSON manifest
emitted by `grim_engine`, and reads assets straight from their LAB offsets to
decode and display BM surfaces (first frame, converted to RGBA). The shared
loader now handles both codec 0 and codec 3 payloads—including the Manny camera
plates and overlays such as `mo_door_open_comp.bm` and `mo_6_mnycu.zbm`—so the
classic backgrounds render correctly without pre-extracting PNGs.
If you pass `--timeline <boot_manifest.json>` the viewer will also ingest the
boot snapshot, enumerate the actors and objects staged during Manny's Office
boot, and let you cycle through them with the ←/→ keys. A lightweight overlay
maps each entity's X/Z position into normalized device coordinates and draws
coloured markers (green/blue for supporting casts, red for the current
selection), giving a quick sanity check that the static simulator and manifest
data line up with our spatial expectations while we work toward full room
geometry and rendering.
Add `--dump-frame <png>` to export the decoded bitmap to disk before the viewer
boots; the command prints basic luminance statistics alongside width/height so
you can confirm the codec 3 payloads expand into real imagery even on setups
where winit cannot create a window.
Pair it with `--dump-render <png>` (or the shorthand `--verify-render` when you
only need the comparison) to execute the full-screen quad through a headless
wgpu render target and diff the post-raster image against the decoder output.
The viewer reports per-quadrant mismatch ratios and will exit with a non-zero
status if the divergence exceeds the `--render-diff-threshold` (default 1%),
making it safe to wire into automated checks for viewport regressions.
Add `--headless` when you only need the decode + verification passes and want
to skip the winit window entirely (handy for CI or remote agents).
`tools/grim_viewer.py verify` wraps `cargo run` with the headless
configuration so automation can trigger a render check without remembering the
full CLI surface.
When you need the Steam runtime's GL/Vulkan stack (mirroring the old
`grim_mod launch` behaviour), build once and launch through `steam-run`:

```bash
cargo build -p grim_viewer
tools/grim_viewer.py verify --use-binary --steam-run --timeline artifacts/boot_timeline.json
```

The same command works over SSH on a GPU host, letting automation fire the
headless verification remotely.
Enable the optional `audio` feature to spin up a rodio output stream so the
audio plumbing is ready when we begin playing sounds. Run it with:

```bash
cargo run -p grim_viewer -- --manifest artifacts/manny_office_assets.json --asset mo_tube_balloon.zbm
```

## Testing
Unit coverage lives in the analysis crate. Run the suite with:

```bash
cargo test --manifest-path grim_analysis/Cargo.toml
```

The engine crate reuses the real Manny fixtures during `cargo test` so timeline
and scheduler schema changes are caught automatically.
`grim_viewer` also includes unit coverage for the render diff guard, so
`cargo test -p grim_viewer` continues to exercise the verification workflow even
when a GPU isn’t available.
For an end-to-end pixel diff from automation, run
```
tools/grim_viewer.py verify --use-binary --steam-run --timeline artifacts/boot_timeline.json
```

## Current Focus
1. Harden the legacy-Lua normaliser so additional constructs keep parsing under
   `full_moon`.
2. Expand the static simulator with more subsystem coverage and regression
   tests.
3. Outline the services (cutscene player, script scheduler, registry) that a
   future Rust runtime must provide so the Lua boot sequence can execute without
   the original binary.
