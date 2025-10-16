# Retail Telemetry & Shim

This directory keeps the artifacts that let the retail executable stream its
runtime state into our analysis pipeline. The code lives next to
`grim_analysis` so capture tooling, format specs, and the Rust consumers stay
in one place.

- `telemetry.lua` is the Lua script injected into the shipping VM. It now
  exports a tidy module composed of:
  - **Coverage helpers** – `telemetry.mark(key)` increments counters written to
    `mods/telemetry_coverage.json`; `telemetry.flush()` forces a save.
  - **Event helpers** – `telemetry.event(label, fields)` appends JSON lines to
    `mods/telemetry_events.jsonl`, which the coordinator or Rust host can tail.
  - **Installers** – runtime wrappers automatically hook `Set.create`,
    `Set.switch_to_set`, `source_all_set_files`, and boot functions. Add new
    hooks by registering another installer near the bottom of the file.
  Instrument retail paths by calling `telemetry.mark("catalog:key")` alongside
  `telemetry.event("set.enter", { set = "mo" })`. The shim aggregates counts so
  `grim_analysis --coverage-counts` can highlight missed resources.
- `shim/` contains the `LD_PRELOAD` hook (`lua_hook.c` + `Makefile`) that
  intercepts the engine's `lua_dofile` calls, injects `telemetry.lua` the first
  time `_system.lua` loads, and logs the hand-off for debugging.

## Coverage workflow

1. Generate the state catalog and copy it (or just its `coverage.keys`) beside
   the retail install:
   ```bash
   cargo run -p grim_analysis -- --state-catalog-json artifacts/state_catalog.json
   ```
2. Place `telemetry.lua` in the game's `mods/` directory, preload the shim, and
   call `telemetry.mark("<catalog key>")` inside the retail scripts you want to
   observe. The helper rewrites `mods/telemetry_coverage.json` after every few
   marks (call `telemetry.flush()` before you exit to force a final write).
3. Run the analysis coverage check to identify gaps:
   ```bash
   cargo run -p grim_analysis -- \
      --coverage-counts mods/telemetry_coverage.json \
      --coverage-summary-json artifacts/coverage_report.json
   ```
   Missing keys point at catalog entries never hit by the retail run; unexpected
   keys indicate telemetry emitted IDs that are not yet part of the catalog.

Build the shim by running `make` in the `shim/` directory (the default compiler
assumes `zig cc`, but any C toolchain that can produce a shared object will do).
Once the shared object is produced, preload it before launching the retail game
and drop `telemetry.lua` into the game's `mods/` directory so the shim can
execute it.
