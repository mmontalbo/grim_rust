#include <stdio.h>
#include <stdlib.h>
#include <lua.h>
#include <lualib.h>

extern void telemetry_native_mark(void);

static void fail(const char *message) {
    fprintf(stderr, "[verify] %s\n", message);
    exit(1);
}

static void report_error(const char *context) {
    lua_Object err = lua_getglobal("__telemetry_bootstrap_error");
    if (err != LUA_NOOBJECT) {
        const char *text = lua_getstring(err);
        if (text) {
            fprintf(stderr, "[verify] %s failed: %s\n", context, text);
            fail("telemetry bootstrap error present");
        }
    }
    fail(context);
}

int main(void) {
    lua_open();
    lua_iolibopen();
    lua_strlibopen();
    lua_mathlibopen();

    lua_pushcclosure(telemetry_native_mark, 0);
    lua_setglobal("telemetry_native_mark");

    if (lua_dofile("mods/telemetry_simple.lua") != 0) {
        report_error("loading telemetry_simple.lua");
    }
    int status = lua_dofile("_system.lua");

    lua_Object result = lua_getglobal("__telemetry_mark_probe");
    if (result == LUA_NOOBJECT) {
        fail("__telemetry_mark_probe missing");
    }
    if (!lua_isstring(result)) {
        fail("__telemetry_mark_probe not a string");
    }
    const char *state = lua_getstring(result);
    if (!state || state[0] == '\0') {
        fail("__telemetry_mark_probe empty");
    }
    if (status != 0) {
        fprintf(stderr, "[verify] _system.lua returned %d\n", status);
        fprintf(stderr, "[verify] probe=%s\n", state);
        fail("executing _system.lua");
    }

    printf("[verify] %s\n", state);
    lua_close();
    return 0;
}
