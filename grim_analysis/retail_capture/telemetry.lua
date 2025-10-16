-- Retail telemetry bridge injected by the shim.  This script stays self-contained so
-- we can drop a single file into the game's mods directory and hook behaviour without
-- touching the shipping Lua sources.

local function telemetry_chunk()

local telemetry = {}

_G.__telemetry_bootstrap_error = "telemetry chunk entered"

-- ---------------------------------------------------------------------------
-- Configuration & utilities
-- ---------------------------------------------------------------------------

local FILE_COVERAGE = "mods/telemetry_coverage.json"
local FILE_EVENTS = "mods/telemetry_events.jsonl"
local FILE_LOG = "mods/telemetry.log"
local COVERAGE_FLUSH_INTERVAL = 1

local unpack = table.unpack or unpack

local function safe_pcall(fn, ...)
    if type(pcall) == "function" then
        return pcall(fn, ...)
    end
    return true, fn(...)
end

local function safe_open(path, mode)
    if type(io) == "table" then
        local open_fn = io.open
        if type(open_fn) == "function" then
            return open_fn(path, mode)
        end
    end
    return nil, "io library unavailable"
end

local function safe_write(path, contents)
    local file, err = safe_open(path, "w")
    if not file then
        if err and type(io) == "table" and type(io.stderr) == "userdata" then
            io.stderr:write("[telemetry] failed to open ", path, ": ", tostring(err), "\n")
        end
        return false
    end
    file:write(contents)
    file:close()
    return true
end

local telemetry_error_log = "mods/telemetry_bootstrap_error.log"
local previous_error_handler = rawget(_G, "_ERRORMESSAGE")

local function telemetry_error_message(err)
    local message = tostring(err)
    _G.__telemetry_bootstrap_error = message
    safe_write(telemetry_error_log, message .. "\n")
    if type(previous_error_handler) == "function" then
        return previous_error_handler(err)
    end
    return err
end

_G._ERRORMESSAGE = telemetry_error_message

local function current_timestamp()
    if type(os) == "table" then
        local date_fn = os.date
        if type(date_fn) == "function" then
            if type(pcall) == "function" then
                local ok, value = pcall(date_fn, "%Y-%m-%d %H:%M:%S")
                if ok and type(value) == "string" then
                    return value
                end
            else
                local value = date_fn("%Y-%m-%d %H:%M:%S")
                if type(value) == "string" then
                    return value
                end
            end
        end
    end
    return "0000-00-00 00:00:00"
end

local function log_message(message)
    local timestamp = current_timestamp()
    local line = string.format("[%s] %s\n", timestamp, message)
    local file = safe_open(FILE_LOG, "a")
    if file then
        file:write(line)
        file:close()
    elseif type(io) == "table" and type(io.stderr) == "userdata" then
        io.stderr:write(line)
    end
end

log_message("telemetry.lua init")

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
    local file = safe_open(FILE_EVENTS, "a")
    if file then
        file:write(line)
        file:write("\n")
        file:close()
        return
    end
    if type(io) == "table" and type(io.stderr) == "userdata" then
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
        timestamp = (type(os) == "table" and type(os.time) == "function") and os.time() or 0,
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
        local ok, result = safe_pcall(installer)
        if not (ok and result) then
            table.insert(remaining, installer)
        end
    end
    installers.queue = remaining
    installers.running = false
end

local original_math_random = type(math) == "table" and math.random or nil
if type(original_math_random) == "function" then
    math.random = function(...)
        run_installers()
        return original_math_random(...)
    end
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
-- Actor instrumentation
-- ---------------------------------------------------------------------------

local instrumented_actors = setmetatable({}, { __mode = "k" })
local actor_globals = setmetatable({}, { __mode = "k" })
local actor_global_hook_installed = false

local function resolve_actor_variable(actor)
    local variable = actor_globals[actor]
    if type(variable) == "string" and variable ~= "" then
        return variable
    end
    if type(actor) == "table" then
        for name, value in pairs(_G) do
            if value == actor and type(name) == "string" then
                actor_globals[actor] = name
                return name
            end
        end
    end
    return nil
