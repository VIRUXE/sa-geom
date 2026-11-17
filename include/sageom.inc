/*
    sa-geom: GTA San Andreas collision queries for SA-MP and open.mp.
    https://github.com/VIRUXE/sa-geom

    The plugin carries the whole vanilla San Andreas collision world; no game files or data files are needed.
    It loads in the background when the server starts (about a second); every native returns false until then,
    SAGeom_IsReady() says when it is done.

    Interiors: `interior` 0 is the outside world; objects in area 13 are seen from every interior; -1 sees all.
*/

#if defined _sageom_included
    #endinput
#endif
#define _sageom_included

/// true once the collision world has finished loading.
native bool:SAGeom_IsReady();

/// The first surface straight down from (x, y, zFrom), like MapAndreas_FindZ_For2DCoord but under bridges and
/// roofs too when zFrom starts below them, and with the surface normal, material and model.
native bool:SAGeom_GetGround(Float:x, Float:y, &Float:z, Float:zFrom = 1000.0, interior = 0,
    &Float:nx = 0.0, &Float:ny = 0.0, &Float:nz = 0.0, &material = 0, &model = 0);

/// The nearest surface on the segment (x1, y1, z1) -> (x2, y2, z2).
native bool:SAGeom_RayCast(Float:x1, Float:y1, Float:z1, Float:x2, Float:y2, Float:z2,
    &Float:hx, &Float:hy, &Float:hz, interior = 0, &Float:nx = 0.0, &Float:ny = 0.0, &Float:nz = 0.0,
    &model = 0, &material = 0);

/// Lay an object on the ground under (x, y): z plus the CreateObject rotation that tilts it onto the slope while
/// it keeps facing `yaw` (degrees, GTA heading). `lift` raises it along the surface normal. A width/depth
/// footprint (metres) takes the highest point under its corners and the plane through them, so wide objects
/// do not sink into uneven ground.
native bool:SAGeom_Place(Float:x, Float:y, Float:zFrom, Float:yaw, &Float:z, &Float:rx, &Float:ry, &Float:rz,
    Float:lift = 0.0, interior = 0, Float:width = 0.0, Float:depth = 0.0);

/// Lay model `modelid` on the ground under (x, y): its collision bounds give the footprint it rests on and how
/// far its origin sits above its base, so the object's bottom (not its centre) meets the ground. `z` is what to
/// pass to CreateObject. Models without collision fall back to a single point with no base offset.
native bool:SAGeom_PlaceModel(modelid, Float:x, Float:y, Float:zFrom, Float:yaw, &Float:z, &Float:rx, &Float:ry,
    &Float:rz, Float:lift = 0.0, interior = 0);

/// Collision bounds of a model in its own space (models with collision only; SA-MP objects after an overlay).
native bool:SAGeom_GetModelBounds(modelid, &Float:minX, &Float:minY, &Float:minZ, &Float:maxX, &Float:maxY, &Float:maxZ);

/// Water surface height at (x, y), when water covers it.
native bool:SAGeom_WaterLevel(Float:x, Float:y, &Float:z);

/// Add a map to the collision world: a .pwn/.txt with CreateObject/CreateDynamicObject/RemoveBuildingForPlayer
/// lines, an MTA .map, an .ipl, a .col, an .img or a .toml overlay manifest. Path relative to scriptfiles/.
native bool:SAGeom_AddOverlay(const path[]);
