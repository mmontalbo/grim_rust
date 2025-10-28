#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <time.h>
#include <sys/stat.h>
#include "lua_runtime.h"

typedef int (*lua_updatetasks_fn)(void);
typedef int (*luaD_call_internal_fn)(void *function_slot, int results);
typedef void (*luaD_taskHook_internal_fn)(void *task, int event);
typedef void *(*lua_currenttask_fn)(void);

typedef struct Lua32TObject Lua32TObject;
typedef struct Lua32TaggedString Lua32TaggedString;
typedef struct Lua32TProtoFunc Lua32TProtoFunc;
typedef struct Lua32Closure Lua32Closure;

typedef struct Lua32GCNode {
    struct Lua32GCNode *next;
    int marked;
} Lua32GCNode;

typedef union {
    lua_CFunction f;
    double n;
    Lua32TaggedString *ts;
    Lua32TProtoFunc *tf;
    Lua32Closure *cl;
    void *a;
    int i;
    void *ptr;
} Lua32Value;

struct Lua32TObject {
    int ttype;
    Lua32Value value;
};

struct Lua32TaggedString {
    Lua32GCNode head;
    unsigned long hash;
    int constindex;
    union {
        struct {
            Lua32TObject globalval;
            long len;
        } s;
        struct {
            int tag;
            void *v;
        } d;
    } u;
    char str[1];
};

struct Lua32TProtoFunc {
    Lua32GCNode head;
    Lua32TObject *consts;
    int nconsts;
    unsigned char *code;
    int lineDefined;
    Lua32TaggedString *source;
    void *locvars;
};

struct Lua32Closure {
    Lua32GCNode head;
    int nelems;
    Lua32TObject consts[1];
};

enum {
    LUA32_T_USERDATA = 0,
    LUA32_T_NUMBER = -1,
    LUA32_T_STRING = -2,
    LUA32_T_ARRAY = -3,
    LUA32_T_PROTO = -4,
    LUA32_T_CPROTO = -5,
    LUA32_T_NIL = -6,
    LUA32_T_CLOSURE = -7,
    LUA32_T_CLMARK = -8,
    LUA32_T_PMARK = -9,
    LUA32_T_CMARK = -10,
};

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
static lua_pushstring_fn real_lua_pushstring = NULL;
static lua_callfunction_fn real_lua_callfunction = NULL;
static lua_beginblock_fn real_lua_beginblock = NULL;
static lua_endblock_fn real_lua_endblock = NULL;
static lua_updatetasks_fn real_lua_updatetasks = NULL;
static luaD_call_internal_fn real_luaD_call = NULL;
static luaD_taskHook_internal_fn real_luaD_taskHook = NULL;
static lua_currenttask_fn real_lua_currenttask = NULL;

static pthread_once_t resolve_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t telemetry_mutex = PTHREAD_MUTEX_INITIALIZER;
static bool telemetry_injected = false;
static bool telemetry_requested = false;
static bool telemetry_wait_logged = false;
static bool telemetry_trace_dofile = false;
static bool telemetry_force_injection = false;
static bool error_method_installed = false;
static bool previous_error_method_valid = false;
static int previous_error_method_ref = -2;
static pthread_mutex_t steamapi_stub_mutex = PTHREAD_MUTEX_INITIALIZER;
static bool steamapi_init_called = false;
static unsigned int steamapi_callback_count = 0;

#define STEAMAPI_EXPORT __attribute__((visibility("default")))

static void log_event(const char *fmt, ...);
static void log_bootstrap_error(void);
static bool function_exists(const char *name);

static TelemetryRuntimeState telemetry_runtime_state = TELEMETRY_RUNTIME_STATE_INIT;
static TelemetryLuaApi telemetry_lua_api = {0};
static TelemetryRuntimeHooks telemetry_runtime_hooks = {
    .log = log_event,
    .log_bootstrap_error = log_bootstrap_error,
};

#define LUA_TASK_EVENT_DISPATCH 2
#define LUA_TASK_EVENT_COMPLETE 4

