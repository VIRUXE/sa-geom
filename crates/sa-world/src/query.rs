//! Queries: raycast, ground height, footprint fit and object placement on a surface.

use crate::bvh;
use crate::euler;
use crate::math::{v3, Aabb, Vec3};
use crate::world::{World, ALL_INTERIORS};

/// Which interiors a query sees: `Some(i)` sees interior `i` plus area 13 (all interiors); `None` sees everything.
#[derive(Debug, Clone, Copy)]
pub struct Filter {
    pub interior: Option<i32>,
}

impl Default for Filter {
    fn default() -> Self {
        Filter { interior: Some(0) }
    }
}

impl Filter {
    pub fn interior(i: i32) -> Filter {
        Filter { interior: if i < 0 { None } else { Some(i) } }
    }
    fn accepts(&self, interior: i32) -> bool {
        match self.interior {
            None => true,
            Some(q) => interior == q || interior == ALL_INTERIORS,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub pos: Vec3,
    /// Unit surface normal in world space, facing against the ray.
    pub normal: Vec3,
    pub material: u32,
    /// Game model id of the hit object.
    pub model: i32,
    /// Index into `World::instances`.
    pub instance: u32,
    /// Fraction along the segment (0 = start, 1 = end).
    pub t: f32,
}

#[inline]
fn ray_tri(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3, tmax: f32) -> Option<(f32, Vec3)> {
    // Moller-Trumbore, double sided
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < -1e-6 || u + v > 1.0 + 1e-6 {
        return None;
    }
    let t = e2.dot(q) * inv;
    if t < 0.0 || t > tmax {
        return None;
    }
    Some((t, e1.cross(e2)))
}

#[inline]
fn ray_sphere(o: Vec3, d: Vec3, c: Vec3, r: f32, tmax: f32) -> Option<f32> {
    let m = o - c;
    let a = d.dot(d);
    let b = m.dot(d);
    let cc = m.dot(m) - r * r;
    if cc > 0.0 && b > 0.0 {
        return None;
    }
    let disc = b * b - a * cc;
    if disc < 0.0 {
        return None;
    }
    let t = ((-b - disc.sqrt()) / a).max(0.0);
    (t <= tmax).then_some(t)
}

impl World {
    /// Nearest surface on the segment `from` -> `to`.
    pub fn raycast(&self, from: Vec3, to: Vec3, filter: Filter) -> Option<Hit> {
        let d = to - from;
        let mut best: Option<(f32, u32, Vec3, u32)> = None; // t, instance, local normal, material
        let mut tmax = 1.0f32;
        bvh::traverse(&self.tlas, from, d, &mut tmax, |first, count, tmax| {
            for k in first..first + count {
                let ii = self.tlas_order[k as usize];
                let inst = &self.instances[ii as usize];
                if !filter.accepts(inst.interior) {
                    continue;
                }
                let m = &self.models[inst.model as usize];
                // affine maps preserve the segment parameter t
                let lo = inst.inv.point(from);
                let ld = inst.inv.vector(d);
                if m.node_count > 0 {
                    let nodes = &self.nodes[m.node_start as usize..(m.node_start + m.node_count) as usize];
                    let tris = &self.tris[m.tri_start as usize..(m.tri_start + m.tri_count) as usize];
                    bvh::traverse(nodes, lo, ld, tmax, |f, c, tmax| {
                        for t in &tris[f as usize..(f + c) as usize] {
                            if let Some((th, n)) = ray_tri(lo, ld, t.a, t.b, t.c, *tmax) {
                                *tmax = th;
                                best = Some((th, ii, n, t.material));
                            }
                        }
                    });
                }
                for s in &self.spheres[m.sphere_start as usize..(m.sphere_start + m.sphere_count) as usize] {
                    if let Some(th) = ray_sphere(lo, ld, s.center, s.radius, *tmax) {
                        *tmax = th;
                        best = Some((th, ii, (lo + ld * th) - s.center, s.material));
                    }
                }
            }
        });
        let (t, ii, ln, material) = best?;
        let inst = &self.instances[ii as usize];
        let mut n = inst.xf.normal(&inst.inv, ln);
        if n.dot(d) > 0.0 {
            n = -n;
        }
        Some(Hit { pos: from + d * t, normal: n, material, model: self.models[inst.model as usize].id, instance: ii, t })
    }

    /// The first surface straight down from (x, y, z_from), searching `depth` metres.
    pub fn ground(&self, x: f32, y: f32, z_from: f32, depth: f32, filter: Filter) -> Option<Hit> {
        self.raycast(v3(x, y, z_from), v3(x, y, z_from - depth), filter)
    }

    /// Ground under a `w` x `d` footprint turned `yaw` degrees: rays at the centre and the 4 corners. The result's
    /// z is the highest hit (so the footprint does not sink in) and its normal is the plane through the corner
    /// hits (the centre's normal when a corner misses). Position x/y stay at the centre.
    pub fn footprint(&self, x: f32, y: f32, z_from: f32, depth: f32, yaw: f32, w: f32, d: f32, filter: Filter) -> Option<Hit> {
        let centre = self.ground(x, y, z_from, depth, filter)?;
        let (s, c) = yaw.to_radians().sin_cos();
        let right = v3(c, s, 0.0) * (w * 0.5);
        let fwd = v3(-s, c, 0.0) * (d * 0.5);
        let corners = [right + fwd, -right + fwd, -right - fwd, right - fwd];
        let hits: Vec<Option<Hit>> = corners.iter().map(|o| self.ground(x + o.x, y + o.y, z_from, depth, filter)).collect();
        let mut out = centre;
        for h in hits.iter().flatten() {
            out.pos.z = out.pos.z.max(h.pos.z);
        }
        if let [Some(a), Some(b), Some(c2), Some(e)] = [hits[0], hits[1], hits[2], hits[3]] {
            let n = (c2.pos - a.pos).cross(e.pos - b.pos).normalize();
            let n = if n.z < 0.0 { -n } else { n };
            if n.len() > 0.5 {
                out.normal = n;
            }
        }
        Some(out)
    }

    /// Water surface height at (x, y), if a water polygon covers it.
    pub fn water_z(&self, x: f32, y: f32) -> Option<f32> {
        let p = v3(x, y, 0.0);
        for w in &self.water {
            let n = w.n as usize;
            let tri_hit = |a: Vec3, b: Vec3, c: Vec3| -> Option<f32> {
                let d = |p1: Vec3, p2: Vec3, p3: Vec3| (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y);
                let (d1, d2, d3) = (d(p, a, b), d(p, b, c), d(p, c, a));
                let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
                let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
                if neg && pos {
                    return None;
                }
                let area = d(a, b, c);
                if area.abs() < 1e-9 {
                    return None;
                }
                let (u, v) = (d(p, b, c) / area, d(p, c, a) / area);
                Some(a.z * u + b.z * v + c.z * (1.0 - u - v))
            };
            let pts = &w.pts;
            // SA water quads are stored as corners 0,1,2,3 = (x0,y0),(x1,y0),(x0,y1),(x1,y1)
            let z = if n == 4 { tri_hit(pts[0], pts[1], pts[3]).or_else(|| tri_hit(pts[0], pts[3], pts[2])) } else { tri_hit(pts[0], pts[1], pts[2]) };
            if z.is_some() {
                return z;
            }
        }
        None
    }

    /// Instances whose bounds overlap a sphere around `p` (live ones only).
    pub fn instances_near(&self, p: Vec3, radius: f32) -> Vec<u32> {
        let r = v3(radius, radius, radius);
        let q = Aabb { min: p - r, max: p + r };
        let mut out = Vec::new();
        bvh::query_box(&self.tlas, q, |f, c| {
            for k in f..f + c {
                let ii = self.tlas_order[k as usize];
                if self.instances[ii as usize].bounds.contains_sphere(p, radius) {
                    out.push(ii);
                }
            }
        });
        out
    }
}

/// An object lying on a surface.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub pos: Vec3,
    /// Columns of the rotation: right, forward, up (= the surface normal).
    pub right: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
    /// GTA object Euler angles in degrees, for SA-MP/open.mp `CreateObject`/`SetObjectRot` and MTA
    /// `createObject`/`setElementRotation` (default rotation order).
    pub rot: [f32; 3],
}

/// Lay an object on `normal` at `pos`, turned `yaw` degrees (GTA heading: 0 faces +Y, 90 faces -X) about the
/// normal and lifted `lift` metres along it. Forward is the yaw direction tilted vertically onto the surface, so
/// the heading seen from above stays `yaw`.
pub fn place(pos: Vec3, normal: Vec3, yaw: f32, lift: f32) -> Placement {
    let up = if normal.len() < 1e-6 { v3(0.0, 0.0, 1.0) } else { normal.normalize() };
    let (s, c) = yaw.to_radians().sin_cos();
    let flat = v3(-s, c, 0.0);
    // tilt the heading vertically onto the surface, so the object still faces `yaw` seen from above
    let mut fwd = if up.z.abs() > 1e-4 { v3(flat.x, flat.y, -(flat.x * up.x + flat.y * up.y) / up.z) } else { flat - up * flat.dot(up) };
    if fwd.len() < 1e-6 {
        fwd = v3(1.0, 0.0, 0.0) - up * up.x;
    }
    let forward = fwd.normalize();
    let right = forward.cross(up).normalize();
    let rot = euler::basis_euler(right, forward, up);
    Placement { pos: pos + up * lift, right, forward, up, rot }
}
