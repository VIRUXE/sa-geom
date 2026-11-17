"""README renders: asks the sa-geom CLI for real placements and collision, then renders each scene in Blender.

    python3 docs/render/make.py [scene ...] [--preview]

Needs a release build of the CLI (cargo build --release -p sa-geom-cli) and `blender` on PATH.
Writes docs/images/<scene>.jpg (intermediate PNGs and scene files go to target/renders/).
"""

import json
import math
import os
import random
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLI = os.path.join(ROOT, "target", "release", "sa-geom")
OUT = os.path.join(ROOT, "docs", "images")
WORK = os.path.join(ROOT, "target", "renders")
PREVIEW = "--preview" in sys.argv


def cli(*a, stdin=None):
    r = subprocess.run([CLI, *map(str, a)], input=stdin, capture_output=True, text=True)
    return r.stdout


def ground(x, y, **kw):
    a = ["ground", x, y]
    for k, v in kw.items():
        a.append(f"--{k}={v}")  # '=' so negative values are not read as flags
    out = cli(*a)
    return json.loads(out) if out.strip() else {"hit": False}


def export(name, lo, hi, origin):
    path = os.path.join(WORK, f"{name}.obj")
    subprocess.run([CLI, "export", "--min", *map(str, lo), "--max", *map(str, hi), "--origin", *map(str, origin), "-o", path], check=True, capture_output=True)
    return path


def rel(p, o):
    return [p[0] - o[0], p[1] - o[1], p[2] - o[2]]


GTA = os.environ.get("GTA_SA", "")
SCOUT = os.environ.get("GTA_SCOUT", "")


def visual(name, o, lo, hi, margin=150, props=()):
    """Real game models for a region: every map placement near the box (from the game install) plus `props`
    models, extracted with gta-scout. Returns the scene's "models" entry, matrices relative to `o`."""
    out = subprocess.run([CLI, "placements", "--gta", GTA, "--min", *map(str, lo), "--max", *map(str, hi), "--margin", str(margin)], capture_output=True, text=True, check=True).stdout
    places = json.loads(out)
    ids = sorted({p["model"] for p in places} | set(props))
    adir = os.path.join(WORK, "assets", name)
    os.makedirs(adir, exist_ok=True)
    r = subprocess.run(["node", os.path.join(SCOUT, "scripts", "prepare-asset-catalog-views.mjs"), GTA, adir, "--catalog-root", os.path.join(SCOUT, "output", "my-sa-source"), *map(str, ids)], capture_output=True, text=True)
    if r.returncode != 0:
        print(r.stderr[-2000:])
        raise SystemExit("gta-scout extraction failed")
    for p in places:
        p["matrix"][3] = [p["matrix"][3][0] - o[0], p["matrix"][3][1] - o[1], p["matrix"][3][2] - o[2], 1.0]
    print(f"{name}: {len(places)} placements, {len(ids)} models")
    return {"input": os.path.join(adir, "input.json"), "scout": SCOUT, "instances": places}


def publish(png):
    """Copy a finished render into docs/images as a JPEG (README images stay small)."""
    if PREVIEW:
        return
    from PIL import Image
    os.makedirs(OUT, exist_ok=True)
    dst = os.path.join(OUT, os.path.splitext(os.path.basename(png))[0] + ".jpg")
    Image.open(png).convert("RGB").save(dst, quality=88, optimize=True, progressive=True)
    print("published", dst)


