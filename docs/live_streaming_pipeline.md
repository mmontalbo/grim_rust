# Live Stream Pipeline (Current State)

We previously explored a large streaming stack that exposed timeline diffs,
coverage tracking, and retail telemetry alongside the viewer. That experiment
has been shelved while we focus on the minimal intro playback loop.

## What Still Exists
- `tools/run_live_preview.py` launches the viewer, the trimmed `grim_engine`
  binary, and (optionally) the retail capture helper. It is the only supported
  way to drive the live intro preview today.
- `grim_engine` exposes a GrimStream socket when invoked with `--stream-bind`.
  The stream only carries the intro playback state needed for the viewer UI.
- The viewer still understands the GrimStream handshake and will render the
  intro overlay when the engine connects.

## What Was Removed
- No CLI flags remain for timeline dumps, hotspot demos, or coverage exports.
- The capture path inside `run_live_preview.py` is strictly optional. Skip it
  with `--no-capture` when you only need the engine/viewer handshake.
- Control messages (`pause`, `seek`, etc.) are not implemented. The viewer
  renders whatever the engine publishes and that is sufficient for the current
  milestone.

## Recommended Flow

```
python tools/run_live_preview.py [--no-capture] [--release]
```

The script wraps all coordination (stream ready files, window sizing, optional
retail boot). If you need a raw engine run, invoke

```
cargo run -p grim_engine -- --stream-bind 127.0.0.1:17500
```

but be aware that nothing writes artefacts to disk anymore.

## Looking Ahead

When the project needs richer streaming again, pull the old design notes from
Git history instead of layering compatibility branches onto the trimmed stack.
Reintroduce capture/telemetry as dedicated milestones so we can keep the intro
playback loop simple in the meantime.
