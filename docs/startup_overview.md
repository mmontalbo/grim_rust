# Startup & New Game Flow

This document captures the vanilla Grim Fandango Remastered boot pipeline so we
can plan a Rust reimplementation that keeps the original data files, cutscenes,
and dialogue intact. File references point at the decompiled Lua shipped with
the retail game.

## High-Level Stages
1. **Engine boots** native code and executes `_system.lua` as the
   `LUA_BOOTSCRIPT`, defining global helpers plus core tables such as `system`,
   `Actor`, `Set`, and `Object` (`extracted/DATA000/_system.decompiled.lua`).
2. **`BOOT(resumeSave, bootParam)`** picks the starting set, seeds input/state,
   and kicks off an asynchronous logo/cutscene pipeline
   (`extracted/DATA000/_system.decompiled.lua:2847`).
3. **`SHOWLOGO` & `BOOTTWO`** handle the opening movies, optional save-game
   restore, and load every set/menu script to populate `system.setTable`
   (`extracted/DATA000/_system.decompiled.lua:2916` and `:2953`).
4. **`FINALIZEBOOT`** selects the starting `Set`, equips Manny, and hands
   control to the room scripts (`extracted/DATA000/_system.decompiled.lua:2967`).
5. **`mo.enter`** (Manny's office) materializes the initial room state and
   launches the intro cutscene the first time the set loads
   (`extracted/DATA000/mo.decompiled.lua:94`).

## Core Data Structures
- **Actors.** `_actors.decompiled.lua` creates Manny, Meche, and the rest of the
  cast. Manny is registered via `Actor:create` and becomes the selected actor in
  `BOOT` when `manny:set_selected()` runs, which also sets
  `system.currentActor = manny` (`extracted/DATA000/_actors.decompiled.lua:1471`
  and `_system.decompiled.lua:1889`). Manny's costume/state machine lives in
  `_manny.decompiled.lua` and is keyed off the active set name
  (`extracted/DATA000/_manny.decompiled.lua:60`).
- **Sets.** `Set:create` allocates a room entry in `system.setTable`, recording
  setup cameras, door metadata, etc. (`extracted/DATA000/_sets.decompiled.lua:79`).
  `Set:switch_to_set` handles transitions, calling `CommonExit`, `CommonEnter`,
  rebuilding verb buttons, and eventually running the set's `enter` function
  (`extracted/DATA000/_sets.decompiled.lua:154`).
- **Object Layer.** `mo.decompiled.lua` shows a representative room script:
  it wires up object states, inventory items, and cutscenes inside `mo:enter`
  before yielding to gameplay (`extracted/DATA000/mo.decompiled.lua:94`).

## Detailed Boot Sequence
1. **Sanity & defaults (`BOOT`).**
   - Reads the last visited set from the registry and a "developer" flag that
     unlocks debug tooling (`extracted/DATA000/_system.decompiled.lua:2849`).
   - Force-selects `defaultSet = "mo.set"`, marks `time_to_run_intro = TRUE`,
     and disables the auto-teleport-to-last-room shortcut so New Game always
     drops us in Manny's office (`_system.decompiled.lua:2866`).
   - Initializes fonts, cursors, preferences, controls, and the primary actor.
   - Enables joystick/mouse control and queues achievement metadata loads.
2. **Logos (`SHOWLOGO`).** Plays the LucasArts/Double Fine movies, waits for
   the global save system to initialize, and decides whether to resume a slot
   (`_system.decompiled.lua:2916`). If `resumeSave` is true and a registry slot
   exists, it calls the engine-level `Load(...)`; otherwise it falls through to
   `BOOTTWO` for a clean start.
3. **Content loading (`BOOTTWO`).** Calls `source_all_set_files()` which in turn
   `dofile`s every year script, all menus, and then each room Lua in batches,
   updating the loading screen between groups
   (`extracted/DATA000/_sets.decompiled.lua:1106`). This populates
   `system.setTable` and registers cutscenes before gameplay begins.
4. **World activation (`FINALIZEBOOT`).**
   - Validates the chosen set exists, then calls `system.setTable[defaultSet]:
     switch_to_set()` which executes `CommonExit` on the previous room (if any),
     runs `CommonEnter`, rebuilds UI, and fires the room's `enter` handler.
   - Sets Manny's costume to the location-appropriate default via
     `manny:default(look_up_correct_costume(system.currentSet))`.
   - Places the player (`system.currentActor`) into the set and positions him at
     the current interest point (`_system.decompiled.lua:2974`).
   - Pulls Manny's scythe into inventory and spins up input-processing scripts
     like `TrackManny` and `WalkManny` to handle control state.
5. **Room-side init (`mo.enter`).**
   - Adjusts object states (tube canister, computer, door overlays) based on
     story flags, sets up Meche if already reaped, and loads Manny's typing
     costume (`extracted/DATA000/mo.decompiled.lua:94`).
   - If `time_to_run_intro` is true, closes the loading menu, flips the flag, and
     starts `cut_scene.intro`, which plays the Manny-to-Meche dialog movie and
     hands control back with Manny standing by his desk
     (`extracted/DATA000/year_1.decompiled.lua:8`).

## Save vs. Fresh Start
- `SHOWLOGO` uses `ReadRegistryIntValue("LastSavedGame")` and the `resumeSave`
  flag passed by the engine to decide whether to issue `Load(slot, TRUE)` or to
  continue into a new game (`extracted/DATA000/_system.decompiled.lua:2926`).
- After a load, if the loading menu closed early (typical after the movie) the
  script re-opens it before the Lua-side resume finishes (`_system.decompiled.lua:2934`).
- Fresh starts always run through `FINALIZEBOOT` → `mo.enter` → intro cutscene
  path, ensuring deterministic initial state.

## Cutscene Wiring
- `_system` pulls in `_cut_scenes.lua` during boot and exposes a global
  `cut_scene` table that acts as a namespace for every movie sequence and its
  skip handler (`extracted/DATA000/_system.decompiled.lua:2792`). The table is
  hydrated by the compiled `_cut_scenes.lua` bytecode plus the year-specific Lua
  files that bolt on story beats (for example `year_1.decompiled.lua:8`).
- `SHOWLOGO` immediately starts the `cut_scene.logos` coroutine so the LucasArts
  and Double Fine movies play before any menu appears. The sequence lives inside
  the `_cut_scenes.lua` bytecode: it wraps the movie in
  `START_CUT_SCENE`/`END_CUT_SCENE`, posts a skip override, and only returns to
  `SHOWLOGO` once the movies (or their skip handler) complete
  (`extracted/DATA000/_system.decompiled.lua:2916`).
- Once the logos finish, `SHOWLOGO` continues the boot pipeline: it waits for
  the global save system to settle, checks for a resume slot, and either queues
  `Load(slot, TRUE)` or launches `BOOTTWO` to begin sourcing the rest of the Lua
  content. The loading menu is reopened if the cutscene closed it early.
- Every cutscene follows the same contract. `START_CUT_SCENE`/`END_CUT_SCENE`
  maintain a `cutSceneLevel` counter, temporarily disable the cameraman/input
  scripts, and optionally turn off head tracking when a scene specifies
  `"no head"` (`extracted/DATA000/_system.decompiled.lua:1812`).
- Cutscenes install skip behaviour by calling `set_override(...)` with a paired
  override function. When the player presses Escape the override tears down the
  movie, restores Manny's pose, and resumes gameplay (`extracted/DATA000/year_1.decompiled.lua:8`).
- The `cutscene_menu` helper records which movies the player has unlocked so
  they can be replayed later from the menus; each cutscene enables its entry via
  `cutscene_menu:enable_cutscene(...)` right before calling
  `RunFullscreenMovie(...)`.
- Long-form scenes often chain back into ordinary scripts (e.g. `start_script(
  lr.walk_in)`) so the same Lua scheduler drives both cinematics and in-room
  follow-ups. A Rust host must be able to resume these queued scripts once the
  movie finishes.

## Implications for the Rust Rewrite
- We need engine-side equivalents for the systems that Lua expects:
  - Registry-backed key/value store for persistent flags (`ReadRegistryValue`,
    `WriteRegistryValue`).
  - Asset loader that can stream LAB resources in the same order as
    `source_all_set_files()` does today.
  - Actor/Set abstractions that expose the same methods used throughout the Lua
    (`Set:switch_to_set`, `Actor:set_selected`, `Load`, `start_script`, etc.).
- Boot orchestration can remain data-driven if the Rust runtime embeds Lua and
  reuses the scripts. Alternatively, we can port these scripts to Rust once the
  runtime faithfully models their side effects.
- For early milestones we should target parity with:
  1. Executing `_system` and its dependencies.
  2. Driving the flow through `BOOT` → `FINALIZEBOOT` and observing Manny's
     office in a headless diagnostic mode (e.g., verifying object tables and
     actor placement without rendering yet).
  3. Triggering `cut_scene.intro` to ensure cutscene hooks are wired even before
     the full renderer exists.

This snapshot should anchor the next wave of work: building minimal Rust
equivalents for actors, sets, and the boot pipeline while continuing to use the
original Lua as executable documentation.


---

The `grim_analysis` crate currently simulates this boot sequence and reports
which Lua files are touched during startup. `cargo run --manifest-path
grim_analysis/Cargo.toml` now also summarizes each room's `enter`/`set_up_*`
handlers—highlighting the actors they spawn and the object methods they invoke—
so we can plan Rust equivalents without spelunking through Lua in the debugger.
