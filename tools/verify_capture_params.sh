#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${ROOT_DIR}/target/telemetry_capture_params"
MODS_DIR="${TARGET_DIR}/mods"
SHIM_PATH="${ROOT_DIR}/target/i686-unknown-linux-gnu/debug/libgrim_telemetry_shim.so"

echo "[verify] building grim_telemetry_shim (debug)"
nix-shell --run "cargo build -p grim_telemetry_shim --target i686-unknown-linux-gnu" >/dev/null

rm -rf "${TARGET_DIR}"
mkdir -p "${MODS_DIR}"

cp "${ROOT_DIR}/grim_analysis/retail_capture/telemetry_simple.lua" "${MODS_DIR}/telemetry_simple.lua"

cat > "${TARGET_DIR}/_system.lua" <<'EOF'
if type(telemetry) ~= "table" then
    telemetry = {}
end

if type(telemetry_native_mark) ~= "function" then
    error("telemetry_native_mark missing")
end

local result = telemetry_native_mark("capture_params.smoke")
__telemetry_mark_probe = "telemetry_native_mark type=" .. type(result) .. " value=" .. tostring(result)
if result ~= 1 then
    error("telemetry_native_mark returned " .. tostring(result))
end

if type(telemetry.mark) ~= "function" then
    error("telemetry.mark missing after bootstrap")
end

telemetry.mark("capture_params.via.telemetry.mark")

__telemetry_mark_probe = __telemetry_mark_probe .. "; telemetry.mark invoked"
EOF

if [ ! -f "${SHIM_PATH}" ]; then
    echo "[verify] shim artifact missing at ${SHIM_PATH}" >&2
    exit 1
fi

if ! command -v lua32 >/dev/null 2>&1; then
    echo "[verify] lua32 interpreter not found in PATH" >&2
    exit 1
fi

LUA32_BIN="$(command -v lua32)"
LUA32_PREFIX="$(cd "$(dirname "${LUA32_BIN}")/.." && pwd)"
LUA32_INCLUDE="${LUA32_PREFIX}/share/lua32/include"
LUA32_LIBDIR="${LUA32_PREFIX}/share/lua32/lib"
HARNESS_BIN="${TARGET_DIR}/telemetry_mark_harness"
GLIBC_MULTI_DIR="$(dirname "$(dirname "$(ldd "${LUA32_BIN}" | awk '/libc.so/ {print $3; exit}')")")"
GLIBC32_LIB="${GLIBC_MULTI_DIR}/lib/32"
SHIM_DIR="$(dirname "${SHIM_PATH}")"

echo "[verify] compiling 32-bit telemetry harness"
cc -m32 -Wl,-E -I"${LUA32_INCLUDE}" \
   -L"${LUA32_LIBDIR}" -L"${GLIBC32_LIB}" -L"${SHIM_DIR}" \
   "${ROOT_DIR}/tools/telemetry_mark_harness.c" \
   "${LUA32_LIBDIR}/liblua.a" "${LUA32_LIBDIR}/liblualib.a" \
   -lgrim_telemetry_shim \
   -lm -ldl -o "${HARNESS_BIN}"

echo "[verify] executing harness via lua32"
(
    cd "${TARGET_DIR}"
    GRIM_TELEMETRY_DEBUG=1 \
    LD_LIBRARY_PATH="${SHIM_DIR}:${LD_LIBRARY_PATH:-}" \
    ./telemetry_mark_harness
)

echo "[verify] capture_lua_params smoke test completed."
