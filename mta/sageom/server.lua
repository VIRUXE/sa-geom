-- sageom: keeps the ml_sageom server collision world in step with the server's objects and buildings.
--
-- Every object and building in dimension 0 is sent to the module once it is ready (one sageomAddObjects batch), then
-- kept in sync through debug hooks on the functions that create, move or change them, onElementDestroy, and a scan
-- of each resource that starts (map files create their elements, and their removeWorldObject entries, without any Lua
-- call). removeWorldModel and restoreWorldModel calls are mirrored. Collisions that only clients load
-- (engineReplaceCOL) reach the module through the replaceCOL export.
--
-- On the server, getElementModel and getElementRotation return false for buildings (createBuilding), so a building's
-- model and rotation come from the createBuilding call this resource saw, or from the setBuildingTransform export for
-- buildings made before it started. Buildings outlive the resource that made them (they move to the root element).
--
-- Everything this resource sends to the module is owned by it: when it stops, the module drops it all.

local TYPES = { object = true, building = true }
local HOOKED = {
    "createObject",
    "createBuilding",
    "cloneElement",
    "loadMapData",
    "setElementPosition",
    "setElementRotation",
    "setElementModel",
    "setElementDimension",
    "setElementInterior",
    "setElementCollisionsEnabled",
    "setObjectScale",
    "moveObject",
    "stopObject",
    "attachElements",
    "detachElements",
    "setElementAttachedOffsets",
    "removeWorldModel",
    "restoreWorldModel",
}

local state = {} -- element -> { h = handle | false (its model has no collision), r = row sent }
local building = {} -- building element -> { model, rx, ry, rz } (the server cannot tell either)
local colModel = {} -- element -> model id whose collision it uses instead of its own (setElementCollisionModel)
local cols = {} -- model id -> { path, data, by = resource }
local mapRemovals = {} -- removeWorldObject element -> its removal args, mirrored while it exists
local moving = {} -- element -> timer that syncs it where moveObject leaves it
local queued = {} -- world model removals and restores made before the module was ready
local ready = false

local function log(fmt, ...)
    outputDebugString("[sageom] " .. string.format(fmt, ...))
end

local function available()
    return type(sageomAddObject) == "function"
end

local function eligible(el)
    if not isElement(el) then
        return false
    end
    local t = getElementType(el)
    if not TYPES[t] or (getElementDimension(el) or 0) ~= 0 then
        return false
    end
    if t == "building" then
        return building[el] ~= nil or colModel[el] ~= nil
    end
    if getElementCollisionsEnabled and not getElementCollisionsEnabled(el) then
        return false
    end
    -- carried by a vehicle or a ped: it moves without telling anyone, so it has no server collision
    local to = getElementAttachedTo(el)
    if to and not TYPES[getElementType(to)] then
        return false
    end
    return true
end

local function row(el)
    local x, y, z = getElementPosition(el)
    local b = building[el]
    if b then
        return { colModel[el] or b[1], x, y, z, b[2], b[3], b[4], getElementInterior(el) or 0, 1, 1, 1 }
    end
    local rx, ry, rz = getElementRotation(el)
    local sx, sy, sz = 1, 1, 1
    if getElementType(el) == "object" then
        sx, sy, sz = getObjectScale(el)
        sx = sx or 1
        sy, sz = sy or sx, sz or sx
    end
    return { colModel[el] or getElementModel(el), x, y, z, rx or 0, ry or 0, rz or 0, getElementInterior(el) or 0, sx, sy, sz }
end

local function drop(el)
    local s = state[el]
    if s then
        if s.h and ready then
            sageomRemoveObject(s.h)
        end
        state[el] = nil
    end
end

