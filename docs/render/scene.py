"""Blender side of the README renders: builds one scene from a JSON description and renders it.

    blender -b --factory-startup -P docs/render/scene.py -- scene.json out.png

scene.json (written by make.py): {"obj": path, "boxes": [...], "rays": [...], "camera": {...}, "sun": {...},
"water": z|null, "samples": n, "size": [w, h]}. Coordinates are already relative to the scene origin.
"""

import json
import math
import sys

import bpy
import mathutils

args = sys.argv[sys.argv.index("--") + 1:]
scene_path, out_path = args[0], args[1]
S = json.load(open(scene_path))

# ---- GTA object Euler angles -> rotation matrix (open.mp GTAQuat, conjugated), as sa-world/src/euler.rs

def gta_matrix(deg):
    rx, ry, rz = (math.radians(d) * -0.5 for d in deg)
    cx, cy, cz = math.cos(rx), math.cos(ry), math.cos(rz)
    sx, sy, sz = math.sin(rx), math.sin(ry), math.sin(rz)
    w = cx * cy * cz + sx * sy * sz
    x = cx * sy * sz + sx * cy * cz
    y = cx * sy * cz - sx * cy * sz
    z = cx * cy * sz - sx * sy * cz
    q = mathutils.Quaternion((w, -x, -y, -z))  # conjugate: the object's rotation
    return q.to_matrix().to_4x4()


# ---- materials

def mat(name, color, rough=0.8, emit=None, metal=0.0):
    m = bpy.data.materials.get(name) or bpy.data.materials.new(name)
    bsdf = next(n for n in m.node_tree.nodes if n.type == "BSDF_PRINCIPLED")
    bsdf.inputs["Base Color"].default_value = (*color, 1.0)
    bsdf.inputs["Roughness"].default_value = rough
    bsdf.inputs["Metallic"].default_value = metal
    if emit:
        bsdf.inputs["Emission Color"].default_value = (*emit, 1.0)
        bsdf.inputs["Emission Strength"].default_value = 4.0
    return m


