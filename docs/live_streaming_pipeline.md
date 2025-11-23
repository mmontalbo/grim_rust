# Live Stream Pipeline (Current State)

The GrimStream experiment was removed while `grim_engine` was trimmed to a minimal intro runner. The engine no longer binds a stream socket, waits for viewer handshakes, or emits live state updates. It simply plays the intro cutscene, logs `intro.timeline` markers, and exits.

## Recommended flow

- `grctl scenario run intro-to-office-computer` (or `intro-to-office-tube`) runs the headless engine with verbose logging and waits for `movie.logos.*` / `movie.intro.*` markers.
- `grctl watch intro-timeline --launch` clears prior logs, starts the headless engine and retail capture, and tails the engine log plus retail telemetry to show intro timeline parity.
- If you need the old viewer/stream overlays, recover them from Git history and reintroduce them as a dedicated follow-up rather than threading compatibility code through the minimal binary.

## What was removed

- GrimStream serving in `grim_engine` and all `--stream-bind`/`--engine-stream` flags.
- Viewer handshakes (`viewer_ready.*`) and stream-ready gates.
- Live movie control messages and incremental state updates (`StateUpdateBuilder`).
- Scenario harness requirements that gated retail capture behind the viewer.

Retail capture and intro telemetry are still available through the `grctl retail` commands, but they no longer integrate with a live engine stream.
