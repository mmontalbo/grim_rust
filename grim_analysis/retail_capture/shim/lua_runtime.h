#ifndef LUA_RUNTIME_H
#define LUA_RUNTIME_H

#include <lua.h>
#include <pthread.h>
#include <stdbool.h>
#include <stddef.h>

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
typedef void (*lua_pushstring_fn)(char *s);
typedef int (*lua_callfunction_fn)(lua_Object function);
typedef void (*lua_beginblock_fn)(void);
typedef void (*lua_endblock_fn)(void);

typedef void (*telemetry_log_fn)(const char *fmt, ...);
typedef void (*telemetry_simple_fn)(void);

typedef struct {
    lua_getglobal_fn getglobal;
    lua_getstring_fn getstring;
    lua_isfunction_fn isfunction;
    lua_strlibopen_fn strlibopen;
    lua_iolibopen_fn iolibopen;
    lua_dostring_fn dostring;
    lua_beginblock_fn beginblock;
    lua_endblock_fn endblock;
} TelemetryLuaApi;

typedef struct {
    bool strlibopen_attempted;
    bool iolibopen_attempted;
    bool string_library_patch_attempted;
    bool native_file_helpers_registered;
    bool guard_logger_registered;
    bool missing_globals_logged;
    bool readiness_summary_logged;
} TelemetryRuntimeState;

#define TELEMETRY_RUNTIME_STATE_INIT                                                         \
    {                                                                                        \
        .strlibopen_attempted = false,                                                       \
        .iolibopen_attempted = false,                                                        \
        .string_library_patch_attempted = false,                                             \
        .native_file_helpers_registered = false,                                             \
        .guard_logger_registered = false,                                                    \
        .missing_globals_logged = false,                                                     \
        .readiness_summary_logged = false                                                    \
    }



typedef struct {
    telemetry_log_fn log;
    telemetry_simple_fn log_bootstrap_error;
} TelemetryRuntimeHooks;

#define TELEMETRY_LOG_CATEGORY(logger, category, fmt, ...)                                    \
    do {                                                                                      \
        if ((logger) != NULL) {                                                               \
            (logger)("[" category "] " fmt, ##__VA_ARGS__);                                    \
        }                                                                                     \
    } while (0)

#define TELEMETRY_LOG_RUNTIME(logger, fmt, ...) TELEMETRY_LOG_CATEGORY(logger, "runtime", fmt, ##__VA_ARGS__)
#define TELEMETRY_LOG_IO(logger, fmt, ...) TELEMETRY_LOG_CATEGORY(logger, "io", fmt, ##__VA_ARGS__)
#define TELEMETRY_LOG_STRING(logger, fmt, ...) TELEMETRY_LOG_CATEGORY(logger, "string", fmt, ##__VA_ARGS__)
#define TELEMETRY_LOG_NATIVE(logger, fmt, ...) TELEMETRY_LOG_CATEGORY(logger, "native", fmt, ##__VA_ARGS__)
#define TELEMETRY_LOG_GUARD(logger, fmt, ...) TELEMETRY_LOG_CATEGORY(logger, "guard", fmt, ##__VA_ARGS__)

void telemetry_runtime_state_init(TelemetryRuntimeState *state);
void telemetry_runtime_refresh_api(
    TelemetryLuaApi *api,
    lua_getglobal_fn getglobal,
    lua_getstring_fn getstring,
    lua_isfunction_fn isfunction,
    lua_strlibopen_fn strlibopen,
    lua_iolibopen_fn iolibopen,
    lua_dostring_fn dostring,
    lua_beginblock_fn beginblock,
    lua_endblock_fn endblock);
bool telemetry_runtime_function_exists(const TelemetryLuaApi *lua, telemetry_log_fn logger, const char *name);
void telemetry_attempt_string_library_open(TelemetryRuntimeState *state, pthread_mutex_t *mutex, const TelemetryLuaApi *lua, telemetry_log_fn logger);
void telemetry_attempt_io_library_open(TelemetryRuntimeState *state, pthread_mutex_t *mutex, const TelemetryLuaApi *lua, const TelemetryRuntimeHooks *hooks);
void telemetry_attempt_string_library_patch(TelemetryRuntimeState *state, pthread_mutex_t *mutex, const TelemetryLuaApi *lua, const TelemetryRuntimeHooks *hooks, telemetry_simple_fn register_native_file_helpers);
void telemetry_ensure_string_primitives(TelemetryRuntimeState *state, pthread_mutex_t *mutex, const TelemetryLuaApi *lua, const TelemetryRuntimeHooks *hooks, telemetry_simple_fn register_native_file_helpers);
bool telemetry_runtime_ready(TelemetryRuntimeState *state, pthread_mutex_t *mutex, const TelemetryLuaApi *lua, const TelemetryRuntimeHooks *hooks, telemetry_simple_fn register_native_file_helpers);

#endif
