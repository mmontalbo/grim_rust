# Retail Binary Map

A tree-style snapshot of the retail ELF and the symbol surface we interact with.

```
dev-install/
└── GrimFandango (ELF, 32-bit x86)
    ├── Native engine subsystems (own process memory; exposed indirectly via Lua closures)
    │   ├── Rendering / camera / scene graph
    │   ├── Audio / speech / music
    │   ├── Input / controller mapping
    │   ├── Actor / pathfinding / animation controllers
    │   ├── Room + cutscene managers
    │   └── Resource streaming (models, textures, VO)
    ├── Embedded Lua host
    │   ├── Dynamically links libLua.so (Lua 3.2 runtime shared object)
    │   ├── Calls exported Lua 3.2 API directly to open states, run scripts, and marshal data
    │   └── Registers native helpers into Lua globals/tables via Lua closures
    └── Lua-facing entry points (refer to libLua.so for implementations)

libLua exports (subset we rely on)
├── Core VM / state (lifecycle + handles)
│   ├── lua_open / lua_close / lua_setstate
│   ├── lua_lua2C (macro target for lua_getparam / lua_getresult)
│   ├── lua_getglobal / lua_setglobal / lua_rawgetglobal / lua_rawsetglobal
│   ├── lua_tag / lua_newtag / lua_settag / lua_settagmethod / lua_gettagmethod
│   └── lua_ref / lua_getref / lua_unref / lua_pushobject / lua_pop
├── Execution (script loading + dispatcher)
│   ├── lua_dofile / lua_dostring / lua_dobuffer
│   ├── lua_callfunction / lua_call / lua_beginblock / lua_endblock
│   ├── luaD_call / luaD_taskHook / lua_updatetasks / lua_currenttask
│   └── lua_error / lua_seterrormethod
├── Stack/object inspection (type checks + conversions)
│   ├── lua_isnil / lua_isnumber / lua_isstring / lua_isfunction / lua_isuserdata
│   ├── lua_getnumber / lua_getstring / lua_strlen / lua_getcfunction / lua_getuserdata
│   ├── lua_pushnumber / lua_pushstring / lua_pushcclosure / lua_pushusertag / lua_pushnil
│   └── lua_next / lua_nextvar / lua_createtable / lua_settable / lua_rawsettable / lua_gettable / lua_rawgettable
├── Libraries (stdlib bring-up + diagnostics)
│   ├── lua_strlibopen / lua_iolibopen / lua_mathlibopen / lua_openlib / lua_recognizelib
│   └── lua_printstack / lua_PrintGlobals
└── Misc helpers (GC, serialization, aliases)
    ├── lua_beginblock / lua_endblock
    ├── lua_collectgarbage
    ├── lua_pushCclosure (alias lua_pushcclosure) / lua_pushCfunction
    └── lua_Save / lua_Restore (engine serialization hooks)
```

Lua closure: in Lua 3.x every function value is a closure that bundles code with any upvalues it captures; the C API mirrors this by letting the engine call `lua_pushcclosure` to wrap a native function plus upvalues into a callable Lua object. Those closures are what the engine binds to globals for scripts to invoke.

libLua.so usage: the retail binary links this shared library at runtime and treats it as the authoritative Lua 3.2 VM. The host code invokes `lua_open`, `lua_dofile`, `lua_pushcclosure`, `lua_callfunction`, `lua_collectgarbage`, and the other exports listed above to manage Lua state. All bytecode interpretation happens inside libLua.so; the native engine’s job is to feed scripts to it and expose C closures so those scripts can reach back into engine subsystems.

Rust shim placement (hypothetical): conceptually, an `LD_PRELOAD` shim wedges itself at the Lua boundary—exporting a handful of Lua functions (usually `lua_dofile` plus any other chokepoints we care about), looking up the real implementations via `dlsym(RTLD_NEXT)`, and layering custom behavior before handing control back. There is rarely a need to patch every libLua export: hooking a focused subset (`lua_dofile` for script injection, possibly `lua_pushcclosure` or `lua_callfunction` for tracepoints) keeps the shim simpler, reduces compatibility risk, and still observes all higher-level interactions because the retail engine funnels its Lua work through those APIs.

Use `readelf -sW dev-install/libLua.so` or `nm -D dev-install/GrimFandango` to confirm offsets and verify additional symbols if needed. This list captures every exported function we touch when embedding instrumentation or interacting with the runtime via GDB.
