# Retail Telemetry & Shim

This directory keeps the artifacts that let the retail executable stream its runtime state into our analysis pipeline. The code lives next to `grim_analysis` so capture tooling, format specs, and the runtime consumers stay in one place.

## Components

- `telemetry.lua` is the Lua 3.1 script injected into the shipping VM. It keeps the API surface tiny (`telemetry.mark`, `telemetry.event`, `telemetry.flush`, `telemetry.reset`) and rebuilds legacy helpers so it survives the stripped-down retail environment. The retail executable always loads `_system.lua` first, so the shim injects `telemetry.lua` the moment that file executes; all other retail scripts (`_cut_scenes.lua`, `year_1.lua`, set loaders, etc.) flow through `_system`.
  - **Boot-safe IO + string shims** – redefines `openfile`, `write`, `call`, `unpack`, and string primitives when the retail build omits them. If the native shim exposes `telemetry_native_write`, it piggybacks on that for writes.
  - **Coverage tracking** – `telemetry.mark(key)` bumps `coverage_counts[key]`, periodically rewrites `mods/telemetry_coverage.json`, and `telemetry.flush()` forces a final write so `grim_analysis --coverage-counts` can diff runs.
  - **Event stream** – `telemetry.event(label, fields)` records JSON objects to `mods/telemetry_events.jsonl` with monotonically increasing `seq`, filtered fields, and timestamps so downstream tools can tail retail behavior.
  - **Intro instrumentation** – monkey-patches `cut_scene` movies, `RunFullscreenMovie`, `StartMovie`, `wait_for_movie`, `start_script`, `wait_for_script`, and Manny’s `Actor.say_line` call to emit labeled `intro.timeline` events that reconstruct the logos → intro → office flow.
  - **Load tracing + installers** – wraps `dofile` so every include is logged to `mods/telemetry.loads.log`, and installs the intro hooks when `_cut_scenes.lua` or `year_1.lua` loads. Additional installers can hook new APIs by swapping functions and calling `telemetry.event` or `telemetry.mark` inside the replacements.
  - **Failure handling** – `telemetry_disable(reason)` swaps in inert stubs if a required primitive is missing, `_ERRORMESSAGE` is wrapped to write bootstrap failures to `mods/telemetry_bootstrap_error.log`, and `telemetry.reset()` wipes all logs so automated tests start from a known state.
  - **Compatibility note** – any helper Lua placed in `mods/` (including `telemetry_simple.lua`) must stay compatible with the game's Lua 3.x interpreter. Avoid Lua 5.x syntax sugar, metamethods, or library calls that were added after Lua 3.x.

- `rust_shim/` contains the new `LD_PRELOAD` hook written in Rust. The crate exports `lua_dofile`, resolves the retail engine’s real symbol, and (for now) logs `_system.lua` loads—the same choke point where we inject `telemetry.lua`. `grctl` builds the shim automatically via `cargo build -p grim_telemetry_shim --release --target i686-unknown-linux-gnu` and preloads the release `.so` before launching retail. See the runtime reference below for the exact launch environment, offsets, and memory layout used during debugging.

- `shim/` keeps the original C implementation. It still builds via `make` with `zig cc` (or any C compiler) and mirrors the legacy Lua 3.2 structs. We keep it around for reference while the Rust port catches up.

## Coverage workflow

1. Generate the state catalog and copy it (or just its `coverage.keys`) beside the retail install:
   ```bash
   cargo run -p grim_analysis -- --state-catalog-json artifacts/state_catalog.json
   ```
2. Place `telemetry.lua` in the game's `mods/` directory, preload the shim (or run `./grctl.sh retail hooks enable` which symlinks the file and ensures the Rust shim is built), and call `telemetry.mark("<catalog key>")` inside the retail scripts you want to observe. The helper rewrites `mods/telemetry_coverage.json` after every few marks (call `telemetry.flush()` before you exit to force a final write).
3. Run the analysis coverage check to identify gaps:
   ```bash
   cargo run -p grim_analysis -- \
      --coverage-counts mods/telemetry_coverage.json \
      --coverage-summary-json artifacts/coverage_report.json
   ```
   Missing keys point at catalog entries never hit by the retail run; unexpected keys indicate telemetry emitted IDs that are not yet part of the catalog.