def render(name, scene):
    os.makedirs(WORK, exist_ok=True)
    if PREVIEW:
        scene["samples"] = 16
        scene["size"] = [s // 2 for s in scene.get("size", [1600, 900])]
    sp = os.path.join(WORK, f"{name}.json")
    json.dump(scene, open(sp, "w"))
    out = os.path.join(WORK, f"{name}.png")
    r = subprocess.run(["blender", "-b", "--factory-startup", "--python-exit-code", "1", "-P", os.path.join(ROOT, "docs", "render", "scene.py"), "--", sp, out], capture_output=True, text=True)
    if "@@RENDERED" not in r.stdout:
        print(r.stdout[-3000:], r.stderr[-3000:])
        raise SystemExit(f"{name}: render failed")
    print("wrote", out)
    return out


MODELS = {1685: "orange", 3798: "teal", 3800: "yellow"}


GROUND = ("TARMAC", "PAVEMENT", "GRASS", "DIRT", "SAND", "GRAVEL", "CONCRETE", "ROCK", "MUD", "STAIRS")


def on_ground(g):
    return g.get("hit") and any(k in g["material_name"] for k in GROUND)


def slope_frame(cx, cy, r=40):
    """Downhill direction (unit x, y) of a least-squares plane through the ground around (cx, cy)."""
    pts = []
    for dx in range(-r, r + 1, 10):
        for dy in range(-r, r + 1, 10):
            g = ground(cx + dx, cy + dy)
            if on_ground(g):
                pts.append((dx, dy, g["z"]))
    n = len(pts)
    sx = sum(p[0] for p in pts); sy = sum(p[1] for p in pts); sz = sum(p[2] for p in pts)
    sxx = sum(p[0] ** 2 for p in pts); syy = sum(p[1] ** 2 for p in pts); sxy = sum(p[0] * p[1] for p in pts)
    sxz = sum(p[0] * p[2] for p in pts); syz = sum(p[1] * p[2] for p in pts)
    # z = a x + b y + c, normal equations (centred, ignoring the small cross term mean)
    det = (sxx - sx * sx / n) * (syy - sy * sy / n) - (sxy - sx * sy / n) ** 2
    a = ((sxz - sx * sz / n) * (syy - sy * sy / n) - (syz - sy * sz / n) * (sxy - sx * sy / n)) / det
    b = ((syz - sy * sz / n) * (sxx - sx * sx / n) - (sxz - sx * sz / n) * (sxy - sx * sy / n)) / det
    l = math.hypot(a, b) or 1.0
    return -a / l, -b / l, math.degrees(math.atan(l))


def hero():
    """Vinewood hillside: crates dropped on a grid, each resting on its base and tilted to the slope under it."""
    cx, cy = 1200.0, -700.0
    z0 = ground(cx, cy)["z"]
    o = [cx, cy, z0]
    dx, dy, deg = slope_frame(cx, cy)
    rnd = random.Random(11)
    boxes, rays = [], []
    step = 5.5
    for i in range(-8, 9):
        for j in range(-8, 9):
            x = cx + i * step + rnd.uniform(-1.2, 1.2)
            y = cy + j * step + rnd.uniform(-1.2, 1.2)
            m = rnd.choice(list(MODELS))
            g = ground(x, y, model=m, yaw=round(rnd.uniform(0, 360), 2))
            if not on_ground(g) or g["nz"] < 0.7:
                continue
            boxes.append({"model": m, "pos": rel(g["pos"], o), "rot": g["rot"]})
            if rnd.random() < 0.12:
                hit = rel([g["x"], g["y"], g["z"]], o)
                rays.append({"from": [hit[0], hit[1], hit[2] + 12], "to": hit, "radius": 0.06, "dot": 0.2})
    # camera across the slope (perpendicular to downhill), a little downhill and above
    px, py = -dy, dx
    cam = [px * 62 + dx * 22, py * 62 + dy * 22, 30]
    print(f"hero: {len(boxes)} crates, {len(rays)} rays, mean slope {deg:.1f} deg, downhill ({dx:.2f}, {dy:.2f})")
    models = visual("hero", o, [cx - 110, cy - 110, z0 - 70], [cx + 110, cy + 110, z0 + 70], props=MODELS)
    models["props"] = boxes
    return render("hero", {
        "models": models, "rays": rays,
        "camera": {"loc": cam, "target": [dx * 4, dy * 4, -6], "lens": 30},
        "sun": {"rot": [48, 0, math.degrees(math.atan2(py, px)) + 120], "energy": 3.4},
        "size": [1440, 810], "samples": 40, "sky": "#a9c8e6",
    })


def batch(qs):
    out = cli("batch", stdin="\n".join(json.dumps(q) for q in qs))
    return [json.loads(l) for l in out.splitlines()]


def compare():
    """A 3 x 7 m container on a 30 degree grass slope: origin at the ground and flat (left) against
    sa-geom's model placement (right). Same camera, composed side by side."""
    x, y, yaw, m = 1203.0, -721.0, 20.0, 2934
    c, p = batch([{"x": x, "y": y}, {"x": x, "y": y, "model": m, "yaw": yaw}])
    o = [x, y, c["z"]]
    models = visual("compare", o, [x - 40, y - 40, c["z"] - 30], [x + 40, y + 40, c["z"] + 30], props=[m])
    naive = {"model": m, "pos": [0, 0, 0], "rot": [0, 0, yaw]}
    good = {"model": m, "pos": rel(p["pos"], o), "rot": p["rot"]}
    hit = rel([p["x"], p["y"], p["z"]], o)
    # camera across the slope, level with the box
    nx, ny = p["nx"], p["ny"]
    l = math.hypot(nx, ny) or 1
    sx, sy = -ny / l, nx / l
    cam = {"loc": [sx * 20 + nx / l * 6, sy * 20 + ny / l * 6, 7.5], "target": [0, 0, 0.6], "lens": 40}
    base = {"camera": cam, "sun": {"rot": [40, 0, math.degrees(math.atan2(sy, sx)) + 140], "energy": 3.6}, "size": [900, 760], "samples": 48, "sky": "#a9c8e6"}
    a = render("compare_naive", {**base, "models": {**models, "props": [naive]}})
    b = render("compare_sageom", {**base, "models": {**models, "props": [good]}, "rays": [{"from": [hit[0], hit[1], hit[2] + 6], "to": hit, "radius": 0.035, "dot": 0.12, "normal": [p["nx"], p["ny"], p["nz"]], "normal_len": 1.6}]})
    from PIL import Image, ImageDraw, ImageFont
    A, B = Image.open(a), Image.open(b)
    W, H = A.width + B.width + 12, A.height + 70
    img = Image.new("RGB", (W, H), (24, 26, 30))
    img.paste(A, (0, 70)); img.paste(B, (A.width + 12, 70))
    d = ImageDraw.Draw(img)
    try:
        f = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 30)
    except OSError:
        f = ImageFont.load_default()
    d.text((24, 18), "origin at ground z, flat", fill=(240, 120, 120), font=f)
    d.text((A.width + 36, 18), "sa-geom PlaceModel", fill=(110, 220, 205), font=f)
    out = os.path.join(WORK, "compare.png")
    img.save(out)
    publish(out)
    print("wrote", out)