def srgb(h):
    h = h.lstrip("#")
    c = [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    return tuple(((v + 0.055) / 1.055) ** 2.4 if v > 0.04045 else v / 12.92 for v in c)


SURFACE = [  # keyword in the SA surface name -> colour
    (("GRASS", "VEG", "FLOWER", "HEDGE", "BUSH"), "#7f9a5a"),
    (("SAND", "DIRT", "MUD", "GRAVEL", "P_DIRT", "DESERT", "CORNFIELD", "TRAIL"), "#b49a72"),
    (("ROCK", "CLIFF", "STONE", "BOULDER"), "#8d8378"),
    (("TARMAC", "ROAD", "ASPHALT"), "#5d6066"),
    (("PAVEMENT", "CONCRETE", "STAIRS", "CURB", "PAVING"), "#a7a49d"),
    (("WOOD", "PLANK"), "#9a7652"),
    (("METAL", "STEEL", "GIRDER", "CORRUGATED", "GLASS"), "#7a8692"),
    (("WATER",), "#4f7ea8"),
]


def surface_color(name):
    up = name.upper()
    for keys, col in SURFACE:
        if any(k in up for k in keys):
            return srgb(col)
    return srgb("#c9c4b8")


# ---- scene

bpy.ops.wm.read_factory_settings(use_empty=True)
sc = bpy.context.scene

if S.get("obj"):
    before = set(bpy.data.objects)
    bpy.ops.wm.obj_import(filepath=S["obj"], forward_axis="Y", up_axis="Z")
    for ob in set(bpy.data.objects) - before:
        for slot in ob.material_slots:
            m = slot.material
            if m is None:
                continue
            bsdf = next(n for n in m.node_tree.nodes if n.type == "BSDF_PRINCIPLED")
            bsdf.inputs["Base Color"].default_value = (*surface_color(m.name), 1.0)
            bsdf.inputs["Roughness"].default_value = 0.85
            bsdf.inputs["Specular IOR Level"].default_value = 0.2
        # flat shading, faint wireframe overlay to show the collision triangles
        for p in ob.data.polygons:
            p.use_smooth = False
        if S.get("wire", True):
            wire = ob.copy()
            wire.data = ob.data
            sc.collection.objects.link(wire)
            mod = wire.modifiers.new("wire", "WIREFRAME")
            mod.thickness = S.get("wire_thickness", 0.035)
            mod.use_replace = True
            mod.material_offset = 0
            wire.data = ob.data.copy()
            wire.data.materials.clear()
            wire.data.materials.append(mat("wire", srgb("#2b2d31"), 0.9))

crate_colors = {
    "orange": mat("crate_orange", srgb("#e8792b"), 0.55),
    "teal": mat("crate_teal", srgb("#2aa198"), 0.55),
    "yellow": mat("crate_yellow", srgb("#e5b931"), 0.55),
    "red": mat("crate_red", srgb("#d64545"), 0.5),
    "ghost": mat("crate_ghost", srgb("#d64545"), 0.5),
    "blue": mat("crate_blue", srgb("#3f7fd6"), 0.5),
}


def box(b):
    lo, hi = b["bounds"]
    verts = [(x, y, z) for x in (lo[0], hi[0]) for y in (lo[1], hi[1]) for z in (lo[2], hi[2])]
    faces = [(0, 1, 3, 2), (4, 6, 7, 5), (0, 4, 5, 1), (2, 3, 7, 6), (0, 2, 6, 4), (1, 5, 7, 3)]
    me = bpy.data.meshes.new("box")
    me.from_pydata(verts, [], faces)
    me.materials.append(crate_colors[b.get("color", "orange")])
    ob = bpy.data.objects.new("box", me)
    sc.collection.objects.link(ob)
    ob.matrix_world = mathutils.Matrix.Translation(b["pos"]) @ gta_matrix(b["rot"])
    bev = ob.modifiers.new("bevel", "BEVEL")
    bev.width = min(0.06, 0.08 * min(hi[i] - lo[i] for i in range(3)))
    bev.segments = 2
    return ob


for b in S.get("boxes", []):
    box(b)

# ---- real game models (DFF + TXD extracted with gta-scout, built by its DragonFF-based renderer)

library = {}


def atomic_frames(path):
    """Frame name of each atomic of a .dff, in the order gta-scout builds them."""
    from gtaLib.dff import dff
    from asset_catalog_dragon import clumps
    d = dff()
    d.load_file(path)
    return [c.frame_list[a.frame].name for c in clumps(d) for a in c.atomic_list]


def load_library(input_path, scout_root):
    import importlib.util
    import os
    os.environ.setdefault("DRAGONFF_PATH", os.path.join(scout_root, "references", "DragonFF"))
    sys.path.insert(0, os.path.join(scout_root, "scripts"))
    sys.path.insert(0, os.environ["DRAGONFF_PATH"])
    spec = importlib.util.spec_from_file_location("scout_render", os.path.join(scout_root, "scripts", "render-asset-catalog-views.py"))
    scout = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(scout)
    assets = json.load(open(input_path))["assets"]
    for a in assets:
        mid = int(a["id"].rsplit(":", 1)[1])
        try:
            objs, _missing = scout.build(a)
        except Exception as e:  # skinned/empty models: skip, they are not map geometry
            print("skip model", mid, e)
            continue
        if 321 <= mid <= 372:  # weapons: drop the muzzle-flash card, the game only shows it while firing
            for ob, fr in zip(list(objs), atomic_frames(a["dff"])):
                if fr == "gunflash":
                    objs.remove(ob)
                    bpy.data.objects.remove(ob)
        parts = []
        for ob in objs:
            for m in ob.data.materials:
                for n in m.node_tree.nodes:
                    if n.type == "TEX_IMAGE":
                        n.interpolation = "Linear"
                bsdf = next((n for n in m.node_tree.nodes if n.type == "BSDF_PRINCIPLED"), None)
                if bsdf:
                    bsdf.inputs["Specular IOR Level"].default_value = 0.25
            parts.append((ob.data, ob.matrix_world.copy()))
            for col in list(ob.users_collection):
                col.objects.unlink(ob)
        library[mid] = parts


def place_model(mid, world):
    for mesh, local in library.get(mid, []):
        ob = bpy.data.objects.new(f"m{mid}", mesh)
        sc.collection.objects.link(ob)
        ob.matrix_world = world @ local


# ---- vehicles: the game assembles them at run time, so build them the same way: drop damaged (_dam) and
# low-detail (_vlo) parts and the random extras, clone the one "wheel" atomic (under wheel_rf_dummy) onto every
# wheel dummy (turned round on the left side), and paint the placeholder colours (green = primary, magenta =
# secondary, the light colours) the way the game does.

LIGHTS = {(255, 175, 0): "#e9e6dc", (0, 255, 200): "#e9e6dc", (185, 255, 0): "#8a1010", (255, 60, 0): "#8a1010"}
WHEELS = ("wheel_rf_dummy", "wheel_lf_dummy", "wheel_rb_dummy", "wheel_lb_dummy", "wheel_rm_dummy", "wheel_lm_dummy")


def vehicle_material(color, tex_path, paint):
    rgb, alpha = tuple(color[:3]), color[3] / 255
    if rgb == (60, 255, 0):
        base = srgb(paint[0])
    elif rgb == (255, 0, 175):
        base = srgb(paint[1])
    elif rgb in LIGHTS:
        base = srgb(LIGHTS[rgb])
    else:
        base = tuple(c / 255 for c in rgb)
    m = bpy.data.materials.new("veh")
    m.use_nodes = True
    nodes, links = m.node_tree.nodes, m.node_tree.links
    bs = nodes.get("Principled BSDF")
    bs.inputs["Roughness"].default_value = 0.45 if rgb in ((60, 255, 0), (255, 0, 175)) else 0.7
    bs.inputs["Specular IOR Level"].default_value = 0.35
    bs.inputs["Base Color"].default_value = (*base, 1.0)
    if tex_path:
        tex = nodes.new("ShaderNodeTexImage")
        tex.image = bpy.data.images.load(tex_path, check_existing=True)
        tex.interpolation = "Linear"
        mix = nodes.new("ShaderNodeMix")
        mix.data_type = "RGBA"
        mix.blend_type = "MULTIPLY"
        mix.inputs["Factor"].default_value = 1.0
        mix.inputs["A"].default_value = (*base, 1.0)
        links.new(tex.outputs["Color"], mix.inputs["B"])
        links.new(mix.outputs["Result"], bs.inputs["Base Color"])
        if alpha < 1:
            mul = nodes.new("ShaderNodeMath")
            mul.operation = "MULTIPLY"
            mul.inputs[1].default_value = alpha
            links.new(tex.outputs["Alpha"], mul.inputs[0])
            links.new(mul.outputs[0], bs.inputs["Alpha"])
        else:
            links.new(tex.outputs["Alpha"], bs.inputs["Alpha"])
    elif alpha < 1:
        bs.inputs["Alpha"].default_value = alpha
    return m


def build_vehicle(asset, shared, paint):
    from gtaLib.dff import dff
    from asset_catalog_dragon import clumps
    d = dff()
    d.load_file(asset["dff"])
    c = clumps(d)[0]
    texpaths = {**shared, **{k.lower(): v for k, v in asset.get("textures", {}).items()}}
    names = [f.name for f in c.frame_list]

    def fm(i):
        f = c.frame_list[i]
        rm = f.rotation_matrix
        local = mathutils.Matrix.Translation(f.position) @ mathutils.Matrix((rm.right, rm.up, rm.at)).transposed().to_4x4()
        return fm(f.parent) @ local if f.parent >= 0 else local

    parts = []
    for at in c.atomic_list:
        name = names[at.frame]
        if name.endswith("_dam") or name.endswith("_vlo") or name.startswith("extra"):
            continue
        g = c.geometry_list[at.geometry]
        faces = [t for t in g.triangles if len({t.a, t.b, t.c}) == 3]
        me = bpy.data.meshes.new(name)
        me.from_pydata([tuple(v) for v in g.vertices], [], [(t.a, t.b, t.c) for t in faces])
        for mt in g.materials:
            tp = texpaths.get(mt.textures[0].name.lower()) if mt.textures else None
            me.materials.append(vehicle_material(mt.color, tp, paint))
        uv = me.uv_layers.new(name="UVMap")
        for poly, t in zip(me.polygons, faces):
            poly.material_index = t.material
            poly.use_smooth = True
            if g.uv_layers:
                for li in poly.loop_indices:
                    u = g.uv_layers[0][me.loops[li].vertex_index]
                    uv.data[li].uv = (u.u, 1 - u.v)
        if name == "wheel":  # one wheel in the file; the game puts a copy on every wheel dummy
            for dn in WHEELS:
                if dn in names:
                    turn = mathutils.Matrix.Rotation(math.pi, 4, "Z") if "_l" in dn else mathutils.Matrix.Identity(4)
                    parts.append((me, fm(names.index(dn)) @ turn))
        else:
            parts.append((me, fm(at.frame)))
    return parts


def load_vehicles(input_path):
    assets = json.load(open(input_path))["assets"]
    shared = next((a["textures"] for a in assets if a["model"] == "vehicle"), {})
    return {int(a["id"].rsplit(":", 1)[1]): a for a in assets if a["model"] != "vehicle"}, shared


if S.get("models"):
    M = S["models"]
    load_library(M["input"], M["scout"])
    for inst in M.get("instances", []):
        place_model(inst["model"], mathutils.Matrix(inst["matrix"]).transposed())
    for p in M.get("props", []):
        place_model(p["model"], mathutils.Matrix.Translation(p["pos"]) @ gta_matrix(p["rot"]))
    if M.get("vehicles"):
        vassets, shared = load_vehicles(M["vehicles"])
        for p in M.get("vehicle_props", []):
            key = (p["model"], tuple(p.get("paint", ["#b8b8b8", "#b8b8b8"])))
            if key not in library:
                library[key] = build_vehicle(vassets[p["model"]], shared, key[1])
            place_model(key, mathutils.Matrix.Translation(p["pos"]) @ gta_matrix(p["rot"]))
    print("models:", len(library), "instances:", len(M.get("instances", [])), "props:", len(M.get("props", [])))

ray_mat = mat("ray", srgb("#ff3b6b"), 0.4, emit=srgb("#ff3b6b"))
ray_mat2 = mat("ray2", srgb("#3bb4ff"), 0.4, emit=srgb("#3bb4ff"))
dot_mat = mat("dot", srgb("#ffffff"), 0.3, emit=srgb("#ffe08a"))


def cylinder(a, b, r, material):
    a, b = mathutils.Vector(a), mathutils.Vector(b)
    d = b - a
    bpy.ops.mesh.primitive_cylinder_add(vertices=12, radius=r, depth=d.length, location=(a + b) / 2)
    ob = bpy.context.active_object
    ob.rotation_mode = "QUATERNION"
    ob.rotation_quaternion = d.to_track_quat("Z", "Y")
    ob.data.materials.append(material)
    return ob


for r in S.get("rays", []):
    m = ray_mat2 if r.get("color") == "blue" else ray_mat
    cylinder(r["from"], r["to"], r.get("radius", 0.06), m)
    bpy.ops.mesh.primitive_uv_sphere_add(radius=r.get("dot", 0.25), location=r["to"], segments=16, ring_count=8)
    bpy.context.active_object.data.materials.append(dot_mat)
    if r.get("normal"):
        n = mathutils.Vector(r["normal"])
        cylinder(r["to"], mathutils.Vector(r["to"]) + n * r.get("normal_len", 2.0), r.get("radius", 0.06) * 0.8, dot_mat)

if S.get("water") is not None:
    depth = S.get("water_depth", 0)
    if depth:
        bpy.ops.mesh.primitive_cube_add(size=1, location=(0, 0, S["water"] - depth / 2))
        w = bpy.context.active_object
        w.scale = (S.get("water_size", 600), S.get("water_size", 600), depth)
    else:
        bpy.ops.mesh.primitive_plane_add(size=S.get("water_size", 600), location=(0, 0, S["water"]))
        w = bpy.context.active_object
    wm = mat("water", srgb("#3d6f9e"), 0.15)
    bsdf = next(n for n in wm.node_tree.nodes if n.type == "BSDF_PRINCIPLED")
    bsdf.inputs["Alpha"].default_value = S.get("water_alpha", 0.75)
    w.data.materials.append(wm)

# world: soft sky gradient
world = bpy.data.worlds.new("sky")
sc.world = world
bg = next(n for n in world.node_tree.nodes if n.type == "BACKGROUND")
bg.inputs["Color"].default_value = (*srgb(S.get("sky", "#cfdcea")), 1.0)
bg.inputs["Strength"].default_value = S.get("sky_strength", 0.9)

sun = bpy.data.lights.new("sun", "SUN")
sun.energy = S.get("sun", {}).get("energy", 3.2)
sun.angle = math.radians(4)
sun.color = srgb("#fff3e2")
so = bpy.data.objects.new("sun", sun)
sc.collection.objects.link(so)
so.rotation_euler = [math.radians(v) for v in S.get("sun", {}).get("rot", [45, 0, 135])]

cam = bpy.data.cameras.new("cam")
c = S["camera"]
cam.lens = c.get("lens", 35)
if c.get("ortho"):
    cam.type = "ORTHO"
    cam.ortho_scale = c["ortho"]
if c.get("clip_start"):
    cam.clip_start = c["clip_start"]
cam.clip_end = c.get("clip_end", 5000)
co = bpy.data.objects.new("cam", cam)
sc.collection.objects.link(co)
co.location = c["loc"]
d = mathutils.Vector(c["target"]) - mathutils.Vector(c["loc"])
co.rotation_euler = d.to_track_quat("-Z", "Y").to_euler()
sc.camera = co

sc.render.engine = "CYCLES"
sc.cycles.device = "CPU"
sc.cycles.samples = S.get("samples", 96)
sc.cycles.use_denoising = True
sc.cycles.max_bounces = 4
sc.render.resolution_x, sc.render.resolution_y = S.get("size", [1600, 900])
sc.render.resolution_percentage = 100
sc.view_settings.view_transform = "AgX"
try:
    sc.view_settings.look = "AgX - Medium High Contrast"
except TypeError:
    pass
sc.render.film_transparent = False
sc.render.image_settings.file_format = "PNG"
sc.render.filepath = out_path
bpy.ops.render.render(write_still=True)
print("@@RENDERED", out_path)
