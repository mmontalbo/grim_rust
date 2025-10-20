#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
    cmd="cargo run -p grctl --"
    if [[ $# -gt 0 ]]; then
        cmd+=" $(printf '%q ' "$@")"
    fi
    exec nix-shell --run "$cmd"
fi

exec cargo run -p grctl -- "$@"
