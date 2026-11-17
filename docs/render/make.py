"""README renders: asks the sa-geom CLI for real placements and collision, then renders each scene in Blender.

    python3 docs/render/make.py [scene ...] [--preview]

Needs a release build of the CLI (cargo build --release -p sa-geom-cli) and `blender` on PATH.
Writes docs/images/<scene>.png.
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
        a += [f"--{k}", v]
    out = cli(*a)
    return json.loads(out) if out.strip() else {"hit": False}


def export(name, lo, hi, origin):
    path = os.path.join(WORK, f"{name}.obj")
    subprocess.run([CLI, "export", "--min", *map(str, lo), "--max", *map(str, hi), "--origin", *map(str, origin), "-o", path], check=True, capture_output=True)
    return path


def rel(p, o):
    return [p[0] - o[0], p[1] - o[1], p[2] - o[2]]


GTA = os.environ.get("GTA_SA", "/gta/sa/gamefiles/GTA San Andreas")
SCOUT = os.environ.get("GTA_SCOUT", "/gta/sa/gta-scout")


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


def render(name, scene):
    os.makedirs(WORK, exist_ok=True)
    if PREVIEW:
        scene["samples"] = 16
        scene["size"] = [s // 2 for s in scene.get("size", [1600, 900])]
    sp = os.path.join(WORK, f"{name}.json")
    json.dump(scene, open(sp, "w"))
    out = os.path.join(WORK if PREVIEW else OUT, f"{name}.png")
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
        "size": [1600, 900], "samples": 96,
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
    base = {"camera": cam, "sun": {"rot": [40, 0, math.degrees(math.atan2(sy, sx)) + 140], "energy": 3.6}, "size": [900, 760], "samples": 128, "wire_thickness": 0.02}
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
    out = os.path.join(WORK if PREVIEW else OUT, "compare.png")
    img.save(out)
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
        "camera": {"loc": [300, 0, -6], "target": [0, 0, -6], "ortho": 300, "clip_start": 299.5, "clip_end": 900},
        "sun": {"rot": [55, 0, 70], "energy": 3.2}, "wire_thickness": 0.18,
        "size": [1600, 900], "samples": 96,
    })


SCENES = {"hero": hero, "compare": compare, "bridge": bridge}

if __name__ == "__main__":
    names = [a for a in sys.argv[1:] if not a.startswith("--")] or list(SCENES)
    for n in names:
        SCENES[n]()