// Track when the retail scheduler is walking tasks so luaD_call hooks know
// whether to snapshot the callable before luaD_call consumes it.
static __thread int lua_updatetasks_depth = 0;
static __thread bool pending_task_log = false;
static __thread void *pending_task_descriptor = NULL;
static __thread bool logging_task_snapshot = false;

static void log_once(const char *label) {
    static pthread_mutex_t once_mutex = PTHREAD_MUTEX_INITIALIZER;
    static bool logged_init = false;
    static bool logged_restart = false;
    static bool logged_run = false;
    static bool logged_shutdown = false;
    static bool logged_register = false;
    static bool logged_unregister = false;

    pthread_mutex_lock(&once_mutex);
    bool *flag = NULL;
    if (strcmp(label, "init") == 0) {
        flag = &logged_init;
    } else if (strcmp(label, "restart") == 0) {
        flag = &logged_restart;
    } else if (strcmp(label, "run") == 0) {
        flag = &logged_run;
    } else if (strcmp(label, "shutdown") == 0) {
        flag = &logged_shutdown;
    } else if (strcmp(label, "register") == 0) {
        flag = &logged_register;
    } else if (strcmp(label, "unregister") == 0) {
        flag = &logged_unregister;
    }
    bool already = flag && *flag;
    if (flag && !*flag) {
        *flag = true;
    }
    pthread_mutex_unlock(&once_mutex);
    if (flag && !already) {
        if (strcmp(label, "init") == 0) {
            log_event("SteamAPI_Init stubbed via grim shim (forcing success)");
        } else if (strcmp(label, "restart") == 0) {
            log_event("SteamAPI_RestartAppIfNecessary stubbed; never requesting restart");
        } else if (strcmp(label, "run") == 0) {
            log_event("SteamAPI_RunCallbacks stub active (callbacks are no-ops)");
        } else if (strcmp(label, "shutdown") == 0) {
            log_event("SteamAPI_Shutdown stubbed; clearing shim bookkeeping only");
        } else if (strcmp(label, "register") == 0) {
            log_event("SteamAPI_RegisterCallback stubbed; callbacks are tracked but never invoked");
        } else if (strcmp(label, "unregister") == 0) {
            log_event("SteamAPI_UnregisterCallback stubbed; callbacks are tracked but idle");
        }
    }
}

STEAMAPI_EXPORT bool SteamAPI_RestartAppIfNecessary(uint32_t app_id) {
    (void)app_id;
    log_once("restart");
    return false;
}

STEAMAPI_EXPORT bool SteamAPI_Init(void) {
    log_once("init");
    pthread_mutex_lock(&steamapi_stub_mutex);
    steamapi_init_called = true;
    steamapi_callback_count = 0;
    pthread_mutex_unlock(&steamapi_stub_mutex);
    return true;
}

STEAMAPI_EXPORT void SteamAPI_RunCallbacks(void) {
    pthread_mutex_lock(&steamapi_stub_mutex);
    bool initialized = steamapi_init_called;
    pthread_mutex_unlock(&steamapi_stub_mutex);
    if (!initialized) {
        return;
    }
    log_once("run");
}

STEAMAPI_EXPORT void SteamAPI_Shutdown(void) {
    log_once("shutdown");
    pthread_mutex_lock(&steamapi_stub_mutex);
    steamapi_init_called = false;
    steamapi_callback_count = 0;
    pthread_mutex_unlock(&steamapi_stub_mutex);
}

STEAMAPI_EXPORT void SteamAPI_RegisterCallback(void *callback, int callback_id) {
    (void)callback;
    (void)callback_id;
    log_once("register");
    pthread_mutex_lock(&steamapi_stub_mutex);
    if (steamapi_init_called && steamapi_callback_count < UINT_MAX) {
        steamapi_callback_count++;
    }
    pthread_mutex_unlock(&steamapi_stub_mutex);
}

STEAMAPI_EXPORT void SteamAPI_UnregisterCallback(void *callback) {
    (void)callback;
    log_once("unregister");
    pthread_mutex_lock(&steamapi_stub_mutex);
    if (steamapi_init_called && steamapi_callback_count > 0) {
        steamapi_callback_count--;
    }
    pthread_mutex_unlock(&steamapi_stub_mutex);
}

