# Retail Closure Trace Shim

We no longer bootstrap Lua helpers or patch the retail scripts. This Rust
library is now a focused tracing shim that records every call the retail C++
engine makes to `lua_pushCclosure`, `lua_setglobal`, and the `lua_do*` /
`lua_call*` entry points. Watching those calls lets us enumerate every native
helper the engine installs into the Lua VM and track which scripts are loaded
without modifying the game's assets.

## Trace schema (shared across retail + Rust engines)
- Event names and fields live in `grim_telemetry_common::LuaEvent` (serde-tagged enum); add variants/fields there so both emitters stay in sync.
- Every line is `engine=retail vm_id=lua32 seq=<counter> event=<name> ...` with
  key/value pairs; values containing whitespace are quoted and escaped. A
  monotonic `ts=<millis>` is included for temporal alignment.
- Output now always includes provenance details like `label`, `origin`, `module`,
  `symbol`, `demangled`, `symbol_source=map`, and per-event extras
  (`push_seq`, `upvalues`, `calls`, `ref`, `lock`) to match the Rust engine
  trace.
- The retail shim always emits `engine=retail` and `vm_id=lua32`; instrument the
  Rust engine side to emit `engine=rust` and reuse the same field names so
  traces align 1:1.
- `lua_getglobal` and `lua_callfunction` logs are always per-call and include
  the running count. `GRIM_SHIM_LOG=/path` redirects output to a file.
- A quick diff helper lives at `tools/trace_diff.py`:
  `./tools/trace_diff.py retail.log rust.log [--ignore field] [--context N]`
  reports the first mismatch and prints ±N lines of context (defaults ignore
  `seq`/`ts` since they diverge across runs).

## Event reference (raw Lua VM surface)
- Common envelope on every line: `seq`, `ts`, `event=<name>`, followed by event
  fields, then `engine=retail vm_id=lua32` (and optional `run_id` if set).
- Origin fields appear when available: `origin`, `module`, `symbol`, `demangled`,
  `symbol_source`, `map_source`.
- Value fields (when a value is inspected): `value_type`, `value`, `value_len`,
  `value_preview`, `tag`, `tag_label`, `func`, `payload`, `payload_hex`
  (pointers are emitted as hex; decimal payload is omitted for userdata pushes;
  `tag_label` comes from built-in tags and cached `settagmethod` names).
- Handles gain a `handle_label` once they've been named via globals/refs or
  fallback registrations; subsequent events touching that handle will echo it.
- Caller origin is included on table creation/mutator/tag-method operations to
  tie writes and tag setup back to the native sites that performed them.
- Pushes: `lua_pushcclosure` (name, func, push_seq, upvalues, origin), `lua_pushnumber`
  (value), `lua_pushnil`, `lua_pushstring`/`lua_pushlstring` (len/preview), `lua_pushusertag`
  (id logged as a hex pointer, value fields include `value_type=userdata`,
  `tag`, plus caller origin fields when available).
- Globals: `lua_setglobal` (name, handle, handle_label, label, value fields,
  origin) and `lua_getglobal` (name, handle, label, handle_label, count milestone).
- Calls: `lua_callfunction` (handle, label, calls milestone, origin), `lua_call`
  (name), `lua_dofile`/`lua_dostring`/`lua_dobuffer` (path/snippet/name + size).
- Refs: `lua_ref` (lock, ref, handle/handle_label/label), `lua_getref` (ref,
  handle/handle_label/label, note when missing).
- Tag plumbing: `lua_settag`, `lua_copytagmethods` (to/from/result), `lua_settagmethod`
  (tag, event_name), `lua_setfallback` (event, handle + value fields). Tagged values
  now carry `tag_label` when known. The shared schema still includes `tag_state`,
  but the retail shim no longer emits it.
- Cutscenes (retail shim only): `cutscene` (movie/movie_label/phase/playing/elapsed_ms/polls/result),
  `cutscene_skip` (phase/movie/movie_label/elapsed_ms/polls), and `post_intro_room`
  (source/set/setup/after_movie) are typed in the shared schema and emitted
  only by the retail shim.
