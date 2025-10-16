-- Retail telemetry bridge injected by the shim.  This script stays self-contained so
-- we can drop a single file into the game's mods directory and hook behaviour without
-- touching the shipping Lua sources.

local telemetry = {}

-- ---------------------------------------------------------------------------
-- Configuration & utilities
-- ---------------------------------------------------------------------------

local FILE_COVERAGE = "mods/telemetry_coverage.json"
local FILE_EVENTS = "mods/telemetry_events.jsonl"
local FILE_LOG = "mods/telemetry.log"
local COVERAGE_FLUSH_INTERVAL = 32

local unpack = table.unpack or unpack

local function safe_write(path, contents)
    local file, err = io.open(path, "w")
    if not file then
        if err and type(io.stderr) == "userdata" then
            io.stderr:write("[telemetry] failed to open ", path, ": ", tostring(err), "\n")
        end
        return false
    end
    file:write(contents)
    file:close()
    return true
end

local function log_message(message)
    local timestamp = os.date("%Y-%m-%d %H:%M:%S")
    local line = string.format("[%s] %s\n", timestamp, message)
    local file = io.open(FILE_LOG, "a")
    if file then
        file:write(line)
        file:close()
    elseif type(io.stderr) == "userdata" then
        io.stderr:write(line)
    end
end

local encode_object -- forward declaration

local function encode_value(value)
    local t = type(value)
    if t == "string" then
        return string.format("%q", value)
    elseif t == "number" then
        return tostring(value)
    elseif t == "boolean" then
        return value and "true" or "false"
    elseif t == "table" then
        return encode_object(value)
    end
    return "null"
end

encode_object = function(map)
    local parts = { "{" }
    local first = true
    for key, value in pairs(map) do
        if type(key) == "string" then
            if not first then
                table.insert(parts, ",")
            end
            table.insert(parts, string.format("%q:", key))
            table.insert(parts, encode_value(value))
            first = false
        end
    end
    table.insert(parts, "}")
    return table.concat(parts)
end

local function normalise_fields(input)
    if type(input) ~= "table" then
        return {}
    end
    local result = {}
    for key, value in pairs(input) do
        if type(key) == "string" then
            if type(value) == "table" then
                result[key] = normalise_fields(value)
            else
                result[key] = value
            end
        end
    end
    return result
end

-- ---------------------------------------------------------------------------
-- Coverage tracking
-- ---------------------------------------------------------------------------

local coverage = {
    counts = {},
    mark_counter = 0,
}

function coverage.mark(key)
    if type(key) ~= "string" or key == "" then
        log_message("ignored telemetry mark with invalid key")
        return
    end
    coverage.counts[key] = (coverage.counts[key] or 0) + 1
    coverage.mark_counter = coverage.mark_counter + 1
    if coverage.mark_counter % COVERAGE_FLUSH_INTERVAL == 0 then
        coverage.flush(true)
    end
end

function coverage.flush(force)
    if not force and coverage.mark_counter % COVERAGE_FLUSH_INTERVAL ~= 0 then
        return
    end
    local payload = encode_object(coverage.counts)
    if safe_write(FILE_COVERAGE, payload) then
        log_message("wrote coverage snapshot (" .. tostring(coverage.mark_counter) .. " marks)")
    end
end

-- ---------------------------------------------------------------------------
-- Event stream
-- ---------------------------------------------------------------------------

local events = {
    sequence = 0,
}

local function append_event_line(line)
    local file = io.open(FILE_EVENTS, "a")
    if file then
        file:write(line)
        file:write("\n")
        file:close()
        return
    end
    if type(io.stderr) == "userdata" then
        io.stderr:write("[telemetry] failed to append event\n")
    end
end

function events.emit(label, fields)
    if type(label) ~= "string" or label == "" then
        log_message("ignored telemetry event with invalid label")
        return
    end
    events.sequence = events.sequence + 1
    local event = {
        seq = events.sequence,
        label = label,
        timestamp = os.time(),
        data = normalise_fields(fields),
    }
    event.data._marks = nil
    append_event_line(encode_object(event))
