#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <sys/stat.h>
#include "../../../lua/include/lua.h"

typedef int (*lua_dofile_fn)(char *filename);
typedef lua_Object (*lua_getglobal_fn)(const char *name);
typedef const char *(*lua_getstring_fn)(lua_Object object);
typedef int (*lua_isfunction_fn)(lua_Object object);
typedef int (*lua_istable_fn)(lua_Object object);
typedef void (*lua_strlibopen_fn)(void);
typedef void (*lua_iolibopen_fn)(void);
typedef int (*lua_dostring_fn)(char *string);
typedef void (*lua_pushcclosure_fn)(lua_CFunction fn, int n);
typedef void (*lua_setglobal_fn)(char *name);

static lua_dofile_fn real_lua_dofile = NULL;
static lua_getglobal_fn real_lua_getglobal = NULL;
static lua_getstring_fn real_lua_getstring = NULL;
static lua_isfunction_fn real_lua_isfunction = NULL;
static lua_istable_fn real_lua_istable = NULL;
static lua_strlibopen_fn real_lua_strlibopen = NULL;
static lua_dostring_fn real_lua_dostring = NULL;
static lua_iolibopen_fn real_lua_iolibopen = NULL;
static lua_pushcclosure_fn real_lua_pushcclosure = NULL;
static lua_setglobal_fn real_lua_setglobal = NULL;

static pthread_once_t resolve_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t telemetry_mutex = PTHREAD_MUTEX_INITIALIZER;
static bool telemetry_injected = false;
static bool telemetry_requested = false;
static bool telemetry_wait_logged = false;
static bool telemetry_missing_globals_logged = false;
static bool strlibopen_attempted = false;
static bool string_library_patch_attempted = false;
static bool iolibopen_attempted = false;
static bool native_file_helpers_registered = false;

static const char *const TARGET_SCRIPT = "_system.lua";
static const char *const TELEMETRY_SCRIPT = "mods/telemetry.lua";
static const char *const LOG_PATH = "mods/telemetry.log";
static const char *const TELEMETRY_BOOTSTRAP_ERROR_GLOBAL = "__telemetry_bootstrap_error";
static const char *const TELEMETRY_STUB_REASON_GLOBAL = "__telemetry_stub_reason";

static void ensure_log_directory(void) {
    const char *slash = strrchr(LOG_PATH, '/');
    if (!slash) {
        return;
    }
    size_t dir_len = (size_t)(slash - LOG_PATH);
    if (dir_len == 0) {
        return;
    }

    char buffer[256];
    if (dir_len >= sizeof(buffer)) {
        return;
    }

    memcpy(buffer, LOG_PATH, dir_len);
    buffer[dir_len] = '\0';

    if (mkdir(buffer, 0755) != 0 && errno != EEXIST) {
        fprintf(stderr, "[grim_lua_hook] mkdir(%s) failed: %s\n", buffer, strerror(errno));
    }
}

static void log_event(const char *fmt, ...) {
    ensure_log_directory();

    FILE *log = fopen(LOG_PATH, "a");
    if (!log) {
        log = stderr;
    }

    time_t now = time(NULL);
    struct tm tm_now;
    localtime_r(&now, &tm_now);

    char timestamp[32];
    if (strftime(timestamp, sizeof(timestamp), "%Y-%m-%d %H:%M:%S", &tm_now) == 0) {
        strncpy(timestamp, "unknown-time", sizeof(timestamp));
        timestamp[sizeof(timestamp) - 1] = '\0';
    }

    fprintf(log, "[%s] ", timestamp);

    va_list args;
    va_start(args, fmt);
    vfprintf(log, fmt, args);
    va_end(args);

    fputc('\n', log);

    if (log != stderr) {
        fclose(log);
    } else {
        fflush(log);
    }
}

