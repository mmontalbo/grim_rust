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
- Geometry verification test: `cargo test -p grim_engine` exercises `verify_geometry_round_trip_matches_static_timeline` to ensure the runtime snapshot stays aligned with the static geometry timeline.
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
  comparison against the static analysis. `_colors`, `_controls`, and the
  music/SFX helpers now install stateful scaffolds that record the current cue,
  queued transitions, mute state, and active sound handles, while the richer
  menu helpers remain stubbed so verbose runs still highlight the next bindings
  we need to land.
  `start_script`/`single_start_script` now spawn cooperative Lua threads,
  `break_here` yields through `coroutine.yield`, and the host advances a few
  frames post-BOOT so long-running trackers stay resident with their yield
  counts for future bindings.

## Next Steps
1. Wire the `--verify-geometry` flow (now exercised by `cargo test -p grim_engine`) into downstream tooling/CI so sector or visibility drift surfaces automatically outside local runs.
2. Keep widening the legacy normalisation pass (additional helper keywords,
   comment forms) so parsing never regresses.
3. Replace the Manny-specific camera fallback with geometry-driven selection and extend the
   parser/lookup path to other sets once their data is decoded, using the snapshot diff to keep
   sector coverage honest.
4. Push the geometry-backed state into the remaining runtime helpers (menu services and the deeper audio routines) so later scenes can react without relying on placeholder logging.


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
- Sector toggles: host-side `MakeSectorActive` updates the LAB-derived sector map,
  so door passages and scripted hotspots enable/disable the same walk/camera
  polygons the original runtime used. Runtime summaries now highlight overrides
  whenever scripts diverge from the set file defaults, `GetVisibleThings` now
  omits hotspots when their covering sectors are inactive, and the commentary/cut-scene helpers
  consume the same state so `SetActiveCommentary` auto-suspends/resumes and the cut-scene ledger
  reports when geometry overrides block or unblock a sequence.
- Geometry snapshot: `--lua-geometry-json <file>` captures the embedded runtime's
  geometry/visibility state (active set/setup, sector activation, actors/objects, hotlists, commentary/cut-scene ledgers, and the event log). Other tools can now consume that JSON directly instead of scraping verbose runs.
- Geometry diff: `grim_engine --geometry-diff <snapshot.json>` compares those runtime snapshots
  against the static timeline's recorded `MakeSectorActive` calls, flagging sectors that don't match,
  unresolved toggles, and polygons referenced by scripts but missing from the LAB geometry. It now
  also recomputes each visible object's range, distance, and bearing from the LAB geometry, so head-
  control drift shows up alongside sector mismatches when Manny's hotspots move off their scripted
  placements.
  Add `--geometry-diff-json <report.json>` to write the same findings as JSON so CI or external tools can archive the sector and visibility mismatches. Run `grim_engine --verify-geometry` to capture a fresh runtime snapshot and diff it in one step, exiting non-zero when the sector or visibility data drifts.
- Scheduler polish: function-based threads now carry source-derived labels
  (e.g., `_system.decompiled.lua:667` for `TrackManny`), and the host seeds
  Manny's set scaffolding (`setups`, `current_setup`, `cameraman`) plus engine
  helpers (`SetActorConstrain`, `GetVisibleThings`) so those trackers yield
  cleanly while we layer in real visibility/camera behaviour.
- Sector tracker: `Actor:find_sector_type`/`find_sector_name` now
  derive Manny's camera/hot/walk selections from his live position, emitting
  zone-specific setup names instead of the old canned responses. Requests are
  still logged for diffing, the runtime summary surfaces the latest sectors,
  and we added unit tests that cover both the legacy desk/door heuristics and the
  new geometry-backed walk lookups.
- Set hook: wrap `Set.create` at runtime so every set table inherits the stock
  methods. We now re-export the legacy helpers (`MakeCurrentSet`,
  `MakeCurrentSetup`, `GetCurrentSetup`, `rebuildButtons`, `NewObjectState`,
  `SendObjectToFront`, `SetActiveCommentary`) alongside script introspection
  shims (`next_script`, `identify_script`, `FunctionName`), so `_system` finishes
  `FINALIZEBOOT` and Manny's Office trackers run on real tables. Interest-actor
  positions now flow back into `GetAngleBetweenActors`, and the host mirrors
  `Actor:set_visibility` toggles so `GetVisibleThings` returns the same objects
  Lua marks as visible. Manny-to-object bearings now log real angles, and the
  runtime loads Manny's Office walk/camera sectors by parsing the shipping
  `mo.set` through `grim_formats::set`. Walk lookups now use the parsed polygons
  while camera/hot queries map through setup interest points, and both head-control
  and the cut-scene/commentary scaffolding now consume that geometry-backed state instead of
  falling back to placeholder logging.
- Visibility sweeps now record per-object distance, bearing, range hits, and the
  derived hotlist so the runtime summary mirrors what Head_Control evaluates each frame.
- Costume/dialogue plumbing: the embedded host now tracks each actor's base
  and active costume, surfaces `Actor:get_costume`, respects `Actor:complete_chore`,
  and routes `Actor:normal_say_line` through `system.lastActorTalking` while logging
  the line so cut-scene monitors see real wardrobe and speaker context.
- Cut-scene ledger: the host now keeps a stack of active cut scenes and override
  handlers, wrapping `START_CUT_SCENE`/`END_CUT_SCENE`, `set_override`,
  `kill_override`, and both global and actor `wait_for_message` calls.
  The instrumentation logs every transition, clears the speaking actor when dialogs
  finish, and ensures `IsMessageGoing` mirrors the embedded ledger instead of
  staying false.
- Choreography helpers: the host now implements `Actor:play_chore`,
  `push_costume`/`pop_costume`, `set_walk_chore`, `set_talk_chore`,
  `set_mumble_chore`, head look controls, and collision toggles so Manny's
  office scripts mutate the same stacks and flags the original runtime exposed.
  The event log captures every chore/costume swap while the actor tables mirror
  the new fields (`walk_chore`, `talk_drop_chore`, `ignoring_boxes`, etc.) for
  downstream Lua helpers.
- Achievement scaffold: `_achievement.lua` now resolves to a host-provided
  table that remembers eligibility toggles (e.g., `ACHIEVE_CLASSIC_DRIVER`) and
  reports them back to Lua, letting Manny's Office fall back to the classic
  driver checks without waiting on Steam platform bindings. Future platform
  bridges can layer on top of this stub once real services are available.
