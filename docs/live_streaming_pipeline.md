# Live Stream Pipeline (Current State)

We previously explored a large streaming stack that exposed timeline diffs,
coverage tracking, and retail telemetry alongside the viewer. That experiment
has been shelved while we focus on the minimal intro playback loop.

## What Still Exists
- `tools/run_movie_debug.py` starts the minimal `grim_engine` + `grim_viewer`
  pair, skips the retail capture path, and waits for the live-stream handshake.
  It is the quickest way to focus on movie playback while iterating.
- `tools/run_live_preview.py` still launches the viewer, the trimmed
  `grim_engine` binary, and (optionally) the retail capture helper when you
  need the full capture loop.
- `grim_engine` exposes a GrimStream socket when invoked with `--stream-bind`.
  The stream only carries the intro playback state needed for the viewer UI.
- The viewer still understands the GrimStream handshake and renders a minimal
  overlay (Manny trail + current position) when the engine connects.

## What Was Removed
- No CLI flags remain for timeline dumps, hotspot demos, or coverage exports.
- The capture path inside `run_live_preview.py` is strictly optional. Skip it
  with `--no-capture` when you only need the engine/viewer handshake.
- Control messages (`pause`, `seek`, etc.) are not implemented. The viewer
  renders whatever the engine publishes and that is sufficient for the current
  milestone.

## Recommended Flow

```
python tools/run_movie_debug.py [--engine-verbose] [--release]
```

The helper binds the engine to `127.0.0.1:17500`, launches the viewer with
`--no-retail`, and tails `/tmp/live_preview.log` until the engine reports
`viewer_ready.open`. Viewer logs now call out when the movie pipeline boots,
when the first frame arrives, and how many frames were uploaded before a
Finished/Skipped/Error control is sent back to the engine.

Use the classic, full pipeline when you also need retail capture or window
automation:

```
python tools/run_live_preview.py [--no-capture] [--release]
```

`run_live_preview.py` still wraps all coordination (stream ready files, window
sizing, optional retail boot). If you need a raw engine run, invoke

```
cargo run -p grim_engine -- --stream-bind 127.0.0.1:17500
```

The startup delay in `run_live_preview.py` comes from two checkpoints: it polls
`xdotool`/`xwininfo` until the retail window is available (skip with
`--no-capture`) and it tails `/tmp/live_preview.log` for the `viewer_ready.open`
log line so the engine only advances once the viewer acknowledges the stream.

but be aware that nothing writes artefacts to disk anymore.

## Looking Ahead

When the project needs richer streaming again, pull the old design notes from
Git history instead of layering compatibility branches onto the trimmed stack.
Reintroduce capture/telemetry as dedicated milestones so we can keep the intro
playback loop simple in the meantime.