def bridge():
    """Gant Bridge cut open at x = -2680: one (x, y), four surfaces. A heightmap only knows the top one."""
    x, y = -2680.0, 1700.0
    o = [x, y, 0.0]
    models = visual("bridge", o, [x - 60, y - 260, -120], [x + 60, y + 260, 160], margin=200)
    starts = [(y - 6, 160), (y, 63), (y + 6, 50)]
    hits = batch([{"x": x - 0.5, "y": yy, "z": z0 - 1} for yy, z0 in starts])  # batch starts rays 1 m above z
    rays = []
    for (yy, z0), h, col in zip(starts, hits, ["red", "blue", "blue"]):
        rays.append({"from": [-0.5, yy - y, z0], "to": [-0.5, yy - y, h["z"]], "radius": 0.45, "dot": 1.6, "color": col})
    print("bridge hits:", [round(h["z"], 1) for h in hits])
    return render("bridge", {
        "models": models, "rays": rays, "water": 0.0, "water_size": 900, "water_depth": 140, "water_alpha": 0.35,
        "camera": {"loc": [300, 0, ZC], "target": [0, 0, ZC], "ortho": ORTHO, "clip_start": 299.5, "clip_end": 900},
        "sun": {"rot": [55, 0, 70], "energy": 3.2}, "sky": "#a9c8e6",
        "size": [1440, 810], "samples": 40,
    }), [(yy - y, h["z"], z0) for (yy, z0), h in zip(starts, hits)]


ZC, ORTHO = 0.0, 330.0


def bridge_labels():
    path, pts = bridge()
    from PIL import Image, ImageDraw, ImageFont
    img = Image.open(path).convert("RGB")
    W, H = img.size
    k = W / ORTHO
    to_px = lambda yy, z: (W / 2 + yy * k, H / 2 - (z - ZC) * k)
    d = ImageDraw.Draw(img)
    try:
        f = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 22)
    except OSError:
        f = ImageFont.load_default()
    texts = ["ray from the sky: top of the bridge", "ray from under it: the road deck", "ray from below the deck: the sea floor"]
    for (yy, z, z0), t in zip(pts, texts):
        px, py = to_px(yy, z)
        label = f"{t}  z = {z:.1f}"
        tw = d.textlength(label, font=f)
        x0 = px + 18
        y0 = py - 14 if z > 0 else py - 34
        d.rounded_rectangle([x0 - 8, y0 - 4, x0 + tw + 8, y0 + 28], radius=6, fill=(20, 22, 26))
        d.text((x0, y0), label, fill=(255, 255, 255), font=f)
    px, py = to_px(0, 0)
    d.rounded_rectangle([W - 260, py + 6, W - 16, py + 38], radius=6, fill=(20, 22, 26))
    d.text((W - 250, py + 9), "water  z = 0.0", fill=(140, 200, 255), font=f)
    img.save(path)
    print("labelled", path)
    publish(path)