end

local function emit_actor_event(actor, phase, extra)
    local variable = resolve_actor_variable(actor)
    if variable then
        coverage.mark("actor:" .. variable)
    end
    local label = nil
    if type(actor) == "table" then
        local name = rawget(actor, "name")
        if type(name) == "string" and name ~= "" then
            label = name
        end
    end
    local fields = normalise_fields(extra or {})
    fields.phase = phase
    if variable then
        fields.actor = variable
    end
    if label then
        fields.label = label
    end
    events.emit("actor." .. phase, fields)
end

local function instrument_actor(actor)
    if type(actor) ~= "table" or instrumented_actors[actor] then
        return actor
    end
    instrumented_actors[actor] = true
    emit_actor_event(actor, "instrument")
    return actor
end

local function record_actor_global(actor, global_name)
    if type(actor) ~= "table" or type(global_name) ~= "string" or global_name == "" then
        return
    end
    actor_globals[actor] = global_name
    emit_actor_event(actor, "bind")
end

local function instrument_existing_actors()
    if type(system) ~= "table" or type(system.actorTable) ~= "table" then
        return
    end
    for _, actor in pairs(system.actorTable) do
        if type(actor) == "table" then
            instrument_actor(actor)
        end
    end
end

local function attach_actor_global_hook()
    if actor_global_hook_installed then
        return
    end
    local mt = getmetatable(_G)
    if mt and mt.__telemetry_actor_hook then
        actor_global_hook_installed = true
        return
    end
    local original = mt and mt.__newindex
    local original_type = type(original)
    mt = mt or {}
    mt.__telemetry_actor_hook = true
    mt.__newindex = function(tbl, key, value)
        if original_type == "function" then
            original(tbl, key, value)
        elseif original_type == "table" then
            original[key] = value
        else
            rawset(tbl, key, value)
        end
        if type(key) == "string" and type(value) == "table" and instrumented_actors[value] and actor_globals[value] == nil then
            record_actor_global(value, key)
        end
    end
    setmetatable(_G, mt)
    actor_global_hook_installed = true
end

local function install_actor_hooks()
    if type(Actor) ~= "table" or type(Actor.create) ~= "function" then
        return false
    end
    if Actor._telemetry_create_wrapped then
        return true
    end

    attach_actor_global_hook()

    local original_create = Actor.create
    Actor.create = function(...)
        local actor = original_create(...)
        instrument_actor(actor)
        emit_actor_event(actor, "create")
        return actor
    end
    Actor._telemetry_create_wrapped = true

    instrument_existing_actors()
    return true
end

-- ---------------------------------------------------------------------------
-- Hotspot instrumentation
-- ---------------------------------------------------------------------------

local hotspot_handles = {}

local function hotspot_context()
    if type(system) == "table" then
        if type(system.currentSet) == "table" then
            return derive_set_key(system.currentSet)
        end
        if type(system.currentActor) == "table" and type(system.currentActor.parent) == "table" then
            return derive_set_key(system.currentActor.parent)
        end
    end
    return nil
end

local function build_hotspot_key(id)
    if type(id) ~= "string" or id == "" then
        return nil, nil
    end
    local set_name = hotspot_context()
    if set_name and set_name ~= "" then
        return "hotspot:" .. set_name .. ":" .. id, set_name
    end
    return "hotspot:" .. id, nil
end