- Other: `lua_collectgarbage`, `lua_error` (message), `lua_setglobal`/`lua_rawsetglobal`/`lua_rawgetglobal`
  variants, userdata/table/number string inspection via value fields. Mutators
  like `lua_createtable`, `lua_settable` / `lua_rawsettable` / `lua_rawsetglobal`, and
  tag plumbing (`lua_copytagmethods`) include caller origin fields.

## How It Works
- Build a `cdylib` that exports the same symbols as libLua (`lua_pushCclosure`
  plus the main `lua_do*`/`lua_call*` entry points).
- Preload the shim via `LD_PRELOAD` so our exports are resolved before the
  engine's copy of libLua.
- When the engine calls `lua_pushCclosure`, we log the function pointer being
  wrapped and the closure name, along with a push sequence number, upvalue
  count, and symbol provenance. `lua_setglobal` looks up the bound Lua handle
  and tracks the global name -> C target mapping so subsequent call tracing can
  show provenance. `lua_getglobal` logs every read to reveal which globals
  scripts actually touch at runtime. `lua_ref` / `lua_getref` track anonymous
  handles stored in the registry so `lua_callfunction` can emit labels even when
  functions never receive globals. `lua_callfunction` emits call counts per
  handle, resolving targets via `lua_getobjname` and the mappings collected from
  globals/refs.
- `lua_dofile`/`lua_dostring` and friends log the chunk or function being
  executed before forwarding the call to the real libLua export via
  `dlsym(RTLD_NEXT, ...)`.
- `lua_settagmethod` logs tag-method registrations to capture VM hook setup.
- All shim lines use a consistent `event=` schema (e.g.
  `event=lua_setglobal name=X handle=0x...` with `label`/`origin` when available),
  keeping `handle=0x...` stable so later calls/refetches match.
- Logs include pid/tid/timestamps. Set `GRIM_SHIM_LOG=/path/to/file` to capture
  them to disk; otherwise they emit to stderr.

### Symbol map fallback for stripped binaries

- If `dladdr` cannot recover a symbol (common with stripped retail ELF), set
  `GRIM_SHIM_SYMBOL_MAP=/path/to/map.txt` to let the shim resolve addresses
  against a pre-generated map (exact address matches win; closest match within
  0x4000 bytes is accepted). The log will emit `symbol=... symbol_source=map`
  when the map provided the name.
- Restrict lookups to a specific module path substring (e.g. `GrimFandango`) by
  setting `GRIM_SHIM_SYMBOL_MAP_MODULE=GrimFandango`; otherwise the map is used
  for any closure.
- Additional maps can be provided with
  `GRIM_SHIM_SYMBOL_MAP_LUALIB=/path/to/libLua.map` (and optional
  `GRIM_SHIM_SYMBOL_MAP_LUALIB_MODULE`, defaulting to `libLua.so`). The shim
  selects the map whose module filter matches `dladdr`'s module path; if none
  match, unfiltered maps are used as a fallback.
- To produce a map, build an unstripped 32-bit binary from the retail checkout,
  making sure it matches the retail architecture/flags. For libLua or other
  shared objects, run `nm -an` or `readelf -Ws` on the unstripped `.so` and
  point `GRIM_SHIM_SYMBOL_MAP` (or the libLua-specific env var) at that file.
  Keep the map in sync with the binary you run against; different builds will
  have different offsets.

## Building

```bash
nix-shell --run 'cargo build -p grim_telemetry_shim --release --target i686-unknown-linux-gnu'
```

The resulting shared object lives at
`grim_analysis/retail_capture/rust_shim/target/release/libgrim_telemetry_shim.so`.
Preload it before starting the retail executable:

```bash
LD_PRELOAD=/path/to/libgrim_telemetry_shim.so ./GrimFandango.exe
```

On startup you should see log lines that look like
`[grim-rust-shim] engine=retail vm_id=lua32 seq=123 ts=456 event=lua_pushcclosure name=lua_pushCclosure func=0xf7e31234`
with symbol/module fields included when discoverable. Each line is a direct
observation of the engine pushing a C closure into Lua.
