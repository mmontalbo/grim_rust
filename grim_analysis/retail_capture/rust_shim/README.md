# Retail Closure Trace Shim

We no longer bootstrap Lua helpers or patch the retail scripts. This Rust
library is now a focused tracing shim that records every call the retail C++
engine makes to `lua_pushCclosure`/`lua_pushcclosure`. Watching those calls lets
us enumerate every native helper the engine installs into the Lua VM without
modifying the game's assets.

## How It Works
- Build a `cdylib` that exports the same symbols as libLua (`lua_pushCclosure`
  and its lowercase alias).
- Preload the shim via `LD_PRELOAD` so our exports are resolved before the
  engine's copy of libLua.
- When the engine calls `lua_pushCclosure`, we log the sequence number, the
  function pointer being wrapped, the requested upvalue count, and (when
  `dladdr` resolves it) the owning module + symbol. After logging we immediately
  forward the call to the real libLua export via `dlsym(RTLD_NEXT, ...)`.
- No other Lua entry points are intercepted. All we care about now is capturing
  the stream of C closures as the game registers them so we can analyze and
  instrument those helpers later.

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
