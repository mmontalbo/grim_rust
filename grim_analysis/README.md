# Grim Analysis

`grim_analysis` maps the Lua boot scripts shipped with *Grim Fandango* into
structured data that the rest of this workspace can consume. It treats the
extracted `DATA000` bundle as source material, normalizes the legacy Lua 3
syntax, and builds reports that describe how the retail engine brings Manny's
office online.

## Role in the Recreation
- **Decode boot flow:** Walks the Lua startup sequence, resolves `source_all`/
  `MakeCurrentSet` behavior, and records the order in which rooms, actors, and
  subsystems come alive.
- **Provide machine-friendly manifests:** Exposes the boot timeline, registry
  mutations, and subsystem simulations as JSON so downstream tools do not have
  to scrape terminal logs.
- **Surface dependencies:** Tracks which scripts, LAB assets, and hooks are
  required for the Manny office milestone, keeping the broader effort focused on
  first-playable boot coverage.
- **Retail telemetry bridge:** Hosts the retail instrumentation scaffold under
  `retail_capture/` so live captures stay versioned alongside the analysis
  passes they inform.

## Key Concepts
- **Resource graph:** Represents every extracted Lua file plus set metadata and
  lets callers resolve include paths without reaching back into the original
  executable.
- **Boot timeline:** Aggregates boot stages with hook ordering, the actors they
  spawn, subsystem writes, and queued cutscenes. Both the engine host and the
  viewer consume this structure.
- **Function simulation:** Runs a static analysis pass over hook bodies to label
  side effects (geometry toggles, inventory updates, movie requests) even when
  we do not execute the Lua yet.

## Typical Usage
- `cargo run -p grim_analysis -- --timeline-json out.json` to produce the boot
  timeline and inspect where Manny's office becomes the active set.
- Link the crate as a library to reuse `ResourceGraph`, `BootTimeline`, and
  simulation helpers from other binaries.

## Extending the Crate
- Add new classifiers in `simulation.rs` when a hook drives behavior that we
  currently label as "unknown"—this keeps the milestone telemetry actionable.
- Tighten fixture coverage in `tests/` whenever the JSON structure changes; the
  engine and viewer rely on stable manifests.
- Keep documentation and comments in American English, and prefer ASCII to stay
  friendly to the tooling chain.
