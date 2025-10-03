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
  boot timeline, and now exports per-subsystem deltas alongside an ordered list
  of subsystem delta events so runtime services can replay mutations without
  re-parsing Lua.
- Asset manifests now tag bitmap entries as `classic` vs `unsupported` so the
  tooling can skip remastered-only surfaces until the codec 3 decompressor
  exists.
- `grim_viewer` consumes the manifest, decodes codec 0 BM surfaces with the
  shared loader, and only falls back to a hashed preview when metadata is
  missing (older manifests) while we finish reversing the remastered payloads.
- Added a lightweight script/movie scheduler in `grim_engine`; the CLI's
  `--simulate-scheduler` switch replays the boot queues using that iterator so
  we can reason about execution order without Lua.

## Next Steps
1. Keep widening the legacy normalisation pass (additional helper keywords,
   comment forms) so parsing never regresses.
2. Start mapping scheduler output into concrete runtime services (actor state
   machines, script stubs) so we can drive early cutscene playback in Rust.
3. Broaden the asset pipeline: add ZBM/geometry decoding, extend manifest
   metadata, and cover the new exporters with regression tests.

## Current Iteration — Manny's Office Prototype
- Objective: render an authentic Manny's Office background using the classic
  assets so `grim_viewer` shows something recognisable instead of a hashed
  preview.
- Asset coverage: extend the Manny's Office manifest to include the
  `mo_0`–`mo_6` camera surfaces (prefer the `.zbm` classic payloads and fall
  back to warning if only the remastered `codec 3` variants exist).
- Codec bridge: decode `codec 3` BM payloads (sliding-window LZ with a seeded
  dictionary) so the Manny's Office camera plates render from the LAB archives
  without relying on pre-baked PNGs. Seed subsequent animation frames with the
  previous frame's pixels to satisfy the differential encodes used by overlays.
- Timeline link: read the default `mo_mcecu` setup selection from the boot
  timeline so the viewer knows which background to load first without hard
  coding the index.
- Viewer spike: add a simple full-screen quad render path that blits the decoded
  background while we work toward real room geometry.
