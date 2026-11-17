//! Model shapes: a small hull of each model's render mesh (from its `.dff`), so things without collision (small
//! props, weapons, pickups) and vehicles (whose collision covers only the body, not the wheels) can be laid on the
//! ground by what they look like. Collision is still what rays hit; shapes are only for what is being placed.

use crate::bvh;
use crate::euler;
use crate::math::{v3, Aabb, Vec3};
use crate::query::{place, Filter, Hit, Placement};
use crate::world::World;
use bytemuck::{Pod, Zeroable};

/// What a shape belongs to.
pub const KIND_OBJECT: u32 = 0;
pub const KIND_VEHICLE: u32 = 1;
pub const KIND_WEAPON: u32 = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct ShapeRec {
    pub id: i32,
    pub kind: u32,
    /// Range of `World::shape_pts`.
    pub start: u32,
    pub count: u32,
}

/// The extreme points of `pts` along 26 directions (axes, edge and corner diagonals), without repeats. Any
/// rotation's lowest point and footprint come out within a few millimetres of the full mesh for game models.
pub fn hull(pts: &[Vec3]) -> Vec<Vec3> {
    let mut dirs = Vec::with_capacity(26);
    for x in -1i32..=1 {
        for y in -1i32..=1 {
            for z in -1i32..=1 {
                if x != 0 || y != 0 || z != 0 {
                    dirs.push(v3(x as f32, y as f32, z as f32));
                }
            }
        }
    }
    let mut out: Vec<Vec3> = Vec::new();
    for d in dirs {
        let Some(best) = pts.iter().copied().filter(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite()).max_by(|a, b| a.dot(d).total_cmp(&b.dot(d))) else { continue };
        if !out.iter().any(|q| (*q - best).len() < 1e-4) {
            out.push(best);
        }
    }
    out
}

/// How a model rests in a given pose: its base rectangle in the yaw frame and how far its origin sits above it.
#[derive(Debug, Clone, Copy)]
pub struct Rest {
    /// [min x, max x, min y, max y] of the model turned by its rest rotation (before yaw).
    pub rect: [f32; 4],
    /// Height of the origin above the lowest point.
    pub base: f32,
    /// Size of the rotated model (x, y, z).
    pub size: Vec3,
}

/// The rest of points `pts` turned by GTA Euler angles `rest` (degrees).
pub fn rest_of(pts: &[Vec3], rest: [f32; 3]) -> Option<Rest> {
    if pts.is_empty() {
        return None;
    }
    let r = euler::object_affine(Vec3::ZERO, rest);
    let mut b = Aabb::EMPTY;
    for p in pts {
        b.grow(r.vector(*p));
    }
    Some(Rest { rect: [b.min.x, b.max.x, b.min.y, b.max.y], base: -b.min.z, size: b.max - b.min })
}

/// A model laid on the ground in a rest pose.
#[derive(Debug, Clone, Copy)]
pub struct Laid {
    pub placement: Placement,
    pub hit: Hit,
    pub rest: Option<Rest>,
    /// How far the model's lowest point sits above the ground straight under its origin. Near 0 on even ground;
    /// large when part of the footprint rests on something higher (a wall top, a kerb, a step) and the model is
    /// propped up on it. Spawn tools should treat a large gap as "pick another spot".
    pub gap: f32,
    /// Where the shape came from: "mesh", "collision" or "point" (nothing known: origin on the ground).
    pub source: &'static str,
}

/// Shape kind by game model id: vehicles 400-611, weapons 321-372, everything else an object.
pub fn kind_of(id: i32) -> u32 {
    match id {
        400..=611 => KIND_VEHICLE,
        321..=372 => KIND_WEAPON,
        _ => KIND_OBJECT,
    }
}

