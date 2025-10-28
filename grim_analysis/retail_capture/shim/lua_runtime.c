#include "lua_runtime.h"

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *const TELEMETRY_IO_STATUS_SCRIPT =
    "local io_type = type(io)\n"
    "local io_open_type = \"nil\"\n"
    "if io_type == \"table\" then\n"
    "  io_open_type = type(io.open)\n"
    "end\n"
    "if io_type == \"table\" and io_open_type == \"function\" then\n"
    "  __telemetry_io_ready = \"ready\"\n"
    "else\n"
    "  __telemetry_io_ready = \"missing (io=\" .. tostring(io_type) .. \", io.open=\" .. tostring(io_open_type) .. \")\"\n"
    "end\n";

static const char *const TELEMETRY_LEGACY_IO_WRAPPERS_SCRIPT =
    "if telemetry_legacy_io_wrapped then return end\n"
    "telemetry_legacy_io_wrapped = true\n"
    "telemetry_legacy_handles = {}\n"
    "function openfile(path, mode)\n"
    "  if type(path) ~= \"string\" then return nil end\n"
    "  local handle = { __path = path, __mode = mode or \"w\" }\n"
    "  telemetry_legacy_handles[handle] = 1\n"
    "  return handle\n"
    "end\n"
    "function write(handle, contents)\n"
    "  if type(handle) ~= \"table\" or type(handle.__path) ~= \"string\" then return nil end\n"
    "  if type(telemetry_native_write) ~= \"function\" then return nil end\n"
    "  local text = contents\n"
    "  if type(text) ~= \"string\" then text = tostring(text or \"\") end\n"
    "  local mode = handle.__mode\n"
    "  if type(mode) ~= \"string\" or mode == \"\" then mode = \"a\" end\n"
    "  telemetry_native_write(handle.__path, text, mode)\n"
    "  return 1\n"
    "end\n"
    "function closefile(handle)\n"
    "  if type(handle) ~= \"table\" then return nil end\n"
    "  if type(telemetry_legacy_handles) == \"table\" then\n"
    "    telemetry_legacy_handles[handle] = nil\n"
    "  end\n"
    "  return 1\n"
    "end\n";

static const char *const TELEMETRY_STRING_PATCH_SCRIPT =
    "if type(strbyte) ~= \"function\" and type(ascii) == \"function\" then strbyte = ascii end\n"
    "if type(strbyte) ~= \"function\" and type(string) == \"table\" and type(string.byte) == \"function\" then strbyte = string.byte end\n"
    "if type(strformat) ~= \"function\" and type(format) == \"function\" then strformat = format end\n"
    "if type(string) == \"table\" then\n"
    "  if type(string.sub) ~= \"function\" and type(strsub) == \"function\" then string.sub = strsub end\n"
    "  if type(string.byte) ~= \"function\" and type(strbyte) == \"function\" then string.byte = strbyte end\n"
    "  if type(string.len) ~= \"function\" and type(strlen) == \"function\" then string.len = strlen end\n"
    "  if type(string.format) ~= \"function\" and type(strformat) == \"function\" then string.format = strformat end\n"
    "end\n";

typedef enum {
    TELEMETRY_LOG_TARGET_RUNTIME = 0,
    TELEMETRY_LOG_TARGET_IO = 1,
    TELEMETRY_LOG_TARGET_STRING = 2,
    TELEMETRY_LOG_TARGET_NATIVE = 3,
    TELEMETRY_LOG_TARGET_GUARD = 4,
} TelemetryLogTarget;

