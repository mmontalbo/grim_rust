# Scene Module Overview

`grim_viewer/src/scene` turns the serialized exports produced by
`grim_engine` into a single `ViewerScene` data structure that the renderer can
consume. Everything in this directory focuses on normalising Manny's office
telemetry so overlays line up with the decoded background plate and the runtime
HUD, while keeping the rest of the crate agnostic of the raw JSON layouts.

## Key Responsibilities
- **Timeline ingestion (`mod.rs`):** Parses the boot timeline manifest,
  collects actors/objects created during replay, and records which hooks or Lua
  scripts spawned them. The same module attaches optional movement traces and
  hotspot events (plus optional geometry for validation) so downstream code does
  not need to worry about file formats, capitalization quirks, or
  Manny-office-only filtering rules.
- **Movement analysis (`movement.rs`):** Summarises Manny's captured path into
  total distance, sector counts, and frame-to-position lookups. It also powers
  the keyboard-driven scrubber that highlights head-target markers and feeds the
  minimap legend.
- **Viewer scene model (`viewer_scene.rs`):** Provides the canonical
  `ViewerScene`, including helper methods that the UI uses to project markers
  into plate space and to synchronise the minimap, timeline, and entity focus.
  This is also where Manny-office entity pruning lives so the renderer only sees
  the actors/objects we actually care about during milestone validation.

## Key Types
- `ViewerScene`: Shared by the renderer, overlay updates, and minimap; exposes
  accessors for camera data, entity markers, movement traces, and hotspot events.
- `MovementTrace` / `MovementScrubber`: Provide summarised movement metadata and
  navigation helpers so `viewer::state` can surface Manny's path without knowing
  about raw samples.
- `LuaGeometrySnapshot`: Lightweight bridge for Lua-side entity poses, used to
  sanity-check the capture pipeline when supplied by the CLI.

## Data Flow
1. `main.rs` requests `load_scene_from_timeline` when the user passes
   `--timeline` (and optional movement, hotspot, or geometry arguments).
2. The loader resolves the referenced assets from the manifest, hydrates a
   `ViewerScene`, and trims entities to the Manny office focus so noisy props do
   not clutter the HUD. It fails fast if any required transform data is missing,
   and uses optional geometry snapshots purely as a consistency check.
3. `ViewerState` receives the `ViewerScene` and immediately exposes its bounds
   to the minimap, its camera parameters to the marker projection code, and its
   movement/hotspot data to the scrubber overlays. Marker batches in
   `viewer::markers` draw directly from these fields.

By keeping all timeline-related parsing here, the rest of the crate only deals
with strongly-typed Rust structures instead of ad-hoc JSON.
