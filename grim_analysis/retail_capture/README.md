# Retail Telemetry & Shim

This directory keeps the artifacts that let the retail executable stream its
runtime state into our analysis pipeline. The code lives next to
`grim_analysis` so capture tooling, format specs, and the Rust consumers stay
in one place.

- `telemetry.lua` is the Lua script injected into the shipping VM. It currently
  just proves the hook loads; expand it to emit structured telemetry when the
  retail instrumentation project resumes.
- `shim/` contains the `LD_PRELOAD` hook (`lua_hook.c` + `Makefile`) that
  intercepts the engine's `lua_dofile` calls, injects `telemetry.lua` the first
  time `_system.lua` loads, and logs the hand-off for debugging.

Build the shim by running `make` in the `shim/` directory (the default compiler
assumes `zig cc`, but any C toolchain that can produce a shared object will do).
Once the shared object is produced, preload it before launching the retail game
and drop `telemetry.lua` into the game's `mods/` directory so the shim can
execute it.
