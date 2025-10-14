# Grim Engine Lua Host Overview

The `grim_engine::lua_host` package now exposes a layered structure so runtime playback,
viewer tooling, and regression harnesses can share the same bootstrap path without
reaching into ad-hoc maps.

## Control Flow
- `run_boot_sequence` (in `lua_host/mod.rs`) stages the resource graph, optional LAB
  assets, and a fresh `mlua::Lua` runtime. It wires in the scripted scaffolding and
  returns an `EngineRunSummary` plus access to the context handle.
- `EngineContextHandle` wraps the internal `EngineContext` state and offers targeted
  commands (`resolve_actor_handle`, `walk_actor_vector`, `run_scripts`, etc.). Callers no
  longer borrow the raw `Rc<RefCell<_>>`, which keeps invariants localized to the host.
- Movement and hotspot demos (`movement.rs`, `hotspot.rs`) now consume the handle and
  only rely on the public methods. They simulate scripted steps, log events, and surface
  samples without touching private maps.

## Module Layout
- `context/audio.rs` collects audio routing state (`MusicState`, `SfxState`,
  `AudioCallback`) and helper formatting so the main context focuses on gameplay data.
- `context/geometry.rs` parses and stores sector polygons, set snapshots, and helpers
  for hit detection.
- `context/geometry_export.rs` converts a cloned, plain-data snapshot of the engine
  state into the serialisable `LuaGeometrySnapshot` for viewer tooling.
- `context/mod.rs` (formerly the monolithic file) retains the runtime bindings,
  gameplay bookkeeping, and the new `EngineContextHandle`.

## Snapshot Export
`EngineContext::geometry_snapshot` now clones a lightweight `SnapshotState` and hands it
to `geometry_export::build_snapshot`. This decouples JSON export from the live context,
making it easier to reuse the geometry state in future viewers or captured baselines.

## Next Steps
Remaining refactor work includes carving out actor- and menu-specific logic into their
own modules, expanding unit coverage around the new handle surface, and continuing to
pay down direct map access inside Lua binding helpers.