static const char *const TARGET_SCRIPT = "_system.lua";
static const char *const TELEMETRY_SCRIPT = "mods/telemetry.lua";
static const char *const LOG_PATH = "mods/telemetry.log";
static const char *const TELEMETRY_BOOTSTRAP_ERROR_GLOBAL = "__telemetry_bootstrap_error";
static const char *const TELEMETRY_STUB_REASON_GLOBAL = "__telemetry_stub_reason";

static void log_lua_stack_error(void) {
    lua_Object err = lua_pop();
    if (err == LUA_NOOBJECT) {
        log_event("telemetry shim could not inspect lua error stack");
        return;
    }
    const char *message = lua_getstring(err);
    if (message && message[0] != '\0') {
        log_event("telemetry lua error detail: %s", message);
    } else {
        int tag = lua_tag(err);
        log_event("telemetry lua error detail: non-string object (tag=%d)", tag);
    }
    lua_pushobject(err);
}

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

static bool telemetry_env_flag_enabled(const char *value) {
    if (!value || value[0] == '\0') {
        return false;
    }
    if (strcasecmp(value, "0") == 0 || strcasecmp(value, "false") == 0 || strcasecmp(value, "off") == 0 ||
        strcasecmp(value, "no") == 0) {
        return false;
    }
    return true;
}

static void telemetry_load_debug_flags(void) {
    static bool flags_loaded = false;
    if (flags_loaded) {
        return;
    }
    flags_loaded = true;

    const char *trace_env = getenv("GRIM_LUA_TRACE_DOFILe");
    if (telemetry_env_flag_enabled(trace_env)) {
        telemetry_trace_dofile = true;
        log_event("GRIM_LUA_TRACE_DOFILe enabled; tracing every lua_dofile invocation");
    }

    const char *force_env = getenv("GRIM_LUA_FORCE_INJECT");
    if (telemetry_env_flag_enabled(force_env)) {
        pthread_mutex_lock(&telemetry_mutex);
        telemetry_force_injection = true;
        telemetry_requested = true;
        pthread_mutex_unlock(&telemetry_mutex);
        log_event(
            "GRIM_LUA_FORCE_INJECT enabled; telemetry injection will not wait for %s",
            TARGET_SCRIPT);
    }
}

static void telemetry_reset_task_tracking(void) {
    pending_task_log = false;
    pending_task_descriptor = NULL;
}

static void telemetry_prepare_task_snapshot(void *task) {
    pending_task_descriptor = task;
    pending_task_log = true;
}

static bool lua32_copy_source_label(const Lua32TaggedString *source, char *buffer, size_t buffer_len) {
    if (!buffer || buffer_len == 0) {
        return false;
    }
    if (!source || buffer_len < 2) {
        buffer[0] = '\0';
        return false;
    }

    long declared_len = source->u.s.len;
    size_t copy_len = 0;
    if (declared_len > 0) {
        copy_len = (size_t)declared_len;
    } else {
        copy_len = strnlen(source->str, buffer_len - 1);
    }
    if (copy_len >= buffer_len) {
        copy_len = buffer_len - 1;
    }
    memcpy(buffer, source->str, copy_len);
    buffer[copy_len] = '\0';
    return copy_len > 0;
}