/// Render-mesh points of a `.dff`, in model space: every atomic except low-detail (`_vlo`) and damaged (`_dam`)
/// variants, which sit where the normal parts are, a weapon's muzzle flash (`gunflash`, shown only when firing) and a
/// vehicle's optional extras (`extra1`..., shown at random).
pub fn dff_points(data: &[u8]) -> Vec<Vec3> {
    let Ok(c) = sa_formats::dff::parse(data) else { return Vec::new() };
    c.points(|n| !n.contains("_vlo") && !n.contains("_dam") && !n.starts_with("gunflash") && !n.starts_with("extra")).into_iter().map(Vec3::from_arr).collect()
}

impl World {
    /// Add the shape of model `id` from its `.dff` bytes. Returns false when the clump has no usable mesh.
    pub fn add_dff(&mut self, id: i32, data: &[u8]) -> bool {
        let pts = dff_points(data);
        let n = self.shapes.len();
        self.add_shape(id, kind_of(id), &pts);
        self.shapes.len() > n
    }

    pub fn add_shape(&mut self, id: i32, kind: u32, pts: &[Vec3]) {
        let h = hull(pts);
        if h.is_empty() {
            return;
        }
        let rec = ShapeRec { id, kind, start: self.shape_pts.len() as u32, count: h.len() as u32 };
        self.shape_pts.extend(h);
        let idx = self.shapes.len() as u32;
        self.shapes.push(rec);
        self.shape_by_id.insert(id, idx);
    }

    /// The hull points and kind of model `id`'s render mesh, if known.
    pub fn shape(&self, id: i32) -> Option<(&[Vec3], u32)> {
        let s = self.shapes[*self.shape_by_id.get(&id)? as usize];
        Some((&self.shape_pts[s.start as usize..(s.start + s.count) as usize], s.kind))
    }

    /// Points describing model `id`: its mesh hull, else its collision bounds' corners.
    fn shape_or_bounds(&self, id: i32) -> Option<(Vec<Vec3>, &'static str)> {
        if let Some((p, _)) = self.shape(id) {
            return Some((p.to_vec(), "mesh"));
        }
        let b = self.model_bounds(id)?;
        let c = |i: u32| v3(if i & 1 != 0 { b.max.x } else { b.min.x }, if i & 2 != 0 { b.max.y } else { b.min.y }, if i & 4 != 0 { b.max.z } else { b.min.z });
        Some(((0..8).map(c).collect(), "collision"))
    }

    /// How model `id` rests when turned by `rest` (GTA Euler degrees), from its mesh or collision.
    pub fn model_rest(&self, id: i32, rest: [f32; 3]) -> Option<(Rest, &'static str)> {
        let (pts, src) = self.shape_or_bounds(id)?;
        Some((rest_of(&pts, rest)?, src))
    }

    /// Lay model `id` on the ground under (x, y), turned by its rest rotation `rest` and then `yaw` about the
    /// vertical, tilted onto the ground under its base and lifted so its lowest point touches it. The returned
    /// rotation is the whole orientation (rest, yaw and tilt) as GTA Euler angles.
    pub fn lay_model(&self, id: i32, x: f32, y: f32, z_from: f32, depth: f32, yaw: f32, rest: [f32; 3], lift: f32, filter: Filter) -> Option<Laid> {
        let shaped = self.model_rest(id, rest);
        let (hit, base, r, source) = match shaped {
            Some((r, src)) => (self.rest_box(x, y, z_from, depth, yaw, r.rect, filter)?, r.base, Some(r), src),
            None => (self.ground(x, y, z_from, depth, filter)?, 0.0, None, "point"),
        };
        let mut p = place(hit.pos, hit.normal, yaw, lift + base);
        let under = self.ground(x, y, z_from, depth, filter).map(|g| g.pos.z).unwrap_or(hit.pos.z);
        let gap = (p.pos.z - p.up.z * (lift + base)) - under;
        let m = euler::object_affine(Vec3::ZERO, rest);
        // whole rotation = surface basis (right, forward, up) * rest rotation
        let col = |v: Vec3| p.right * v.x + p.forward * v.y + p.up * v.z;
        p.rot = euler::basis_euler(col(m.x), col(m.y), col(m.z));
        Some(Laid { placement: p, hit, rest: r, gap, source })
    }
}

