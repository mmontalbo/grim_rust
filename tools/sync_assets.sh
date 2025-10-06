#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${GRIM_INSTALL_PATH:-}" ]]; then
  echo "GRIM_INSTALL_PATH is not set. Point it at your Grim Fandango Remastered install." >&2
  exit 1
fi

DEST="extracted"
if [[ $# -gt 0 && $1 != --* ]]; then
  DEST="$1"
  shift
fi

mkdir -p "$DEST"

cargo run -p grim_formats --bin lab_extract -- \
  --root "$GRIM_INSTALL_PATH" \
  --dest "$DEST" \
  "$@"
