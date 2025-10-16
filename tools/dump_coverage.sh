#!/usr/bin/env bash
set -euo pipefail

ARTIFACT_DIR=${ARTIFACT_DIR:-artifacts}
mkdir -p "${ARTIFACT_DIR}"

RUST_COVERAGE_JSON="${ARTIFACT_DIR}/rust_coverage_extended.json"
COVERAGE_DIFF_JSON="${ARTIFACT_DIR}/rust_vs_retail_coverage_extended.json"
MOVEMENT_LOG_JSON="${ARTIFACT_DIR}/movement_extended.json"
AUDIO_LOG_JSON="${ARTIFACT_DIR}/hotspot_audio_extended.json"
EVENT_LOG_JSON="${ARTIFACT_DIR}/hotspot_events_extended.json"

ENGINE_CMD=(
  "cargo run -p grim_engine -- --run-lua"
  "--movement-demo"
  "--hotspot-demo computer"
  "--coverage-json ${RUST_COVERAGE_JSON}"
  "--movement-log-json ${MOVEMENT_LOG_JSON}"
  "--audio-log-json ${AUDIO_LOG_JSON}"
  "--event-log-json ${EVENT_LOG_JSON}"
)

ANALYSIS_CMD=(
  "cargo run -p grim_analysis --"
  "--data-root extracted/DATA000"
  "--coverage-counts ${RUST_COVERAGE_JSON}"
  "--coverage-summary-json ${COVERAGE_DIFF_JSON}"
)

echo "[dump_coverage] running engine capture"
nix-shell --command "${ENGINE_CMD[*]}"

echo "[dump_coverage] comparing against catalog"
nix-shell --command "${ANALYSIS_CMD[*]}"

echo "[dump_coverage] coverage counts: ${RUST_COVERAGE_JSON}"
echo "[dump_coverage] coverage diff:  ${COVERAGE_DIFF_JSON}"