### Building the shims

- **Rust shim (recommended)** – from the repo root, run `nix-shell --run 'cargo build -p grim_telemetry_shim --release --target i686-unknown-linux-gnu'`. `grctl retail start` does this automatically if the release artifact is missing. Preload `target/i686-unknown-linux-gnu/release/libgrim_telemetry_shim.so` when launching retail.

- **Legacy C shim** – run `make` in `shim/`. The default compiler is `zig cc`, but any toolchain that can emit an ELF shared object works. The `Makefile` auto-locates the Lua 3.2 headers provided by `shell.nix` (override `LUA32_PREFIX` if needed). Preload `shim/libgrim_lua_hook.so` before launching retail.

---

## Runtime & Memory Layout Reference

Use these diagrams alongside the GDB probe workflow (section 8) whenever you validate the shim inside the live retail process.

### 1. Launch & Runtime Context

```
Retail launcher: dev-install/GrimFandango
Shim: /home/mmontalbo/Developer/grim_mod/target/i686-unknown-linux-gnu/release/libgrim_telemetry_shim.so
Injected via:
  LD_PRELOAD=/…/libgrim_telemetry_shim.so:/…/steamclient.so

Lua VM: Lua 3.1 (alpha)
Shim base (gdb-probe): 0xf7e8f000
telemetry_native_mark offset: 0x00010ee0
⇒ Runtime address = 0xf7e9fee0
```

### 2. Program Headers (ELF 32 DYN)

```
Type        VirtAddr   MemSiz  Flags  Sections
LOAD        0x00000000 0x026e0 R      (.hash …)
LOAD        0x00003000 0x4a27c R E    (.init .plt .text .fini)
LOAD        0x0004e000 0x143b4 R      (.rodata .eh_frame*)
LOAD        0x00063e64 0x027ec RW     (.tdata .init_array .data.rel.ro .data .bss)
PT_TLS      0x00063e64 0x0002c R      (.tdata .tbss)
GNU_RELRO   0x00063e64 0x0219c R
```

Highlights
• `.text` @ 0x00003330 (~0x49f37 B) • `.rodata` @ 0x0004e000 (~0x6e70 B)
• `.tdata` @ 0x00063e64 (16 B) • `.tbss` + 28 B • `.bss` @ 0x000665e0 (112 B)

### 3. ELF Section Stack

```
Mapped module: libgrim_telemetry_shim.so @ 0xf7e8f000
───────────────────────────────────────────────────────────
0xf7e8f000  .text  (R E) → telemetry_native_mark, helpers
0xf7edf000  .rodata (R)  → literals, fmt strings
0xf7ee2e64  .tdata/.tbss/.data.rel.ro/.data/.bss (RW)
0xf7ef6e64  end of module mapping
```

Each worker thread copies `.tdata/.tbss` into its own TLS block.

### 4. Lua → Native Call Path

```
Lua script: mods/telemetry_simple.lua
telemetry.mark("capture_params.smoke")

↓ global resolve
telemetry_native_mark  (C closure from shim)
 ├─ lua_gettop()       validate args
 ├─ lua_lua2C(1)       get Lua handle
 ├─ luaA_Address(h)    read slot words
 ├─ lua_getstring(h)   → "capture_params.smoke"
 ├─ write mods/telemetry.log
 └─ write mods/telemetry_simple_trace.log
```

### 5. Lua Stack Slot #1 (32-bit TObject)

