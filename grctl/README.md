# grctl

`grctl` is a BusyBox-style control utility that supervises the Grim Fandango
study stack. It centralises the ad-hoc tooling in `tools/` so the viewer,
engine, and retail binary share the same lifecycle and logging conventions.

## Quick start

Run `grctl` from the repository root inside `nix-shell`:

```bash
nix-shell --run 'cargo run -p grctl -- status'
```

Common subcommands:

- `engine`, `viewer`, and `retail` each support `start`, `stop`, `status`, and
  `logs`. Pass `--tail <n>` to control the initial output and `--follow` (or
  `-f`) to stream updates continuously. Use `--attach` on `engine start` or
  `retail start` to tee the log to your terminal right after launch.
- `scenario run <name>` launches an end-to-end harness; see below for usage tips.

All child processes are launched with a generated session id and a consistent
environment:

```
GRCTL_MANAGED=1
GRCTL_SESSION_ID=<uuid>
GRCTL_COMPONENT=<component label>
GRCTL_LOG_PATH=<log file destination>
GRCTL_STATE_DIR=<target/grctl/state>
```

State files live in `target/grctl/state/*.json` and logs in
`target/grctl/logs/*.log`. `grctl` clears stale state automatically when it
detects that a recorded PID has already exited.

## Live parity watch

- `watch intro-timeline [--engine-log <path>] [--retail-events <path>] [--poll-interval-ms <ms>] [--from-end]` tails `target/grctl/logs/grim_engine.log` and `dev-install/mods/telemetry_events.jsonl`, parses `intro.timeline` events, and prints a rolling missing/extra/order summary.
- Example: `nix-shell --run 'cargo run -p grctl -- watch intro-timeline --poll-interval-ms 500'`.
- `watch intro-timeline --launch [--engine-release]` clears the intro logs, starts grim_engine headless with verbose logging, launches the retail capture without a timeout, and then begins the watch. Press Ctrl-C to stop the watch and shut down the launched components.

## Scenario runs

- Use `grctl scenario run intro-to-office-computer` (or `intro-to-office-tube`)
  for the intro playback. The harness now runs grim_engine headless with verbose
  logging and waits for the `movie.logos.*` / `movie.intro.*` timeline markers
  the engine emits as it simulates fullscreen playback.
- `--hold-seconds <seconds>` keeps tailing the log briefly after all markers
  land. The engine exits once the intro cutscene completes, so the extra wait is
  primarily for log collection.
- `--detach` leaves grim_engine running under grctl supervision. Stop it later
  with `grctl scenario stop` or `grctl engine stop` once you are done inspecting
  the session.
- `--artifacts-dir <path>` writes the scenario JSON report to a directory of your
  choosing. The `intro_timeline` field is currently empty because the live
  streaming pipeline was removed; only the observed markers are recorded.
- GrimStream exposure and the viewer handshake were trimmed from grim_engine, so
  streaming overlays are no longer available during scenario runs.
- `grctl retail start --vanilla` skips the Lua hook/LD_PRELOAD shim so you can
  compare a clean retail run against the instrumented build. Leave the flag off
  (default) to keep telemetry events flowing into the scenario harness.

## Retail helpers

- `grctl retail copy [--source <path>] [--force]` copies the Steam install into
  `dev-install/`. Point `--source` at an alternate directory when Steam lives in
  a non-default location.
- `grctl retail start --vanilla` skips the LD_PRELOAD shim when you need to
  compare against a pristine retail boot. The default launch path always
  preloads the shim once it is built.
## Design notes

- Launchers open append-only log files and run a background reaper that removes
  state and records the process exit code.
- Scenario wrappers run under `cargo run` so the shared logging and timeout
  handling stay consistent across runs.
- All orchestration commands accept `--timeout`. Passing `--timeout 0` disables
  the guard when longer sessions are unavoidable.

## Known gaps / follow-up ideas

1. Extend the scenario harness beyond the intro playback while continuing to
   consolidate shared setup so new runs stay lightweight.
2. Consider teeing component logs to stdout during startup to complement the
   `grctl logs --follow` workflow for quicker feedback.
3. There is no `resume` tracking for processes restarted outside `grctl`.
   Downstream tooling could expose hooks that opt-in components call on clean
   shutdown so we can detect intentional exits vs. crashes.
4. Configuration is hard-coded; exposing a config file (e.g. target ports) will
   make it easier to coordinate multiple developers or automated runs.

These items can be filed as incremental follow-ups once we stabilise the
initial interface.
