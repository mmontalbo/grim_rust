# Grim Engine Lua Host Overview

The Lua host still carries the full boot pipeline for Manny's intro, but the
exposed surface was trimmed to a headless-only intro runner. The GrimStream
socket and viewer handshakes were removed alongside the old streaming
experiment. Use this note as a quick map of what remains relevant.

## Control Flow
- `run_boot_sequence` (`lua_host/mod.rs`) loads assets, initialises Lua, and
  drives the boot scripts until we reach the intro playback loop. It always
  yields an `EngineRuntime` because we now keep the Lua VM alive long enough to
  finish the intro movie before exiting.
- `EngineRuntime::run` advances the Lua scheduler at ~30 Hz, polls fullscreen
  movie completion, and prints fresh events when running headless. There is no
  stream publishing path anymore.

## Module Layout
- `context/` holds the gameplay state and binding glue. Many modules remain
  (actors, sets, script runtime, etc.), but they are currently exercised solely
  by the intro boot path.
- `types.rs` groups lightweight data structures (`Vec3`, seed transforms, etc.)
  shared between the host and the streaming layer.

## Out of Scope
- JSON exporters (`timeline`, `movement`, `geometry`, …) are no longer wired
  up; pull them from history if they become relevant again instead of reviving
  the old entry points.
- Hotspot and movement demos are gone from the CLI. If you need them back, pull
  from history and reintroduce them as focused modules rather than reviving
  broad entry points.

Keep the Lua host changes laser-focused on the intro playback handshake so we
avoid growing a new surface area before the first playable milestone lands.
