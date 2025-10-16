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

typedef int (*lua_dofile_fn)(const char *filename);
typedef unsigned int lua_Object;
typedef lua_Object (*lua_getglobal_fn)(const char *name);
typedef const char *(*lua_getstring_fn)(lua_Object object);

static lua_dofile_fn real_lua_dofile = NULL;
static lua_getglobal_fn real_lua_getglobal = NULL;
static lua_getstring_fn real_lua_getstring = NULL;

static pthread_once_t resolve_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t telemetry_mutex = PTHREAD_MUTEX_INITIALIZER;
static bool telemetry_injected = false;

static const char *const TARGET_SCRIPT = "_system.lua";
static const char *const TELEMETRY_SCRIPT = "mods/telemetry.lua";
static const char *const LOG_PATH = "mods/telemetry.log";
static const char *const TELEMETRY_BOOTSTRAP_ERROR_GLOBAL = "__telemetry_bootstrap_error";

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

static void inject_telemetry(void) {
    if (!real_lua_dofile) {
        log_event("telemetry injection skipped: real lua_dofile unavailable");
        return;
    }

    int result = real_lua_dofile(TELEMETRY_SCRIPT);
    if (result != 0) {
        log_event("telemetry script %s returned error code %d", TELEMETRY_SCRIPT, result);
        log_bootstrap_error();
    } else {
        log_event("telemetry script %s executed", TELEMETRY_SCRIPT);
    }
}

static void maybe_inject(const char *filename, int original_result) {
    if (!filename || original_result != 0) {
        return;
    }

    const char *basename = basename_or_self(filename);
    if (!basename || strcmp(basename, TARGET_SCRIPT) != 0) {
        return;
    }

    bool should_inject = false;

    pthread_mutex_lock(&telemetry_mutex);
    if (!telemetry_injected) {
        telemetry_injected = true;
        should_inject = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);

    if (should_inject) {
        log_event("detected %s load, injecting telemetry", TARGET_SCRIPT);
        inject_telemetry();
    } else {
        log_event("repeat %s load encountered; telemetry already injected", TARGET_SCRIPT);
    }
}

static int forward_lua_call(const char *filename, lua_dofile_fn real_fn, const char *label) {
    if (!real_fn) {
        log_event("no real implementation found for %s", label);
        return -1;
    }

    int result = real_fn(filename);
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

int lua_dofile(const char *filename) {
    pthread_once(&resolve_once, resolve_real_symbols);
    return forward_lua_call(filename, real_lua_dofile, "lua_dofile");
}