local function install_hotspot_hooks()
    if type(AddHotspot) ~= "function" then
        return false
    end

    if not AddHotspot._telemetry_wrapped then
        local original_add = AddHotspot
        AddHotspot = function(id, ...)
            local results = { original_add(id, ...) }
            local handle = results[1]
            local key, set_name = build_hotspot_key(id)
            if key then
                coverage.mark(key)
            end
            local fields = normalise_fields({
                hotspot = id,
                key = key,
                set = set_name,
                handle = handle and tostring(handle) or nil,
            })
            events.emit("hotspot.add", fields)
            if handle ~= nil then
                hotspot_handles[handle] = { id = id, key = key, set = set_name }
            end
            return unpack(results)
        end
        AddHotspot._telemetry_wrapped = true
    end

    if type(RemoveHotspot) == "function" and not RemoveHotspot._telemetry_wrapped then
        local original_remove = RemoveHotspot
        RemoveHotspot = function(handle, ...)
            local info = hotspot_handles[handle]
            hotspot_handles[handle] = nil
            local fields = {
                handle = handle and tostring(handle) or nil,
            }
            if info then
                fields.hotspot = info.id
                fields.key = info.key
                fields.set = info.set
            end
            events.emit("hotspot.remove", normalise_fields(fields))
            local results = { original_remove(handle, ...) }
            return unpack(results)
        end
        RemoveHotspot._telemetry_wrapped = true
    end

    if type(LinkHotspot) == "function" and not LinkHotspot._telemetry_wrapped then
        local original_link = LinkHotspot
        LinkHotspot = function(handle, target, ...)
            local info = hotspot_handles[handle]
            local target_name = target
            if type(target) == "table" then
                target_name = rawget(target, "name") or rawget(target, "string_name") or target_name
            end
            local fields = normalise_fields({
                handle = handle and tostring(handle) or nil,
                hotspot = info and info.id or nil,
                key = info and info.key or nil,
                set = info and info.set or nil,
                target = target_name,
            })
            events.emit("hotspot.link", fields)
            local results = { original_link(handle, target, ...) }
            return unpack(results)
        end
        LinkHotspot._telemetry_wrapped = true
    end

    return true
end

-- ---------------------------------------------------------------------------
-- Scheduler instrumentation
-- ---------------------------------------------------------------------------

local scheduler_handles = {}

local function describe_script_callable(value)
    if scheduler_handles[value] then
        return scheduler_handles[value]
    end
    local value_type = type(value)
    if value_type == "function" then
        local info = debug and debug.getinfo(value, "nS")
        if info then
            if info.name and info.name ~= "" then
                return info.name
            end
            if info.short_src then
                local line = info.linedefined or 0
                return info.short_src .. ":" .. tostring(line)
            end
        end
        return tostring(value)
    elseif value_type == "table" then
        local name = rawget(value, "name")
        if type(name) == "string" and name ~= "" then
            return name
        end
        local script = rawget(value, "script")
        if type(script) == "function" then
            return describe_script_callable(script)
        end
    elseif value_type == "string" then
        return value
    elseif value ~= nil then
        return tostring(value)
    end
    return nil
end

local function install_scheduler_hooks()
    if type(start_script) ~= "function" then
        return false
    end

    if not start_script._telemetry_wrapped then
        local original_start = start_script
        start_script = function(task, ...)
            local results = { original_start(task, ...) }
            local handle = results[1]
            local name = describe_script_callable(task)
            if name then
                coverage.mark("scheduler:" .. name)
                local fields = normalise_fields({
                    script = name,
                    handle = handle and tostring(handle) or nil,
                })
                events.emit("scheduler.start", fields)
                if handle ~= nil then
                    scheduler_handles[handle] = name
                end
            end
            return unpack(results)
        end
        start_script._telemetry_wrapped = true
    end

    if type(stop_script) == "function" and not stop_script._telemetry_wrapped then
        local original_stop = stop_script
        stop_script = function(task, ...)
            local name = describe_script_callable(task) or scheduler_handles[task]
            local fields = normalise_fields({
                script = name,
                handle = task and tostring(task) or nil,
            })
            events.emit("scheduler.stop", fields)
            scheduler_handles[task] = nil
            local results = { original_stop(task, ...) }
            return unpack(results)
        end
        stop_script._telemetry_wrapped = true
    end

    if type(kill_script) == "function" and not kill_script._telemetry_wrapped then
        local original_kill = kill_script
        kill_script = function(task, ...)
            local name = describe_script_callable(task) or scheduler_handles[task]
            local fields = normalise_fields({
                script = name,
                handle = task and tostring(task) or nil,
            })
            events.emit("scheduler.kill", fields)
            scheduler_handles[task] = nil
            local results = { original_kill(task, ...) }
            return unpack(results)
        end
        kill_script._telemetry_wrapped = true
    end

    return true