static void resolve_real_symbols(void) {
    dlerror(); // Clear any stale error.
    real_lua_dofile = (lua_dofile_fn)dlsym(RTLD_NEXT, "lua_dofile");
    const char *err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_dofile: %s", err);
    }

    dlerror();
    real_lua_getglobal = (lua_getglobal_fn)dlsym(RTLD_NEXT, "lua_getglobal");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_getglobal: %s", err);
    }

    dlerror();
    real_lua_getstring = (lua_getstring_fn)dlsym(RTLD_NEXT, "lua_getstring");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_getstring: %s", err);
    }

    dlerror();
    real_lua_isfunction = (lua_isfunction_fn)dlsym(RTLD_NEXT, "lua_isfunction");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_isfunction: %s", err);
    }

    dlerror();
    real_lua_istable = (lua_istable_fn)dlsym(RTLD_NEXT, "lua_istable");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_istable: %s", err);
    }

    dlerror();
    real_lua_strlibopen = (lua_strlibopen_fn)dlsym(RTLD_NEXT, "lua_strlibopen");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_strlibopen: %s", err);
    }

    dlerror();
    real_lua_iolibopen = (lua_iolibopen_fn)dlsym(RTLD_NEXT, "lua_iolibopen");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_iolibopen: %s", err);
    }

    dlerror();
    real_lua_pushcclosure = (lua_pushcclosure_fn)dlsym(RTLD_NEXT, "lua_pushcclosure");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_pushcclosure: %s", err);
    }

    dlerror();
    real_lua_setglobal = (lua_setglobal_fn)dlsym(RTLD_NEXT, "lua_setglobal");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_setglobal: %s", err);
    }

    dlerror();
    real_lua_dostring = (lua_dostring_fn)dlsym(RTLD_NEXT, "lua_dostring");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_dostring: %s", err);
    }
}

static void log_bootstrap_error(void) {
    if (!real_lua_getglobal || !real_lua_getstring) {
        return;
    }
    lua_Object obj = real_lua_getglobal(TELEMETRY_BOOTSTRAP_ERROR_GLOBAL);
    if (obj != 0) {
        const char *message = real_lua_getstring(obj);
        if (message && message[0] != '\0') {
            log_event("telemetry bootstrap error: %s", message);
        }
    }
}

static void log_stub_reason(void) {
    if (!real_lua_getglobal || !real_lua_getstring) {
        return;
    }
    lua_Object obj = real_lua_getglobal(TELEMETRY_STUB_REASON_GLOBAL);
    if (obj != 0) {
        const char *message = real_lua_getstring(obj);
        if (message && message[0] != '\0') {
            log_event("telemetry stub reason: %s", message);
        }
    }
}

static const char *basename_or_self(const char *path) {
    if (!path) {
        return NULL;
    }
    const char *slash = strrchr(path, '/');
    if (slash) {
        return slash + 1;
    }
    return path;
}

static void telemetry_native_write(void) {
    lua_Object path_obj = lua_getparam(1);
    lua_Object contents_obj = lua_getparam(2);
    lua_Object mode_obj = lua_getparam(3);

    if (path_obj == LUA_NOOBJECT || contents_obj == LUA_NOOBJECT) {
        lua_pushnumber(0);
        return;
    }

    if (!lua_isstring(path_obj) || !lua_isstring(contents_obj)) {
        lua_pushnumber(0);
        return;
    }

    const char *path = lua_getstring(path_obj);
    const char *contents = lua_getstring(contents_obj);
    const char *mode = "a";
    if (mode_obj != LUA_NOOBJECT && lua_isstring(mode_obj)) {
        const char *requested = lua_getstring(mode_obj);
        if (requested && requested[0] != '\0') {
            mode = requested;
        }
    }

    if (!path || path[0] == '\0' || !contents) {
        lua_pushnumber(0);
        return;
    }

    FILE *file = fopen(path, mode);
    if (!file) {
        lua_pushnumber(0);
        return;
    }

    size_t written = fwrite(contents, 1, strlen(contents), file);
    int success = (written == strlen(contents)) ? 1 : 0;
    if (fclose(file) != 0) {
        success = 0;
    }

    lua_pushnumber(success);
}

static void register_native_file_helpers(void) {
    bool should_register = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (!native_file_helpers_registered) {
        native_file_helpers_registered = true;
        should_register = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);

    if (!should_register) {
        return;
    }

    if (!real_lua_pushcclosure || !real_lua_setglobal) {
        log_event("cannot register native file helpers: lua_pushcclosure or lua_setglobal missing");
        return;
    }

    real_lua_pushcclosure(telemetry_native_write, 0);
    real_lua_setglobal((char *)"telemetry_native_write");
    log_event("telemetry native file helpers registered");
}

static void inject_telemetry(void) {
    if (!real_lua_dofile) {
        log_event("telemetry injection skipped: real lua_dofile unavailable");
        return;
    }

    int result = real_lua_dofile((char *)TELEMETRY_SCRIPT);
    if (result != 0) {
        log_event("telemetry script %s returned error code %d", TELEMETRY_SCRIPT, result);
        log_bootstrap_error();
    } else {
        log_event("telemetry script %s executed", TELEMETRY_SCRIPT);
        log_stub_reason();
    }
}

