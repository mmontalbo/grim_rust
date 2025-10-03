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
  re-parsing Lua. The replay snapshot now tracks actor transforms (position,
  rotation, facing) plus basic chore state/history, and the timeline manifest
  fixtures cover the richer schema so downstream tools can rely on it.
- Asset manifests now tag bitmap entries as `classic` vs `unsupported` so the
  tooling can skip remastered-only surfaces until the codec 3 decompressor
  exists.
- `grim_viewer` consumes the manifest, decodes codec 0 BM surfaces with the
  shared loader, and only falls back to a hashed preview when metadata is
  missing (older manifests) while we finish reversing the remastered payloads.
  It can now ingest the boot timeline manifest, surface the actors staged during
  boot, and lets you cycle through them in the viewer (prepping for placements
  once geometry decoding lands).
- Added a lightweight script/movie scheduler in `grim_engine`; the CLI's
  `--simulate-scheduler` switch replays the boot queues using that iterator so
  we can reason about execution order without Lua, and `--scheduler-json`
  persists the exact boot queue order for downstream tooling.

## Next Steps
1. Feed the new marker overlay data back into `grim_engine` (e.g., emit a
   machine-readable placement log) so other tooling can validate set geometry
   without parsing console output.
2. Start mapping the ordered subsystem deltas into a reusable runtime service
   so we can replay boot mutations without re-simulating Lua (foundation for
   actor/object state machines).
3. Keep widening the legacy normalisation pass (additional helper keywords,
   comment forms) so parsing never regresses.

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
- PNG dump: `grim_viewer --dump-frame <path>` exports the decoded classic frame
  as a PNG and reports luminance coverage per quadrant so we can validate codec
  3 output even on machines where winit can't open a window or captures are
  needed for automated comparisons.
- Render verification: `grim_viewer --verify-render` runs the same fullscreen
  quad through a headless wgpu render target, diffing the output against the
  decoder result (use `--dump-render <path>` alongside it if you want the PNG).
  The CLI reports total/quadrant mismatch ratios and exits non-zero when they
  exceed `--render-diff-threshold` (default 1%), so diagonal clipping or
  viewport bugs surface automatically in scripts/CI.
- Timeline link: read the default `mo_mcecu` setup selection from the boot
  timeline so the viewer knows which background to load first without hard
  coding the index, and surface the boot-time actors in the viewer so we can
  start planning placements.
- Viewer spike: add a simple full-screen quad render path that blits the decoded
  background while we work toward real room geometry, and overlay actor
  placement markers derived from the new replay snapshot data.
- Fullscreen shader fix: correct the UV mapping on the blit triangle so Manny's
  Office backgrounds render at full scale, removing the lower-corner zoom seen
  earlier and aligning the PNG dump with the raw decoder output.
- Test hook: lightweight unit tests exercise the render-diff guard so
  `cargo test -p grim_viewer` keeps the verification threshold and failure
  messaging honest even on headless machines.
- Automation pass: `tools/grim_viewer.py verify --use-binary --steam-run`
  wraps the viewer in headless mode through `steam-run`, so CI (or this
  assistant) can diff decoded vs rendered frames without a real window while
  borrowing Steam's GPU runtime.
