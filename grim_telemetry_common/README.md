# Telemetry streams

This crate defines the shared telemetry schema used by the engine host and the
retail shim. Events flow into two streams:

- **Semantic (default):** composite events that describe the Lua-facing
  contract (bindings, table writes, registry refs). Parity and diffs should be
  driven by this stream.
- **Raw:** VM-level details (pushes, GC, call plumbing) kept for debugging.
  Divergence here should not block parity unless it breaks behaviour.

Tools such as `grctl parity logs` and `trace_tui` default to the semantic
stream; use their `--raw` / `--stream raw` switches to opt in to the raw view.
For the wider Lua API goals and telemetry background, see
`docs/lua_native_api.md`.

Origin and handle helpers in `src/trace_utils.rs` are shared by the engine host
and retail shim; use `is_runtime_frame` as the baseline skip list and
`caller_origin_fields`/`origin_fields_for_ptr` to avoid diverging caller
attribution across the two telemetry streams.