static bool function_exists(const char *name) {
    if (!real_lua_getglobal || !real_lua_isfunction) {
        return false;
    }
    lua_Object object = real_lua_getglobal((char *)name);
    if (object == 0) {
        return false;
    }
    return real_lua_isfunction(object) != 0;
}

static void attempt_string_library_open(void) {
    bool should_attempt = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (!strlibopen_attempted) {
        strlibopen_attempted = true;
        should_attempt = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);
    if (!should_attempt) {
        return;
    }

    if (real_lua_strlibopen) {
        log_event("lua_strlibopen invoked by telemetry shim");
        real_lua_strlibopen();
    } else {
        log_event("lua_strlibopen unavailable; cannot preload string library");
    }
}

static void attempt_io_library_open(void) {
    bool should_attempt = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (!iolibopen_attempted) {
        iolibopen_attempted = true;
        should_attempt = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);
    if (!should_attempt) {
        return;
    }

    if (real_lua_iolibopen) {
        log_event("lua_iolibopen invoked by telemetry shim");
        real_lua_iolibopen();
        if (real_lua_dostring && real_lua_getglobal && real_lua_getstring) {
            static const char *const IO_STATUS_SCRIPT =
                "if type(io) == \"table\" and type(io.open) == \"function\" then\n"
                "  __telemetry_io_ready = \"ready\"\n"
                "else\n"
                "  __telemetry_io_ready = \"missing\"\n"
                "end\n";
            int status_result = real_lua_dostring((char *)IO_STATUS_SCRIPT);
            if (status_result != 0) {
                log_event("io readiness script failed (%d)", status_result);
                log_bootstrap_error();
            } else {
                lua_Object state = real_lua_getglobal("__telemetry_io_ready");
                const char *state_str = NULL;
                if (state != 0) {
                    state_str = real_lua_getstring(state);
                }
                if (state_str && state_str[0] != '\0') {
                    log_event("io library readiness: %s", state_str);
                } else {
                    log_event("io library readiness unknown");
                }
            }
        }
        bool openfile_ready = function_exists("openfile");
        bool write_ready = function_exists("write");
        bool closefile_ready = function_exists("closefile");
        log_event(
            "legacy io functions (openfile=%s, write=%s, closefile=%s)",
            openfile_ready ? "ready" : "missing",
            write_ready ? "ready" : "missing",
            closefile_ready ? "ready" : "missing");
    } else {
        log_event("lua_iolibopen unavailable; cannot enable io library");
    }
}

static void attempt_string_library_patch(void) {
    bool should_attempt = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (!string_library_patch_attempted) {
        string_library_patch_attempted = true;
        should_attempt = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);
    if (!should_attempt) {
        return;
    }

    if (!real_lua_dostring) {
        log_event("lua_dostring unavailable; cannot patch string library aliases");
        return;
    }

    static const char *const STRING_LIB_PATCH_SCRIPT =
        "if type(strbyte) ~= \"function\" and type(ascii) == \"function\" then strbyte = ascii end\n"
        "if type(strbyte) ~= \"function\" and type(string) == \"table\" and type(string.byte) == \"function\" then strbyte = string.byte end\n"
        "if type(strformat) ~= \"function\" and type(format) == \"function\" then strformat = format end\n"
        "if type(string) == \"table\" then\n"
        "  if type(string.sub) ~= \"function\" and type(strsub) == \"function\" then string.sub = strsub end\n"
        "  if type(string.byte) ~= \"function\" and type(strbyte) == \"function\" then string.byte = strbyte end\n"
        "  if type(string.len) ~= \"function\" and type(strlen) == \"function\" then string.len = strlen end\n"
        "  if type(string.format) ~= \"function\" and type(strformat) == \"function\" then string.format = strformat end\n"
        "end\n";

    int result = real_lua_dostring((char *)STRING_LIB_PATCH_SCRIPT);
    if (result != 0) {
        log_event("string library patch script failed (%d)", result);
        log_bootstrap_error();
    } else {
        bool sub_ready = function_exists("strsub");
        bool byte_ready = function_exists("strbyte");
        bool format_ready = function_exists("strformat");
        log_event("string library globals/table patched by telemetry shim");
        log_event(
            "post-patch primitives (strsub=%s, strbyte=%s, strformat=%s)",
            sub_ready ? "ready" : "missing",
            byte_ready ? "ready" : "missing",
            format_ready ? "ready" : "missing");
        register_native_file_helpers();
    }
}

