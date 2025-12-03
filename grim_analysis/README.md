# Grim Telemetry Shim

This crate builds the retail Lua tracing shim (`grim_telemetry_shim`). It is a
`cdylib` meant to be `LD_PRELOAD`ed alongside the retail binary so we can log
Lua C API calls without modifying game assets. See `README.rust_shim.md` for
usage, environment variables, and schema details.
