# Grim Engine Host

`grim_engine` currently ships as a **minimal intro playback binary**. The crate
exists purely to bring up the retail intro sequence far enough for the viewer
handshake and to stream state over GrimStream. All analysis helpers, Lua demos,
and JSON artefact generators were intentionally removed to reduce maintenance
overhead while we focus on first-playable.

## Current Scope
- Boots the intro Lua bundle until the viewer handshake succeeds.
- Streams intro state via GrimStream when `--stream-bind` is supplied.
- Supports configuring data/lab roots for developer installs.
- Emits verbose logging behind `--verbose` for troubleshooting.

Anything else that previously lived in this crate (timeline dumps, hotspot
demos, coverage analysis, regression tests) is out of scope for the current
milestone.

## Command Line

```
cargo run -p grim_engine -- \
    [--data-root <path>] \
    [--verbose] \
    [--lab-root <path>] \
    [--stream-bind <addr>] \
    [--stream-ready-file <path>]
```

- `--data-root` defaults to `extracted/DATA000`.
- `--lab-root` defaults to `dev-install/` when present.
- `--stream-bind` exposes a GrimStream server; omit it to run without
  networking.
- `--stream-ready-file` writes a marker once streaming starts. This is used by
  `tools/run_live_preview.py` to coordinate viewer bring-up.

No other flags are recognised. Scripts that still reference `--run-lua`,
`--timeline-json`, `--movement-demo`, etc. must be updated or removed.

## Typical Usage

- Local smoke test without streaming:
  ```
  cargo run -p grim_engine --
  ```
- Live preview with the viewer:
  ```
  python tools/run_live_preview.py
  ```
  The helper script launches the viewer and the engine, passing
  `--stream-bind 127.0.0.1:17500` and a temporary ready marker automatically.

## Restoring Legacy Behaviour

When we eventually need the richer tooling again, retrieve it from commit
history instead of threading compatibility code through the minimal binary. Use
`git log grim_engine` to locate the pre-minimalisation revisions and resurrect
the specific demos or JSON exporters as dedicated follow-up work.

Until then, keep new development constrained to the minimal flow so the intro
playback path remains easy to reason about.
