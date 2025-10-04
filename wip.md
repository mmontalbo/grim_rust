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
- Embedded Lua host: `grim_engine --run-lua` now boots an `mlua` VM backed by a
  shared `EngineContext`, letting the stock `_actors.lua` and `_objects.lua`
  scripts execute so Manny's Office uses the real object tables. Actor selection,
  set switches, object state mutations, and inventory changes are logged for
  comparison against the static analysis. `_colors`, `_sfx`, and `_controls`
  now install host-provided scaffolds, while the richer menu helpers remain
  stubbed so verbose runs still highlight the next bindings we need to land.
  `start_script`/`single_start_script` now spawn cooperative Lua threads,
  `break_here` yields through `coroutine.yield`, and the host advances a few
  frames post-BOOT so long-running trackers stay resident with their yield
  counts for future bindings.

## Next Steps
1. Flesh out the boot-time trackers (visibility queries, sector lookups,
   menu helpers, cut-scene services, control handlers) inside the coroutine
   host so the embedded runtime can march further into Manny's Office. With
   camera/hot sector queries faked in Rust, the next blockers are wiring
   `Head_Control`/dialog state and feeding real visibility data back into the
   loop so the trackers stop idling.
2. Feed the new marker overlay data back into `grim_engine` (e.g., emit a
   machine-readable placement log) so other tooling can validate set geometry
   without parsing console output.
3. Keep widening the legacy normalisation pass (additional helper keywords,
   comment forms) so parsing never regresses.
4. Correlate the captured Manny/object transforms with the head-control scripts so we
   can start replacing the canned sector heuristics with real geometry data. That means
   diffing runtime bearings against the static analysis timeline and planning how to surface
   visibility/collision metadata next.


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
- Runtime spike: intercept `_actors.lua`, `_objects.lua`, `_dialog.lua`,
  `_music.lua`, `_mouse.lua`, `_ui.lua`, the menu helpers, and the inventory
  variants inside the embedded host so Manny boots with the real tables; log
  actor/object/inventory/inventory-room events to map the dialog, music, UI,
  and inventory services we still need to implement for real gameplay. Cooperative
  threads keep the long-running Manny trackers alive so future bindings can
  observe their loops.
- Scheduler polish: function-based threads now carry source-derived labels
  (e.g., `_system.decompiled.lua:667` for `TrackManny`), and the host seeds
  Manny's set scaffolding (`setups`, `current_setup`, `cameraman`) plus engine
  helpers (`SetActorConstrain`, `GetVisibleThings`) so those trackers yield
  cleanly while we layer in real visibility/camera behaviour.
- Sector tracker: `Actor:find_sector_type`/`find_sector_name` now
  return canned camera and hotspot hits for Manny, record the requested kinds
  in the event log, and surface the latest sectors in the runtime summary so
  `FINALIZEBOOT` can pick `mo_mcecu` without the original geometry tables.
- Set hook: wrap `Set.create` at runtime so every set table inherits the stock
  methods. We now re-export the legacy helpers (`MakeCurrentSet`,
  `MakeCurrentSetup`, `GetCurrentSetup`, `rebuildButtons`, `NewObjectState`,
  `SendObjectToFront`, `SetActiveCommentary`) alongside script introspection
  shims (`next_script`, `identify_script`, `FunctionName`), so `_system` finishes
  `FINALIZEBOOT` and Manny's Office trackers run on real tables. Interest-actor
  positions now flow back into `GetAngleBetweenActors`, yielding real Manny-to-object
  bearings; next up is feeding those transforms into the visibility and cut-scene
  helpers that still rely on canned sector hits.
