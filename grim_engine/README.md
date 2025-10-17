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
- `cargo test -p grim_engine -- runtime_regression` boots the Lua host, records
  the Manny movement demo, runs the computer hotspot interaction, and captures
  fresh artefacts. It now acts as a smoke test: we assert that the run produces
  the expected hotspot markers and non-empty movement/audio/depth/timeline
  outputs without diffing against historical fixtures.
- `cargo test -p grim_engine -- hotspot_demo_logs_hotspot_markers` exercises
  the computer hotspot demo and asserts the Lua host emits the expected
  approach/start/end markers. Run
  `cargo run -p grim_engine -- --run-lua --hotspot-demo computer \
  --audio-log-json hotspot_audio.json` to inspect the captured audio/events
  manually.

## Extending the Crate
- When adding Lua bindings, mirror ScummVM's semantics and document any gaps so
  downstream tools understand the partial implementations.
- Keep regression tests in `src/main.rs`'s test module aligned with new output
  formats; fixtures under `tests/fixtures/` should represent Manny's office
  ground truth.
- Prefer lightweight logging that steers attention toward first-playable
  blockers instead of dumping raw Lua traces.
