# grim_viewer Module Guide

The viewer entry point (`main.rs`) now just wires together the subsystems that
actually load data, decode textures, and spin up the window/event loop. Use this
note as a map of the supporting modules plus the quickest way to exercise the
split without a desktop session.

## Module Map
- `cli.rs` handles argument parsing, including layout preset deserialisation and
  the flag defaults that match the Manny office regression captures.
- `texture.rs` is responsible for fetching bytes from the LAB archives,
  decoding colour/depth frames, and servicing `--dump-frame` PNG exports.
- `scene.rs` owns the timeline bootstrap: it reads the Lua geometry snapshot,
  movement trace, and hotspot event log, builds the entity catalogue, and keeps
  helper routines for CLI summaries.
- `audio.rs` tracks the optional audio overlay by tailing the runtime JSON log
  and exposing a watcher that the window thread can poll.
- `viewer.rs` hosts all wgpu state, overlay layout, and per-frame rendering for
  the windowed path; `ui_layout.rs`, `timeline.rs`, and `audio_log.rs` support
  the overlay formatting that `viewer.rs` consumes.

## Headless Regression Loop
1. Ensure the standard Manny fixtures exist (generated via `grim_engine`):
   - `artifacts/manny_office_assets.json`
   - `tools/tests/manny_office_timeline.json`
   - `tools/tests/movement_log.json`
   - `tools/tests/hotspot_events.json`
   - `artifacts/run_cache/manny_geometry.json` (Lua geometry snapshot)
2. Run the viewer helper with the default overlays and geometry aligned:

   ```bash
   python tools/grim_viewer.py -- \
     --headless \
     --lua-geometry-json artifacts/run_cache/manny_geometry.json
   ```

   The helper injects the timeline, movement, and hotspot fixtures for you. The
   console output should list the entities discovered in the timeline manifest,
   report the movement trace summary, and note the first hotspot events.
3. Tail an audio log at the same time by adding
   `--audio-log tools/tests/hotspot_audio.json`; headless mode will print cue
   updates until events stabilise.

## Geometry Snapshot Expectations
- Always pass `--lua-geometry-json` when running the viewer against the Manny
  baselines. Without the snapshot, Manny/desk/tube markers fall back to movement
  heuristics and drift away from the minimap anchors.
- When you regenerate geometry (for example after tweaking entity placement in
  `grim_engine`), rerun the headless command above and confirm the marker legends
  still report Manny in the expected sectors. The overlay summary now calls out
  the active timeline stage and hook so mismatches are easy to spot.

Keep this flow in sync with new overlay affordances; any future module split or
CLI change should be reflected here so engineers can validate the first-playable
loop quickly.