static void ensure_string_primitives(void) {
    attempt_string_library_open();
    attempt_io_library_open();
    attempt_string_library_patch();
}

static bool telemetry_runtime_ready(void) {
    static const char *const required_globals[] = {"strsub", "strbyte", "strformat"};

    ensure_string_primitives();

    bool globals_ready = true;
    bool global_status[sizeof(required_globals) / sizeof(required_globals[0])];
    for (size_t i = 0; i < sizeof(required_globals) / sizeof(required_globals[0]); ++i) {
        global_status[i] = function_exists(required_globals[i]);
        if (!global_status[i]) {
            globals_ready = false;
        }
    }
    if (globals_ready) {
        return true;
    }
    if (!globals_ready) {
        bool should_log = false;
        pthread_mutex_lock(&telemetry_mutex);
        if (!telemetry_missing_globals_logged) {
            telemetry_missing_globals_logged = true;
            should_log = true;
        }
        pthread_mutex_unlock(&telemetry_mutex);
        if (should_log) {
            log_event(
                "telemetry runtime waiting on global functions (strsub=%s, strbyte=%s, strformat=%s)",
                global_status[0] ? "ready" : "missing",
                global_status[1] ? "ready" : "missing",
                global_status[2] ? "ready" : "missing");
        }
        return false;
    }

    return true;
}

static void attempt_telemetry_injection(void) {
    bool should_check = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (telemetry_requested && !telemetry_injected) {
        should_check = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);

    if (!should_check) {
        return;
    }

    if (!telemetry_runtime_ready()) {
        bool should_log_wait = false;
        pthread_mutex_lock(&telemetry_mutex);
        if (!telemetry_wait_logged) {
            telemetry_wait_logged = true;
            should_log_wait = true;
        }
        pthread_mutex_unlock(&telemetry_mutex);

        if (should_log_wait) {
            log_event("telemetry runtime prerequisites missing; deferring injection");
        }
        return;
    }

    bool inject_now = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (telemetry_requested && !telemetry_injected) {
        telemetry_injected = true;
        inject_now = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);

    if (inject_now) {
        log_event("telemetry runtime ready; injecting telemetry");
        inject_telemetry();
    }
}

static void maybe_inject(const char *filename, int original_result) {
    if (!filename || original_result != 0) {
        attempt_telemetry_injection();
        return;
    }

    const char *basename = basename_or_self(filename);
    if (!basename || strcmp(basename, TARGET_SCRIPT) != 0) {
        attempt_telemetry_injection();
        return;
    }

    bool first_detection = false;
    bool already_injected = false;
    bool pending_injection = false;

    pthread_mutex_lock(&telemetry_mutex);
    if (!telemetry_requested) {
        telemetry_requested = true;
        first_detection = true;
    }
    already_injected = telemetry_injected;
    pending_injection = telemetry_requested && !telemetry_injected;
    pthread_mutex_unlock(&telemetry_mutex);

    if (first_detection) {
        log_event("detected %s load; telemetry will inject once runtime is ready", TARGET_SCRIPT);
    } else if (already_injected) {
        log_event("repeat %s load encountered; telemetry already injected", TARGET_SCRIPT);
    } else if (pending_injection) {
        log_event("repeat %s load encountered; telemetry awaiting runtime readiness", TARGET_SCRIPT);
    }

    attempt_telemetry_injection();
}

static int forward_lua_call(const char *filename, lua_dofile_fn real_fn, const char *label) {
    if (!real_fn) {
        log_event("no real implementation found for %s", label);
        return -1;
    }

    int result = real_fn((char *)filename);
    if (filename && filename[0] != '\0') {
        log_event("%s called for %s -> %d", label, filename, result);
        if (result != 0 && filename && strcmp(filename, TELEMETRY_SCRIPT) == 0) {
            log_bootstrap_error();
        }
    }
    maybe_inject(filename, result);
    return result;
}

__attribute__((constructor))
static void loader_notice(void) {
    pthread_once(&resolve_once, resolve_real_symbols);
    log_event("grim Lua hook shim loaded");
}

int lua_dofile(char *filename) {
    pthread_once(&resolve_once, resolve_real_symbols);
    return forward_lua_call(filename, real_lua_dofile, "lua_dofile");
}