def hero_publish():
    publish(hero())


# ---- fixing a spawn list offline: points from a real server's loot and vehicle spawn lists, placed the usual way
# (stored z plus a fixed offset, flat) against sa-geom's `ground --model --rest` (lowest point on the ground,
# tilted to the slope)

def vehicle_assets(name, ids):
    """Extract vehicle .dff/.txd (and the shared vehicle.txd) with gta-scout, through a small catalog built from
    vehicles.ide. Returns the input.json path for scene.py."""
    import re
    cat = os.path.join(WORK, "vehicle-catalog")
    os.makedirs(os.path.join(cat, "models", "data"), exist_ok=True)
    ms = []
    for line in open(os.path.join(GTA, "data", "vehicles.ide"), errors="replace"):
        p = [x for x in re.split(r"[,\s]+", line.split("#")[0].strip()) if x]
        if len(p) > 5 and p[0].isdigit():
            ms.append({"id": int(p[0]), "name": p[1], "dff": p[1], "txd": p[2], "source": "vehicles.ide", "tags": []})
    ms.append({"id": 1, "name": "vehicle", "dff": "landstal", "txd": "vehicle", "source": "vehicles.ide", "tags": []})
    json.dump({"models": ms}, open(os.path.join(cat, "models", "data", "models.json"), "w"))
    adir = os.path.join(WORK, "assets", name + "-vehicles")
    r = subprocess.run(["node", os.path.join(SCOUT, "scripts", "prepare-asset-catalog-views.mjs"), GTA, adir, "--catalog-root", cat, *map(str, ids), "1"], capture_output=True, text=True)
    if r.returncode != 0:
        print(r.stdout[-2000:], r.stderr[-2000:])
        raise SystemExit("gta-scout vehicle extraction failed")
    return os.path.join(adir, "input.json")


def side_by_side(a, b, labels, out, colors=((240, 120, 120), (110, 220, 205))):
    from PIL import Image, ImageDraw, ImageFont
    A, B = Image.open(a), Image.open(b)
    W, H = A.width + B.width + 12, A.height + 70
    img = Image.new("RGB", (W, H), (24, 26, 30))
    img.paste(A, (0, 70)); img.paste(B, (A.width + 12, 70))
    d = ImageDraw.Draw(img)
    try:
        f = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 30 if not PREVIEW else 20)
    except OSError:
        f = ImageFont.load_default()
    d.text((24, 18), labels[0], fill=colors[0], font=f)
    d.text((A.width + 36, 18), labels[1], fill=colors[1], font=f)
    img.save(out)
    publish(out)
    print("wrote", out)


# Item types: model, rest rotation (rx, ry, rz) and the fixed z offset the naive placement adds.
ITEMS = {
    "bat": (336, (90, 0, 0), 0.0), "spade": (337, (90, 0, 0), 0.0), "golfclub": (333, (90, 0, 0), 0.0),
    "extinguisher": (366, (90, 0, 0), 0.0), "pistol": (346, (90, 0, 0), 0.0), "shotgun": (349, (90, 0, 0), 0.0),
    "can": (2601, (0, 0, 0), 0.054), "bottle": (1543, (0, 0, 0), 0.0), "petrol": (1650, (0, 0, 0), 0.27),
    "ammo": (2038, (0, 0, 0), 0.082), "pizza": (1582, (0, 0, 0), 0.0), "bag": (363, (270, 0, 0), 0.0),
    "pot": (2203, (0, 0, 0), 0.138), "pills": (2709, (0, 0, 0), 0.09),
}

# A loot point (x, y, stored z) by a doorstep on a steep street, with the items (and headings) one roll gave.
LOOT = [
    ((2099.463623, -1052.48645, 27.35659), [("spade", 75), ("pizza", 15), ("bottle", 0), ("bag", 250), ("bat", 140)]),
]
SPAWN_ITEMS_CAM = {"loc": [-2.1, -1.65, 1.85], "target": [0.0, 0.05, 0.05], "lens": 30}  # relative to the point


