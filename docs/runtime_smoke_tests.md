# Runtime Smoke Tests

The Lua-hosted smoke tests described in earlier revisions of this document no
longer exist. `grim_engine` has been trimmed down to a minimal intro playback
binary that only supports the viewer handshake and GrimStream state updates.

For the current milestone:

- Use `grctl scenario run intro-to-office-computer --with-viewer` and confirm
  the intro stream flows end-to-end. Add `--with-retail` only when you need the
  retail capture pane.
- Rely on commit history if you need to resurrect the old timeline/hotspot demo
  captures; none of those CLI flags are available in the trimmed binary.

This page stays in the tree as a pointer for anyone searching for the old flow.
Update it once we reintroduce broader runtime coverage.
