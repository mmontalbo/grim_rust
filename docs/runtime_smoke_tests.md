# Runtime Smoke Tests

This guide captures the minimal loop for exercising the Manny office runtime
demo so new contributors can validate changes without spelunking through the
code first. Follow it any time you need to check that the Lua host still drives
the computer hotspot end-to-end.

## Prerequisites

- The retail assets must be extracted into `extracted/` (run
  `tools/sync_assets.sh` if you have not done this yet).
- Build artifacts from `grim_engine` must be available (`cargo build -p
  grim_engine` or any command that compiles the crate).

## Running the demo

```bash
cargo run -p grim_engine -- --run-lua --hotspot-demo computer \
  --movement-demo --movement-log-json tools/tests/movement_log.json \
  --audio-log-json tools/tests/hotspot_audio.json
```

Key flags:

- `--run-lua` switches the engine into runtime mode.
- `--hotspot-demo computer` walks Manny to his desk and executes the scripted
  interaction.
- `--movement-demo` records Manny's trajectory; `--movement-log-json` stores
  the per-frame samples (position, yaw, walk-sector hits).
- `--audio-log-json` captures the SFX/music events emitted by the run.

The command prints a transcript to stdout that includes `hotspot.demo.start`/
`hotspot.demo.end` markers alongside dialogue/cutscene logs. The JSON artefacts
are written to the paths you supplied—place them under `tools/tests/` when you
want to share them with the regression harness.

## Inspecting artefacts

- **Movement log (`movement_log.json`)** – an array of `{frame, position,
  yaw, sector}` samples that the movement regression test ingests.
- **Audio log (`hotspot_audio.json`)** – a list of objects describing each
  `SfxPlay`, `SfxStop`, `MusicPlay`, and `MusicStop` call that the runtime
  issued during the demo.
- **Stdout transcript** – includes `dialog.begin manny /moma112/` style markers
  and any geometry/debug events the Lua host recorded. Redirect it to a file if
  you need to diff runs across branches.

## Hooking into the regression harness

`grim_engine/tests/hotspot_demo.rs` exercises the same CLI path. Updating the
files above gives you a quick manual smoke test; running

```bash
cargo test -p grim_engine -- hotspot_demo
```

verifies that the automated check still sees the typed dialogue and keyboard
SFX cues. When you extend the demo (for example, to cover another hotspot),
re-run the command and update the test expectations together so future
contributors can rely on the regression.

