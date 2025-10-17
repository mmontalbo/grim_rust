-- Retail telemetry bridge rewritten to run inside the game's Lua 3.1 runtime.
-- This version keeps the API surface tiny (mark / event / flush / reset) and
-- only relies on core language primitives that shipped with Lua 3.1.

telemetry_error_log = "mods/telemetry_bootstrap_error.log"
telemetry_events_log = "mods/telemetry_events.jsonl"
telemetry_coverage_log = "mods/telemetry_coverage.json"

telemetry_log_path = "mods/telemetry.log"
telemetry_flush_interval = 32

__telemetry_bootstrap_error = "telemetry initialising"
____telemetry_stub_reason = nil

telemetry = {}

coverage_counts = {}
coverage_mark_counter = 0
events_sequence = 0
telemetry_dirty = 0

-- ---------------------------------------------------------------------------
-- Compatibility helpers (string primitives)
-- ---------------------------------------------------------------------------

local string_table = string

if type(strsub) ~= "function" and type(string_table) == "table" and type(string_table.sub) == "function" then
    strsub = string_table.sub
end
if type(strbyte) ~= "function" and type(string_table) == "table" and type(string_table.byte) == "function" then
    strbyte = string_table.byte
end
if type(strlen) ~= "function" and type(string_table) == "table" and type(string_table.len) == "function" then
    strlen = string_table.len
end
if type(strformat) ~= "function" and type(string_table) == "table" and type(string_table.format) == "function" then
    strformat = string_table.format
end

if type(strsub) ~= "function" or type(strbyte) ~= "function" or type(strformat) ~= "function" then
    __telemetry_stub_reason = "telemetry disabled: string library unavailable"
    telemetry = {
        mark = function() end,
        event = function() end,
        flush = function() end,
        flush_all = function() end,
        reset = function() end,
        _reason = __telemetry_stub_reason,
    }
    __telemetry_bootstrap_error = __telemetry_stub_reason
    return telemetry
end

__telemetry_builtin_strlen = strlen
telemetry_strlen = function(text)
    if type(__telemetry_builtin_strlen) == "function" then
        return __telemetry_builtin_strlen(text)
    end
    if type(strsub) ~= "function" then
        return 0
    end
    if type(text) ~= "string" then
        return 0
    end
    local length = 0
    while 1 do
        local ch = strsub(text, length + 1, length + 1)
        if ch == nil or ch == "" then
            return length
        end
        length = length + 1
    end
end

telemetry_mod = function(a, b)
    if type(b) ~= "number" or b == 0 then
        return 0
    end
    if type(math) == "table" then
        local mod_func = math.mod
        if type(mod_func) ~= "function" then
            mod_func = math.fmod
        end
        if type(mod_func) == "function" then
            return mod_func(a, b)
        end
        if type(math.floor) == "function" then
            return a - math.floor(a / b) * b
        end
    end
    return 0
end

-- ---------------------------------------------------------------------------
-- File helpers (support both io library and legacy openfile/write)
-- ---------------------------------------------------------------------------

function telemetry_write_file(path, contents, mode)
    mode = mode or "w"
    if type(telemetry_native_write) == "function" then
        local ok = telemetry_native_write(path, contents, mode)
        if ok then
            return 1
        end
    end
    if type(io) == "table" and type(io.open) == "function" then
        local file = io.open(path, mode)
        if file then
            file:write(contents)
            file:close()
            return 1
        end
    end
    if type(openfile) == "function" and type(write) == "function" and type(closefile) == "function" then
        local file = openfile(path, mode)
        if file then
            write(file, contents)
            closefile(file)
            return 1
        end
    end
    return nil
end

function telemetry_append_line(path, line)
    if not telemetry_write_file(path, line .. "\n", "a") then
        if type(io) == "table" and type(io.stderr) == "userdata" then
            io.stderr:write("[telemetry] append failed ", path, "\n")
        elseif type(print) == "function" then
            print("[telemetry] append failed " .. path)
        end
    end
end

-- ---------------------------------------------------------------------------
-- Minimal JSON encoding (flat objects only)
-- ---------------------------------------------------------------------------

function telemetry_encode_string(value)
    if type(value) ~= "string" then
        return "\"\""
    end
    local out = "\""
    local i = 1
    local length = telemetry_strlen(value)
    while i <= length do
        local ch = strsub(value, i, i)
        local byte = strbyte(ch)
        if ch == "\\" then
            out = out .. "\\\\"
        elseif ch == "\"" then
            out = out .. "\\\""
        elseif byte == 8 then
            out = out .. "\\b"
        elseif byte == 12 then
            out = out .. "\\f"
        elseif byte == 10 then
            out = out .. "\\n"
        elseif byte == 13 then
            out = out .. "\\r"
        elseif byte == 9 then
            out = out .. "\\t"
        elseif byte < 32 then
            out = out .. strformat("\\u%04x", byte)
        else
            out = out .. ch
        end
        i = i + 1
    end
    out = out .. "\""
    return out
