# Grim Viewer

`grim_viewer` is the wgpu-based visualizer that helps us inspect decoded assets
and milestone telemetry without launching the retail executable. It consumes the
manifests emitted by `grim_engine` to render Manny's office plates, overlay boot
timeline markers, and track audio cues during development.

## Role in the Recreation
- **Asset inspection:** Loads `.bm` color plates and `.zbm` depth buffers from
  the LAB archives so we can validate codec3 output and frame compositions.
- **Telemetry overlays:** Projects boot timeline hooks, audio logs, and
  interaction states onto the scene to highlight why Manny can or cannot
  interact with a hotspot.
- **Regression guard:** Supports headless render verification, giving us a quick
  diff between decoded pixels and the GPU pipeline before we ship changes to the
  renderer.

## Manny's Office Focus
- Defaults to the Manny office manifest exported from the engine host, keeping
  iteration tight on the first-playable goal.
- Highlights hotspots and dialogue triggers from the timeline JSON so scene
  setup drift is obvious.
- Watches live audio logs (when provided) to ensure cues fire when Manny uses
  the pneumatic tube, desk, or other milestone interactions.

## Typical Usage
- `python tools/grim_viewer.py run` launches the viewer preloaded with the Manny
  computer baseline (timeline, movement, hotspot markers). The recovered camera
  projects the overlay directly onto the plate, so the default view now matches
  the in-game perspective.
- `cargo run -p grim_viewer -- --manifest artifacts/manny_office_assets.json`
  still works for custom runs; pass `--timeline`, `--movement-log`, and
  `--event-log` explicitly when you want the overlay in other scenes.
- `--audio-log` streams cue updates captured during a Lua run.
- `--headless --verify-render` performs the offscreen render diff, useful in CI
  or quick sanity checks before editing shader code.

## Extending the Crate
- Use the types in `audio_log.rs` to normalize new event sources so overlays
  stay consistent.
- Keep UI prompts concise; the target audience is engineers verifying asset
  correctness rather than end users.
- When adding new overlays, gate them behind CLI flags so automated runs remain
  deterministic.
