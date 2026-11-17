# Fixing a spawn list offline

`fix_spawns.py` lays every point of a spawn list on the real ground for every model it can spawn, with the
`sa-geom` CLI, and reports the points a human should look at. Run it once, store the result with your spawns,
and the server never has to work out a height at spawn time.

```console
$ cargo build --release -p sa-geom-cli
$ python3 examples/spawn-points/fix_spawns.py points.csv --models 400,422,461 --flip -o placements.jsonl --report check.csv
```

## Input

A CSV (`id,x,y,z[,yaw[,interior]]`, header optional) or JSON lines:

```json
{"id": 17, "x": -2300.0, "y": -1700.0, "z": 478.7, "yaw": 45, "models": [400, 422]}
```

`z` is what you stored. Rays start `--above` metres over it (1.5 by default), so a point recorded a little under
or over the floor still finds it, and a point inside a building finds its floor, not the roof.

## Options

| | |
|---|---|
| `--models 400,422` | models every point can spawn (a point's own `models` list wins) |
| `--rest 355=90,0,0` | a model's rest rotation before the heading (a rifle on its side) |
| `--flip` | also place every model facing the other way (spawners that pick either) |
| `--scatter 0.6 --max 6` | loot scattered around the point: item *i* of *n* at angle *i*·360/*n*, R metres out |
| `--overlay FILE` | your server's own maps and models (see [`../overlay.toml`](../overlay.toml)) |
| `--moved 0.25`, `--steep 20`, `--gap 0.3`, `--margin 0.15` | thresholds for the report |

## Output

One JSON line per point × model × heading × slot:

```json
{"id": "17", "model": 400, "yaw": 45.0, "slot": [1, 1], "pos": [-2299.98, -1700.3, 479.63],
 "rot": [12.82, 348.64, 45.0], "normal": [0.015, -0.293, 0.956], "base": 0.9997, "dz": 0.93, "tilt": 17.1,
 "shape": "mesh", "flags": []}
```

`pos` and `rot` go straight to `createVehicle`/`createObject` (MTA; for a vehicle use
`setElementRotation(v, rx, ry, rz, "ZXY")`, the order these angles are in) or `CreateObject` (SA-MP).
`base` is how high the model's origin sits above its lowest point.

The report lists flagged points, worst first:

| flag | meaning |
|---|---|
| `no_ground` | nothing under the point: keep it as it was, or move it |
| `blocked` | the model's body runs into collision (a wall, fence, pole, trunk, low roof) more than `--margin` (0.15 m: mirrors and trim may graze); `blocked_by` names the object |
| `fell` | the ground is more than 2 m under the stored z: the point stands on something the world doesn't have (load your maps with `--overlay`) |
| `underwater` | water above the ground |
| `propped` | part of the footprint rests on something higher (a wall top, a kerb) and the model leans on it, more than `--gap` (0.3) metres over the ground under it |
| `steep` | the slope is over `--steep` degrees: cars slide, items roll |
| `moved` | the ground is more than `--moved` metres from the stored z |

## Items: one rest height per type

A loot point that rolls one of 300 item types doesn't need 300 placements. How high an item's origin sits above
the ground in its rest pose doesn't depend on the heading or the place, so store the ground per slot (`--scatter`
with any one model) and one height per type (`sa-geom model ID --rest RX,RY,RZ`, the `base` field). At spawn
time the origin goes `base` metres out from the slot's ground along its normal.
