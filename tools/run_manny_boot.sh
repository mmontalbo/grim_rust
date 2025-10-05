#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: tools/run_manny_boot.sh [viewer-arg ...]

Runs the Manny's office Lua boot inside grim_engine with audio logging enabled
and launches grim_viewer pointed at the freshly captured timeline and audio log.
Any additional arguments are forwarded to grim_viewer (for example --headless
or --asset overrides).
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

VIEWER_ARGS=("$@")
NEEDS_DEFAULT_ASSET=1
for arg in "${VIEWER_ARGS[@]}"; do
    if [[ "$arg" == "--asset" ]]; then
        NEEDS_DEFAULT_ASSET=0
        break
    fi
done
if [[ ${NEEDS_DEFAULT_ASSET} -eq 1 ]]; then
    VIEWER_ARGS+=("--asset" "mo_0_ddtws.zbm")
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

RUN_CACHE="${REPO_ROOT}/artifacts/run_cache"
mkdir -p "${RUN_CACHE}"

AUDIO_LOG="${RUN_CACHE}/manny_audio_log.json"
TIMELINE_JSON="${RUN_CACHE}/manny_timeline.json"

echo "[run_manny_boot] Generating Manny timeline via grim_engine analysis..."
cargo run --bin grim_engine -- \
    --timeline-json "${TIMELINE_JSON}"

echo "[run_manny_boot] Bootstrapping grim_engine Lua runtime for audio capture..."
cargo run --bin grim_engine -- \
    --run-lua \
    --audio-log-json "${AUDIO_LOG}"

echo "[run_manny_boot] Launching grim_viewer with audio overlay..."
cargo run -p grim_viewer -- \
    --timeline "${TIMELINE_JSON}" \
    --audio-log "${AUDIO_LOG}" "${VIEWER_ARGS[@]}"