```
slot[1] @ e.g. 0x082349a0
┌──────────────┬────────────────────┬─────────────┐
│ ttype=-2     │ ptr→LuaString      │ aux/hash    │
│ 0xFFFFFFFE   │ 0x08234A10         │ 0x000000??  │
└──────────────┴────────────────────┴─────────────┘

ptr → LuaString on VM heap:
┌──────────────────────────────────────────┐
│ struct TString { ttype=-2; len; hash; …  │
│ char data[] = "capture_params.smoke\0";  │
└──────────────────────────────────────────┘
```

### 6. Thread-Local Storage (TLS)

```
TLS segment @ vaddr 0x00063e64
filesz 0x10 B, memsz 0x2c B
.tdata (16 B): template copied to new threads
.tbss  (28 B): zero-filled tail

Thread A TLS → { lua_state*, seen_first? }
Thread B TLS → { lua_state*, seen_first? }
```

Each thread keeps its own shim cache and guard bits.

### 7. Process Map (Excerpt)

```
00400000-00bff000 r-xp  main binary .text
00bff000-00c20000 r--p  main binary .rodata
…
f7e8f000-f7edf000 r-xp  libgrim_telemetry_shim.so .text
f7edf000-f7eea000 r--p  libgrim_telemetry_shim.so .rodata
f7eea000-f7ef1000 rw-p  libgrim_telemetry_shim.so .data/.bss/TLS
fffde000-ffffe000 rw-p  [stack]
```

### 8. GDB-Probe Flow (via grctl)

```
grctl retail gdb-probe
 ├─ find PID → read /proc/<pid>/maps → base 0xf7e8f000
 ├─ break *0xf7e9fee0 (telemetry_native_mark)
 ├─ on hit:
 │     print lua_lua2C(1)
 │     x/6wx slot
 │     lua_getstring → "capture_params.smoke"
 └─ logs auto-written under target/grctl/logs/
```

### 9. Harness ↔ Retail Parity

```
┌──────────────────────────┬──────────────────────────┐
│ grim_analysis harness    │ Retail (live game)       │
├──────────────────────────┼──────────────────────────┤
│ Lua 3.1 (alpha)          │ Lua 3.1 (alpha)          │
│ Same shim build          │ Same offsets             │
│ telemetry_native_mark @ 0x10ee0 │ identical layout │
│ Logs: grim_analysis/…     │ Logs: target/grctl/…    │
└──────────────────────────┴──────────────────────────┘
```

### 10. Log & Artifact Paths

```
mods/
 ├─ telemetry_simple.lua
 ├─ telemetry.log
 └─ telemetry_simple_trace.log

grim_analysis/retail_capture/
 └─ harness logs

target/grctl/logs/
 └─ retail_game.log
```

### 11. 32-Bit Process Memory Slice (Annotated)

```
Hi addresses (↓ growth)

0xfffff000  [vdso]  r-xp
0xfffde000  [stack] rw-p   ← grows downward
            │
0xf7eea000  libgrim_telemetry_shim.so  rw-p (.data/.bss/TLS)
0xf7edf000  libgrim_telemetry_shim.so  r--p (RELRO)
0xf7e8f000  libgrim_telemetry_shim.so  r-xp (.text)
            └─ telemetry_native_mark @ 0xf7e9fee0
0xf7d00000  liblua/libc/libm/steamclient …
            Heap (malloc/brk) rw-p ↑
0x08048000  main binary .text  r-xp
0x08100000  main binary .data/.bss  rw-p
Lo addresses

Legend:
 r-xp  executable code (.text)
 r--p  read-only (.rodata / RELRO)
 rw-p  writable (.data / .bss / TLS image)

Notes:
 • PIC rule: runtime addr = base + offset
 • RELRO pages → r--p after relocation
 • TLS template (.tdata + .tbss = 44 B) copied into each thread.
```

### Optional GDB Reference

```bash
# quick inspection helpers
nm -n libgrim_telemetry_shim.so | grep telemetry
readelf -S libgrim_telemetry_shim.so | grep -E '\.text|\.tdata|\.tbss'
cat /proc/$(pidof grim_fandango)/maps | grep grim_telemetry_shim
```
