# Grim Engine Host

`grim_engine` currently ships as a **stripped intro runner**. The crate exists
purely to bring up the retail intro sequence, simulate fullscreen playback
locally, and exit once the intro cutscene completes. All streaming, analysis
helpers, Lua demos, and JSON artefact generators were intentionally removed to
reduce maintenance overhead while we focus on first-playable.

## Current Scope
- Boots the intro Lua bundle and drives scripts until the intro movie ends.
- Simulates fullscreen playback locally (no viewer/socket handshake).
- Supports configuring data/lab roots for developer installs.
- Emits verbose logging behind `--verbose` and prints events when headless.

Anything else that previously lived in this crate (timeline dumps, hotspot
demos, coverage analysis, regression tests) is out of scope for the current
milestone.

## Command Line

```
cargo run -p grim_engine -- \
    [--data-root <path>] \
    [--lab-root <path>] \
    [--headless] \
    [--verbose]
```

- `--data-root` defaults to `extracted/DATA000`.
- `--lab-root` defaults to `dev-install/` when present and is used to locate
  retail movie assets.
- `--headless` prints emitted engine events to stdout.
- `--verbose` enables extra logging from the Lua host.

No other flags are recognised. Scripts that still reference `--run-lua`,
`--timeline-json`, `--movement-demo`, etc. must be updated or removed.

## Typical Usage

- Quick headless smoke test:
  ```
  cargo run -p grim_engine -- --headless --verbose
  ```
  The command advances the intro loop, simulates intro playback, and prints
  emitted events to the terminal before exiting.

## Restoring Legacy Behaviour

When we eventually need the richer tooling again, retrieve it from commit
history instead of threading compatibility code through the minimal binary. Use
`git log grim_engine` to locate the pre-minimalisation revisions and resurrect
the specific demos or JSON exporters as dedicated follow-up work.

Until then, keep new development constrained to the minimal flow so the intro
playback path remains easy to reason about.
