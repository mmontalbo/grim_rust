# Intro Timeline Comparison

This document explains how the scenario harness verifies that the Rust engine
and the retail capture produce the same `intro.timeline` events during the intro
movie.

## Data sources

- `grim_engine` emits JSON lines (matching the retail telemetry schema) such as
  `{"label":"intro.timeline","data":{"event":"movie.logos.start"}}` whenever it
  enters or exits one of the fullscreen videos. `grctl watch intro-timeline`
  clears `target/grctl/logs/grim_engine.log` before every launched session so
  the log only contains fresh data.
- `tools/live_retail_capture` writes a JSONL stream to
  `dev-install/mods/telemetry_events.jsonl` with the same shape: `label` set to
  `intro.timeline` and `data.event` holding the specific marker
  (for example `movie.intro.end`).

`grctl watch intro-timeline` tails both files and builds a structured diff.

## Running the comparison

1. Build `grctl` (`cargo build -p grctl`).
2. Launch the intro timeline watch with managed components:

   ```bash
   nix-shell --run "cargo run -p grctl -- watch intro-timeline --launch"
   ```

   The helper clears prior intro logs, starts a headless `grim_engine` with
   verbose logging plus the retail capture, and tails both sources until you
   press Ctrl-C.

3. Inspect the rolling summary printed during the watch. Matching timelines
   produce `intro timeline matches across engine and retail` once all markers
   land.

4. If you already have intro logs captured, point the watch at them instead:

   ```bash
   nix-shell --run 'cargo run -p grctl -- watch intro-timeline \
     --engine-log <path/to/grim_engine.log> \
     --retail-events dev-install/mods/telemetry_events.jsonl'
   ```

   Use `--from-end` if you want the watch to start from the existing tail of
   each file instead of processing everything from the beginning.

## Report schema

The intro watch prints a summary for every poll and keeps the latest diff in
memory. When you need a structured dump for debugging, the helpers in
`grctl/grim_scenarios/src/timeline.rs` still expose a serialisable report
shape:

```json
"intro_timeline": {
  "engine_events": ["movie.logos.start", "movie.logos.end", "movie.intro.start", "..."],
  "retail_events": ["movie.logos.start", "movie.logos.end", "movie.intro.start", "..."],
  "missing_in_engine": [],
  "missing_in_retail": [],
  "order_matches": true
}
```

- `engine_events` – ordered list of every `intro.timeline` log emitted by
  `grim_engine`.
- `retail_events` – ordered list of retail telemetry events with the same label.
- `missing_in_engine` / `missing_in_retail` – elements that appear in one list
  but not the other (duplicates are counted accurately).
- `order_matches` – `true` when both lists are identical; `false` when the same
  events appear but in a different order.

When either list is empty, the harness warns in stdout so you can immediately
spot capture/logging regressions.

## Extending the comparison

- The watch loop lives in `grctl/src/main.rs` under `run_intro_timeline_loop`.
  Extend it if you need additional validations (for example timestamp deltas or
  stricter sequencing).
- The structured helpers in `grctl/grim_scenarios/src/timeline.rs` are still
  available if you want to persist comparison reports alongside other scenario
  artifacts in the future.
