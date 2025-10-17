#!/usr/bin/env bash
# Helper for launching the dev-install retail build with the telemetry shim.
# Wraps the existing dev-install/run.sh under steam-run and applies an optional
# timeout so captures don't hang indefinitely.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEV_INSTALL_DIR="${REPO_ROOT}/dev-install"
DEV_RUN_SCRIPT="${DEV_INSTALL_DIR}/run.sh"

if [[ ! -x "${DEV_RUN_SCRIPT}" ]]; then
  echo "[run_dev_install] expected executable at ${DEV_RUN_SCRIPT}" >&2
  exit 1
fi

if ! command -v steam-run >/dev/null 2>&1; then
  echo "[run_dev_install] steam-run not found on PATH; install nixpkgs.steam-run or adjust PATH" >&2
  exit 1
fi

TIMEOUT="20s"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --timeout)
      if [[ $# -lt 2 ]]; then
        echo "[run_dev_install] --timeout expects a value" >&2
        exit 1
      fi
      TIMEOUT="$2"
      shift 2
      ;;
    --no-timeout)
      TIMEOUT=""
      shift
      ;;
    *)
      break
      ;;
  esac
done

if [[ -n "${TIMEOUT}" ]]; then
  exec timeout "${TIMEOUT}" steam-run "${DEV_RUN_SCRIPT}" "$@"
else
  exec steam-run "${DEV_RUN_SCRIPT}" "$@"
fi
