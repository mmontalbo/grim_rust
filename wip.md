# Work in Progress

## Current Direction
- Embedded runtime now installs stateful menu tables so overlay visibility and pause behaviour reflect the original game.
- Inventory/object scaffolding and audio helpers continue to converge so Manny's office boot can reach its first dialogue beat inside the Rust host.

## Active Threads
- Object helpers propagate touchable/visible changes via metatables; the desk tube/computer path still needs an interaction log fixture.
- Audio scaffolding captures music/SFX history; the callback bridge needs to drive external viewers once wired through the helpers.
- Geometry-backed selection informs commentary and cut-scene state, but the head-control fallback still assumes Manny-specific sectors.

## Next Steps
1. Finish the `_mo.lua` tube/computer interaction loop so touchability toggles and logging match the shipped scripts.
2. Extend the audio bridge with a callback trait, thread it through runtime helpers, and add focused unit coverage.
3. Replace the Manny-focused head-control fallback with geometry-driven targeting plus regression tests.

## Workstreams

### interaction_loop
Interaction loop polish

```
Objective: flesh out the remaining object and inventory helpers needed for Manny's office interactions. Focus on the TODOs around object controls in grim_engine/src/lua_host.rs (e.g. install_runtime_tables, make_object_touchable, inventory helpers). Implement the missing helpers so Manny can interact with the desk tube/computer path when running grim_engine --run-lua --verbose. Add an integration test that drives Manny through the boot sequence and asserts the interaction log includes the expected touchable state changes. Run cargo fmt and cargo test -p grim_engine. Constraints: reference only code/documentation sources online; stop once the helpers and tests are committed and note any remaining gaps instead of continuing beyond the scope.
```

### audio_bridge
Audio playback bridge

```
Objective: introduce a minimal audio playback adapter inside grim_engine so menu/runtime events can trigger viewer hooks later. Work within grim_engine/src/lua_host.rs, optionally creating a helper module under grim_engine/src/audio_bridge.rs, but do not add new crates. Expose a lightweight trait (for example AudioCallback) and thread it through EngineContext, updating music and sfx helpers to invoke the callback. Add unit tests verifying callbacks receive play and stop events while existing state tracking remains intact. Run cargo fmt and cargo test -p grim_engine. Constraints: limit external references to code/documentation sources; stop after the callbacks and tests are in place.
```