#define TELEMETRY_LOG_TARGET_DISPATCH(target, logger, fmt, ...)                                          \
    do {                                                                                                 \
        switch (target) {                                                                                \
        case TELEMETRY_LOG_TARGET_IO:                                                                    \
            TELEMETRY_LOG_IO(logger, fmt, ##__VA_ARGS__);                                                \
            break;                                                                                       \
        case TELEMETRY_LOG_TARGET_STRING:                                                                \
            TELEMETRY_LOG_STRING(logger, fmt, ##__VA_ARGS__);                                            \
            break;                                                                                       \
        case TELEMETRY_LOG_TARGET_NATIVE:                                                                \
            TELEMETRY_LOG_NATIVE(logger, fmt, ##__VA_ARGS__);                                            \
            break;                                                                                       \
        case TELEMETRY_LOG_TARGET_GUARD:                                                                 \
            TELEMETRY_LOG_GUARD(logger, fmt, ##__VA_ARGS__);                                             \
            break;                                                                                       \
        case TELEMETRY_LOG_TARGET_RUNTIME:                                                               \
        default:                                                                                         \
            TELEMETRY_LOG_RUNTIME(logger, fmt, ##__VA_ARGS__);                                           \
            break;                                                                                       \
        }                                                                                                \
    } while (0)

typedef struct {
    bool openfile;
    bool write;
    bool closefile;
    bool custom;
    char source[64];
} TelemetryLegacyIoProbeConfig;

static TelemetryLegacyIoProbeConfig telemetry_legacy_io_probe_config = {
    .openfile = true,
    .write = true,
    .closefile = true,
    .custom = false,
    .source = {0},
};
static pthread_once_t telemetry_legacy_io_probe_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t telemetry_legacy_io_probe_log_mutex = PTHREAD_MUTEX_INITIALIZER;
static bool telemetry_legacy_io_probe_log_emitted = false;

typedef struct {
    TelemetryLogTarget category;
    bool log_stack;
    const char *context;
} TelemetryFunctionExistsOptions;

static const char *telemetry_describe_io_ready(const TelemetryLuaApi *lua, char *buffer, size_t buffer_len) {
    if (!buffer || buffer_len == 0) {
        return "unknown";
    }
    buffer[0] = '\0';
    if (!lua || !lua->getglobal) {
        snprintf(buffer, buffer_len, "%s", "unknown");
        return buffer;
    }
    bool used_block = lua->beginblock && lua->endblock;
    if (used_block) {
        lua->beginblock();
    }
    lua_Object ready_obj = lua->getglobal("__telemetry_io_ready");
    const char *ready_state = NULL;
    if (ready_obj != 0 && lua->getstring) {
        ready_state = lua->getstring(ready_obj);
    }
    if (used_block) {
        lua->endblock();
    } else if (ready_obj != 0) {
        lua_pop();
    }
    if (ready_state && ready_state[0] != '\0') {
        snprintf(buffer, buffer_len, "%s", ready_state);
    } else {
        snprintf(buffer, buffer_len, "%s", "unknown");
    }
    return buffer;
}

static void telemetry_log_readiness_summary(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger) {
    if (!state || !mutex || !logger) {
        return;
    }
    bool already_logged = false;
    pthread_mutex_lock(mutex);
    if (state->readiness_summary_logged) {
        already_logged = true;
    } else {
        state->readiness_summary_logged = true;
    }
    pthread_mutex_unlock(mutex);
    if (already_logged) {
        return;
    }
    bool strsub_ready = telemetry_runtime_function_exists(lua, NULL, "strsub");
    bool strbyte_ready = telemetry_runtime_function_exists(lua, NULL, "strbyte");
    bool strformat_ready = telemetry_runtime_function_exists(lua, NULL, "strformat");
    bool call_ready = telemetry_runtime_function_exists(lua, NULL, "call");
    bool unpack_ready = telemetry_runtime_function_exists(lua, NULL, "unpack");
    char io_buffer[96];
    const char *io_state = telemetry_describe_io_ready(lua, io_buffer, sizeof(io_buffer));
    if (!io_state || io_state[0] == '\0') {
        io_state = "unknown";
    }
    TELEMETRY_LOG_RUNTIME(
        logger,
        "readiness summary: strsub=%s strbyte=%s strformat=%s io=%s call=%s unpack=%s",
        strsub_ready ? "ready" : "missing",
        strbyte_ready ? "ready" : "missing",
        strformat_ready ? "ready" : "missing",
        io_state,
        call_ready ? "ready" : "missing",
        unpack_ready ? "ready" : "missing");
}

static bool telemetry_token_equals(const char *start, size_t len, const char *token);
static void telemetry_init_legacy_io_probe_config(void);
static const TelemetryLegacyIoProbeConfig *telemetry_get_legacy_io_probe_config(void);
static void telemetry_log_probe_config_if_needed(telemetry_log_fn logger, const TelemetryLegacyIoProbeConfig *config);
static const char *telemetry_legacy_io_probe_source_label(const TelemetryLegacyIoProbeConfig *config);
static int telemetry_lua_stack_depth(void);
static void telemetry_log_stack_event(
    TelemetryLogTarget target,
    telemetry_log_fn logger,
    const char *context,
    const char *operation,
    const char *phase,
    const char *symbol,
    lua_Object object);
static bool telemetry_runtime_function_exists_internal(
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger,
    const char *name,
    const TelemetryFunctionExistsOptions *options);
static void telemetry_probe_legacy_io(
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger,
    const TelemetryLegacyIoProbeConfig *config,
    const char *context_label,
    const char *summary_label);
static const char *telemetry_describe_io_ready(const TelemetryLuaApi *lua, char *buffer, size_t buffer_len);
static void telemetry_log_readiness_summary(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger);

static bool telemetry_token_equals(const char *start, size_t len, const char *token) {
    if (!start || !token) {
        return false;
    }
    size_t token_len = strlen(token);
    if (len != token_len) {
        return false;
    }
    for (size_t i = 0; i < len; ++i) {
        unsigned char lhs = (unsigned char)start[i];
        unsigned char rhs = (unsigned char)token[i];
        if ((unsigned char)tolower(lhs) != (unsigned char)tolower(rhs)) {
            return false;
        }
    }
    return true;
}

static void telemetry_init_legacy_io_probe_config(void) {
    telemetry_legacy_io_probe_config.openfile = true;
    telemetry_legacy_io_probe_config.write = true;
    telemetry_legacy_io_probe_config.closefile = true;
    telemetry_legacy_io_probe_config.custom = false;
    telemetry_legacy_io_probe_config.source[0] = '\0';

    const char *env = getenv("TELEMETRY_LEGACY_IO_PROBES");
    if (!env || env[0] == '\0') {
        return;
    }
    telemetry_legacy_io_probe_config.custom = true;
    snprintf(telemetry_legacy_io_probe_config.source, sizeof(telemetry_legacy_io_probe_config.source), "%s", env);

    size_t env_len = strlen(env);
    if (telemetry_token_equals(env, env_len, "none") || telemetry_token_equals(env, env_len, "off") ||
        telemetry_token_equals(env, env_len, "disable")) {
        telemetry_legacy_io_probe_config.openfile = false;
        telemetry_legacy_io_probe_config.write = false;
        telemetry_legacy_io_probe_config.closefile = false;
        return;
    }
    if (telemetry_token_equals(env, env_len, "all") || telemetry_token_equals(env, env_len, "default")) {
        telemetry_legacy_io_probe_config.openfile = true;
        telemetry_legacy_io_probe_config.write = true;
        telemetry_legacy_io_probe_config.closefile = true;
        return;
    }

    telemetry_legacy_io_probe_config.openfile = false;
    telemetry_legacy_io_probe_config.write = false;
    telemetry_legacy_io_probe_config.closefile = false;

    const char *cursor = env;
    while (*cursor != '\0') {
        while (*cursor == ',' || *cursor == ';' || isspace((unsigned char)*cursor)) {
            cursor++;
        }
        if (*cursor == '\0') {
            break;
        }
        const char *start = cursor;
        while (*cursor != '\0' && *cursor != ',' && *cursor != ';') {
            cursor++;
        }
        const char *end = cursor;
        while (end > start && isspace((unsigned char)*(end - 1))) {
            end--;
        }
        size_t token_len_trimmed = (size_t)(end - start);
        if (token_len_trimmed == 0) {
            if (*cursor == ',' || *cursor == ';') {
                cursor++;
            }
            continue;
        }
        if (telemetry_token_equals(start, token_len_trimmed, "openfile")) {
            telemetry_legacy_io_probe_config.openfile = true;
        } else if (telemetry_token_equals(start, token_len_trimmed, "write")) {
            telemetry_legacy_io_probe_config.write = true;
        } else if (telemetry_token_equals(start, token_len_trimmed, "closefile")) {
            telemetry_legacy_io_probe_config.closefile = true;
        } else if (telemetry_token_equals(start, token_len_trimmed, "all")) {
            telemetry_legacy_io_probe_config.openfile = true;
            telemetry_legacy_io_probe_config.write = true;
            telemetry_legacy_io_probe_config.closefile = true;
        }
        if (*cursor == ',' || *cursor == ';') {
            cursor++;
        }
    }
}

static const TelemetryLegacyIoProbeConfig *telemetry_get_legacy_io_probe_config(void) {
    pthread_once(&telemetry_legacy_io_probe_once, telemetry_init_legacy_io_probe_config);
    return &telemetry_legacy_io_probe_config;
}

static void telemetry_log_probe_config_if_needed(telemetry_log_fn logger, const TelemetryLegacyIoProbeConfig *config) {
    if (!config || !config->custom) {
        return;
    }
    pthread_mutex_lock(&telemetry_legacy_io_probe_log_mutex);
    bool already_logged = telemetry_legacy_io_probe_log_emitted;
    if (!already_logged) {
        telemetry_legacy_io_probe_log_emitted = true;
    }
    pthread_mutex_unlock(&telemetry_legacy_io_probe_log_mutex);
    if (already_logged) {
        return;
    }
    TELEMETRY_LOG_IO(
        logger,
        "legacy io probe config via TELEMETRY_LEGACY_IO_PROBES=\"%s\" (openfile=%s, write=%s, closefile=%s)",
        config->source[0] != '\0' ? config->source : "<empty>",
        config->openfile ? "enabled" : "disabled",
        config->write ? "enabled" : "disabled",
        config->closefile ? "enabled" : "disabled");
}

static const char *telemetry_legacy_io_probe_source_label(const TelemetryLegacyIoProbeConfig *config) {
    if (!config || !config->custom) {
        return "default";
    }
    if (config->source[0] == '\0') {
        return "<empty>";
    }
    return config->source;
}

static int telemetry_lua_stack_depth(void) {
    (void)lua_state;
    return -1;
}

static void telemetry_log_stack_event(
    TelemetryLogTarget target,
    telemetry_log_fn logger,
    const char *context,
    const char *operation,
    const char *phase,
    const char *symbol,
    lua_Object object) {
    const char *label = context ? context : "lua";
    const char *name = symbol ? symbol : "(null)";
    const char *point = phase ? phase : "unknown";
    int depth = telemetry_lua_stack_depth();

    if (object == LUA_NOOBJECT) {
        if (depth >= 0) {
            TELEMETRY_LOG_TARGET_DISPATCH(
                target,
                logger,
                "%s %s %s('%s') (stack=%d, object=n/a, tag=n/a)",
                label,
                point,
                operation ? operation : "operation",
                name,
                depth);
        } else {
            TELEMETRY_LOG_TARGET_DISPATCH(
                target,
                logger,
                "%s %s %s('%s') (stack=n/a, object=n/a, tag=n/a)",
                label,
                point,
                operation ? operation : "operation",
                name);
        }
        return;
    }

    int tag = lua_tag(object);
    if (depth >= 0) {
        TELEMETRY_LOG_TARGET_DISPATCH(
            target,
            logger,
            "%s %s %s('%s') (stack=%d, object=%p, tag=%d)",
            label,
            point,
            operation ? operation : "operation",
            name,
            depth,
            (void *)object,
            tag);
    } else {
        TELEMETRY_LOG_TARGET_DISPATCH(
            target,
            logger,
            "%s %s %s('%s') (stack=n/a, object=%p, tag=%d)",
            label,
            point,
            operation ? operation : "operation",
            name,
            (void *)object,
            tag);
    }
}

void telemetry_runtime_state_init(TelemetryRuntimeState *state) {
    if (!state) {
        return;
    }
    *state = (TelemetryRuntimeState)TELEMETRY_RUNTIME_STATE_INIT;
}

void telemetry_runtime_refresh_api(
    TelemetryLuaApi *api,
    lua_getglobal_fn getglobal,
    lua_getstring_fn getstring,
    lua_isfunction_fn isfunction,
    lua_strlibopen_fn strlibopen,
    lua_iolibopen_fn iolibopen,
    lua_dostring_fn dostring,
    lua_beginblock_fn beginblock,
    lua_endblock_fn endblock) {
    if (!api) {
        return;
    }
    api->getglobal = getglobal;
    api->getstring = getstring;
    api->isfunction = isfunction;
    api->strlibopen = strlibopen;
    api->iolibopen = iolibopen;
    api->dostring = dostring;
    api->beginblock = beginblock;
    api->endblock = endblock;
}

static bool telemetry_runtime_function_exists_internal(
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger,
    const char *name,
    const TelemetryFunctionExistsOptions *options) {
    TelemetryFunctionExistsOptions local = {
        .category = TELEMETRY_LOG_TARGET_RUNTIME,
        .log_stack = false,
        .context = NULL,
    };
    if (options) {
        local = *options;
    }
    if (!lua || !lua->getglobal || !lua->isfunction) {
        TELEMETRY_LOG_TARGET_DISPATCH(
            local.category,
            logger,
            "function_exists(%s) unavailable (lua_getglobal=%p, lua_isfunction=%p)",
            name ? name : "(null)",
            lua ? (void *)lua->getglobal : NULL,
            lua ? (void *)lua->isfunction : NULL);
        return false;
    }
    if (!name || name[0] == '\0') {
        TELEMETRY_LOG_TARGET_DISPATCH(local.category, logger, "function_exists invoked with empty name");
        return false;
    }
    bool used_block = lua && lua->beginblock && lua->endblock;
    if (used_block) {
        TELEMETRY_LOG_TARGET_DISPATCH(
            local.category,
            logger,
            "%s begin block for function_exists('%s')",
            local.context ? local.context : "runtime",
            name ? name : "(null)");
        lua->beginblock();
    }
    if (local.log_stack) {
        telemetry_log_stack_event(
            local.category,
            logger,
            local.context,
            "lua_getglobal",
            "before",
            name,
            LUA_NOOBJECT);
    }
    lua_Object object = lua->getglobal(name);
    if (local.log_stack) {
        telemetry_log_stack_event(
            local.category,
            logger,
            local.context,
            "lua_getglobal",
            "after",
            name,
            object);
    }
    if (object == 0) {
        TELEMETRY_LOG_TARGET_DISPATCH(local.category, logger, "function_exists(%s) -> missing (lua_getglobal returned 0)", name);
        if (used_block) {
            if (local.log_stack) {
                telemetry_log_stack_event(
                    local.category,
                    logger,
                    local.context,
                    "lua_endblock",
                    "before",
                    name,
                    LUA_NOOBJECT);
            }
            lua->endblock();
            if (local.log_stack) {
                telemetry_log_stack_event(
                    local.category,
                    logger,
                    local.context,
                    "lua_endblock",
                    "after",
                    name,
                    LUA_NOOBJECT);
            }
        }
        return false;
    }
    bool exists = lua->isfunction(object) != 0;
    if (used_block) {
        if (local.log_stack) {
            telemetry_log_stack_event(
                local.category,
                logger,
                local.context,
                "lua_endblock",
                "before",
                name,
                LUA_NOOBJECT);
        }
        lua->endblock();
        if (local.log_stack) {
            telemetry_log_stack_event(
                local.category,
                logger,
                local.context,
                "lua_endblock",
                "after",
                name,
                LUA_NOOBJECT);
        }
    } else {
        if (local.log_stack) {
            telemetry_log_stack_event(
                local.category,
                logger,
                local.context,
                "lua_pop",
                "before",
                name,
                LUA_NOOBJECT);
        }
        lua_Object popped = lua_pop();
        if (local.log_stack) {
            telemetry_log_stack_event(
                local.category,
                logger,
                local.context,
                "lua_pop",
                "after",
                name,
                popped);
        } else {
            (void)popped;
        }
    }
    if (!exists) {
        TELEMETRY_LOG_TARGET_DISPATCH(local.category, logger, "function_exists(%s) -> not a function", name);
    }
    return exists;
}

// Runs the legacy IO function probes with consistent logging so callers can reuse it pre/post wrapper install.
static void telemetry_probe_legacy_io(
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger,
    const TelemetryLegacyIoProbeConfig *config,
    const char *context_label,
    const char *summary_label) {
    const TelemetryLegacyIoProbeConfig *probe_config = config;
    if (!probe_config) {
        probe_config = telemetry_get_legacy_io_probe_config();
    }
    TelemetryFunctionExistsOptions io_probe_options = {
        .category = TELEMETRY_LOG_TARGET_IO,
        .log_stack = true,
        .context = context_label ? context_label : "legacy io probe",
    };
    const char *probe_source = telemetry_legacy_io_probe_source_label(probe_config);
    bool openfile_probe_enabled = !probe_config || probe_config->openfile;
    bool write_probe_enabled = !probe_config || probe_config->write;
    bool closefile_probe_enabled = !probe_config || probe_config->closefile;

    bool openfile_ready = false;
    bool write_ready = false;
    bool closefile_ready = false;

    if (openfile_probe_enabled) {
        openfile_ready = telemetry_runtime_function_exists_internal(lua, logger, "openfile", &io_probe_options);
    } else {
        TELEMETRY_LOG_IO(
            logger,
            "%s for openfile skipped (TELEMETRY_LEGACY_IO_PROBES=\"%s\")",
            io_probe_options.context,
            probe_source);
    }
    if (write_probe_enabled) {
        write_ready = telemetry_runtime_function_exists_internal(lua, logger, "write", &io_probe_options);
    } else {
        TELEMETRY_LOG_IO(
            logger,
            "%s for write skipped (TELEMETRY_LEGACY_IO_PROBES=\"%s\")",
            io_probe_options.context,
            probe_source);
    }
    if (closefile_probe_enabled) {
        closefile_ready = telemetry_runtime_function_exists_internal(lua, logger, "closefile", &io_probe_options);
    } else {
        TELEMETRY_LOG_IO(
            logger,
            "%s for closefile skipped (TELEMETRY_LEGACY_IO_PROBES=\"%s\")",
            io_probe_options.context,
            probe_source);
    }

    const char *openfile_label = openfile_probe_enabled ? (openfile_ready ? "ready" : "missing") : "skipped";
    const char *write_label = write_probe_enabled ? (write_ready ? "ready" : "missing") : "skipped";
    const char *closefile_label = closefile_probe_enabled ? (closefile_ready ? "ready" : "missing") : "skipped";
    const char *summary = (summary_label && summary_label[0] != '\0') ? summary_label : "legacy io functions";
    TELEMETRY_LOG_IO(
        logger,
        "%s (openfile=%s, write=%s, closefile=%s)",
        summary,
        openfile_label,
        write_label,
        closefile_label);
}

bool telemetry_runtime_function_exists(
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger,
    const char *name) {
    TelemetryFunctionExistsOptions options = {
        .category = TELEMETRY_LOG_TARGET_RUNTIME,
        .log_stack = false,
        .context = "runtime function_exists",
    };
    return telemetry_runtime_function_exists_internal(lua, logger, name, &options);
}

static bool telemetry_mark_attempt(
    bool *flag,
    bool *already_attempted,
    pthread_mutex_t *mutex) {
    bool should_attempt = false;
    bool already = false;
    pthread_mutex_lock(mutex);
    if (!*flag) {
        *flag = true;
        should_attempt = true;
    } else {
        already = true;
    }
    pthread_mutex_unlock(mutex);
    if (already_attempted) {
        *already_attempted = already;
    }
    return should_attempt;
}

void telemetry_attempt_string_library_open(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    telemetry_log_fn logger) {
    if (!state || !mutex) {
        return;
    }
    bool already_attempted = false;
    if (!telemetry_mark_attempt(&state->strlibopen_attempted, &already_attempted, mutex)) {
        if (already_attempted) {
            TELEMETRY_LOG_STRING(logger, "attempt_string_library_open skipped (already attempted)");
        }
        return;
    }
    if (!lua || !lua->strlibopen) {
        TELEMETRY_LOG_STRING(logger, "lua_strlibopen unavailable; cannot preload string library");
        return;
    }
    TELEMETRY_LOG_STRING(logger, "lua_strlibopen invoked by telemetry shim");
    lua->strlibopen();
}

void telemetry_attempt_io_library_open(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    const TelemetryRuntimeHooks *hooks) {
    if (!state || !mutex) {
        return;
    }
    bool already_attempted = false;
    if (!telemetry_mark_attempt(&state->iolibopen_attempted, &already_attempted, mutex)) {
        if (already_attempted) {
            TELEMETRY_LOG_IO(hooks ? hooks->log : NULL, "attempt_io_library_open skipped (already attempted)");
        }
        return;
    }
    telemetry_log_fn logger = hooks ? hooks->log : NULL;
    if (!lua || !lua->iolibopen) {
        TELEMETRY_LOG_IO(logger, "lua_iolibopen unavailable; cannot enable io library");
        return;
    }
    const TelemetryLegacyIoProbeConfig *probe_config = telemetry_get_legacy_io_probe_config();
    telemetry_log_probe_config_if_needed(logger, probe_config);
    TELEMETRY_LOG_IO(logger, "lua_iolibopen invoked by telemetry shim");
    telemetry_log_stack_event(
        TELEMETRY_LOG_TARGET_IO,
        logger,
        "iolibopen",
        "lua_iolibopen",
        "before",
        "lua_iolibopen",
        LUA_NOOBJECT);
    lua->iolibopen();
    telemetry_log_stack_event(
        TELEMETRY_LOG_TARGET_IO,
        logger,
        "iolibopen",
        "lua_iolibopen",
        "after",
        "lua_iolibopen",
        LUA_NOOBJECT);

    if (lua->dostring && lua->getglobal && lua->getstring) {
        int status = lua->dostring((char *)TELEMETRY_IO_STATUS_SCRIPT);
        TELEMETRY_LOG_IO(logger, "io readiness script completed with status %d", status);
        if (status != 0) {
            TELEMETRY_LOG_IO(logger, "io readiness script failed (%d)", status);
            if (hooks && hooks->log_bootstrap_error) {
                hooks->log_bootstrap_error();
            }
        } else {
            bool readiness_block = lua->beginblock && lua->endblock;
            if (readiness_block) {
                TELEMETRY_LOG_IO(logger, "io readiness using lua_beginblock/lua_endblock guard");
                lua->beginblock();
            }
            telemetry_log_stack_event(
                TELEMETRY_LOG_TARGET_IO,
                logger,
                "io readiness",
                "lua_getglobal",
                "before",
                "__telemetry_io_ready",
                LUA_NOOBJECT);
            lua_Object state_obj = lua->getglobal("__telemetry_io_ready");
            telemetry_log_stack_event(
                TELEMETRY_LOG_TARGET_IO,
                logger,
                "io readiness",
                "lua_getglobal",
                "after",
                "__telemetry_io_ready",
                state_obj);
            const char *state_str = NULL;
            if (state_obj == 0) {
                TELEMETRY_LOG_IO(logger, "__telemetry_io_ready missing after readiness script");
            } else {
                state_str = lua->getstring(state_obj);
                if (!state_str) {
                    TELEMETRY_LOG_IO(
                        logger,
                        "__telemetry_io_ready present but lua_getstring returned NULL (object=%p)",
                        (void *)state_obj);
                }
            }
            if (state_str && state_str[0] != '\0') {
                TELEMETRY_LOG_IO(logger, "io library readiness: %s", state_str);
            } else {
                TELEMETRY_LOG_IO(logger, "io library readiness unknown");
            }
            if (readiness_block) {
                telemetry_log_stack_event(
                    TELEMETRY_LOG_TARGET_IO,
                    logger,
                    "io readiness",
                    "lua_endblock",
                    "before",
                    "__telemetry_io_ready",
                    LUA_NOOBJECT);
                lua->endblock();
                telemetry_log_stack_event(
                    TELEMETRY_LOG_TARGET_IO,
                    logger,
                    "io readiness",
                    "lua_endblock",
                    "after",
                    "__telemetry_io_ready",
                    LUA_NOOBJECT);
            } else if (state_obj != 0) {
                telemetry_log_stack_event(
                    TELEMETRY_LOG_TARGET_IO,
                    logger,
                    "io readiness",
                    "lua_pop",
                    "before",
                    "__telemetry_io_ready",
                    LUA_NOOBJECT);
                lua_Object popped = lua_pop();
                telemetry_log_stack_event(
                    TELEMETRY_LOG_TARGET_IO,
                    logger,
                    "io readiness",
                    "lua_pop",
                    "after",
                    "__telemetry_io_ready",
                    popped);
            }
        }
    } else {
        TELEMETRY_LOG_IO(
            logger,
            "io readiness script skipped (lua_dostring=%p, lua_getglobal=%p, lua_getstring=%p)",
            lua ? (void *)lua->dostring : NULL,
            lua ? (void *)lua->getglobal : NULL,
            lua ? (void *)lua->getstring : NULL);
    }

    telemetry_probe_legacy_io(lua, logger, probe_config, "legacy io probe", NULL);

    if (lua && lua->getglobal) {
        bool io_block = lua->beginblock && lua->endblock;
        if (io_block) {
            TELEMETRY_LOG_IO(logger, "io global fetch using lua_beginblock/lua_endblock guard");
            lua->beginblock();
        }
        telemetry_log_stack_event(
            TELEMETRY_LOG_TARGET_IO,
            logger,
            "io global fetch",
            "lua_getglobal",
            "before",
            "io",
            LUA_NOOBJECT);
        lua_Object io_object = lua->getglobal("io");
        telemetry_log_stack_event(
            TELEMETRY_LOG_TARGET_IO,
            logger,
            "io global fetch",
            "lua_getglobal",
            "after",
            "io",
            io_object);
        TELEMETRY_LOG_IO(logger, "post-iolibopen lua_getglobal('io') -> %p", (void *)io_object);
        if (io_block) {
            telemetry_log_stack_event(
                TELEMETRY_LOG_TARGET_IO,
                logger,
                "io global fetch",
                "lua_endblock",
                "before",
                "io",
                LUA_NOOBJECT);
            lua->endblock();
            telemetry_log_stack_event(
                TELEMETRY_LOG_TARGET_IO,
                logger,
                "io global fetch",
                "lua_endblock",
                "after",
                "io",
                LUA_NOOBJECT);
        } else if (io_object != 0) {
            telemetry_log_stack_event(
                TELEMETRY_LOG_TARGET_IO,
                logger,
                "io global fetch",
                "lua_pop",
                "before",
                "io",
                LUA_NOOBJECT);
            lua_Object popped = lua_pop();
            telemetry_log_stack_event(
                TELEMETRY_LOG_TARGET_IO,
                logger,
                "io global fetch",
                "lua_pop",
                "after",
                "io",
                popped);
        }
    } else {
        TELEMETRY_LOG_IO(logger, "post-iolibopen lua_getglobal unavailable (cannot fetch io)");
    }
}

void telemetry_attempt_string_library_patch(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    const TelemetryRuntimeHooks *hooks,
    telemetry_simple_fn register_native_file_helpers) {
    if (!state || !mutex) {
        return;
    }
    bool already_attempted = false;
    if (!telemetry_mark_attempt(
            &state->string_library_patch_attempted,
            &already_attempted,
            mutex)) {
        if (already_attempted) {
            TELEMETRY_LOG_STRING(hooks ? hooks->log : NULL, "attempt_string_library_patch skipped (already attempted)");
        }
        return;
    }

    telemetry_log_fn logger = hooks ? hooks->log : NULL;
    if (!lua || !lua->dostring) {
        TELEMETRY_LOG_STRING(logger, "lua_dostring unavailable; cannot patch string library aliases");
        return;
    }

    TELEMETRY_LOG_STRING(
        logger,
        "attempting string library patch (lua_dostring=%s, lua_getglobal=%s, lua_isfunction=%s)",
        (lua->dostring ? "ready" : "missing"),
        (lua->getglobal ? "ready" : "missing"),
        (lua->isfunction ? "ready" : "missing"));

    int result = lua->dostring((char *)TELEMETRY_STRING_PATCH_SCRIPT);
    TELEMETRY_LOG_STRING(logger, "string library patch script completed with status %d", result);
    if (result != 0) {
        TELEMETRY_LOG_STRING(logger, "string library patch script failed (%d)", result);
        if (hooks && hooks->log_bootstrap_error) {
            hooks->log_bootstrap_error();
        }
        return;
    }

    TELEMETRY_LOG_STRING(
        logger,
        "string library patch verifying primitives via function_exists (lua_getglobal=%p, lua_isfunction=%p)",
        lua ? (void *)lua->getglobal : NULL,
        lua ? (void *)lua->isfunction : NULL);

    bool sub_ready = telemetry_runtime_function_exists(lua, logger, "strsub");
    bool byte_ready = telemetry_runtime_function_exists(lua, logger, "strbyte");
    bool format_ready = telemetry_runtime_function_exists(lua, logger, "strformat");
    TELEMETRY_LOG_STRING(logger, "string library globals/table patched by telemetry shim");
    TELEMETRY_LOG_STRING(
        logger,
        "post-patch primitives (strsub=%s, strbyte=%s, strformat=%s)",
        sub_ready ? "ready" : "missing",
        byte_ready ? "ready" : "missing",
        format_ready ? "ready" : "missing");
    if (!sub_ready || !byte_ready || !format_ready) {
        TELEMETRY_LOG_STRING(logger, "string library patch missing at least one primitive immediately after script execution");
    }
    TELEMETRY_LOG_STRING(logger, "string library patch succeeded; invoking register_native_file_helpers");
    if (register_native_file_helpers) {
        register_native_file_helpers();
    }
    int wrapper_status = -1;
    if (lua->dostring) {
        wrapper_status = lua->dostring((char *)TELEMETRY_LEGACY_IO_WRAPPERS_SCRIPT);
        TELEMETRY_LOG_IO(logger, "legacy io wrapper script completed with status %d", wrapper_status);
        if (wrapper_status == 0) {
            telemetry_probe_legacy_io(
                lua,
                logger,
                NULL,
                "legacy io probe (post-wrapper)",
                "post-wrapper legacy io functions");
        } else {
            TELEMETRY_LOG_IO(logger, "post-wrapper legacy io probe skipped (wrapper status %d)", wrapper_status);
        }
    } else {
        TELEMETRY_LOG_IO(logger, "legacy io wrapper script skipped (lua_dostring unavailable)");
    }
}

void telemetry_ensure_string_primitives(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    const TelemetryRuntimeHooks *hooks,
    telemetry_simple_fn register_native_file_helpers) {
    telemetry_log_fn logger = hooks ? hooks->log : NULL;
    TELEMETRY_LOG_RUNTIME(
        logger,
        "ensure_string_primitives invoked (strlibattempted=%d, iolibattempted=%d, stringpatchattempted=%d)",
        state ? (state->strlibopen_attempted ? 1 : 0) : -1,
        state ? (state->iolibopen_attempted ? 1 : 0) : -1,
        state ? (state->string_library_patch_attempted ? 1 : 0) : -1);
    telemetry_attempt_string_library_open(state, mutex, lua, logger);
    telemetry_attempt_io_library_open(state, mutex, lua, hooks);
    telemetry_attempt_string_library_patch(state, mutex, lua, hooks, register_native_file_helpers);
    telemetry_log_readiness_summary(state, mutex, lua, logger);
}

bool telemetry_runtime_ready(
    TelemetryRuntimeState *state,
    pthread_mutex_t *mutex,
    const TelemetryLuaApi *lua,
    const TelemetryRuntimeHooks *hooks,
    telemetry_simple_fn register_native_file_helpers) {
    static const char *const required_globals[] = {"strsub", "strbyte", "strformat"};
    telemetry_log_fn logger = hooks ? hooks->log : NULL;

    telemetry_ensure_string_primitives(state, mutex, lua, hooks, register_native_file_helpers);

    bool globals_ready = true;
    bool global_status[sizeof(required_globals) / sizeof(required_globals[0])];
    for (size_t i = 0; i < sizeof(required_globals) / sizeof(required_globals[0]); ++i) {
        global_status[i] = telemetry_runtime_function_exists(lua, logger, required_globals[i]);
        if (!global_status[i]) {
            globals_ready = false;
        }
    }
    if (globals_ready) {
        return true;
    }

    bool should_log = false;
    if (state && mutex) {
        pthread_mutex_lock(mutex);
        if (!state->missing_globals_logged) {
            state->missing_globals_logged = true;
            should_log = true;
        }
        pthread_mutex_unlock(mutex);
    } else {
        should_log = true;
    }

    if (should_log) {
        TELEMETRY_LOG_RUNTIME(
            logger,
            "telemetry runtime waiting on global functions (strsub=%s, strbyte=%s, strformat=%s)",
            global_status[0] ? "ready" : "missing",
            global_status[1] ? "ready" : "missing",
            global_status[2] ? "ready" : "missing");
    }
    return false;
}
