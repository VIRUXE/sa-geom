// sa-geom example filterscript for SA-MP and open.mp.
// /crate [model]  drops a crate 3 m in front of you, resting on the ground and tilted to the slope.
// /groundhere     prints the ground under you and its slope.

#define FILTERSCRIPT

#include <open.mp>   // or <a_samp>
#include <sageom>

public OnPlayerCommandText(playerid, cmdtext[])
{
    if (!strcmp(cmdtext, "/crate", true, 6))
    {
        new model = strval(cmdtext[6]);
        if (model <= 0) model = 1271; // gunbox
        if (!SAGeom_IsReady()) return SendClientMessage(playerid, -1, "sa-geom is not loaded yet"), 1;

        new Float:px, Float:py, Float:pz, Float:heading;
        GetPlayerPos(playerid, px, py, pz);
        GetPlayerFacingAngle(playerid, heading);
        new Float:x = px - floatsin(heading, degrees) * 3.0;
        new Float:y = py + floatcos(heading, degrees) * 3.0;

        new Float:z, Float:rx, Float:ry, Float:rz;
        // start the ray a little above the player so a roof overhead is not mistaken for the ground
        if (!SAGeom_PlaceModel(model, x, y, pz + 2.0, heading, z, rx, ry, rz, 0.0, GetPlayerInterior(playerid)))
            return SendClientMessage(playerid, -1, "no ground there"), 1;

        CreateObject(model, x, y, z, rx, ry, rz);
        new msg[128];
        format(msg, sizeof msg, "model %d at %.2f %.2f %.2f, rotation %.1f %.1f %.1f", model, x, y, z, rx, ry, rz);
        SendClientMessage(playerid, -1, msg);
        return 1;
    }
    if (!strcmp(cmdtext, "/groundhere", true))
    {
        new Float:x, Float:y, Float:z, Float:gz, Float:nx, Float:ny, Float:nz, material, model;
        GetPlayerPos(playerid, x, y, z);
        if (!SAGeom_GetGround(x, y, gz, z + 1.0, GetPlayerInterior(playerid), nx, ny, nz, material, model))
            return SendClientMessage(playerid, -1, "no ground under you"), 1;
        new msg[128];
        format(msg, sizeof msg, "ground z %.3f, slope %.1f degrees, material %d, model %d", gz, acos(nz), material, model);
        SendClientMessage(playerid, -1, msg);
        return 1;
    }
    return 0;
}
