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
  `-f`) to stream updates continuously.
- `check intro-resume` and `check engine-overlay` execute the validation scripts
  with a default 90s guard.

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

## Design notes

- Launchers open append-only log files and run a background reaper that removes
  state and records the process exit code.
- Scenario and check wrappers still defer to the Python scripts in `tools/` so
  we retain their existing handshake logic while providing a uniform entrypoint.
- All orchestration commands accept `--timeout`. Passing `--timeout 0` disables
  the guard when longer sessions are unavoidable.

## Known gaps / follow-up ideas

1. Scenario orchestration is currently disabled; when we revisit it, porting the
   Python drivers into Rust should help unify process supervision.
2. The retail launcher currently shells out to `tools/run_dev_install.sh`. A
   native implementation could unify timeout handling and telemetry setup.
3. Consider teeing component logs to stdout during startup to complement the
   `grctl logs --follow` workflow for quicker feedback.
4. There is no `resume` tracking for processes restarted outside `grctl`.
   Downstream tooling could expose hooks that opt-in components call on clean
   shutdown so we can detect intentional exits vs. crashes.
5. Configuration is hard-coded; exposing a config file (e.g. target ports) will
   make it easier to coordinate multiple developers or automated runs.

These items can be filed as incremental follow-ups once we stabilise the
initial interface.