end

-- ---------------------------------------------------------------------------
-- Inventory instrumentation
-- ---------------------------------------------------------------------------

local function derive_inventory_slug(item)
    if type(item) ~= "table" then
        return nil, nil
    end
    local slug = rawget(item, "string_name")
    if type(slug) ~= "string" or slug == "" then
        slug = rawget(item, "name")
    end
    local parent = rawget(item, "parent")
    local set_name = nil
    if type(parent) == "table" then
        set_name = derive_set_key(parent)
    end
    if (slug == nil or slug == "") and type(parent) == "table" then
        for key, value in pairs(parent) do
            if value == item and type(key) == "string" then
                slug = key
                break
            end
        end
    end
    if (slug == nil or slug == "") then
        for key, value in pairs(_G) do
            if value == item and type(key) == "string" then
                slug = key
                break
            end
        end
    end
    if type(slug) ~= "string" or slug == "" then
        slug = nil
    end
    return slug, set_name
end

local function inventory_coverage_key(item)
    local slug, set_name = derive_inventory_slug(item)
    if not slug then
        return nil, slug, set_name
    end
    if set_name and set_name ~= "" then
        return "inventory:" .. set_name .. ":" .. slug, slug, set_name
    end
    return "inventory:" .. slug, slug, set_name
end

local function install_inventory_hooks()
    if type(Inventory) ~= "table" then
        return false
    end

    local add_fn = Inventory.add_item_to_inventory
    if type(add_fn) ~= "function" then
        return false
    end

    if not add_fn._telemetry_wrapped then
        Inventory.add_item_to_inventory = function(self, item, ...)
            local results = { add_fn(self, item, ...) }
            local key, slug, set_name = inventory_coverage_key(item)
            local fields = normalise_fields({
                key = key,
                item = slug,
                set = set_name,
            })
            if key then
                coverage.mark(key)
            end
            events.emit("inventory.add", fields)
            return unpack(results)
        end
        Inventory.add_item_to_inventory._telemetry_wrapped = true
    end

    local remove_fn = Inventory.remove_item_from_inventory
    if type(remove_fn) == "function" and not remove_fn._telemetry_wrapped then
        Inventory.remove_item_from_inventory = function(self, item, ...)
            local key, slug, set_name = inventory_coverage_key(item)
            events.emit(
                "inventory.remove",
                normalise_fields({
                    key = key,
                    item = slug,
                    set = set_name,
                })
            )
            local results = { remove_fn(self, item, ...) }
            return unpack(results)
        end
        Inventory.remove_item_from_inventory._telemetry_wrapped = true
    end

    return true
end

-- ---------------------------------------------------------------------------
-- Audio instrumentation
-- ---------------------------------------------------------------------------

local audio_handles = {}

local function derive_sound_key(resource)
    if type(resource) == "string" and resource ~= "" then
        return resource
    end
    if resource ~= nil then
        local as_string = tostring(resource)
        if as_string ~= "" then
            return as_string
        end
    end
    return nil
end