/// Triangle vs box centred on the origin with half extents `h` (separating axis test, Akenine-Moller).
fn tri_box(v: [Vec3; 3], h: Vec3) -> bool {
    let [v0, v1, v2] = v;
    let e = [v1 - v0, v2 - v1, v0 - v2];
    let axes = [v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), v3(0.0, 0.0, 1.0)];
    let sep = |a: Vec3| -> bool {
        if a.dot(a) < 1e-12 {
            return false;
        }
        let (p0, p1, p2) = (v0.dot(a), v1.dot(a), v2.dot(a));
        let r = h.x * a.x.abs() + h.y * a.y.abs() + h.z * a.z.abs();
        p0.min(p1).min(p2) > r || p0.max(p1).max(p2) < -r
    };
    for ax in axes {
        for ed in e {
            if sep(ax.cross(ed)) {
                return false;
            }
        }
    }
    for ax in axes {
        if sep(ax) {
            return false;
        }
    }
    !sep(e[0].cross(e[1]))
}

/// What a laid model's body runs into.
#[derive(Debug, Clone, Copy)]
pub struct Blocked {
    /// Game model id of the first object found inside the body.
    pub model: i32,
    /// World point (a vertex or centre of what was hit) for reporting.
    pub at: Vec3,
}

impl World {
    /// Does model `id`, laid as `l` (from [`World::lay_model`]), run into any collision? The body tested is the
    /// model's box in its pose, shrunk by `margin` on every side and with its bottom `floor` metres up, so the ground
    /// it stands on, kerbs and bumps under it do not count while walls, poles, fences, trunks and roofs do.
    pub fn obstruction(&self, l: &Laid, margin: f32, floor: f32, filter: Filter) -> Option<Blocked> {
        let r = l.rest?;
        let p = &l.placement;
        let (lo, hi) = (v3(r.rect[0] + margin, r.rect[2] + margin, -r.base + floor), v3(r.rect[1] - margin, r.rect[3] - margin, r.size.z - r.base - margin));
        if lo.x >= hi.x || lo.y >= hi.y || lo.z >= hi.z {
            return None;
        }
        let c = (lo + hi) * 0.5;
        let h = (hi - lo) * 0.5;
        // box centre in the world, and world <-> box space
        let centre = p.pos + p.right * c.x + p.forward * c.y + p.up * c.z;
        let to_box = |w: Vec3| {
            let d = w - centre;
            v3(d.dot(p.right), d.dot(p.forward), d.dot(p.up))
        };
        let ext = p.right * h.x;
        let reach = v3(ext.x.abs(), ext.y.abs(), ext.z.abs()) + {
            let f = p.forward * h.y;
            v3(f.x.abs(), f.y.abs(), f.z.abs())
        } + {
            let u = p.up * h.z;
            v3(u.x.abs(), u.y.abs(), u.z.abs())
        };
        let world_box = Aabb { min: centre - reach, max: centre + reach };
        for ii in self.instances_in(world_box, filter) {
            let inst = &self.instances[ii as usize];
            let m = &self.models[inst.model as usize];
            // the world box in the model's own space, to walk its triangle BVH
            let mut q = Aabb::EMPTY;
            for k in 0..8u32 {
                let corner = v3(if k & 1 != 0 { world_box.max.x } else { world_box.min.x }, if k & 2 != 0 { world_box.max.y } else { world_box.min.y }, if k & 4 != 0 { world_box.max.z } else { world_box.min.z });
                q.grow(inst.inv.point(corner));
            }
            let mut found = None;
            if m.node_count > 0 {
                let nodes = &self.nodes[m.node_start as usize..(m.node_start + m.node_count) as usize];
                let tris = &self.tris[m.tri_start as usize..(m.tri_start + m.tri_count) as usize];
                bvh::query_box(nodes, q, |f, n| {
                    if found.is_some() {
                        return;
                    }
                    for t in &tris[f as usize..(f + n) as usize] {
                        let w = [inst.xf.point(t.a), inst.xf.point(t.b), inst.xf.point(t.c)];
                        if tri_box([to_box(w[0]), to_box(w[1]), to_box(w[2])], h) {
                            found = Some((w[0] + w[1] + w[2]) * (1.0 / 3.0));
                            return;
                        }
                    }
                });
            }
            if found.is_none() {
                for sp in &self.spheres[m.sphere_start as usize..(m.sphere_start + m.sphere_count) as usize] {
                    let b = to_box(inst.xf.point(sp.center));
                    let q = v3(b.x.clamp(-h.x, h.x), b.y.clamp(-h.y, h.y), b.z.clamp(-h.z, h.z));
                    // spheres scale with the instance like everything else; IPL placements are unscaled
                    if (b - q).len() < sp.radius {
                        found = Some(inst.xf.point(sp.center));
                        break;
                    }
                }
            }
            if let Some(at) = found {
                return Some(Blocked { model: m.id, at });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).len() < 1e-4
    }

    #[test]
    fn yaw_adds_to_the_euler_z() {
        // a rest rotation then a heading equals the Euler angles with the heading added to rz, which is how
        // scripts combine a model's rest rotation with a random heading
        for rest in [[90.0f32, 0.0, 0.0], [0.0, 90.0, 30.0], [45.0, 10.0, 0.0]] {
            for yaw in [0.0f32, 30.0, 135.0] {
                let a = euler::object_affine(Vec3::ZERO, [rest[0], rest[1], rest[2] + yaw]);
                let r = euler::object_affine(Vec3::ZERO, rest);
                let p = place(Vec3::ZERO, v3(0.0, 0.0, 1.0), yaw, 0.0);
                let col = |v: Vec3| p.right * v.x + p.forward * v.y + p.up * v.z;
                assert!(close(a.x, col(r.x)) && close(a.y, col(r.y)) && close(a.z, col(r.z)), "{rest:?} {yaw}");
            }
        }
    }

    #[test]
    fn triangle_box_overlap() {
        let h = v3(1.0, 1.0, 1.0);
        assert!(tri_box([v3(-5.0, 0.0, 0.0), v3(5.0, 0.5, 0.0), v3(0.0, -5.0, 0.0)], h)); // slices through
        assert!(!tri_box([v3(2.0, 2.0, 2.0), v3(3.0, 2.0, 2.0), v3(2.0, 3.0, 2.0)], h)); // off to a corner
        assert!(!tri_box([v3(-5.0, -5.0, 1.5), v3(5.0, -5.0, 1.5), v3(0.0, 5.0, 1.5)], h)); // a roof above
        assert!(tri_box([v3(-0.1, -0.1, 0.0), v3(0.1, -0.1, 0.0), v3(0.0, 0.1, 0.0)], h)); // inside
    }

    #[test]
    fn hull_keeps_the_extremes() {
        let pts = [v3(0.0, 0.0, 1.0), v3(1.0, 0.0, 0.0), v3(0.0, 2.0, 0.0), v3(0.0, 0.0, -3.0), v3(0.1, 0.1, 0.1)];
        let h = hull(&pts);
        assert!(h.iter().any(|p| close(*p, v3(0.0, 0.0, -3.0))));
        assert!(!h.iter().any(|p| close(*p, v3(0.1, 0.1, 0.1))));
        // upright the base is the bottom point, 3 m below the origin
        assert!((rest_of(&h, [0.0, 0.0, 0.0]).unwrap().base - 3.0).abs() < 1e-4);
        // rx = 90 lays it on its side: the 2 m forward extent (+Y) now spans the height
        let r = rest_of(&h, [90.0, 0.0, 0.0]).unwrap();
        assert!((r.size.z - 2.0).abs() < 1e-4, "{r:?}");
    }
}
