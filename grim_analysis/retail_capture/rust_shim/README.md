# Rust Telemetry Shim (Step 1)

The original C shim under `../shim/` loads `telemetry.lua`, patches Lua's task
hooks, and stubs Steam APIs. That surface area is difficult to reason about, so
this directory hosts a fresh, incremental rewrite in Rust.

## How It Works
- We compile this crate as a shared library (`.so`) that exports the same
  symbols as the retail executable expects.
- Linux's dynamic linker lets us **preload** that library via `LD_PRELOAD`.
  Anything we export with the same symbol name (e.g., `lua_dofile`) is used
  before the game's original version. The runtime still resolves the real
  symbol via `dlsym(RTLD_NEXT, ...)`, so we can forward the call after we do our
  bookkeeping.
- `lua_dofile` is the Lua runtime entry point that loads and executes a file on
  disk (`lua do file`). By watching every `lua_dofile` invocation we learn when
  `_system.lua`—the retail bootstrap script—runs, which is the precise moment we
  want to install our telemetry helpers.
- The retail executable hardcodes `_system.lua` as the first script it loads
  during boot. Every other include (`_cut_scenes.lua`, `year_1.lua`, etc.)
  happens after `_system.lua` calls into the game’s registry. Hooking there is
  the earliest safe window to execute `telemetry.lua`, because the Lua runtime
  is initialized but no gameplay scripts have run yet.

## Current Scope
- Build a `cdylib` (`libgrim_telemetry_shim.so`) with Cargo.
- Intercept `lua_dofile` via `LD_PRELOAD`.
- Log whenever `_system.lua` loads. The log proves the hook fires, and it marks
  the exact spot where we will later inject `telemetry.lua`.

No telemetry runtime, Steam shims, or Lua struct mirrors exist yet—each will be
ported once we are satisfied with the previous step.

## Building

```bash
nix-shell --run 'cargo build -p grim_telemetry_shim --release --target i686-unknown-linux-gnu'
```
If you have never built for that target before, run `rustup target add i686-unknown-linux-gnu`
inside the dev shell once (or just let `grctl retail start` do it for you).

The artifact lands in
`grim_analysis/retail_capture/rust_shim/target/release/libgrim_telemetry_shim.so`.
Preload it before launching the retail game:

```bash
LD_PRELOAD=/path/to/libgrim_telemetry_shim.so ./GrimFandango.exe
```

You should see `[grim-rust-shim] observed lua_dofile call for
mods/_system.lua` as soon as the VM loads `_system.lua`.

## Using with `grctl`

Once the release build exists, `grctl` automatically picks it up (falling back
to the Rust debug build if that’s the only artifact on disk):

1. `./grctl.sh retail hooks enable` – links `telemetry.lua` into `dev-install/`.
   The command expects the release build at
   `target/i686-unknown-linux-gnu/release/libgrim_telemetry_shim.so` (falling
   back to `grim_analysis/retail_capture/rust_shim/target/i686-unknown-linux-gnu/release/...`
   or either directory's debug build if that is all that exists).
2. `./grctl.sh retail start` – auto-builds the release shim if it is missing,
   then launches the retail binary with the shim in `LD_PRELOAD`, so hook logs
   appear immediately in the session log or your terminal if you used
   `--attach`.

## Next Steps
1. Replace the log with a tiny injector that runs our existing `telemetry.lua`.
2. Mirror the Lua task helpers from the C shim (task snapshots, `luaD_call`,
   etc.).
3. Port the telemetry runtime bridge once the hook parity feels solid.
