local message = "[telemetry.lua] loaded"

if type(print) == "function" then
    print(message)
elseif type(io) == "table" and type(io.stderr) == "userdata" and type(io.stderr.write) == "function" then
    io.stderr:write(message, "\n")
end
