# Grim Engine Host

`grim_engine` is the prototype game host that stitches the analysis output into
an interactive runtime. It loads Manny's office directly from the extracted
assets, boots the Lua scripts inside an embedded interpreter, and records the
state we need to validate the first playable milestone.

## Role in the Recreation
- **Runtime harness:** Mirrors the retail boot path from title screen through
  Manny's office while exposing the data structures that the original engine
  hid behind C++ callbacks.
- **Diagnostics:** Emits structured JSON (timeline, scheduler, geometry diffs,
  audio logs) so we can verify that the Rust host reproduces the shipping
  behavior.
- **Interoperability:** Bridges the static analysis with live Lua execution,
  providing the viewer and tests with snapshots that reflect real-time script
  outcomes.

## Manny's Office Focus
- Validates that Manny spawns in the correct set, walkboxes activate as
  expected, and hotspots remain interactable during the milestone demo.
- Captures codec3 decode regressions by pairing color `.bm` and depth `.zbm`
  outputs with the static manifests.
- Exercises the boot-time scheduler so we can keep the first-run dialogue and
  cutscene triggers aligned with the retail game.

## Typical Usage
- `cargo run -p grim_engine -- --run-lua --lua-geometry-json snapshot.json`
  captures a live geometry snapshot for comparison against the static timeline.
- `cargo run -p grim_engine -- --timeline-json timeline.json` exports the boot
  stages that the analysis crate derived.
- `cargo run -p grim_engine -- --verify-geometry` replays both paths and fails
  if the runtime diverges, providing a convenient acceptance test before making
  engine changes.

## Regression Harnesses
- `cargo test -p grim_engine -- movement_regression` boots the Lua host,
  records a fresh movement log, and verifies it matches
  `tests/fixtures/movement_demo_log.json`. Refresh the fixture with
  `cargo run -p grim_engine -- --run-lua --movement-demo --movement-log-json \
  grim_engine/tests/fixtures/movement_demo_log.json` whenever the intended walk
  path changes (document the reasoning in the commit that updates it).
- `cargo test -p grim_engine -- runtime_regression` boots the Lua host, records
  the Manny movement demo, runs the computer hotspot interaction, and captures
  depth/timeline artefacts. The test compares the freshly captured outputs
  against the committed baselines in `tools/tests/`. Refresh them via
  `cargo run -p grim_engine -- --timeline-json tools/tests/manny_office_timeline.json`
  followed by
  `cargo run -p grim_engine -- --run-lua --movement-demo \
  --movement-log-json tools/tests/movement_log.json \
  --hotspot-demo computer --audio-log-json tools/tests/hotspot_audio.json \
  --depth-stats-json tools/tests/manny_office_depth_stats.json` whenever the
  intended walk path, audio sequence, or depth metrics change (call out the
  rationale when updating the snapshots).

## Extending the Crate
- When adding Lua bindings, mirror ScummVM's semantics and document any gaps so
  downstream tools understand the partial implementations.
- Keep regression tests in `src/main.rs`'s test module aligned with new output
  formats; fixtures under `tests/fixtures/` should represent Manny's office
  ground truth.
- Prefer lightweight logging that steers attention toward first-playable
  blockers instead of dumping raw Lua traces.
