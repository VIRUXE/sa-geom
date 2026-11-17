#!/usr/bin/env python3
"""Fix a spawn list offline: every point x every model it can spawn, laid on the real ground by sa-geom.

    fix_spawns.py POINTS --models 400,422,461 [--rest 355=90,0,0 ...] [--flip] [--scatter R --max N]
                  [--overlay FILE ...] [--above M] [-o placements.jsonl] [--report report.csv]

POINTS is a CSV (`id,x,y,z[,yaw[,interior]]`, header optional) or JSON lines ({"id", "x", "y", "z", "yaw"?,
"interior"?, "models"?: [...]}). A point's own "models" list overrides --models.

For each point and model the ground is searched from the stored z + --above, the model is laid on it by its shape
(render mesh, so items without collision and vehicle wheels count) and turned to the slope, and the result is one
JSON line: {"id", "model", "yaw", "slot", "pos": [x, y, z], "rot": [rx, ry, rz], "normal": [nx, ny, nz],
"base", "dz", "tilt", "flags"}. `pos` and `rot` go straight into createObject/createVehicle (MTA) or
CreateObject/CreateVehicle (SA-MP: CreateVehicle takes only the heading; use the z, or SetVehicleZAngle + the
quaternion natives of your server for the tilt).

--rest MODEL=RX,RY,RZ  the model's rest rotation before the heading (a rifle lying on its side: 90,0,0).
--flip                 also place every model facing yaw + 180 (spawn systems that pick either way).
--scatter R --max N    items scattered around the point the way loot scripts do: item i of n at angle i*360/n,
                       R metres out, for n = 1..N. Each slot is a separate line ("slot": [i, n]).

flags: no_ground (nothing under the point: keep it as it was or move it), blocked (the model's body runs into a wall,
fence, pole, trunk or roof more than --margin metres; "blocked_by" names it), moved (the placed origin is more than
--moved metres from where the stored z put it), steep (slope above --steep degrees: vehicles slide, items roll),
underwater (water above the ground), propped (part of the footprint rests on something higher, a wall top or kerb,
and the model leans on it, more than --gap metres over the ground under it), fell (the ground found is more than 2 m under the point: the point stands on
something the world does not have, e.g. an object your server adds - load it with --overlay).

The report (--report) lists every point with a flag, worst first, for a human to look at.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_CLI = os.path.join(HERE, "..", "..", "target", "release", "sa-geom")


def read_points(path: str) -> list[dict]:
    pts = []
    with open(path, encoding="utf8") as f:
        first = f.readline()
        f.seek(0)
        if first.lstrip().startswith("{"):
            for line in f:
                if line.strip():
                    pts.append(json.loads(line))
        else:
            for row in csv.reader(f):
                if not row or row[0].strip().startswith("#"):
                    continue
                try:
                    vals = [float(v) for v in row[1:]]
                except ValueError:
                    continue  # header
                p = {"id": row[0].strip(), "x": vals[0], "y": vals[1], "z": vals[2]}
                if len(vals) > 3:
                    p["yaw"] = vals[3]
                if len(vals) > 4:
                    p["interior"] = int(vals[4])
                pts.append(p)
    return pts


def slots(scatter: float, n_max: int) -> list[tuple[int, int, float, float]]:
    """(i, n, dx, dy) for item i of n, i = 1..n, n = 1..n_max."""
    out = []
    for n in range(1, n_max + 1):
        for i in range(1, n + 1):
            a = math.radians(i * (360.0 / n))
            out.append((i, n, math.cos(a) * scatter, math.sin(a) * scatter))
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("points")
    ap.add_argument("--models", default="", help="comma separated model ids")
    ap.add_argument("--rest", action="append", default=[], metavar="MODEL=RX,RY,RZ")
    ap.add_argument("--flip", action="store_true")
    ap.add_argument("--scatter", type=float, default=0.0)
    ap.add_argument("--max", type=int, default=1)
    ap.add_argument("--overlay", action="append", default=[])
    ap.add_argument("--above", type=float, default=1.5, help="start each ray this far above the stored z")
    ap.add_argument("--moved", type=float, default=0.25)
    ap.add_argument("--steep", type=float, default=20.0)
    ap.add_argument("--margin", type=float, default=0.15, help="how far collision may reach into a model's box (mirrors, trim)")
    ap.add_argument("--gap", type=float, default=0.3, help="flag a model propped this far over the ground under it")
    ap.add_argument("--cli", default=os.environ.get("SA_GEOM", DEFAULT_CLI))
    ap.add_argument("-o", "--out", default="-")
    ap.add_argument("--report")
    a = ap.parse_args(argv)

    rest = {}
    for r in a.rest:
        m, v = r.split("=", 1)
        rest[int(m)] = [float(x) for x in v.split(",")]
    default_models = [int(m) for m in a.models.split(",") if m.strip()]
    pts = read_points(a.points)
    ring = slots(a.scatter, a.max) if a.scatter > 0 else [(1, 1, 0.0, 0.0)]

    # one batch query per point x model x heading x slot; the input rides along in the output
    lines, keys = [], []
    for p in pts:
        models = p.get("models") or default_models
        yaw0 = float(p.get("yaw", 0.0))
        for m in models:
            for yaw in ([yaw0, (yaw0 + 180.0) % 360.0] if a.flip else [yaw0]):
                for i, n, dx, dy in ring:
                    q = {"x": p["x"] + dx, "y": p["y"] + dy, "z": p["z"], "yaw": yaw, "model": int(m), "interior": int(p.get("interior", 0)), "margin": a.margin}
                    if int(m) in rest:
                        q["rest"] = rest[int(m)]
                    lines.append(json.dumps(q))
                    keys.append((p, int(m), yaw, (i, n), q))
    cmd = [a.cli, "batch", "--above", str(a.above)]
    for o in a.overlay:
        cmd += ["--overlay", o]
    res = subprocess.run(cmd, input="\n".join(lines) + "\n", capture_output=True, text=True)
    if res.returncode != 0:
        sys.stderr.write(res.stderr)
        return res.returncode
    results = [json.loads(l) for l in res.stdout.splitlines() if l.strip()]

    out = sys.stdout if a.out == "-" else open(a.out, "w", encoding="utf8")
    flagged: dict[str, dict] = {}
    for (p, m, yaw, slot, q), r in zip(keys, results):
        row = {"id": p["id"], "model": m, "yaw": round(yaw, 4), "slot": list(slot)}
        flags = []
        if not r.get("hit"):
            flags.append("no_ground")
            row.update(pos=None, rot=None)
        else:
            pos, n = r["pos"], [r["nx"], r["ny"], r["nz"]]
            tilt = math.degrees(math.acos(max(-1.0, min(1.0, n[2]))))
            base = r.get("base") or 0.0
            dz = pos[2] - (p["z"])
            row.update(pos=pos, rot=r["rot"], normal=n, base=base, dz=round(dz, 4), tilt=round(tilt, 2), shape=r.get("shape"))
            if p["z"] - r["z"] > 2.0:
                flags.append("fell")
            if abs(r["z"] - p["z"]) > a.moved:
                flags.append("moved")
            if tilt > a.steep:
                flags.append("steep")
            if r.get("underwater"):
                flags.append("underwater")
            if (r.get("gap") or 0.0) > a.gap:
                flags.append("propped")
            if r.get("blocked"):
                flags.append("blocked")
                row["blocked_by"] = r["blocked"]["name"] or r["blocked"]["model"]
        row["flags"] = flags
        out.write(json.dumps(row) + "\n")
        if flags:
            f = flagged.setdefault(str(p["id"]), {"id": p["id"], "x": p["x"], "y": p["y"], "z": p["z"], "flags": set(), "worst_dz": 0.0, "max_tilt": 0.0})
            f["flags"].update(flags)
            if row.get("dz") is not None:
                f["worst_dz"] = max(f["worst_dz"], abs(row["dz"] - (row["base"] or 0.0)))
                f["max_tilt"] = max(f["max_tilt"], row["tilt"])
    if out is not sys.stdout:
        out.close()

    if a.report:
        order = {"no_ground": 0, "fell": 1, "blocked": 2, "underwater": 3, "propped": 4, "steep": 5, "moved": 6}
        rows = sorted(flagged.values(), key=lambda f: (min(order[x] for x in f["flags"]), -f["worst_dz"]))
        with open(a.report, "w", newline="", encoding="utf8") as f:
            w = csv.writer(f)
            w.writerow(["id", "x", "y", "z", "flags", "ground_off_m", "max_tilt_deg"])
            for r in rows:
                w.writerow([r["id"], r["x"], r["y"], r["z"], " ".join(sorted(r["flags"], key=order.get)), round(r["worst_dz"], 3), r["max_tilt"]])
    total = len(results)
    sys.stderr.write(f"{len(pts)} points, {total} placements, {len(flagged)} points flagged\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
