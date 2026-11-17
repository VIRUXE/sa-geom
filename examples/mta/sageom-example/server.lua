-- /crate [model]: drops a crate 3 m in front of you, resting on the ground and tilted to the slope.
-- Needs the ml_sageom module (see the sa-geom README). Everything runs on the server; no client is involved.

addCommandHandler("crate", function(player, _, model)
    model = tonumber(model) or 1271 -- gunbox
    if not sageomIsReady or not sageomIsReady() then
        outputChatBox("sa-geom is not loaded yet", player)
        return
    end
    local px, py, pz = getElementPosition(player)
    local _, _, heading = getElementRotation(player)
    local r = math.rad(heading)
    local x, y = px - math.sin(r) * 3, py + math.cos(r) * 3

    -- start the ray a little above the player so a roof overhead is not mistaken for the ground
    local ox, oy, z, rx, ry, rz = sageomPlaceModel(model, x, y, pz + 2, heading, 0, getElementInterior(player))
    if not ox then
        outputChatBox("no ground there", player)
        return
    end
    local obj = createObject(model, ox, oy, z, rx, ry, rz)
    setElementInterior(obj, getElementInterior(player))
    setElementDimension(obj, getElementDimension(player))
    outputChatBox(string.format("model %d at %.2f %.2f %.2f, rotation %.1f %.1f %.1f", model, ox, oy, z, rx, ry, rz), player)
end)

-- /groundhere: the ground under you, its slope and its surface material
addCommandHandler("groundhere", function(player)
    local x, y, z = getElementPosition(player)
    local gz, nx, ny, nz, material, hitModel = sageomGround(x, y, z + 1)
    if not gz then
        outputChatBox("no ground under you", player)
        return
    end
    local slope = math.deg(math.acos(nz))
    outputChatBox(string.format("ground z %.3f, slope %.1f degrees, material %d, model %d", gz, slope, material, hitModel), player)
end)
