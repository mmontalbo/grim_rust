# Runtime Smoke Tests

The Lua-hosted smoke tests described in earlier revisions of this document no
longer exist. `grim_engine` has been trimmed down to a minimal intro playback
binary that only drives the intro and exits; the viewer handshake and
GrimStream state updates are gone.

For the current milestone:

- Use `grctl scenario run intro-to-office-computer` to drive the headless intro
  and confirm the expected `movie.*` markers land in the engine log.
- Use `grctl watch intro-timeline --launch` when you need to compare the engine
  `intro.timeline` telemetry against the retail capture. Streaming overlays are
  gone, so the viewer pane stays in placeholder mode.
- Rely on commit history if you need to resurrect the old timeline/hotspot demo
  captures; none of those CLI flags are available in the trimmed binary.

This page stays in the tree as a pointer for anyone searching for the old flow.
Update it once we reintroduce broader runtime coverage.
