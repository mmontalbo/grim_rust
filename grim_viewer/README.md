# Grim Viewer (Minimal)

`grim_viewer` now mirrors the trimmed `grim_engine` scope: it exists to watch
the intro playback live, nothing more. The application keeps the split viewport
UI (retail capture on the left, Rust runtime overlay on the right) but all of
the manifest/timeline/movement tooling has been removed to keep iteration fast.

## Current Scope
- Connects to a retail GrimStream endpoint and displays incoming frames.
- Connects to the engine GrimStream endpoint (when provided) and renders a
  lightweight 2D overlay showing Manny's recent path and current position.
- Exposes the same keyboard controls as before:
  - `Space` toggles pause/resume of the retail stream.
  - `.` / `>` steps one frame while paused.
  - `D` toggles the frame delta readout in the debug panel.
- Surfaces basic session/debug information (stream status, last engine update,
  active cutscene) in the lower panel.

## CLI

```
cargo run -p grim_viewer -- \
    [--retail-stream <addr>] \
    [--engine-stream <addr>] \
    [--window-width <px>] \
    [--window-height <px>]
```

- `--retail-stream` defaults to `127.0.0.1:17400`.
- `--engine-stream` is optional; when omitted the right-hand viewport keeps the
  placeholder overlay.
- Window dimensions seed the initial layout; resize interactively as needed.

## Implementation Notes
- `src/live_scene.rs` owns the simplified overlay renderer. It keeps an RGBA
  buffer in memory, tracks Manny's recent positions, and draws them onto a flat
  background. No asset manifests, depth buffers, or geometry snapshots are
  involved anymore.
- `src/live_stream.rs` retains the streaming client code and still reports
  protocol issues so we notice handshake failures quickly.
- `src/display.rs` and `src/overlay.rs` continue to manage the wgpu surface and
  text overlays; the layout code is unchanged so the UI feel matches earlier
  builds.

## Looking Ahead
When we need richer overlays (plates, geometry, timeline hooks, movie playback),
pull the relevant modules back from history instead of rebuilding them ad-hoc.
Until the intro milestone ships, keep the viewer focused on the live stream so
every contributor can reason about the path from retail capture to Rust overlay
at a glance.