def spawns_items():
    """Loot items scattered round their spawn point by a doorstep on a steep street: the stored z plus a fixed offset, flat
    (left) against `sa-geom ground --model --rest` per item (right)."""
    naive, good = [], []
    for (x, y, z), items in LOOT:
        n = len(items)
        for i, (kind, rz) in enumerate(items, 1):
            m, rest, dz = ITEMS[kind]
            a = math.radians(i * 360 / n)
            ix, iy = x + math.cos(a) * 0.6, y + math.sin(a) * 0.6
            g = ground(ix, iy, model=m, yaw=rz, rest=",".join(map(str, rest)), **{"from": z + 1.5})
            naive.append({"model": m, "pos": [ix, iy, z + 0.1032 + dz], "rot": [rest[0], rest[1], rz + rest[2]]})
            good.append({"model": m, "pos": g["pos"], "rot": g["rot"]})
            # naive lowest point above the ground under the item (+ floats, - buried), and the slope there
            print(f"  {kind:12s} {z + 0.1032 + dz - g['base'] - g['z']:+.2f} m  slope {math.degrees(math.acos(g['nz'])):4.1f} deg  {g['material_name']}")
    o = [sum(p[0][k] for p in LOOT) / len(LOOT) for k in range(3)]
    for p in naive + good:
        p["pos"] = rel(p["pos"], o)
    models = visual("spawns-items", o, [o[0] - 40, o[1] - 40, o[2] - 30], [o[0] + 40, o[1] + 40, o[2] + 30], margin=120, props=sorted({v[0] for v in ITEMS.values()}))
    base = {"camera": SPAWN_ITEMS_CAM, "sun": {"rot": [40, 0, 30], "energy": 3.6}, "size": [900, 760], "samples": 64, "sky": "#a9c8e6"}
    a = render("spawns-items_naive", {**base, "models": {**models, "props": naive}})
    b = render("spawns-items_sageom", {**base, "models": {**models, "props": good}})
    side_by_side(a, b, ("before: stored z + offset, flat", "after: sa-geom ground --model"), os.path.join(WORK, "spawns-items.png"))


# Vehicle points (x, y, stored z, heading) in sloping front yards, with a vehicle the point's classes allow and a paint.
VEHICLES = [
    ((2493.679687, -1276.285644, 33.002799, 448.848907), 543, ("#8c6a3a", "#d8d2c2")),
    ((2489.902587, -1292.775024, 32.090999, 246.301101), 422, ("#3c6e8f", "#d8d2c2")),
    ((2491.154296, -1299.415771, 32.157798, 456.499511), 475, ("#7a1e1e", "#d8d2c2")),
]
SPAWN_VEHICLES_CAM = {"loc": [-1.0, -19.5, 4.2], "target": [-1.0, -4.5, -0.8], "lens": 34}


def spawns_vehicles():
    """Vehicles at their spawn points: created at the stored z, flat (left) against `sa-geom ground --model`
    (right), which rests the wheels on the ground and tilts the body to it."""
    naive, good = [], []
    for (x, y, z, rz), m, paint in VEHICLES:
        g = ground(x, y, model=m, yaw=rz, **{"from": z + 2})
        naive.append({"model": m, "pos": [x, y, z], "rot": [0, 0, rz], "paint": paint})
        good.append({"model": m, "pos": g["pos"], "rot": g["rot"], "paint": paint})
        print(f"  {m}: origin {z - g['pos'][2]:+.2f} m vs sa-geom, slope {math.degrees(math.acos(g['nz'])):4.1f} deg, rot {g['rot']}  {g['material_name']}")
    o = [sum(v[0][k] for v in VEHICLES) / len(VEHICLES) for k in range(3)]
    for p in naive + good:
        p["pos"] = rel(p["pos"], o)
    models = visual("spawns-vehicles", o, [o[0] - 50, o[1] - 50, o[2] - 30], [o[0] + 50, o[1] + 50, o[2] + 30], margin=120)
    models["vehicles"] = vehicle_assets("spawns-vehicles", sorted({v[1] for v in VEHICLES}))
    base = {"camera": SPAWN_VEHICLES_CAM, "sun": {"rot": [42, 0, 230], "energy": 3.6}, "size": [900, 760], "samples": 64, "sky": "#a9c8e6"}
    a = render("spawns-vehicles_naive", {**base, "models": {**models, "vehicle_props": naive}})
    b = render("spawns-vehicles_sageom", {**base, "models": {**models, "vehicle_props": good}})
    side_by_side(a, b, ("before: stored z, flat", "after: sa-geom ground --model"), os.path.join(WORK, "spawns-vehicles.png"))


SCENES = {"hero": hero_publish, "compare": compare, "bridge": bridge_labels, "spawns-items": spawns_items, "spawns-vehicles": spawns_vehicles}

if __name__ == "__main__":
    names = [a for a in sys.argv[1:] if not a.startswith("--")] or list(SCENES)
    for n in names:
        SCENES[n]()