end

function telemetry_encode_number(value)
    if type(value) == "number" then
        return strformat("%g", value)
    end
    return "0"
end

function telemetry_encode_value(value)
    local t = type(value)
    if t == "string" then
        return telemetry_encode_string(value)
    elseif t == "number" then
        return telemetry_encode_number(value)
    elseif t == "boolean" then
        if value then
            return "true"
        else
            return "false"
        end
    elseif value == nil then
        return "null"
    elseif t == "table" then
        return "{}"
    end
    return telemetry_encode_string(tostring(value))
end

function telemetry_encode_object(tbl)
    if type(tbl) ~= "table" then
        return "{}"
    end
    local out = "{"
    local first = 1
    local key, value = next(tbl, nil)
    while key do
        if type(key) == "string" then
            if first == 0 then
                out = out .. ","
            end
            out = out .. telemetry_encode_string(key) .. ":" .. telemetry_encode_value(value)
            first = 0
        end
        key, value = next(tbl, key)
    end
    out = out .. "}"
    return out
end

-- ---------------------------------------------------------------------------
-- Coverage tracking
-- ---------------------------------------------------------------------------

function telemetry_flush_coverage(force)
    if telemetry_dirty == 0 and force ~= 1 then
        return
    end
    local payload = telemetry_encode_object(coverage_counts)
    if telemetry_write_file(telemetry_coverage_log, payload, "w") then
        telemetry_dirty = 0
    end
end

function telemetry.mark(key)
    if type(key) ~= "string" or key == "" then
        return
    end
    local current = coverage_counts[key] or 0
    coverage_counts[key] = current + 1
    coverage_mark_counter = coverage_mark_counter + 1
    telemetry_dirty = 1
    if telemetry_flush_interval > 0 and telemetry_mod(coverage_mark_counter, telemetry_flush_interval) == 0 then
        telemetry_flush_coverage(0)
    end
end

function telemetry.flush()
    telemetry_flush_coverage(1)
end

-- ---------------------------------------------------------------------------
-- Event stream
-- ---------------------------------------------------------------------------

function telemetry_simple_fields(input)
    if type(input) ~= "table" then
        return {}
    end
    local result = {}
    local key, value = next(input, nil)
    while key do
        if type(key) == "string" then
            local t = type(value)
            if t == "string" or t == "number" or t == "boolean" then
                result[key] = value
            elseif value ~= nil then
                result[key] = tostring(value)
            end
        end
        key, value = next(input, key)
    end
    return result
end

function telemetry.event(label, fields)
    if type(label) ~= "string" or label == "" then
        return
    end
    events_sequence = events_sequence + 1
    local entry = {
        seq = events_sequence,
        label = label,
        timestamp = (type(os) == "table" and type(os.time) == "function") and os.time() or 0,
        data = telemetry_simple_fields(fields),
    }
    telemetry_append_line(telemetry_events_log, telemetry_encode_object(entry))
end

-- ---------------------------------------------------------------------------
-- Intro timeline instrumentation
-- ---------------------------------------------------------------------------

local function telemetry_intro_unpack(results)
    if type(unpack) == "function" then
        return unpack(results)
    end
    if type(table) == "table" and type(table.unpack) == "function" then
        return table.unpack(results)
    end
    return results[1]
end

local telemetry_intro_hooks = {
    installed = false,
    pending_movie = nil,
    scripts = {},
}

local function telemetry_intro_event(name, extra)
    if type(name) ~= "string" or name == "" then
        return
    end
    local payload = { event = name }
    if type(extra) == "table" then
        local key, value = next(extra, nil)
        while key do
            payload[key] = value
            key, value = next(extra, key)
        end
    end
    telemetry.event("intro.timeline", payload)
end

local function telemetry_intro_clear_scripts(label)
    local removals = {}
    local count = 0
    local key, value = next(telemetry_intro_hooks.scripts, nil)
    while key do
        if value == label then
            count = count + 1
            removals[count] = key
        end
        key, value = next(telemetry_intro_hooks.scripts, key)
    end
    local index = 1
    while index <= count do
        telemetry_intro_hooks.scripts[removals[index]] = nil
        index = index + 1
    end
end

