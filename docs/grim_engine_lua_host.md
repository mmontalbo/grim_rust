# Grim Engine Lua Host Overview

The Lua host still carries the full boot pipeline for Manny's intro, but the
exposed surface was tightened to support the minimal viewer stream only. Use
this note as a quick map of what remains relevant.

## Control Flow
- `run_boot_sequence` (`lua_host/mod.rs`) loads assets, initialises Lua, and
  drives the boot scripts until we reach the intro playback loop. It returns
  an `EngineRuntime` only when a GrimStream server was requested.
- `EngineRuntime::run` advances the Lua scheduler at ~30 Hz, pipes deltas
  through `StateUpdateBuilder`, and publishes them over the bound stream.
- `EngineContextHandle` now only exposes the tiny set of helpers needed by
  `StateUpdateBuilder` (actor lookup, hotspot state, coverage counters).

## Module Layout
- `context/` holds the gameplay state and binding glue. Many modules remain
  (actors, sets, script runtime, etc.), but they are currently exercised solely
  by the intro boot path.
- `state_update.rs` owns the translation from context state into the
  serialisable `StateUpdate` payloads used by the viewer.
- `types.rs` groups lightweight data structures (`Vec3`, seed transforms, etc.)
  shared between the host and the streaming layer.

## Out of Scope
- JSON exporters (`timeline`, `movement`, `geometry`, …) are no longer wired
  up. Leave the stale helpers in place only if they keep the intro boot alive;
  rip the rest out when warnings point at them.
- Hotspot and movement demos are gone from the CLI. If you need them back, pull
  from history and reintroduce them as focused modules rather than reviving
  broad entry points.

Keep the Lua host changes laser-focused on the intro playback handshake so we
avoid growing a new surface area before the first playable milestone lands.
