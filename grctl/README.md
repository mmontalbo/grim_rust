# grctl

`grctl` is a BusyBox-style control utility that supervises the Grim Fandango
study stack. It centralises the ad-hoc tooling in `tools/` so the engine and
retail binary share the same lifecycle and logging conventions.

## Quick start

Run `grctl` from the repository root inside `nix-shell`:

```bash
nix-shell --run 'cargo run -p grctl -- status'
```

Common subcommands:

- `compare boot [--run-id <tag>] [--engine-release] [--engine-headless] [--retail-vanilla] [--retail-no-timeout]`
  starts grim_engine and retail together with a shared run id and prints the log
  paths for both.
- `engine start|stop|status|logs` and `retail start|stop|status|logs` are still
  available. Pass `--run-id <tag>` on `start` to control the telemetry run id,
  or rely on the generated default. Use `--attach` on `start` to follow the log
  immediately. `logs` accepts `--run latest|<id>`; the default is `latest`.
  A minimal manual flow is `grctl engine start --run <id> --attach` then
  `grctl retail start --run <id> --attach` so both logs share the same run id.

All child processes are launched with a generated session id and a consistent
environment:

```
GRCTL_MANAGED=1
GRCTL_SESSION_ID=<uuid>
GRCTL_COMPONENT=<component label>
GRCTL_LOG_PATH=<per-run log path>
GRCTL_STATE_DIR=<target/grctl/state>
GRIM_TRACE_RUN_ID=<run id injected into grim_engine and the retail shim>
```

State files live in `target/grctl/state/*.json`. Logs are per-run at
`target/grctl/logs/<component>/<run_id>.log`, and `target/grctl/logs/<component>.log`
is a convenience symlink to the newest run. `grctl` clears stale state
automatically when it detects that a recorded PID has already exited.

## Live parity watch

- `watch intro-timeline [--engine-log <path>] [--retail-events <path>] [--poll-interval-ms <ms>] [--from-end]` tails `target/grctl/logs/grim_engine.log` (symlink to the latest run) and `dev-install/mods/telemetry_events.jsonl`, parses `intro.timeline` events, and prints a rolling missing/extra/order summary.
- Example: `nix-shell --run 'cargo run -p grctl -- watch intro-timeline --poll-interval-ms 500'`.
- `watch intro-timeline --launch [--engine-release]` clears the intro logs, starts grim_engine headless with verbose logging, launches the retail capture without a timeout, and then begins the watch. Press Ctrl-C to stop the watch and shut down the launched components.

## Retail helpers

- `grctl retail copy [--source <path>] [--force]` copies the Steam install into
  `dev-install/`. Point `--source` at an alternate directory when Steam lives in
  a non-default location.
- `grctl retail start --vanilla` skips the LD_PRELOAD shim when you need to
  compare against a pristine retail boot. The default launch path always
  preloads the shim once it is built.
## Design notes

- Launchers open per-run log files and run a background reaper that removes
  state and records the process exit code. A symlink in `target/grctl/logs/`
  always points to the most recent run for quick tails.
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
