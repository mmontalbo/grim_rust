# Intro Timeline Comparison

This document explains how the scenario harness verifies that the Rust engine
and the retail capture produce the same `intro.timeline` events during the intro
movie.

## Data sources

- `grim_engine` emits JSON lines (matching the retail telemetry schema) such as
  `{"label":"intro.timeline","data":{"event":"movie.logos.start"}}` whenever it
  enters or exits one of the fullscreen videos. The scenario harness resets
  `target/grctl/logs/grim_engine.log` before every run so the log only contains
  the current session.
- `tools/live_retail_capture` writes a JSONL stream to
  `dev-install/mods/telemetry_events.jsonl` with the same shape: `label` set to
  `intro.timeline` and `data.event` holding the specific marker
  (for example `movie.intro.end`).

`grim_scenarios` reads both files and builds a structured diff.

## Running the comparison

1. Build `grctl` (`cargo build -p grctl`).
2. Launch the intro scenario with retail capture enabled:

   ```bash
   nix-shell --run "cargo run -p grctl -- scenario run intro-to-office-computer \
     --with-viewer --with-retail --artifacts-dir target/grctl/scenario_reports"
   ```

   The helper clears the telemetry file, runs the intro playback on both
   binaries, and stops once the required markers land (or the timeout expires).

3. Inspect the summary printed at the end of the run. Matching timelines produce
   a one-line success message (`intro timeline matches across engine and retail`).

4. Review the full JSON report written to the artifacts directory for deeper
   debugging.

For a quick live view without the full scenario harness, you can also run:

```bash
nix-shell --run 'cargo run -p grctl -- watch intro-timeline --launch --with-viewer'
```

This clears the intro sources, starts a headless `grim_engine` (or streams to
`grim_viewer` when `--with-viewer` is provided), launches the retail capture
with no timeout, and tails both JSON feeds until you press Ctrl-C.

## Report schema

Each scenario report stores the diff under the `intro_timeline` key:

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

- Update `INTRO_RETAIL_REQUIRED_EVENTS` in
  `grctl/grim_scenarios/src/main.rs` if new intro events must be enforced before
  the scenario completes.
- The comparison helpers live in `grctl/grim_scenarios/src/timeline.rs`. Keep
  additional validations there (for example timestamp deltas) so the main
  scenario logic stays focused on orchestration.
- Add new scenario variants by extending `ScenarioKind` and pointing them at the
  shared helpers to reuse the existing artifact and summary flow.