local function telemetry_intro_install()
    if telemetry_intro_hooks.installed then
        return
    end
    if type(cut_scene) ~= "table" then
        return
    end
    if type(RunFullscreenMovie) ~= "function" or type(StartMovie) ~= "function" or type(wait_for_movie) ~= "function" then
        return
    end
    if type(start_script) ~= "function" or type(wait_for_script) ~= "function" then
        return
    end
    if type(Actor) ~= "table" or type(Actor.say_line) ~= "function" then
        return
    end
    if type(manny) ~= "table" then
        return
    end

    if type(cut_scene.logos) == "function" then
        local original_logos = cut_scene.logos
        cut_scene.logos = function(...)
            telemetry_intro_event("cut_scene.logos.begin")
            local results = { original_logos(...) }
            telemetry_intro_event("cut_scene.logos.end")
            return telemetry_intro_unpack(results)
        end
    end

    if type(cut_scene.intro) == "function" then
        local original_intro = cut_scene.intro
        cut_scene.intro = function(...)
            telemetry_intro_event("cut_scene.intro.begin")
            local results = { original_intro(...) }
            telemetry_intro_event("cut_scene.intro.end")
            return telemetry_intro_unpack(results)
        end
    end

    local original_run_fullscreen_movie = RunFullscreenMovie
    RunFullscreenMovie = function(name, ...)
        local label = nil
        if name == "logos.snm" then
            label = "movie.logos"
        elseif name == "intro.snm" then
            label = "movie.intro"
        end
        if label then
            telemetry_intro_event(label .. ".start", { movie = name })
        end
        local results = { original_run_fullscreen_movie(name, ...) }
        if label then
            telemetry_intro_event(label .. ".end", { movie = name })
        end
        return telemetry_intro_unpack(results)
    end

    local original_start_movie = StartMovie
    local original_wait_for_movie = wait_for_movie
    StartMovie = function(name, ...)
        if name == "mo_ts.snm" then
            telemetry_intro_event("movie.mo_ts.start", { movie = name })
            telemetry_intro_hooks.pending_movie = name
        end
        return original_start_movie(name, ...)
    end
    wait_for_movie = function(...)
        local results = { original_wait_for_movie(...) }
        if telemetry_intro_hooks.pending_movie ~= nil then
            telemetry_intro_event("movie.mo_ts.end", { movie = telemetry_intro_hooks.pending_movie })
            telemetry_intro_hooks.pending_movie = nil
        end
        return telemetry_intro_unpack(results)
    end

    local original_start_script = start_script
    local original_wait_for_script = wait_for_script
    start_script = function(fn, ...)
        local results = { original_start_script(fn, ...) }
        if manny and fn == manny.walk_and_face then
            telemetry_intro_event("script.manny.walk_and_face.start")
            telemetry_intro_hooks.scripts[fn] = "script.manny.walk_and_face"
            local handle = results[1]
            if handle ~= nil then
                telemetry_intro_hooks.scripts[handle] = "script.manny.walk_and_face"
            end
        end
        return telemetry_intro_unpack(results)
    end
    wait_for_script = function(target, ...)
        local label = telemetry_intro_hooks.scripts[target]
        local results = { original_wait_for_script(target, ...) }
        if label then
            telemetry_intro_event(label .. ".end")
            telemetry_intro_clear_scripts(label)
        end
        return telemetry_intro_unpack(results)
    end

    local original_say_line = Actor.say_line
    Actor.say_line = function(self, line, ...)
        if self == manny and line == "/intma39/" then
            telemetry_intro_event("dialog.manny.intma39", { line = line })
        end
        return original_say_line(self, line, ...)
    end

    telemetry_intro_hooks.installed = true
end

local telemetry_original_dofile = dofile
if type(telemetry_original_dofile) == "function" then
    dofile = function(path)
        local results = { telemetry_original_dofile(path) }
        if type(path) == "string" then
            if path == "_cut_scenes.lua" or path == "year_1.lua" then
                telemetry_intro_install()
            end
        end
        return telemetry_intro_unpack(results)
    end
end

telemetry_intro_install()

-- ---------------------------------------------------------------------------
-- Utilities for tests & dev harness
-- ---------------------------------------------------------------------------

function telemetry.flush_all()
    telemetry_flush_coverage(1)
end

function telemetry.reset()
    coverage_counts = {}
    coverage_mark_counter = 0
    events_sequence = 0
    telemetry_dirty = 0
    telemetry_write_file(telemetry_events_log, "", "w")
    telemetry_write_file(telemetry_coverage_log, "{}", "w")
end

-- ---------------------------------------------------------------------------
-- Error handler wiring
-- ---------------------------------------------------------------------------

previous_error_handler = _ERRORMESSAGE

function _ERRORMESSAGE(err)
    local message = tostring(err)
    __telemetry_bootstrap_error = message
    telemetry_append_line(telemetry_error_log, message)
    if type(previous_error_handler) == "function" then
        return previous_error_handler(err)
    end
    return err
end

telemetry_append_line(telemetry_log_path, "telemetry.lua (Lua 3.1 rewrite) loaded")

telemetry.reset()

local telemetry_native_state = "missing"
if type(telemetry_native_write) == "function" then
    telemetry_native_state = "enabled"
end

telemetry.event(
    "telemetry.runtime",
    { phase = "loaded", native = telemetry_native_state, version = "lua31_rewrite" }
)

telemetry_event = telemetry.event
telemetry_mark = telemetry.mark

__telemetry_bootstrap_error = nil

return telemetry
