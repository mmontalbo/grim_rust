#!/usr/bin/env bash
# Collects a perf recording (and optional flamegraph) for trace_tui.
# Usage: tools/profile_trace_tui.sh path/to/log [path/to/other.log]

set -euo pipefail

if (( $# < 1 || $# > 2 )); then
  echo "Usage: $0 path/to/log [path/to/other.log]" >&2
  exit 1
fi

BIN="${BIN:-target/release/grctl}"
OUT_DIR="${OUT_DIR:-perf}"
PERF_DATA="$OUT_DIR/trace_tui.perf.data"
PERF_FREQ="${PERF_FREQ:-199}"

if [ ! -x "$BIN" ]; then
  echo "[profile] building grctl release binary..."
  cargo build -p grctl --release
fi

mkdir -p "$OUT_DIR"

echo "[profile] recording perf (-F ${PERF_FREQ} --call-graph dwarf) to $PERF_DATA"
perf record -g -F "$PERF_FREQ" --call-graph dwarf -o "$PERF_DATA" -- "$BIN" trace_tui "$@"
echo "[profile] perf data captured; inspect with: perf report -i $PERF_DATA"

if command -v stackcollapse-perf.pl >/dev/null 2>&1 && command -v flamegraph.pl >/dev/null 2>&1; then
  PERF_SCRIPT="$OUT_DIR/trace_tui.perf.script"
  PERF_FOLDED="$OUT_DIR/trace_tui.folded"
  PERF_SVG="$OUT_DIR/trace_tui.svg"

  echo "[profile] generating flamegraph..."
  perf script -i "$PERF_DATA" > "$PERF_SCRIPT"
  stackcollapse-perf.pl "$PERF_SCRIPT" > "$PERF_FOLDED"
  flamegraph.pl "$PERF_FOLDED" > "$PERF_SVG"
  echo "[profile] flamegraph ready: $PERF_SVG"
fi
