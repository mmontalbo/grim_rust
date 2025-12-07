# Lua <-> Engine API Surface Goals

This document tracks the native API we need to expose from the engine to the Lua VM while recreating the retail runtime. It treats the retail trace as ground truth and the `grim_engine` host as the implementation that must converge on that shape.

## Ground truth and signals
- Retail instrumentation lives in `grim_analysis` (the `grim_analysis` `LD_PRELOAD` hook) and emits `grim_telemetry_common::LuaEvent` lines with `engine=retail vm_id=lua32`.
- The Rust host logs the same schema from `grim_engine::lua_host::telemetry` (`engine=grim_engine vm_id=lua`). A matching event stream means our exposed surface and sequencing align with retail.
- The events that define the API surface are the registrations the engine makes into Lua: `registered_global`, `registered_constant`, `set_table_entry` (table population), `lua_setfallback` / `lua_settagmethod` (fallback and tag hook wiring), refs (`lua_ref`/`lua_getref`/`lua_unref`), and any helper pushes (`lua_push*`) that precede those registrations.
- Telemetry streams should be split into **semantic (composite) events** and **raw VM events**. Semantic events describe what scripts observe (e.g. “set table entry `system.camChangeHandler` to closure X”, “bind global foo”, “store/ref/lookup”), and drive parity. Raw VM events capture stack mechanics (push ordering, call conventions, GC) for debugging and for deriving semantic events from retail traces; parity should not hinge on these implementation details.

## What "API surface" means here
- All C closures, constants, and tables the engine inserts into Lua before user scripts run.
- The shape and population order of bootstrap tables (e.g. `system`, its `controls` table, bootstrap refs).
- Legacy Lua behaviours that affect scripts: fallback handlers (`setfallback`), tag methods (`settagmethod`, `gettagmethod`, `seterrormethod`, `tag`), and registry refs (`lua_ref`/`lua_getref`/`lua_unref`).
- Host-provided helpers that emulate engine systems (script scheduling, cutscene playback, IO stubs, platform/config helpers).
- Sequencing expectations (e.g. retail pushes `system`, stores it as ref 0, later `lua_getref` + `settable` to add `controls`; GC timing around bootstrap).

## Retail surface (as observed in traces)
- Lua VM bootstrap uses Lua 3.1/3.2-era primitives: fallback API (`setfallback`, `gettagmethod`, `settagmethod`, `seterrormethod`, `tag`), registry refs (`lua_ref` etc.), tag plumbing (`lua_copytagmethods`, `lua_settag`, `lua_settagmethod`), and legacy IO tags.
- Engine registers a set of globals early: core helpers (`type` shim, math/string aliases such as `strsub`, `strfind`, `sqrt`, `abs`), platform/config helpers (`GetPlatform`, registry read/write shims), input/control toggles, and a `break_here` hook.
- A `system` table is created and bound as a global; retail stores it as ref 0, fetches it again, and populates `controls`, native input handlers (`camChangeHandler`, `axisHandler`, `inputModeHandler`, `buttonHandler`), and other fields via `settable`. Subtables like `system.controls` and actor stubs (`currentActor` / `manny` functions) are populated during bootstrap.
- After installing the concat fallback, retail immediately `lua_ref`s the fallback itself (`typeFB`) and a bundle of text-property string constants (`x`, `y`, `cache`, `font`, `width`, `leftclip`, `height`, `fgcolor`, `bgcolor`, `fxcolor`, `hicolor`, `duration`, `center`, `ljustify`, `rjustify`, `layer`, `highlight`, `coords`, `volume`, `pan`, `background`, `alpha`, `fade`, `mirrormode`) for later `lua_getref` lookups.
- Menu/cutscene helpers are registered as globals (e.g. `cut_scene.logos`/`intro`, `loading_menu.run`, `boot_warning_menu.run`, `concept_menu.unlock_concepts`) along with movie control functions (`StartFullscreenMovie`, `RunFullscreenMovie`, `StartMovie`, `StopMovie`, `IsFullscreenMoviePlaying`, `IsMoviePlaying`, `hideSkipButton`, `showSkipButton`).
- Script control helpers (`start_script`, `single_start_script`, `wait_for_script`, `stop_script`, `GetCurrentScript`) are exposed so Lua can schedule and track threads via engine callbacks.
- Constants include `_VERSION` reported as "Lua 3.1 (alpha)", `_TRIGMODE`, sector/mode constants (`NONE`, `WALK`, `CAMERA`, `SPECIAL`, `HOT`), and IO globals (`_INPUT`, `_OUTPUT`, `_STDIN`, `_STDOUT`, `_STDERR`) tagged appropriately.

