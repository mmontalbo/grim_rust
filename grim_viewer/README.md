# Grim Viewer

`grim_viewer` is the wgpu-based visualizer that helps us inspect decoded assets
and milestone telemetry without launching the retail executable. It consumes the
manifests emitted by `grim_engine` to render Manny's office plates, overlay boot
timeline markers, and track audio cues during development.

### How the pieces fit
- **CLI bootstrap (`src/main.rs`, `src/cli.rs`):** Parses the manifest paths,
  decodes the requested `.bm`/`.zbm` frame, and loads optional timeline,
  movement, hotspot, audio, and geometry fixtures into memory.
- **Scene builder (`src/scene`):** Normalises `grim_engine` exports into a
  `ViewerScene` – actors/objects, timeline metadata, Manny's movement trace, and
  color-coded hotspot events – so the renderer has one source of truth.
- **Runtime state (`src/viewer/state`):** Owns the wgpu surface, uploads the
  decoded frame, computes UI layout, and keeps overlays and selection state in
  sync with the scene.
- **Overlays and markers (`src/viewer/overlays.rs`, `src/viewer/markers.rs`):**
  Project Manny, hotspot, and geometry markers into plate space using the
  recovered camera, while text overlays narrate the current selection, movement
  scrubber frame, and audio log status.
- **Utility crates:** `texture.rs` handles codec3 decoding and PNG export,
  `audio.rs` tails Lua audio logs, and `ui_layout.rs` describes the HUD layout
  constraints used by the viewer.
- **Deeper docs:** Each module owns its own README (`src/scene/README.md`,
  `src/viewer/state/README.md`) that maps the JSON fixtures and render loops in
  more detail for contributors touching those areas.

## Role in the Recreation
- **Asset inspection:** Loads `.bm` color plates and `.zbm` depth buffers from
  the LAB archives so we can validate codec3 output and frame compositions.
- **Telemetry overlays:** Projects boot timeline hooks, audio logs, and
  interaction states onto the scene to highlight why Manny can or cannot
  interact with a hotspot.
- **Regression guard:** Keeps the Manny baseline overlays, audio, and geometry in
  one place so we can spot first-playable regressions without replaying the
  retail build.

## Manny's Office Focus
- Defaults to the Manny office manifest exported from the engine host, keeping
  iteration tight on the first-playable goal.
- Highlights hotspots and dialogue triggers from the timeline JSON so scene
  setup drift is obvious.
- Overlays Manny's path, hotspot events, and entity anchors directly on the plate using
  color-coded discs (teal current frame, amber tube anchor, jade desk, gold highlighted hotspot, green/blue props, red selection) so
  the minimap and perspective view stay in sync at a glance.
- Watches live audio logs (when provided) to ensure cues fire when Manny uses
  the pneumatic tube, desk, or other milestone interactions.
- Prunes the Manny office entity list down to Manny plus the desk/tube props; the
  expectations live in `grim_viewer/src/main.rs` under `entity_filter_tests`, so extend
  those when the allowlist changes.

## Typical Usage
- `python tools/grim_viewer.py` launches the viewer preloaded with the Manny
  computer baseline (timeline, movement, hotspot markers). The recovered camera
  projects the overlay directly onto the plate, so the default view now matches
  the in-game perspective.
- `cargo run -p grim_viewer -- --manifest artifacts/manny_office_assets.json`
  still works for custom runs; pass `--timeline`, `--movement-log`, and
  `--event-log` explicitly when you want the overlay in other scenes.
- `--audio-log` streams cue updates captured during a Lua run.
- Append `-- --headless` to run the viewer without opening a window (useful for
  scripted captures or remote machines).

## Layout Presets
- `grim_viewer` accepts `--layout-preset <file>` to size HUD panels with the
  Taffy helper instead of hardcoding coordinates. JSON presets expose per-panel
  `width`, `height`, `padding_x`, and `padding_y` fields; add `"enabled": false`
  to hide a panel without editing Rust.
- The Manny baseline preset lives at
  `grim_viewer/presets/manny_office_layout.json`. The helper script
  `python tools/grim_viewer.py` automatically forwards it, so day-to-day
  launches always share the same declarative layout.
- The preset keeps the timeline and scrubber panes 640px wide so the 78-column
  overlays (timeline summary + movement scrubber legend) render without clipping;
  bump these widths if future overlay text grows.
- Tweak the preset (or point `--layout-preset` at a copy) when you need extra
  room for new overlays. Minimap sizing uses `min_side`, `preferred_fraction`,
  and `max_fraction` to describe its responsive bounds.
- Example snippet that shrinks the timeline while disabling the audio panel:
  ```json
  {
    "audio": { "enabled": false },
    "timeline": { "width": 560, "height": 200 }
  }
  ```

## Extending the Crate
- **Scene data:** Extend `scene::ViewerScene` when the engine emits new
  telemetry. Builders live next to the serde models so you can see how timeline
  hooks map to overlay markers.
- **Runtime overlays:** `viewer/state` owns layout and render order; add new
  panels through `ui_layout.rs` so the Taffy layout stays declarative.
- **Audio feeds:** Use `audio_log.rs` types to normalise new event sources so
  the existing aggregation pipeline can surface them without bespoke UI.
- **Automation:** Gate new overlays or expensive processing behind CLI flags so
  scripted `--headless` runs and regression jobs remain deterministic.
