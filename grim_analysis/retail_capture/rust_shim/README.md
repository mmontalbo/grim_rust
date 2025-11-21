# Retail Closure Trace Shim

We no longer bootstrap Lua helpers or patch the retail scripts. This Rust
library is now a focused tracing shim that records every call the retail C++
engine makes to `lua_pushCclosure`, `lua_setglobal`, and the `lua_do*` /
`lua_call*` entry points. Watching those calls lets us enumerate every native
helper the engine installs into the Lua VM and track which scripts are loaded
without modifying the game's assets.

## Trace schema (shared across retail + Rust engines)
- Every line is `engine=retail vm_id=lua32 seq=<counter> event=<name> ...` with
  key/value pairs; values containing whitespace are quoted and escaped. A
  monotonic `ts=<millis>` is included for temporal alignment.
- Common fields: `handle=0x...`, `label=<callable label>`, `origin=0x...`,
  `module=<shared object>`, `symbol=<mangled>`, `demangled=<pretty C++ signature>`,
  `symbol_source=map` / `map_source=<filename>` when a provided symbol map
  resolved the name; per-event extras like `push_seq`, `calls`, `ref`, `lock`
  track local counters.
- The retail shim always emits `engine=retail` and `vm_id=lua32`; instrument the
  Rust engine side to emit `engine=rust` and reuse the same field names so
  traces align 1:1.
- Verbosity toggles: `GRIM_SHIM_GETGLOBAL_VERBOSE=1` logs every `lua_getglobal`;
  `GRIM_SHIM_CALLFUNCTION_VERBOSE=1` logs every `lua_callfunction` instead of
  milestone counts only. `GRIM_SHIM_LOG=/path` redirects output to a file.
- A quick diff helper lives at `tools/trace_diff.py`:
  `./tools/trace_diff.py retail.log rust.log [--ignore field] [--context N]`
  reports the first mismatch and prints ±N lines of context (defaults ignore
  `seq`/`ts` since they diverge across runs).

## How It Works
- Build a `cdylib` that exports the same symbols as libLua (`lua_pushCclosure`
  plus the main `lua_do*`/`lua_call*` entry points).
- Preload the shim via `LD_PRELOAD` so our exports are resolved before the
  engine's copy of libLua.
- When the engine calls `lua_pushCclosure`, we log the sequence number, the
  function pointer being wrapped, the requested upvalue count, and (when
  `dladdr` resolves it) the owning module + symbol. `lua_setglobal` looks up the
  bound Lua handle and tracks the global name -> C target mapping so subsequent
  call tracing can show provenance. `lua_getglobal` logs milestone reads
  (set `GRIM_SHIM_GETGLOBAL_VERBOSE=1` for per-call logs) to reveal which globals
  scripts actually touch at runtime. `lua_ref` / `lua_getref` track anonymous
  handles stored in the registry so `lua_callfunction` can emit labels even when
  functions never receive globals. `lua_callfunction` emits milestone call counts
  per handle (set `GRIM_SHIM_CALLFUNCTION_VERBOSE=1` for per-call logs),
  resolving targets via `lua_getobjname` and the mappings collected from
  globals/refs.
- `lua_dofile`/`lua_dostring` and friends log the chunk or function being
  executed before forwarding the call to the real libLua export via
  `dlsym(RTLD_NEXT, ...)`.
- `lua_settagmethod` logs tag-method registrations to capture VM hook setup.
- All shim lines use a consistent `event=` schema (e.g. `event=bind_global name=X handle=0x... label=global:X origin=...`,
  `event=store_ref ref=2 handle=0x... label=ref:2`), keeping `handle=0x...` stable so later calls/refetches match.
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
- To produce a map, build an unstripped 32-bit binary from the retail checkout,
  making sure it matches the retail architecture/flags. For libLua or other
  shared objects, run `nm -an` or `readelf -Ws` on the unstripped `.so` and
  point `GRIM_SHIM_SYMBOL_MAP` at that file. Keep the map in sync with the
  binary you run against; different builds will have different offsets.

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
`[grim-rust-shim] #000123 lua_pushCclosure func=0xf7e31234 upvalues=0 module=/path/libGrim.so symbol=telemetry_native_mark`.
Each line is a direct observation of the engine pushing a C closure into Lua.