## Current `grim_engine` surface (mlua host)
- Uses `mlua` with `StdLib::ALL_SAFE`, installs `_VERSION` override and legacy IO shims with tags (-16/-17) plus IO globals and file-handle fallbacks.
- Implements legacy fallback API (`setfallback`, `gettagmethod`, `settagmethod`, `seterrormethod`, `tag`) and registry refs (`lua_ref`, `lua_getref`, `lua_unref`), logging tag/fallback plumbing to telemetry.
- Registers math/string aliases, platform/config helpers, input control toggles, registry access shims, and `break_here`; sector mode constants now match retail values (`NONE`=0, `WALK`=4096, `CAMERA`=8192, `SPECIAL`=12288, `HOT`=16384) alongside `PI`.
- Mirrors retail’s post-concat `lua_ref` burst so the concat fallback and text property string constants are stored as refs before other globals bind, keeping ref IDs and later `lua_getref` lookups aligned.
- Builds the `system` table, logs creation, stores it as ref 0, then retrieves and attaches `controls`, default handler closures for `camChangeHandler`/`axisHandler`/`inputModeHandler`/`buttonHandler`, `setTable`, and `currentActor` (with `set_selected` / `default` / `put_in_set` stubs) to mirror retail sequencing.
- Provides stubbed tables for prefs (`system_prefs`), cutscenes (`cut_scene.logos`/`intro`), loading/boot menus, concept unlocks, and `footsteps`; movie helpers drive the simplified fullscreen movie state machine.
- Boot stubs wrap script scheduling and playback helpers (`start_script`, `single_start_script`, `stop_script`, `wait_for_script`, movie functions) but currently no real script runner or asset streaming.
- `dofile` supports retail path variants and short-circuits special files (e.g. `_controls.lua`, menu scripts) to keep intro bootstrap moving while we fill in missing native behaviours.

## Compatibility goals and how to validate
- **Match registrations:** The set of `registered_global` / `registered_constant` names, upvalues, and push sequences in `grim_engine` should match the retail trace captured by `grim_analysis`.
- **Match table shape/order:** `set_table_entry` logs for `system` and other bootstrap tables should align (creation, ref/getref usage, and insertion order), including any GC that occurs between steps.
- **Match fallback/tag wiring:** Ensure `setfallback`/`settagmethod`/`seterrormethod` traffic and tag IDs mirror retail (including default handlers and tag copying).
- **Match ref usage:** `lua_ref`/`lua_getref`/`lua_unref` patterns (handles, lock values, labels) should line up so anonymous closures and tables are retrievable the same way scripts expect.
- **Behavioural parity:** Stubs should evolve into functional equivalents (script scheduling, cutscene playback, controls handling) while preserving the same API shape; telemetry provides sequencing checks, and script outcomes provide behavioural checks.

## Next steps
- Generate a fresh retail trace with `grim_analysis` to enumerate the authoritative set of globals/constants/tables at bootstrap (filter `event=registered_global|registered_constant|set_table_entry|lua_setfallback|lua_settagmethod|lua_ref`).
- Diff that list against `grim_engine`'s telemetry to spot missing registrations, mismatched upvalue counts, or order differences.
- Fill gaps in `grim_engine/src/lua_host/context/bindings` (add missing globals/tables, adjust sequencing, replace stubs with real engine calls) and re-run parity to confirm the event stream aligns.
- Keep this document updated as we add or retire bindings so future work knows which parts of the surface are authoritative vs. temporary stubs.

## Telemetry: semantic vs. raw (parity guidance)
- Parity should be judged on semantic events that encode the Lua-visible contract (bind global/constant, set table entry, store/fetch/unref, setfallback/tagmethod). Push ordering and VM calling conventions are raw details and should not break parity.
- For retail traces, derive semantic events in the parser by windowing around `lua_settable`/`lua_setglobal`/`lua_ref`/`lua_getref` and normalizing table/key/value/upvalue info; discard ordering quirks after extraction.
- For `grim_engine`, emit semantic events directly when we perform a binding; keep optional raw metadata only for debugging.
- Semantic composites log as `event=semantic_*` with `stream=semantic`; raw VM events carry `stream=raw`. `grctl parity logs` defaults to the semantic stream (`--raw` forces the old view) so push-order divergence no longer blocks parity checks.
- Composite binding/table/ref events exist only in the semantic stream; the raw stream now sticks to VM-level primitives (push/call/GC/tag/fallback/settable/ref) for debugging.
