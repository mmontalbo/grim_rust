#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: tools/run_manny_boot.sh [viewer-arg ...]

Runs the Manny's office Lua boot inside grim_engine and captures the baseline
artefacts (timeline, audio, movement, depth stats, hotspot events) before
launching grim_viewer against the fresh snapshot. Any additional arguments are
forwarded to grim_viewer (for example --headless or --asset overrides).
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

TIMELINE_JSON="${RUN_CACHE}/manny_timeline.json"
AUDIO_LOG="${RUN_CACHE}/manny_audio_log.json"
MOVEMENT_LOG="${RUN_CACHE}/manny_movement_log.json"
DEPTH_STATS_JSON="${RUN_CACHE}/manny_depth_stats.json"
EVENT_LOG_JSON="${RUN_CACHE}/manny_hotspot_events.json"

echo "[run_manny_boot] Generating Manny timeline via grim_engine analysis..."
cargo run --bin grim_engine -- \
    --timeline-json "${TIMELINE_JSON}"

echo "[run_manny_boot] Bootstrapping grim_engine Lua runtime for hotspot capture..."
cargo run --bin grim_engine -- \
    --run-lua \
    --movement-demo \
    --movement-log-json "${MOVEMENT_LOG}" \
    --hotspot-demo computer \
    --audio-log-json "${AUDIO_LOG}" \
    --depth-stats-json "${DEPTH_STATS_JSON}" \
    --event-log-json "${EVENT_LOG_JSON}"

echo "[run_manny_boot] Launching grim_viewer with hotspot overlays..."
cargo run -p grim_viewer -- \
    --timeline "${TIMELINE_JSON}" \
    --audio-log "${AUDIO_LOG}" \
    --movement-log "${MOVEMENT_LOG}" \
    --event-log "${EVENT_LOG_JSON}" "${VIEWER_ARGS[@]}"
