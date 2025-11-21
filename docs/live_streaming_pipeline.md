# Live Stream Pipeline (Current State)

We previously explored a large streaming stack that exposed timeline diffs,
coverage tracking, and retail telemetry alongside the viewer. That experiment
has been shelved while we focus on the minimal intro playback loop.

## What Still Exists
- `grctl scenario run intro-to-office-computer` is the canonical intro playback
  harness. Add `--with-viewer` to watch the stream and `--with-retail` if you
  also need the retail capture pane.
- `grim_engine` exposes a GrimStream socket when invoked with `--stream-bind`.
  The stream only carries the intro playback state needed for the viewer UI.
- The viewer still understands the GrimStream handshake and renders a minimal
  overlay (Manny trail + current position) when the engine connects. Movie
  playback uses the ffmpeg pipeline.

## What Was Removed
- No CLI flags remain for timeline dumps, hotspot demos, or coverage exports.
- Control messages (`pause`, `seek`, etc.) are not implemented. The viewer
  renders whatever the engine publishes and that is sufficient for the current
  milestone.

## Recommended Flow

```
grctl scenario run intro-to-office-computer --with-viewer
```

The scenario harness launches the engine and viewer under `grctl` supervision,
waits for the `viewer_ready` handshake, applies a default timeout, and tails
logs in `target/grctl/logs/`. Pass `--with-retail` to include the retail capture
pane or `--detach` when you only need a managed launch.

```
cargo run -p grim_engine -- --stream-bind 127.0.0.1:17500
```

Use the raw engine command above only when debugging the stream endpoint
directly; otherwise prefer the `grctl` wrapper so logs and timeouts stay
consistent.

## Looking Ahead

When the project needs richer streaming again, pull the old design notes from
Git history instead of layering compatibility branches onto the trimmed stack.
Reintroduce capture/telemetry as dedicated milestones so we can keep the intro
playback loop simple in the meantime.