local function install_audio_hooks()
    if type(start_sfx) ~= "function" then
        return false
    end

    if not start_sfx._telemetry_wrapped then
        local original = start_sfx
        start_sfx = function(resource, priority, volume, ...)
            local results = { original(resource, priority, volume, ...) }
            local handle = results[1]
            local key = derive_sound_key(resource)
            if key then
                coverage.mark("audio:" .. key)
            end
            events.emit(
                "audio.start_sfx",
                normalise_fields({
                    sound = key,
                    handle = handle and tostring(handle) or nil,
                    priority = priority,
                    volume = volume,
                })
            )
            if handle ~= nil then
                audio_handles[handle] = key
            end
            return unpack(results)
        end
        start_sfx._telemetry_wrapped = true
    end

    if type(single_start_sfx) == "function" and not single_start_sfx._telemetry_wrapped then
        local original_single = single_start_sfx
        single_start_sfx = function(resource, priority, volume, ...)
            local results = { original_single(resource, priority, volume, ...) }
            local handle = results[1]
            local key = derive_sound_key(resource)
            if key then
                coverage.mark("audio:" .. key)
            end
            events.emit(
                "audio.single_start_sfx",
                normalise_fields({
                    sound = key,
                    handle = handle and tostring(handle) or nil,
                    priority = priority,
                    volume = volume,
                })
            )
            if handle ~= nil then
                audio_handles[handle] = key
            end
            return unpack(results)
        end
        single_start_sfx._telemetry_wrapped = true
    end

    if type(start_loop_sfx) == "function" and not start_loop_sfx._telemetry_wrapped then
        local original_loop = start_loop_sfx
        start_loop_sfx = function(resource, priority, ...)
            local results = { original_loop(resource, priority, ...) }
            local handle = results[1]
            local key = derive_sound_key(resource)
            if key then
                coverage.mark("audio:" .. key)
            end
            events.emit(
                "audio.start_loop_sfx",
                normalise_fields({
                    sound = key,
                    handle = handle and tostring(handle) or nil,
                    priority = priority,
                })
            )
            if handle ~= nil then
                audio_handles[handle] = key
            end
            return unpack(results)
        end
        start_loop_sfx._telemetry_wrapped = true
    end

    if type(stop_sound) == "function" and not stop_sound._telemetry_wrapped then
        local original_stop_sound = stop_sound
        stop_sound = function(handle, ...)
            events.emit(
                "audio.stop_sound",
                normalise_fields({
                    sound = audio_handles[handle],
                    handle = handle and tostring(handle) or nil,
                })
            )
            audio_handles[handle] = nil
            local results = { original_stop_sound(handle, ...) }
            return unpack(results)
        end
        stop_sound._telemetry_wrapped = true
    end

    if type(fade_sfx) == "function" and not fade_sfx._telemetry_wrapped then
        local original_fade = fade_sfx
        fade_sfx = function(handle, duration, target_volume, ...)
            events.emit(
                "audio.fade_sfx",
                normalise_fields({
                    sound = audio_handles[handle],
                    handle = handle and tostring(handle) or nil,
                    duration = duration,
                    target = target_volume,
                })
            )
            local results = { original_fade(handle, duration, target_volume, ...) }
            return unpack(results)
        end
        fade_sfx._telemetry_wrapped = true
    end

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
register_installer(install_actor_hooks)
register_installer(install_hotspot_hooks)
register_installer(install_scheduler_hooks)
register_installer(install_inventory_hooks)
register_installer(install_audio_hooks)
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
end

local function telemetry_error_handler(err)
    local message = tostring(err)
    if type(debug) == "table" then
        local traceback = debug.traceback
        if type(traceback) == "function" then
            if type(pcall) == "function" then
                local ok, trace = safe_pcall(traceback, err, 2)
                if ok and type(trace) == "string" then
                    message = trace
                end
            else
                local trace = traceback(err, 2)
                if type(trace) == "string" then
                    message = trace
                end
            end
        end
    end

    if type(io) == "table" then
        local open_fn = io.open
        if type(open_fn) == "function" then
            local file = open_fn("mods/telemetry_bootstrap_error.log", "a")
            if file then
                file:write(message, "\n")
                file:close()
            end
        end
        if type(io.stderr) == "userdata" then
            io.stderr:write(message, "\n")
        end
    end
    if type(print) == "function" then
        print(message)
    end
    return err
end

local ok_bootstrap, telemetry_module
if type(xpcall) == "function" then
    ok_bootstrap, telemetry_module = xpcall(telemetry_chunk, telemetry_error_handler)
elseif type(pcall) == "function" then
    ok_bootstrap, telemetry_module = pcall(telemetry_chunk)
    if not ok_bootstrap then
        telemetry_error_handler(telemetry_module)
        telemetry_module = nil
    end
else
    ok_bootstrap = true
    telemetry_module = telemetry_chunk()
end

if ok_bootstrap then
    return telemetry_module
end

return nil
