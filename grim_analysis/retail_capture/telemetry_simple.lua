-- Minimal telemetry hook that must run inside the game's embedded Lua 3.x
-- runtime. Avoid using Lua 5+ conveniences so the retail interpreter can load it.
telemetry = telemetry or {}

telemetry_simple_native_mark_fn = telemetry_native_mark
telemetry_simple_previous_error_handler = _ERRORMESSAGE

function telemetry_simple_log_error(message)
    if type(message) ~= "string" then
        return
    end
    if type(telemetry_native_write) == "function" then
        telemetry_native_write("mods/telemetry_bootstrap_error.log", message .. "\n", "a")
        return
    end
    if type(write) == "function" then
        write("[telemetry_simple] " .. message .. "\n")
        return
    end
    if type(print) == "function" then
        print("[telemetry_simple] " .. message)
    end
end

function _ERRORMESSAGE(err)
    telemetry_simple_log_error(tostring(err))
    if type(telemetry_simple_previous_error_handler) == "function" then
        return telemetry_simple_previous_error_handler(err)
    end
    return err
end

function telemetry.mark(key)
    if type(key) ~= "string" or key == "" then
        return
    end
    if type(telemetry_simple_native_mark_fn) == "function" then
        telemetry_simple_native_mark_fn(key)
    end
end

function telemetry.event(label, fields)
end

function telemetry.flush()
end

function telemetry.reset()
end

return telemetry
