#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEFAULT_INSTALL="${REPO_ROOT}/dev-install"

if [[ -z "${GRIM_INSTALL_PATH:-}" ]]; then
  GRIM_INSTALL_PATH="${DEFAULT_INSTALL}"
fi

if [[ ! -d "${GRIM_INSTALL_PATH}" ]]; then
  echo "[sync_assets] expected retail LAB archives under ${GRIM_INSTALL_PATH}" >&2
  echo "[sync_assets] populate dev-install/ or set GRIM_INSTALL_PATH to an existing install" >&2
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