end

-- ---------------------------------------------------------------------------
-- Hook installer registry
-- ---------------------------------------------------------------------------

local installers = {
    queue = {},
    running = false,
}

local function register_installer(installer)
    table.insert(installers.queue, installer)
end

local function run_installers()
    if installers.running or #installers.queue == 0 then
        return
    end
    installers.running = true
    local remaining = {}
    for _, installer in ipairs(installers.queue) do
        local ok, result = pcall(installer)
        if not (ok and result) then
            table.insert(remaining, installer)
        end
    end
    installers.queue = remaining
    installers.running = false
end

-- ---------------------------------------------------------------------------
-- Set instrumentation
-- ---------------------------------------------------------------------------

local instrumented_sets = setmetatable({}, { __mode = "k" })

local function derive_set_key(set_table)
    if type(set_table) ~= "table" then
        return nil
    end
    local cached = rawget(set_table, "_telemetry_key")
    if type(cached) == "string" then
        return cached
    end
    local set_file = rawget(set_table, "setFile")
    if type(set_file) == "string" then
        local base = string.match(set_file, "([^/\\]+)%.set$")
        return base or set_file
    end
    local name = rawget(set_table, "name")
    if type(name) == "string" then
        return name
    end
    return nil
end

local function mark_set_key(key, suffix)
    if not key then
        return
    end
    local full = "set:" .. key
    if suffix then
        full = full .. ":" .. suffix
    end
    coverage.mark(full)
end

local function emit_set_event(label, key, extra)
    if not key then
        return
    end
    local fields = normalise_fields(extra)
    fields.set = key
    events.emit(label, fields)
end

local function classify_set_method(name)
    local lower = string.lower(name)
    if lower == "enter" then
        return "hook:enter", "set.enter"
    elseif lower == "exit" then
        return "hook:exit", "set.exit"
    elseif lower == "camerachange" or lower == "camera_change" then
        return "hook:camera_change", "set.camera_change"
    elseif string.sub(lower, 1, 6) == "set_up" then
        return "hook:setup:" .. name, "set.setup"
    else
        return "hook:other:" .. name, "set.method"
    end
end

local function wrap_set_method(set_table, key)
    local fn = rawget(set_table, key)
    if type(fn) ~= "function" then
        return
    end
    set_table._telemetry_wrapped = set_table._telemetry_wrapped or {}
    if set_table._telemetry_wrapped[key] then
        return
    end
    set_table._telemetry_wrapped[key] = true

    local suffix, label = classify_set_method(key)
    set_table[key] = function(self, ...)
        local set_key = derive_set_key(self)
        mark_set_key(set_key, suffix)
        emit_set_event(label, set_key, { method = key })
        return fn(self, ...)
    end
end

local function attach_set_metatable(set_table)
    local mt = getmetatable(set_table)
    if mt and mt.__telemetry_wrapped then
        return
    end
    local original_newindex = mt and mt.__newindex or rawset
    mt = mt or {}
    mt.__telemetry_wrapped = true
    mt.__newindex = function(tbl, key, value)
        if original_newindex == rawset then
            rawset(tbl, key, value)
        else
            original_newindex(tbl, key, value)
        end
        if type(value) == "function" then
            wrap_set_method(tbl, key)
        end
    end
    setmetatable(set_table, mt)
end

local function instrument_set_table(set_table)
    if type(set_table) ~= "table" then
        return set_table
    end
    if instrumented_sets[set_table] then
        return set_table
    end
    instrumented_sets[set_table] = true

    local key = derive_set_key(set_table)
    if key then
        set_table._telemetry_key = key
        mark_set_key(key)
    end

    for name, value in pairs(set_table) do
        if type(value) == "function" then
            wrap_set_method(set_table, name)
        end
    end
    attach_set_metatable(set_table)
    return set_table
end