static void lua32_describe_callable(
    const Lua32TObject *slot,
    char *buffer,
    size_t buffer_len,
    void **callable_out,
    int *tag_out) {
    if (!buffer || buffer_len == 0) {
        return;
    }
    buffer[0] = '\0';
    if (callable_out) {
        *callable_out = NULL;
    }
    if (tag_out) {
        *tag_out = LUA32_T_NIL;
    }
    if (!slot) {
        snprintf(buffer, buffer_len, "<null>");
        return;
    }

    int tag = slot->ttype;
    if (tag_out) {
        *tag_out = tag;
    }

    if (tag == LUA32_T_PROTO || tag == LUA32_T_PMARK) {
        Lua32TProtoFunc *proto = slot->value.tf;
        if (callable_out) {
            *callable_out = proto;
        }
        if (lua32_copy_source_label(proto ? proto->source : NULL, buffer, buffer_len)) {
            return;
        }
    } else if (tag == LUA32_T_CLOSURE || tag == LUA32_T_CLMARK) {
        Lua32Closure *closure = slot->value.cl;
        if (callable_out) {
            *callable_out = closure;
        }
        if (closure) {
            const Lua32TObject *proto_obj = &closure->consts[0];
            if (proto_obj && (proto_obj->ttype == LUA32_T_PROTO || proto_obj->ttype == LUA32_T_PMARK)) {
                Lua32TProtoFunc *proto = proto_obj->value.tf;
                if (lua32_copy_source_label(proto ? proto->source : NULL, buffer, buffer_len)) {
                    return;
                }
            }
        }
    } else if (tag == LUA32_T_CPROTO || tag == LUA32_T_CMARK) {
        if (callable_out) {
            *callable_out = (void *)slot->value.f;
        }
        snprintf(buffer, buffer_len, "<cfunc:%p>", (void *)slot->value.f);
        return;
    } else if (tag == LUA32_T_STRING) {
        if (lua32_copy_source_label(slot->value.ts, buffer, buffer_len)) {
            return;
        }
    }

    void *raw = slot ? slot->value.ptr : NULL;
    snprintf(buffer, buffer_len, "<tag:%d value:%p>", tag, raw);
}

static void telemetry_log_task_dispatch(void *task, const Lua32TObject *slot, int results) {
    if (logging_task_snapshot) {
        return;
    }
    logging_task_snapshot = true;

    char script_label[256];
    void *callable = NULL;
    int tag = LUA32_T_NIL;
    lua32_describe_callable(slot, script_label, sizeof(script_label), &callable, &tag);
    if (script_label[0] == '\0') {
        snprintf(script_label, sizeof(script_label), "<unknown>");
    }

    void *current = NULL;
    if (real_lua_currenttask) {
        current = real_lua_currenttask();
    }

    log_event(
        "lua scheduler task dispatch: descriptor=%p current=%p slot=%p callable=%p tag=%d results=%d script=%s",
        task,
        current,
        slot,
        callable,
        tag,
        results,
        script_label);

    logging_task_snapshot = false;
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
    if (err != NULL || !real_lua_pushcclosure) {
        const char *primary_err = err;
        dlerror();
        real_lua_pushcclosure = (lua_pushcclosure_fn)dlsym(RTLD_NEXT, "lua_pushCclosure");
        err = dlerror();
        if (err != NULL || !real_lua_pushcclosure) {
            const char *alias_err = err;
            log_event(
                "failed to resolve lua_pushcclosure (primary: %s; alias lua_pushCclosure: %s)",
                primary_err ? primary_err : "unknown",
                alias_err ? alias_err : "unknown");
        } else {
            log_event("resolved lua_pushcclosure via alias lua_pushCclosure");
        }
    }

    dlerror();
    real_lua_setglobal = (lua_setglobal_fn)dlsym(RTLD_NEXT, "lua_setglobal");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_setglobal: %s", err);
    }

    dlerror();
    real_lua_pushstring = (lua_pushstring_fn)dlsym(RTLD_NEXT, "lua_pushstring");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_pushstring: %s", err);
    }

    dlerror();
    real_lua_callfunction = (lua_callfunction_fn)dlsym(RTLD_NEXT, "lua_callfunction");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_callfunction: %s", err);
    }

    dlerror();
    real_lua_dostring = (lua_dostring_fn)dlsym(RTLD_NEXT, "lua_dostring");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_dostring: %s", err);
    }

    dlerror();
    real_lua_beginblock = (lua_beginblock_fn)dlsym(RTLD_NEXT, "lua_beginblock");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_beginblock: %s", err);
    }

    dlerror();
    real_lua_endblock = (lua_endblock_fn)dlsym(RTLD_NEXT, "lua_endblock");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_endblock: %s", err);
    }

    dlerror();
    real_lua_updatetasks = (lua_updatetasks_fn)dlsym(RTLD_NEXT, "lua_updatetasks");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_updatetasks: %s", err);
    }

    dlerror();
    real_luaD_call = (luaD_call_internal_fn)dlsym(RTLD_NEXT, "luaD_call");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve luaD_call: %s", err);
    }

    dlerror();
    real_luaD_taskHook = (luaD_taskHook_internal_fn)dlsym(RTLD_NEXT, "luaD_taskHook");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve luaD_taskHook: %s", err);
    }

    dlerror();
    real_lua_currenttask = (lua_currenttask_fn)dlsym(RTLD_NEXT, "lua_currenttask");
    err = dlerror();
    if (err != NULL) {
        log_event("failed to resolve lua_currenttask: %s", err);
    }

    telemetry_runtime_refresh_api(
        &telemetry_lua_api,
        real_lua_getglobal,
        real_lua_getstring,
        real_lua_isfunction,
        real_lua_strlibopen,
        real_lua_iolibopen,
        real_lua_dostring,
        real_lua_beginblock,
        real_lua_endblock);
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