-- Bring one element's collision in line with what it is now: added, changed, or dropped.
local function update(el)
    if not ready then
        return
    end
    local s = state[el]
    if not eligible(el) then
        drop(el)
        return
    end
    local r = row(el)
    if s and s.h and s.r[8] == r[8] then
        local o = s.r
        if o[2] ~= r[2] or o[3] ~= r[3] or o[4] ~= r[4] or o[5] ~= r[5] or o[6] ~= r[6] or o[7] ~= r[7] then
            sageomMoveObject(s.h, r[2], r[3], r[4], r[5], r[6], r[7])
        end
        if o[9] ~= r[9] or o[10] ~= r[10] or o[11] ~= r[11] then
            sageomSetObjectScale(s.h, r[9], r[10], r[11])
        end
        if o[1] ~= r[1] then
            sageomSetObjectModel(s.h, r[1]) -- false: no collision for it; the handle stays
        end
        s.r = r
    else
        if s and s.h then
            sageomRemoveObject(s.h)
        end
        state[el] = { h = sageomAddObject(unpack(r)), r = r }
    end
end

local function updateWithAttached(el, depth)
    update(el)
    if depth < 4 then
        for _, a in ipairs(getAttachedElements(el) or {}) do
            if TYPES[getElementType(a)] then
                updateWithAttached(a, depth + 1)
            end
        end
    end
end

