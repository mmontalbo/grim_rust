# Work in Progress

## Current Direction
- Boot simulation now stubs Manny's desk cut-scene so the Lua host logs memo retrieval and computer interactions without the original precompiled scripts.
- Viewer now renders the audio overlay direct from `--audio-log-json`; the next milestone focuses on projecting boot timeline metadata alongside the cue stream.

## Active Threads
- Geometry-driven head targeting now records real sector hits; upcoming work focuses on surfacing those cues to downstream tooling.
- Timeline overlay plumbing is queued so the viewer can surface hook sequencing while geometry instrumentation continues to evolve.

## Next Steps
1. Project Manny's office boot timeline overlays into the viewer to close out milestone 1 instrumentation.
2. Feed the overlay with hook sequencing and selection affordances so geometry and timeline views stay in sync.

## Workstreams

### viewer_timeline_overlay
Timeline overlay instrumentation

```
Objective: layer Manny's office boot timeline metadata into grim_viewer so hook sequencing and entity focus appear directly in the HUD. Consume the existing --timeline JSON manifest, project stage labels and hook indices alongside the marker grid, and let ←/→ cycling highlight the corresponding overlay entry. Preserve behaviour when --timeline is absent. Document the flag pairing in docs/startup_overview.md, add targeted unit coverage for any timeline parsing helpers, and run cargo fmt && cargo test -p grim_viewer before handing off.
```
