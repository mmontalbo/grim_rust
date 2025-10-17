#include <stdio.h>
#include <lua.h>
#include <lualib.h>

int main(void) {
    lua_open();
    lua_iolibopen();
    lua_strlibopen();
    lua_mathlibopen();

    const char *script = "grim_analysis/retail_capture/telemetry.lua";
    int result = lua_dofile((char *)script);
    printf("lua_dofile(%s) -> %d\n", script, result);
    if (result != 0) {
        lua_Object err = lua_getglobal("__telemetry_bootstrap_error");
        if (err != LUA_NOOBJECT) {
            const char *message = lua_getstring(err);
            if (message) {
                printf("__telemetry_bootstrap_error = %s\n", message);
            } else {
                printf("__telemetry_bootstrap_error present but not a string\n");
            }
        } else {
            printf("__telemetry_bootstrap_error not set\n");
        }
    }
    lua_close();
    return result;
}