static void telemetry_emit_label(const char *label) {
    if (!telemetry_injected || !label || label[0] == '\0') {
        return;
    }
    if (!real_lua_getglobal || !real_lua_isfunction || !real_lua_pushstring || !real_lua_callfunction) {
        return;
    }
    lua_Object fn = real_lua_getglobal("telemetry_event");
    if (fn == 0 || !real_lua_isfunction(fn)) {
        return;
    }
    real_lua_pushstring((char *)label);
    int call_result = real_lua_callfunction(fn);
    if (call_result != 0) {
        log_event("telemetry_event(%s) failed (%d)", label, call_result);
    }
}

static void telemetry_mark_key(const char *key) {
    if (!telemetry_injected || !key || key[0] == '\0') {
        return;
    }
    if (!real_lua_getglobal || !real_lua_isfunction || !real_lua_pushstring || !real_lua_callfunction) {
        return;
    }
    lua_Object fn = real_lua_getglobal("telemetry_mark");
    if (fn == 0 || !real_lua_isfunction(fn)) {
        return;
    }
    real_lua_pushstring((char *)key);
    int call_result = real_lua_callfunction(fn);
    if (call_result != 0) {
        log_event("telemetry_mark(%s) failed (%d)", key, call_result);
    }
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

static void telemetry_guard_capture(void) {
    lua_Object message_obj = lua_getparam(1);
    if (message_obj == LUA_NOOBJECT) {
        TELEMETRY_LOG_GUARD(log_event, "telemetry guard capture invoked without error object");
        return;
    }
    const char *message = lua_isstring(message_obj) ? lua_getstring(message_obj) : NULL;
    if (message && message[0] != '\0') {
        TELEMETRY_LOG_GUARD(log_event, "telemetry guard captured lua error: %s", message);
        return;
    }
    TELEMETRY_LOG_GUARD(log_event, "telemetry guard captured lua error (non-string object)");
}

static void register_native_file_helpers(void) {
    bool should_register = false;
    bool already_registered = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (!telemetry_runtime_state.native_file_helpers_registered) {
        telemetry_runtime_state.native_file_helpers_registered = true;
        should_register = true;
    } else {
        already_registered = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);

    if (!should_register) {
        if (already_registered) {
            TELEMETRY_LOG_NATIVE(log_event, "telemetry native file helpers already registered; skipping duplicate request");
        } else {
            TELEMETRY_LOG_NATIVE(log_event, "telemetry native file helpers registration skipped (flag mismatch)");
        }
        return;
    }

    if (!real_lua_pushcclosure || !real_lua_setglobal) {
        TELEMETRY_LOG_NATIVE(
            log_event,
            "cannot register native file helpers: lua_pushcclosure=%s, lua_setglobal=%s",
            real_lua_pushcclosure ? "ready" : "missing",
            real_lua_setglobal ? "ready" : "missing");
        return;
    }

    TELEMETRY_LOG_NATIVE(
        log_event,
        "registering telemetry native file helpers (lua_pushcclosure=%p, lua_setglobal=%p)",
        (void *)real_lua_pushcclosure,
        (void *)real_lua_setglobal);
    real_lua_pushcclosure(telemetry_native_write, 0);
    real_lua_setglobal((char *)"telemetry_native_write");
    TELEMETRY_LOG_NATIVE(log_event, "telemetry native file helpers registered");
}

static void register_guard_logger(void) {
    bool should_register = false;
    pthread_mutex_lock(&telemetry_mutex);
    if (!telemetry_runtime_state.guard_logger_registered) {
        telemetry_runtime_state.guard_logger_registered = true;
        should_register = true;
    }
    pthread_mutex_unlock(&telemetry_mutex);

    if (!should_register) {
        TELEMETRY_LOG_GUARD(log_event, "telemetry guard logger already registered; skipping duplicate request");
        return;
    }

    if (!real_lua_pushcclosure || !real_lua_setglobal) {
        TELEMETRY_LOG_GUARD(
            log_event,
            "cannot register guard logger: lua_pushcclosure=%s, lua_setglobal=%s",
            real_lua_pushcclosure ? "ready" : "missing",
            real_lua_setglobal ? "ready" : "missing");
        return;
    }

    real_lua_pushcclosure(telemetry_guard_capture, 0);
    real_lua_setglobal((char *)"telemetry_guard_capture");
    TELEMETRY_LOG_GUARD(log_event, "telemetry guard logger registered");
}

static void forward_error_to_previous(lua_Object err) {
    if (!error_method_installed || !previous_error_method_valid || !real_lua_callfunction) {
        return;
    }
    lua_beginblock();
    lua_Object handler = lua_getref(previous_error_method_ref);
    if (handler == LUA_NOOBJECT) {
        lua_endblock();
        return;
    }
    if (err != LUA_NOOBJECT) {
        lua_pushobject(err);
    } else {
        lua_pushnil();
    }
    int forward_result = real_lua_callfunction(handler);
    lua_endblock();
    if (forward_result != 0) {
        log_event("previous Lua error handler failed (%d)", forward_result);
    }
}

static void telemetry_error_interceptor(void) {
    lua_Object err = lua_getparam(1);
    const char *message = NULL;
    if (err != LUA_NOOBJECT && lua_isstring(err)) {
        message = lua_getstring(err);
    }
    if (!message || message[0] == '\0') {
        message = "telemetry lua error (no message)";
    }
    log_event("telemetry bootstrap error (interceptor): %s", message);
    if (real_lua_pushstring && real_lua_setglobal) {
        real_lua_pushstring((char *)message);
        real_lua_setglobal((char *)TELEMETRY_BOOTSTRAP_ERROR_GLOBAL);
    }
    forward_error_to_previous(err);
    if (real_lua_pushstring) {
        real_lua_pushstring((char *)message);
    } else {
        lua_pushstring((char *)message);
    }
}

static void install_error_interceptor(void) {
    if (error_method_installed) {
        return;
    }
    if (!real_lua_pushcclosure) {
        log_event("cannot install telemetry error interceptor: lua_pushcclosure missing");
        return;
    }
    lua_beginblock();
    real_lua_pushcclosure(telemetry_error_interceptor, 0);
    lua_Object previous_error_method = lua_seterrormethod();
    previous_error_method_valid = false;
    previous_error_method_ref = -2;
    if (previous_error_method != LUA_NOOBJECT) {
        lua_pushobject(previous_error_method);
        previous_error_method_ref = lua_ref(1);
        if (previous_error_method_ref >= 0) {
            previous_error_method_valid = true;
        } else if (previous_error_method_ref == -1) {
            previous_error_method_valid = false;
        }
    }
    lua_endblock();
    error_method_installed = true;
    if (previous_error_method_valid) {
        log_event("telemetry error interceptor installed (delegating to previous handler)");
    } else {
        log_event("telemetry error interceptor installed");
    }
}

static void restore_error_interceptor(void) {
    if (!error_method_installed) {
        return;
    }
    lua_beginblock();
    if (previous_error_method_valid && previous_error_method_ref >= 0) {
        lua_Object handler = lua_getref(previous_error_method_ref);
        if (handler != LUA_NOOBJECT) {
            lua_pushobject(handler);
        } else {
            lua_pushnil();
        }
        lua_seterrormethod();
        lua_unref(previous_error_method_ref);
    } else {
        lua_pushnil();
        lua_seterrormethod();
        if (previous_error_method_ref >= 0) {
            lua_unref(previous_error_method_ref);
        }
    }
    lua_endblock();
    error_method_installed = false;
    previous_error_method_valid = false;
    previous_error_method_ref = -2;
    log_event("telemetry error interceptor restored");
}

static bool telemetry_can_call_dofile_function(void) {
    return real_lua_getglobal && real_lua_isfunction && real_lua_pushstring && real_lua_callfunction;
}

static int telemetry_execute_via_callfunction(void) {
    if (!telemetry_can_call_dofile_function()) {
        log_event("telemetry protected call unavailable: lua_getglobal, lua_callfunction, or lua_pushstring missing");
        return -1;
    }
    lua_Object dofile_fn = real_lua_getglobal("dofile");
    if (dofile_fn == 0 || !real_lua_isfunction(dofile_fn)) {
        log_event("telemetry protected call unavailable: global dofile missing");
        return -1;
    }
    log_event(
        "pcall unavailable; executing telemetry script %s via lua_callfunction(dofile)",
        TELEMETRY_SCRIPT);
    lua_beginblock();
    real_lua_pushstring((char *)TELEMETRY_SCRIPT);
    int call_result = real_lua_callfunction(dofile_fn);
    lua_endblock();
    return call_result;
}

static int telemetry_execute_guarded_script(void) {
    bool guard_available = real_lua_dostring && function_exists("pcall");
    install_error_interceptor();
    int result = -1;

    if (guard_available) {
        register_guard_logger();
        char chunk[512];
        int written = snprintf(
            chunk,
            sizeof(chunk),
            "local ok, payload = pcall(function() return dofile('%s') end)\n"
            "if ok then %s = nil; return 0 end\n"
            "local msg = payload\n"
            "if type(msg) ~= 'string' then msg = tostring(msg or 'telemetry error') end\n"
            "if type(telemetry_guard_capture) == 'function' then telemetry_guard_capture(msg) end\n"
            "%s = msg\n"
            "return error(msg)\n",
            TELEMETRY_SCRIPT,
            TELEMETRY_BOOTSTRAP_ERROR_GLOBAL,
            TELEMETRY_BOOTSTRAP_ERROR_GLOBAL);
        if (written >= 0 && (size_t)written < sizeof(chunk)) {
            log_event("executing telemetry script %s via guarded lua_dostring chunk", TELEMETRY_SCRIPT);
            result = real_lua_dostring(chunk);
            restore_error_interceptor();
            return result;
        }
        log_event("telemetry guard chunk exceeded buffer; falling back to protected call");
    }

    int callfunction_result = telemetry_execute_via_callfunction();
    if (callfunction_result != -1) {
        restore_error_interceptor();
        return callfunction_result;
    }

    if (!real_lua_dofile) {
        log_event("telemetry injection skipped: no lua entrypoint resolved");
        restore_error_interceptor();
        return -1;
    }

    log_event(
        "protected call unavailable; executing telemetry script %s via lua_dofile (unprotected)",
        TELEMETRY_SCRIPT);
    result = real_lua_dofile((char *)TELEMETRY_SCRIPT);
    restore_error_interceptor();
    return result;
}

static void inject_telemetry(void) {
    if (!real_lua_dofile && !real_lua_dostring) {
        log_event("telemetry injection skipped: no lua entrypoint resolved");
        return;
    }

    int result = telemetry_execute_guarded_script();
    if (result != 0) {
        log_event("telemetry script %s returned error code %d", TELEMETRY_SCRIPT, result);
        log_bootstrap_error();
        log_lua_stack_error();
    } else {
        log_event("telemetry script %s executed", TELEMETRY_SCRIPT);
        log_stub_reason();
    }
}

static bool function_exists(const char *name) {
    return telemetry_runtime_function_exists(&telemetry_lua_api, log_event, name);
}

static bool telemetry_runtime_dependencies_ready(void) {
    return telemetry_runtime_ready(
        &telemetry_runtime_state,
        &telemetry_mutex,
        &telemetry_lua_api,
        &telemetry_runtime_hooks,
        register_native_file_helpers);
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

    if (!telemetry_runtime_dependencies_ready()) {
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
    if (telemetry_trace_dofile) {
        bool requested = false;
        bool injected = false;
        pthread_mutex_lock(&telemetry_mutex);
        requested = telemetry_requested;
        injected = telemetry_injected;
        pthread_mutex_unlock(&telemetry_mutex);
        const char *name = (filename && filename[0] != '\0') ? filename : "(null)";
        log_event(
            "%s trace: %s -> %d (requested=%d injected=%d)",
            label,
            name,
            result,
            requested ? 1 : 0,
            injected ? 1 : 0);
    } else if (filename && filename[0] != '\0') {
        log_event("%s called for %s -> %d", label, filename, result);
    }
    if (result != 0 && filename && filename[0] != '\0' && strcmp(filename, TELEMETRY_SCRIPT) == 0) {
        log_bootstrap_error();
    }
    maybe_inject(filename, result);
    if (result == 0 && filename && filename[0] != '\0' && telemetry_injected) {
        const char *base = basename_or_self(filename);
        const char *name = (base && base[0] != '\0') ? base : filename;
        char event_label[128];
        char mark_key[128];
        snprintf(event_label, sizeof(event_label), "lua.load:%s", name);
        snprintf(mark_key, sizeof(mark_key), "script:%s", name);
        telemetry_emit_label(event_label);
        telemetry_mark_key(mark_key);
    }
    return result;
}

__attribute__((constructor))
static void loader_notice(void) {
    pthread_once(&resolve_once, resolve_real_symbols);
    telemetry_load_debug_flags();
    log_event("grim Lua hook shim loaded");
    if (telemetry_force_injection) {
        attempt_telemetry_injection();
    }
}

int lua_dofile(char *filename) {
    pthread_once(&resolve_once, resolve_real_symbols);
    return forward_lua_call(filename, real_lua_dofile, "lua_dofile");
}

int lua_updatetasks(void) {
    pthread_once(&resolve_once, resolve_real_symbols);
    if (!real_lua_updatetasks) {
        log_event("lua_updatetasks intercept missing real symbol");
        return -1;
    }
    lua_updatetasks_depth++;
    int result = real_lua_updatetasks();
    lua_updatetasks_depth--;
    if (lua_updatetasks_depth <= 0) {
        lua_updatetasks_depth = 0;
        telemetry_reset_task_tracking();
    }
    return result;
}

void luaD_taskHook(void *task, int event) {
    pthread_once(&resolve_once, resolve_real_symbols);
    if (lua_updatetasks_depth > 0) {
        if (event == LUA_TASK_EVENT_DISPATCH) {
            telemetry_prepare_task_snapshot(task);
        } else if (event == LUA_TASK_EVENT_COMPLETE) {
            telemetry_reset_task_tracking();
        }
    }

    if (real_luaD_taskHook) {
        real_luaD_taskHook(task, event);
    } else {
        log_event("luaD_taskHook intercept missing real symbol");
    }
}

int luaD_call(void *function_slot, int results) {
    pthread_once(&resolve_once, resolve_real_symbols);
    if (lua_updatetasks_depth > 0 && pending_task_log) {
        telemetry_log_task_dispatch(pending_task_descriptor, (const Lua32TObject *)function_slot, results);
        telemetry_reset_task_tracking();
    }

    if (!real_luaD_call) {
        log_event("luaD_call intercept missing real symbol");
        return -1;
    }
    return real_luaD_call(function_slot, results);
}
