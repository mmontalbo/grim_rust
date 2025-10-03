# Work in Progress

## Current Direction
- Pivoted away from patching the original executable; the repository now
  documents the shipping boot flow and builds a Rust analysis toolchain that can
  reason about the Lua sources directly.
- Decomposed legacy Lua quirks so `full_moon` can parse every decompiled file
  without fallback string hacks.

## Active Threads
- `grim_analysis` parses `_system`, `_sets`, and every room script, building an
  aggregate of set hooks and the actors they spawn.
- Static simulator groups stateful calls by subsystem (objects, inventory,
  interest actors, actors, audio, progression) so we can see which engine
  services the Lua scripts expect.
- JSON report (`--json-report`) persists hook simulations plus any unclassified
  method calls, helping us track coverage gaps over time.
- Registry shim reads/writes JSON snapshots so repeated runs mimic the original
  engine's registry mutations (e.g., `GrimLastSet`).
- `grim_formats` now exposes a reusable `LabArchive` reader and `lab_dump`
  example so we can inspect LAB contents without shell scripts.
- `grim_engine` consumes the shared analysis layer, materialises the stage-aware
  boot timeline, and produces an `EngineState` snapshot plus optional JSON
  exports.
- `grim_viewer` uses the asset manifest to stand up a wgpu preview window, ready
  to swap in real BM decoding once the loader lands.

## Next Steps
1. Keep widening the legacy normalisation pass (additional helper keywords,
   comment forms) so parsing never regresses.
2. Expand unit tests for both the simulator and the LAB parser so regressions
   surface quickly.
3. Expand the host prototype and viewer together: persist boot manifests,
   expose per-subsystem deltas, decode a real Manny's Office asset, and outline
   the services (script scheduler, cutscene playback, save/load) required for a
   full Rust runtime.
