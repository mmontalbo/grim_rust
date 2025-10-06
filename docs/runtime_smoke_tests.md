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

First capture the boot timeline manifest so the overlay baseline stays current:

```bash
cargo run -p grim_engine -- --timeline-json tools/tests/manny_office_timeline.json
```

Then run the Lua runtime demo to record the hotspot artefacts:

```bash
cargo run -p grim_engine -- --run-lua --hotspot-demo computer \
  --movement-demo --movement-log-json tools/tests/movement_log.json \
  --audio-log-json tools/tests/hotspot_audio.json \
  --depth-stats-json tools/tests/manny_office_depth_stats.json \
  --event-log-json tools/tests/hotspot_events.json
```

Key flags:

- `--timeline-json` writes the boot-stage manifest that powers the viewer's
  timeline overlay and regression comparisons.
- `--run-lua` switches the engine into runtime mode.
- `--hotspot-demo computer` walks Manny to his desk and executes the scripted
  interaction.
- `--movement-demo` records Manny's trajectory; `--movement-log-json` stores
  the per-frame samples (position, yaw, walk-sector hits).
- `--audio-log-json` captures the SFX/music events emitted by the run.
- `--depth-stats-json` snapshots the codec3 depth summary so the viewer can
  cross-check depth ranges without re-decoding assets. The capture stores the
  seeded checksum alongside the minimum/maximum depth values so overlays can
  confirm codec3 parity without re-decoding the bitmap each run.
- `--event-log-json` records the hotspot/head-target trace used by the viewer's
  movement overlay. Each entry carries the runtime sequence counter and, when
  available, the movement frame that triggered the event so geometry changes
  line up with Manny's position.

The command prints a transcript to stdout that includes `hotspot.demo.start`/
`hotspot.demo.end` markers alongside dialogue/cutscene logs. The JSON artefacts
are written to the paths you supplied—place them under `tools/tests/` when you
want to share them with the regression harness.

## Inspecting artefacts

- **Movement log (`movement_log.json`)** – an array of `{frame, position,
  yaw, sector}` samples that the runtime regression harness compares against
  the committed baseline.
- **Audio log (`hotspot_audio.json`)** – a list of objects describing each
  `SfxPlay`, `SfxStop`, `MusicPlay`, and `MusicStop` call that the runtime
  issued during the demo. The harness checks the captured sequence matches the
  recorded baseline exactly so unexpected audio diffs fail fast.
- **Timeline manifest (`manny_office_timeline.json`)** – the boot-stage and
  hook summary produced by `--timeline-json`; the regression harness keeps it
  in lockstep with the hotspot artefacts so the viewer overlay reflects the
  same snapshot.
- **Depth stats (`manny_office_depth_stats.json`)** – codec3 metadata for the Manny desk
  depth map (`mo_0_ddtws.zbm`). The JSON payload stores the image dimensions, seeded
  FNV-1a checksum, and a small histogram (minimum/maximum values, their hexadecimal
  forms, and zero/non-zero pixel counts). This lets regression runs confirm the depth
  decode matches the retail engine without sharing the full bitmap.
  Example capture:
    ```json
    {
      "asset": "mo_0_ddtws.zbm",
      "dimensions": [640, 480],
      "checksum_fnv1a": 2336104930108325863,
      "depth": {
        "min": 7,
        "max": 45045,
        "zero_pixels": 0,
        "nonzero_pixels": 307200
      }
    }
    ```
- **Hotspot event log (`hotspot_events.json`)** – selected runtime events
  (hotspot markers, set selections, Manny head-target updates, ignore-box
  toggles, dialogue prompts) annotated with the last seen movement frame and
  ordered by the runtime sequence counter. `grim_viewer` overlays these markers
  on the movement trace to highlight geometry interactions, and the regression
  harness fails if the structured log diverges from the baseline.
  Sample entries:
    ```json
    {
      "events": [
        {
          "sequence": 1136,
          "frame": 24,
          "label": "hotspot.demo.start computer"
        },
        {
          "sequence": 11419,
          "frame": 24,
          "label": "dialog.begin manny /moma112/"
        }
      ]
    }
    ```
- **Stdout transcript** – includes `dialog.begin manny /moma112/` style markers
  and any geometry/debug events the Lua host recorded. Redirect it to a file if
  you need to diff runs across branches.
- Launch the viewer with `python tools/grim_viewer.py run` once the artefacts
  are refreshed. The script now feeds in the Manny timeline, movement trace, and
  hotspot log by default, so the markers render in the recovered camera's
  perspective. Add `-- --headless` when you just want the textual summary
  without opening a window.

## Hooking into the regression harness

`grim_engine/tests/runtime_regression.rs` exercises the same CLI path while
asserting that fresh captures match the checked-in artefacts. Updating the
files above gives you a quick manual smoke test; running

```bash
cargo test -p grim_engine -- runtime_regression
```

verifies that the automated check still sees the typed dialogue, keyboard SFX
sequence, and Manny's walk path. When you extend the demo (for example, to
cover another hotspot), re-run the command and update the baselines together so
future contributors can rely on the regression.