local function worldModel(fn, model, radius, x, y, z, interior)
    if not ready then
        queued[#queued + 1] = { fn, model, radius, x, y, z, interior }
        return
    end
    if fn == "removeWorldModel" then
        sageomRemoveWorldModel(model, radius, x, y, z, interior or -1)
    else
        sageomRestoreWorldModel(model, radius, x, y, z, interior or -1)
    end
end

-- A map file's <removeWorldObject> (and its lodModel), mirrored for as long as its element exists.
local function mapRemoval(el)
    if mapRemovals[el] then
        return
    end
    local d = function(k)
        return tonumber(getElementData(el, k))
    end
    local model, radius, x, y, z = d("model"), d("radius"), d("posX"), d("posY"), d("posZ")
    if not (model and radius and x and y and z) then
        return
    end
    local r = { model, radius, x, y, z, d("interior") or -1, d("lodModel") }
    mapRemovals[el] = r
    worldModel("removeWorldModel", r[1], r[2], r[3], r[4], r[5], r[6])
    if r[7] and r[7] > 0 then
        worldModel("removeWorldModel", r[7], r[2], r[3], r[4], r[5], r[6])
    end
end

-- Send every object and building under `parent` not yet known, in one batch, and mirror its map removals.
local function scan(parent)
    if not ready then
        return 0
    end
    for _, el in ipairs(getElementsByType("removeWorldObject", parent)) do
        mapRemoval(el)
    end
    local rows, els = {}, {}
    for t in pairs(TYPES) do
        for _, el in ipairs(getElementsByType(t, parent)) do
            if state[el] == nil and eligible(el) then
                els[#els + 1] = el
                rows[#rows + 1] = row(el)
            end
        end
    end
    if #rows == 0 then
        return 0
    end
    local handles = sageomAddObjects(rows)
    if not handles then
        return 0
    end
    for i, el in ipairs(els) do
        state[el] = { h = handles[i], r = rows[i] }
    end
    return #rows
end

local function applyCol(id)
    local c = cols[id]
    local ok, err = sageomReplaceCOL(c.data, id)
    if not ok then
        log("replaceCOL %s for model %d failed: %s", c.path, id, tostring(err))
        cols[id] = nil
        return false
    end
    -- elements of that model sent before it had collision get it now
    for el, s in pairs(state) do
        if not s.h and s.r[1] == id then
            update(el)
        end
    end
    return true
end

local function stopMoving(el)
    if moving[el] then
        if isTimer(moving[el]) then
            killTimer(moving[el])
        end
        moving[el] = nil
    end
end

-- postFunction hooks get the call's arguments followed by its return value. Kept short: setElementPosition and
-- friends run for every element type.
local function onPostFunction(_, fn, _, _, _, ...)
    local n = select("#", ...)
    local ret = select(n, ...)
    if not ret then
        return
    end
    if fn == "createObject" or fn == "cloneElement" then
        update(ret)
    elseif fn == "createBuilding" then
        -- createBuilding(model, x, y, z [, rx, ry, rz, interior]): the only time the server tells a building's model
        local model, _, _, _, rx, ry, rz = ...
        if n >= 8 then -- the arguments then the return value: rotation given
            building[ret] = { model, tonumber(rx) or 0, tonumber(ry) or 0, tonumber(rz) or 0 }
        else
            building[ret] = { model, 0, 0, 0 }
        end
        update(ret)
    elseif fn == "loadMapData" then
        scan(ret)
    elseif fn == "removeWorldModel" or fn == "restoreWorldModel" then
        local model, radius, x, y, z = ...
        worldModel(fn, model, radius, x, y, z, n >= 7 and select(6, ...) or -1)
    else
        local el = ...
        if state[el] == nil and not (isElement(el) and TYPES[getElementType(el)]) then
            return -- a player, vehicle, ped...
        end
        if fn == "setElementModel" and building[el] then
            building[el][1] = select(2, ...)
        elseif fn == "setElementRotation" and building[el] then
            local _, rx, ry, rz = ...
            building[el][2], building[el][3], building[el][4] = rx, ry, rz
        end
        if fn == "moveObject" then
            stopMoving(el)
            local ms = tonumber((select(2, ...))) or 0
            moving[el] = setTimer(function()
                moving[el] = nil
                updateWithAttached(el, 0)
            end, math.max(50, ms + 50), 1)
        elseif fn == "stopObject" then
            stopMoving(el)
            updateWithAttached(el, 0)
        elseif fn == "setElementPosition" or fn == "setElementRotation" then
            updateWithAttached(el, 0)
        else
            update(el)
        end
    end
end

-- Children of a destroyed element get the event too. A table lookup is all it costs for anything else.
addEventHandler("onElementDestroy", root, function()
    local el = source
    if state[el] then
        drop(el)
    end
    if building[el] then
        building[el] = nil
    end
    if colModel[el] then
        colModel[el] = nil
    end
    if moving[el] then
        stopMoving(el)
    end
    local r = mapRemovals[el]
    if r then
        mapRemovals[el] = nil
        worldModel("restoreWorldModel", r[1], r[2], r[3], r[4], r[5], r[6])
        if r[7] and r[7] > 0 then
            worldModel("restoreWorldModel", r[7], r[2], r[3], r[4], r[5], r[6])
        end
    end
end)

addEventHandler("onResourceStart", root, function(res)
    if res ~= resource and ready then
        local t = getTickCount()
        local n = scan(getResourceRootElement(res))
        if n > 0 then
            log("%s: %d objects added (%d ms)", getResourceName(res), n, getTickCount() - t)
        end
    end
end)

-- A stopped resource's collisions go back to normal. Its elements' collision models stay: buildings outlive their
-- resource (destroyed elements are forgotten through onElementDestroy).
addEventHandler("onResourceStop", root, function(res)
    if res == resource then
        return
    end
    for id, c in pairs(cols) do
        if c.by == res then
            cols[id] = nil
            if ready then
                sageomRestoreCOL(id)
            end
        end
    end
end)

local function begin()
    ready = true
    local t = getTickCount()
    for id in pairs(cols) do
        applyCol(id)
    end
    for _, q in ipairs(queued) do
        worldModel(unpack(q, 1, 7))
    end
    queued = {}
    local n = scan(root)
    local st = sageomGetStats() or {}
    log(
        "%d objects and buildings sent in %d ms (%s with collision, %s without), %s world model removals, %s collisions replaced",
        n,
        getTickCount() - t,
        tostring((st.objects or 0) - (st.objectsWithoutCollision or 0)),
        tostring(st.objectsWithoutCollision),
        tostring(st.removals),
        tostring(st.cols)
    )
end

addEventHandler("onResourceStart", resourceRoot, function()
    if not available() then
        log("ml_sageom is not loaded (or too old for live objects): nothing to do")
        return
    end
    if not addDebugHook("postFunction", onPostFunction, HOOKED) then
        -- still worth running: what exists now and what resources bring when they start is sent, and destroyed
        -- elements are dropped; only later script changes (moves, models, new objects...) go unseen
        log(
            "WARNING: addDebugHook was refused, so only starting resources and destroyed elements are followed: "
                .. "give this resource the right function.addDebugHook (see the README)"
        )
    end
    if sageomIsReady() then
        begin()
    else
        log("waiting for the module's world to load")
        local waiting
        waiting = setTimer(function()
            if sageomIsReady() then
                killTimer(waiting)
                begin()
            end
        end, 500, 0)
    end
end)

addEventHandler("onResourceStop", resourceRoot, function()
    removeDebugHook("postFunction", onPostFunction)
    -- the module drops everything this resource sent it
end)

-- Exports ------------------------------------------------------------------------------------------------------------

-- Use the collision in the .col file at `path` (":resource/file.col", or a path inside the calling resource) for
-- model `id`, for every object and building of that model. Kept until restoreCOL or until the calling resource stops.
-- Reading another resource's file needs the right general.ModifyOtherObjects.
function replaceCOL(path, id)
    id = tonumber(id)
    if type(path) ~= "string" or not id then
        return false
    end
    if path:sub(1, 1) ~= ":" and sourceResource then
        path = ":" .. getResourceName(sourceResource) .. "/" .. path
    end
    local f = fileOpen(path, true)
    if not f then
        log("replaceCOL: cannot open %s", path)
        return false
    end
    local data = fileRead(f, fileGetSize(f))
    fileClose(f)
    if not data or #data == 0 then
        return false
    end
    cols[id] = { path = path, data = data, by = sourceResource }
    if not ready then
        return true -- sent once the module is ready
    end
    return applyCol(id)
end

-- Back to the model's own collision (none, for a model only replaceCOL gave one).
function restoreCOL(id)
    id = tonumber(id)
    if not id or not cols[id] then
        return false
    end
    cols[id] = nil
    if ready then
        return sageomRestoreCOL(id)
    end
    return true
end

-- Make the module use model `id`'s collision for `element` instead of its own model's (an object created with a
-- stand-in model whose real custom model only clients load). `id` false or nil goes back to the element's model.
-- Kept across setElementModel and moves, until the element is destroyed.
function setElementCollisionModel(element, id)
    if not isElement(element) or not TYPES[getElementType(element)] then
        return false
    end
    colModel[element] = tonumber(id)
    update(element)
    return true
end

-- Tell a building's model and rotation, which the server cannot read back (getElementModel and getElementRotation
-- return false for buildings): for buildings created before this resource started. Later createBuilding calls are
-- seen by the debug hook.
function setBuildingTransform(element, model, rx, ry, rz)
    if not isElement(element) or getElementType(element) ~= "building" or not tonumber(model) then
        return false
    end
    building[element] = { tonumber(model), tonumber(rx) or 0, tonumber(ry) or 0, tonumber(rz) or 0 }
    update(element)
    return true
end

function getStats()
    local tracked, without, overrides, replaced, buildings = 0, 0, 0, 0, 0
    for _, s in pairs(state) do
        tracked = tracked + 1
        if not s.h then
            without = without + 1
        end
    end
    for _ in pairs(colModel) do
        overrides = overrides + 1
    end
    for _ in pairs(cols) do
        replaced = replaced + 1
    end
    for _ in pairs(building) do
        buildings = buildings + 1
    end
    local st = ready and sageomGetStats() or {}
    st.ready = ready
    st.tracked = tracked
    st.trackedWithoutCollision = without
    st.collisionModels = overrides
    st.replacedCols = replaced
    st.buildingsKnown = buildings
    return st
end
