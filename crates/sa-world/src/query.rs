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
    pub fn accepts(&self, interior: i32) -> bool {
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
        self.ray_candidates(from, d, &mut tmax, |ii, tmax| {
            {
                let inst = &self.instances[ii as usize];
                if !filter.accepts(inst.interior) {
                    return;
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

    /// Ground under a `w` x `d` footprint centred on (x, y) and turned `yaw` degrees. See [`World::rest_box`].
    pub fn footprint(&self, x: f32, y: f32, z_from: f32, depth: f32, yaw: f32, w: f32, d: f32, filter: Filter) -> Option<Hit> {
        self.rest_box(x, y, z_from, depth, yaw, [-w * 0.5, w * 0.5, -d * 0.5, d * 0.5], filter)
    }

    /// Where a flat-bottomed object with origin (x, y) rests: `rect` = [min x, max x, min y, max y] of its base in
    /// its own space, turned `yaw` degrees. Rays go down at the origin and at the 4 corners. With every corner on
    /// the ground the result lies on the plane through the corners at the origin, raised along that plane's normal
    /// just enough that no corner (nor the ground under the origin) pokes through the base; otherwise it is the
    /// ground under the origin with its own normal.
    pub fn rest_box(&self, x: f32, y: f32, z_from: f32, depth: f32, yaw: f32, rect: [f32; 4], filter: Filter) -> Option<Hit> {
        let centre = self.ground(x, y, z_from, depth, filter)?;
        let (s, c) = yaw.to_radians().sin_cos();
        // local x (right) -> (c, s), local y (forward) -> (-s, c)
        let at = |lx: f32, ly: f32| v3(x + lx * c - ly * s, y + lx * s + ly * c, 0.0);
        let [x0, x1, y0, y1] = rect;
        let corners = [at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)];
        let hits: Vec<Option<Hit>> = corners.iter().map(|p| self.ground(p.x, p.y, z_from, depth, filter)).collect();
        let [Some(a), Some(b), Some(c2), Some(e)] = [hits[0], hits[1], hits[2], hits[3]] else { return Some(centre) };
        let mut n = (c2.pos - a.pos).cross(e.pos - b.pos).normalize();
        if n.z < 0.0 {
            n = -n;
        }
        if n.z < 0.05 {
            return Some(centre);
        }
        let mid = (a.pos + b.pos + c2.pos + e.pos) * 0.25;
        let plane_z = mid.z - (n.x * (x - mid.x) + n.y * (y - mid.y)) / n.z;
        let base = v3(x, y, plane_z);
        let lift = [a.pos, b.pos, c2.pos, e.pos, centre.pos].iter().map(|p| (*p - base).dot(n)).fold(0.0f32, f32::max);
        let mut out = centre;
        out.pos = base + n * lift;
        out.normal = n;
        Some(out)
    }

    /// Bounds of a model's collision in its own space, if the world has collision for it.
    pub fn model_bounds(&self, id: i32) -> Option<Aabb> {
        self.by_id.get(&id).map(|&m| self.models[m as usize].bounds)
    }

    /// Lay model `id` on the ground under (x, y), upright and turned `yaw`: its shape (render mesh, else
    /// collision bounds) gives the footprint and how far its origin sits above its base. Falls back to a single
    /// point (no base offset) for a model the world knows nothing about. See [`World::lay_model`] for rest poses.
    pub fn place_model(&self, id: i32, x: f32, y: f32, z_from: f32, depth: f32, yaw: f32, lift: f32, filter: Filter) -> Option<(Placement, Hit)> {
        self.lay_model(id, x, y, z_from, depth, yaw, [0.0; 3], lift, filter).map(|l| (l.placement, l.hit))
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

    /// Live instances whose bounds overlap the box `q`.
    pub fn instances_in(&self, q: Aabb, filter: Filter) -> Vec<u32> {
        let mut out = Vec::new();
        self.box_candidates(q, |ii| {
            if filter.accepts(self.instances[ii as usize].interior) {
                out.push(ii);
            }
        });
        out
    }

    /// World-space triangles (a, b, c, material, model) of every live instance whose triangle centroid lies in `q`.
    /// Spheres are tessellated coarsely.
    pub fn triangles_in(&self, q: Aabb, filter: Filter) -> Vec<([Vec3; 3], u32, i32)> {
        let inside = |p: Vec3| p.x >= q.min.x && p.x <= q.max.x && p.y >= q.min.y && p.y <= q.max.y && p.z >= q.min.z && p.z <= q.max.z;
        let mut out = Vec::new();
        for ii in self.instances_in(q, filter) {
            let inst = &self.instances[ii as usize];
            let m = &self.models[inst.model as usize];
            for t in &self.tris[m.tri_start as usize..(m.tri_start + m.tri_count) as usize] {
                let w = [inst.xf.point(t.a), inst.xf.point(t.b), inst.xf.point(t.c)];
                if inside((w[0] + w[1] + w[2]) * (1.0 / 3.0)) {
                    out.push((w, t.material, m.id));
                }
            }
            for s in &self.spheres[m.sphere_start as usize..(m.sphere_start + m.sphere_count) as usize] {
                // octahedron subdivided once
                let c = s.center;
                let r = s.radius;
                let mut faces: Vec<[Vec3; 3]> = Vec::new();
                let px = [v3(1.0, 0.0, 0.0), v3(-1.0, 0.0, 0.0)];
                let py = [v3(0.0, 1.0, 0.0), v3(0.0, -1.0, 0.0)];
                let pz = [v3(0.0, 0.0, 1.0), v3(0.0, 0.0, -1.0)];
                for a in px {
                    for b in py {
                        for cz in pz {
                            let ab = (a + b).normalize();
                            let bc = (b + cz).normalize();
                            let ca = (cz + a).normalize();
                            faces.extend([[a, ab, ca], [ab, b, bc], [ca, bc, cz], [ab, bc, ca]]);
                        }
                    }
                }
                for f in faces {
                    let w = f.map(|p| inst.xf.point(c + p * r));
                    if inside((w[0] + w[1] + w[2]) * (1.0 / 3.0)) {
                        out.push((w, s.material, m.id));
                    }
                }
            }
        }
        out
    }

    /// Instances whose bounds overlap a sphere around `p` (live ones only).
    pub fn instances_near(&self, p: Vec3, radius: f32) -> Vec<u32> {
        let r = v3(radius, radius, radius);
        let q = Aabb { min: p - r, max: p + r };
        let mut out = Vec::new();
        self.box_candidates(q, |ii| {
            if self.instances[ii as usize].bounds.contains_sphere(p, radius) {
                out.push(ii);
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
