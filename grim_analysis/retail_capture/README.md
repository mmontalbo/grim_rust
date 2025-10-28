# Retail Telemetry & Shim

This directory keeps the artifacts that let the retail executable stream its
runtime state into our analysis pipeline. The code lives next to
`grim_analysis` so capture tooling, format specs, and the runtime consumers stay
in one place.

## Components

- `telemetry.lua` is the Lua 3.1 script injected into the shipping VM. It keeps
  the API surface tiny (`telemetry.mark`, `telemetry.event`, `telemetry.flush`,
  `telemetry.reset`) and rebuilds legacy helpers so it survives the stripped-down
  retail environment.
  The retail executable always loads `_system.lua` first, so the shim injects
  `telemetry.lua` the moment that file executes; all other retail scripts
  (`_cut_scenes.lua`, `year_1.lua`, set loaders, etc.) flow through `_system`.
  - **Boot-safe IO + string shims** – redefines `openfile`, `write`, `call`,
    `unpack`, and string primitives when the retail build omits them. If the
    native shim exposes `telemetry_native_write`, it piggybacks on that for
    writes.
  - **Coverage tracking** – `telemetry.mark(key)` bumps `coverage_counts[key]`,
    periodically rewrites `mods/telemetry_coverage.json`, and `telemetry.flush()`
    forces a final write so `grim_analysis --coverage-counts` can diff runs.
  - **Event stream** – `telemetry.event(label, fields)` records JSON objects to
    `mods/telemetry_events.jsonl` with monotonically increasing `seq`, filtered
    fields, and timestamps so downstream tools can tail retail behavior.
  - **Intro instrumentation** – monkey-patches `cut_scene` movies,
    `RunFullscreenMovie`, `StartMovie`, `wait_for_movie`, `start_script`,
    `wait_for_script`, and Manny’s `Actor.say_line` call to emit labeled
    `intro.timeline` events that reconstruct the logos → intro → office flow.
  - **Load tracing + installers** – wraps `dofile` so every include is logged to
    `mods/telemetry.loads.log`, and installs the intro hooks when
    `_cut_scenes.lua` or `year_1.lua` loads. Additional installers can hook new
    APIs by swapping functions and calling `telemetry.event` or `telemetry.mark`
    inside the replacements.
  - **Failure handling** – `telemetry_disable(reason)` swaps in inert stubs if a
    required primitive is missing, `_ERRORMESSAGE` is wrapped to write bootstrap
    failures to `mods/telemetry_bootstrap_error.log`, and `telemetry.reset()` wipes
    all logs so automated tests start from a known state.
  - **Compatibility note** – any helper Lua placed in `mods/` (including
    `telemetry_simple.lua`) must stay compatible with the game's Lua 3.x
    interpreter. Avoid Lua 5.x syntax sugar, metamethods, or library calls that
    were added after Lua 3.x.

- `rust_shim/` contains the new `LD_PRELOAD` hook written in Rust. The crate
  exports `lua_dofile`, resolves the retail engine’s real symbol, and (for now)
  logs `_system.lua` loads—the same choke point where we inject `telemetry.lua`.
  `grctl` builds the shim automatically via
  `cargo build -p grim_telemetry_shim --release --target i686-unknown-linux-gnu`
  and preloads the release `.so` before launching retail.

- `shim/` keeps the original C implementation. It still builds via `make` with
  `zig cc` (or any C compiler) and mirrors the legacy Lua 3.2 structs. We keep
  it around for reference while the Rust port catches up.

## Coverage workflow

1. Generate the state catalog and copy it (or just its `coverage.keys`) beside
   the retail install:
   ```bash
   cargo run -p grim_analysis -- --state-catalog-json artifacts/state_catalog.json
   ```
2. Place `telemetry.lua` in the game's `mods/` directory, preload the shim (or
   run `./grctl.sh retail hooks enable` which symlinks the file and ensures the
   Rust shim is built), and call `telemetry.mark("<catalog key>")` inside the
   retail scripts you want to observe. The helper rewrites
   `mods/telemetry_coverage.json` after every few marks (call `telemetry.flush()`
   before you exit to force a final write).
3. Run the analysis coverage check to identify gaps:
   ```bash
   cargo run -p grim_analysis -- \
      --coverage-counts mods/telemetry_coverage.json \
      --coverage-summary-json artifacts/coverage_report.json
   ```
   Missing keys point at catalog entries never hit by the retail run; unexpected
   keys indicate telemetry emitted IDs that are not yet part of the catalog.

### Building the shims

- **Rust shim (recommended)** – from the repo root, run
  `nix-shell --run 'cargo build -p grim_telemetry_shim --release --target i686-unknown-linux-gnu'`.
  `grctl retail start` does this automatically if the release artifact is
  missing. Preload `target/i686-unknown-linux-gnu/release/libgrim_telemetry_shim.so`
  when launching retail.

- **Legacy C shim** – run `make` in `shim/`. The default compiler is `zig cc`,
  but any toolchain that can emit an ELF shared object works. The `Makefile`
  auto-locates the Lua 3.2 headers provided by `shell.nix` (override
  `LUA32_PREFIX` if needed). Preload `shim/libgrim_lua_hook.so` before launching
  retail.
