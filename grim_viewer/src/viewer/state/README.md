# Viewer State Overview

`grim_viewer/src/viewer/state` owns the live runtime state for the viewer. Once
`main.rs` finishes loading assets and timeline data, it hands everything to this
module so the wgpu pipeline, overlays, and input handling stay in sync. Think of
this directory as the glue between scene data and pixels on screen.

## What Lives Here
- **`init.rs`:** Asynchronously constructs the wgpu device/surface, uploads the
  decoded bitmap into GPU memory, seeds marker buffers, and builds the initial
  UI layout. It also builds the camera projector from the `ViewerScene` so the
  first render already aligns markers with the plate.
- **`layout.rs`:** Responds to window resizes by recalculating the swapchain
  configuration and refreshing panel rectangles from `UiLayout`.
- **`overlay_updates.rs`:** Updates the text overlays (timeline, scrubber,
  audio) whenever the scene or audio log watcher reports new information.
- **`render.rs`:** Issues the draw calls: renders the plate quad, emits marker
  instance batches, composites the minimap, and blits HUD overlays to the
  swapchain surface.
- **`selection.rs`:** Handles keyboard shortcuts for cycling between entities
  and stepping through Manny's movement samples so the overlays reflect the
  current focus. It also notifies `overlay_updates` when the selection changes.

## Interaction With The Rest Of The Crate
1. `main.rs` builds an `Arc<ViewerScene>` and optional audio log receiver, then
   calls `ViewerState::new` (implemented in `init.rs`).
2. The event loop forwards winit input events to `ViewerState` helpers,
   triggering selection or scrubber updates. The state module calls back into
   `scene::MovementScrubber` or `UiLayout` as needed.
3. Each frame, `ViewerState::render` combines decoded texture data, marker
   instances derived from the scene, minimap geometry, and overlay text into a
   single composited image. The result is what headless runs snapshot for
   regressions and what interactive users drive via keyboard input.

Centralising rendering concerns in this directory keeps the higher-level CLI
and scene modules focused on data acquisition rather than GPU state management.

## 3D Marker Proxies
- `render.rs` seeds three procedural meshes (sphere, cube, cone) and reuses
  them for world-space markers before decoded assets arrive. Manny's anchor uses
  the sphere so it pops against nearby props, and any timeline `Actor` entry
  would reuse that primitive.
- Timeline `Object` entities render as cubes, while `InterestActor` entities
  render as cones. This mirrors the classification in `scene::ViewerScene` so
  the viewer keeps exposing the engine's categories in 3D.
- Cones also appear for the tube anchor fallback, the gold selection pointer,
  and the axis gizmo arms; those helpers reuse the same primitive batch path.
- Overlap between shapes is expected: `scene::manny::apply_geometry_overrides`
  stamps identical transforms onto each object/interest-actor pair (cards,
  tube, Manny), so their cube and cone proxies stack until we swap in the
  decoded meshes.