local function instrument_all_sets()
    if type(system) ~= "table" or type(system.setTable) ~= "table" then
        return
    end
    for _, set_table in pairs(system.setTable) do
        if type(set_table) == "table" then
            instrument_set_table(set_table)
        end
    end
end

local function install_set_hooks()
    if type(Set) ~= "table" then
        return false
    end
    if type(Set.create) == "function" and not Set._telemetry_create_wrapped then
        local original_create = Set.create
        Set.create = function(...)
            local set_table = original_create(...)
            return instrument_set_table(set_table)
        end
        Set._telemetry_create_wrapped = true
    end

    if type(Set.switch_to_set) == "function" and not Set._telemetry_switch_wrapped then
        local original_switch = Set.switch_to_set
        Set.switch_to_set = function(set_table, ...)
            local previous = nil
            if type(system) == "table" and type(system.currentSet) == "table" then
                previous = derive_set_key(system.currentSet)
            end
            local key = derive_set_key(set_table)
            emit_set_event("set.switch_to_set.begin", key, { previous = previous })
            local results = { original_switch(set_table, ...) }
            instrument_all_sets()
            local current = nil
            if type(system) == "table" and type(system.currentSet) == "table" then
                current = derive_set_key(system.currentSet)
            end
            emit_set_event("set.switch_to_set.end", key, { previous = previous, current = current })
            return unpack(results)
        end
        Set._telemetry_switch_wrapped = true
    end

    instrument_all_sets()
    return true
end

local function install_source_hook()
    if type(source_all_set_files) ~= "function" then
        return false
    end
    if source_all_set_files._telemetry_wrapped then
        return true
    end
    local original = source_all_set_files
    source_all_set_files = function(...)
        local results = { original(...) }
        instrument_all_sets()
        return unpack(results)
    end
    source_all_set_files._telemetry_wrapped = true
    return true
end

-- ---------------------------------------------------------------------------
-- Boot instrumentation
-- ---------------------------------------------------------------------------

local wrapped_globals = {}

local function wrap_global_function(name, label)
    local fn = rawget(_G, name)
    if type(fn) ~= "function" then
        return false
    end
    if wrapped_globals[name] then
        return true
    end
    local original = fn
    rawset(_G, name, function(...)
        events.emit(label .. ".begin", { function_name = name })
        local results = { original(...) }
        events.emit(label .. ".end", { function_name = name })
        return unpack(results)
    end)
    wrapped_globals[name] = true
    return true
end

local function install_boot_wrappers()
    local ok1 = wrap_global_function("BOOT", "boot")
    local ok2 = wrap_global_function("BOOTTWO", "boot_two")
    local ok3 = wrap_global_function("FINALIZEBOOT", "finalize_boot")
    return ok1 and ok2 and ok3
end

-- Register installers so they fire once the relevant globals exist.
register_installer(install_set_hooks)
register_installer(install_source_hook)
register_installer(install_boot_wrappers)

-- ---------------------------------------------------------------------------
-- Telemetry API (public surface)
-- ---------------------------------------------------------------------------

function telemetry.mark(key)
    run_installers()
    coverage.mark(key)
end

function telemetry.event(label, fields)
    run_installers()
    events.emit(label, fields)
end

function telemetry.flush()
    coverage.flush(true)
end

function telemetry.instrument_all_sets()
    instrument_all_sets()
end

telemetry.register_installer = register_installer
telemetry.run_installers = run_installers

-- ---------------------------------------------------------------------------
-- Bootstrap
-- ---------------------------------------------------------------------------

local function install_gc_flush()
    local proxy = newproxy and newproxy(true)
    if not proxy then
        log_message("warning: GC proxy unavailable; call telemetry.flush() before exit")
        return false
    end
    getmetatable(proxy).__gc = function()
        coverage.flush(true)
    end
    telemetry._gc_proxy = proxy
    return true
end

install_gc_flush()
run_installers()

log_message("telemetry.lua loaded")

_G.telemetry = telemetry

return telemetry
