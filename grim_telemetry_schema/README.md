# Telemetry schema

This crate is the source of truth for telemetry events and helpers shared by
the engine host and the retail shim. Event names, fields, and stream markers
are defined here so both emitters stay aligned. Events flow into two streams:

- **Semantic (default):** composite events that describe the Lua-facing
  contract (bindings, table writes, registry refs). Parity and diffs should be
  driven by this stream.
- **Raw:** VM-level details (pushes, GC, call plumbing) kept for debugging.
  Divergence here should not block parity unless it breaks behaviour.

Tools such as `grctl parity logs` and `trace_tui` default to the semantic
stream; use their `--raw` / `--stream raw` switches to opt in to the raw view.
For the wider Lua API goals and telemetry background, see
`docs/lua_native_api.md`.

## Sequence numbers

Each telemetry line carries two counters:

- `seq` counts events within the tagged stream (`raw` or `semantic`).
- `log_seq` is the overall emission order across all streams.

Viewers prefer the stream-specific `seq` so raw and semantic numbering stay
independent.

## Event catalog

Semantic stream

- `engine_boot_phase`: phase name, `status` (start|ok|error), `elapsed_ms` (completion only).
- `engine_exit`: `status` (ok|exit_code|signal|unknown), optional `note`/`code`/`signal`/`cause`, `component`.
- `lua_parent_cycle_scan`: `table` handle, optional `label`, `depth`, `path` of the detected parent chain.
- `intro_timeline`: `label`=`intro.timeline`, `data.event` (e.g., `movie.intro.start/end`), optional `seq` for local ordering.
- `semantic_bind_global_{closure,constant}`: `name`, `handle`, optional `label`/`values`/`upvalues`, `origin`.
- `semantic_set_table_entry` / `semantic_set_fallback` / `semantic_set_tagmethod`: table/tag handles + value previews, `caller`, optional notes.
- `semantic_ref_*`: `ref`/`lock`/`alias` metadata for registry refs, optional value kind and `origin`.

Raw stream

- `lua_*` pushes/calls/registry ops from `LuaEvent` (handles, value previews, caller metadata) used for parity debugging.
- `cutscene` / `cutscene_skip` / `post_intro_room`: movie playback state, skip requests, and post-intro transitions.
- `register_native` / `register_constant` / `register_global` plus low-level get/set/push events; see `LuaEvent` in `src/lib.rs` for full field sets.

## Crate layout

Origin and handle helpers in `src/trace_utils.rs` are shared by the engine host
and retail shim; use `is_runtime_frame` as the baseline skip list and
`caller_origin_fields`/`origin_fields_for_ptr` to avoid diverging caller
attribution across the two telemetry streams. `TelemetryLogger` owns the shared
event formatting/metadata; pair it with `JsonlWriter` via
`log_event_to_writer` when you need to emit schema-shaped JSONL files from
either runtime.
