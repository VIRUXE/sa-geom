//! The collision world: models (local-space meshes with their own BVH), placed instances, a top-level BVH over
//! the instances, and water.

use crate::bvh::{self, Node};
use crate::math::{v3, Aabb, Affine, Vec3};
use bytemuck::{Pod, Zeroable};
use sa_formats::col::ColModel;
use std::collections::HashMap;

/// A collision model. Its triangles, spheres and BVH nodes are ranges of the world's flat arrays.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct ModelRec {
    /// Game model id.
    pub id: i32,
    pub tri_start: u32,
    pub tri_count: u32,
    pub node_start: u32,
    pub node_count: u32,
    pub sphere_start: u32,
    pub sphere_count: u32,
    pub _pad: u32,
    pub bounds: Aabb,
}

/// A triangle in model space with its surface material.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Tri {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
    pub material: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct SphereRec {
    pub center: Vec3,
    pub radius: f32,
    pub material: u32,
}

pub const INST_REMOVED: u32 = 1;
/// The instance came from the base game files (removals only apply to these, as in MTA/SA-MP).
pub const INST_BASE: u32 = 2;
/// A live object with a handle (see `live`), not part of the static map.
pub const INST_DYN: u32 = 4;
/// Hidden by a live world model removal (`World::remove_world_model`), undone by a restore.
pub const INST_LREMOVED: u32 = 8;
/// An unused slot (a removed live object), waiting to be reused.
pub const INST_FREE: u32 = 16;
/// An instance whose model has no collision (yet): it keeps its place until a collision for the model arrives.
pub const INST_NOCOL: u32 = 32;
/// The instance is listed in `World::pending`.
pub const INST_PENDING: u32 = 64;
/// The instance's current bounds are in the static top-level BVH (`tlas`).
pub const INST_TREE_STATIC: u32 = 128;
/// The instance's current bounds are in the live top-level BVH (`dyn_tlas`).
pub const INST_TREE_DYN: u32 = 256;
/// The instance's current bounds are in the recent top-level BVH (`recent_tlas`).
pub const INST_TREE_RECENT: u32 = 512;
/// Never hit by queries.
pub const INST_SKIP: u32 = INST_REMOVED | INST_LREMOVED | INST_FREE | INST_NOCOL;
const TREE_BITS: u32 = INST_TREE_STATIC | INST_TREE_DYN | INST_TREE_RECENT;
/// Changed instances are tested one by one until there are this many, then go into the recent BVH.
pub const PENDING_MAX: usize = 64;
/// The recent BVH holds this many instances at most, then everything goes into the live BVH.
pub const RECENT_MAX: usize = 4096;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct InstRec {
    /// Index into `World::models`.
    pub model: u32,
    pub interior: i32,
    pub flags: u32,
    /// Index into `World::sources`.
    pub source: u32,
    pub xf: Affine,
    pub inv: Affine,
    pub bounds: Aabb,
}

/// A water polygon (3 or 4 corners).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct WaterRec {
    pub pts: [Vec3; 4],
    pub n: u32,
}

#[derive(Default)]
pub struct World {
    pub models: Vec<ModelRec>,
    pub tris: Vec<Tri>,
    pub nodes: Vec<Node>,
    pub spheres: Vec<SphereRec>,
    pub instances: Vec<InstRec>,
    pub water: Vec<WaterRec>,
    /// Instance source files (IPL names, overlay files).
    pub sources: Vec<String>,
    /// Surface material names by id (from surfinfo.dat), may be empty.
    pub materials: Vec<String>,
    /// Model name by game id, for models with collision or a shape.
    pub names: HashMap<i32, String>,
    /// Game model id -> index into `models` (the latest definition wins).
    pub by_id: HashMap<i32, u32>,
    /// Top-level BVH over live instances (built by `finish`), and its instance order.
    pub tlas: Vec<Node>,
    pub tlas_order: Vec<u32>,
    /// Render-mesh hulls of models (see `shape`), their points, and game model id -> index into `shapes`.
    pub shapes: Vec<crate::shape::ShapeRec>,
    pub shape_pts: Vec<Vec3>,
    pub shape_by_id: HashMap<i32, u32>,
    /// Helicopter id -> index into `shapes` of its main rotor (kind KIND_ROTOR).
    pub rotor_by_id: HashMap<i32, u32>,
    /// Top-level BVH over instances changed after `finish` (live objects, re-pointed models), and its order.
    pub dyn_tlas: Vec<Node>,
    pub dyn_order: Vec<u32>,
    /// A small top-level BVH over instances changed since the live one was built, rebuilt often, and its order.
    pub recent_tlas: Vec<Node>,
    pub recent_order: Vec<u32>,
    /// Instances in no BVH (changed since the last rebuild), tested one by one.
    pub pending: Vec<u32>,
    /// Live edits: objects with handles, world model removals, replaced collisions (see `live`).
    pub live: crate::live::Live,
}

/// SA "visible in every interior" area code.
pub const ALL_INTERIORS: i32 = 13;

impl World {
    /// Add (or replace) the collision model for game id `id`. Boxes become triangles; spheres stay spheres.
    pub fn add_model(&mut self, id: i32, name: &str, col: &ColModel) -> Option<u32> {
        let mut tris: Vec<Tri> = col
            .faces
            .iter()
            .map(|f| Tri {
                a: Vec3::from_arr(col.verts[f.a as usize]),
                b: Vec3::from_arr(col.verts[f.b as usize]),
                c: Vec3::from_arr(col.verts[f.c as usize]),
                material: f.material as u32,
            })
            .collect();
        for b in &col.boxes {
            let (lo, hi) = (Vec3::from_arr(b.min), Vec3::from_arr(b.max));
            let p = |i: u32| v3(if i & 1 != 0 { hi.x } else { lo.x }, if i & 2 != 0 { hi.y } else { lo.y }, if i & 4 != 0 { hi.z } else { lo.z });
            const QUADS: [[u32; 4]; 6] = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
            for q in QUADS {
                tris.push(Tri { a: p(q[0]), b: p(q[1]), c: p(q[2]), material: b.material as u32 });
                tris.push(Tri { a: p(q[0]), b: p(q[2]), c: p(q[3]), material: b.material as u32 });
            }
        }
        let spheres: Vec<SphereRec> = col
            .spheres
            .iter()
            .filter(|s| s.radius > 0.0)
            .map(|s| SphereRec { center: Vec3::from_arr(s.center), radius: s.radius, material: s.material as u32 })
            .collect();
        if tris.is_empty() && spheres.is_empty() {
            return None;
        }
        let boxes: Vec<Aabb> = tris
            .iter()
            .map(|t| {
                let mut b = Aabb::EMPTY;
                b.grow(t.a);
                b.grow(t.b);
                b.grow(t.c);
                b
            })
            .collect();
        let (nodes, order) = bvh::build(&boxes);
        let mut bounds = Aabb::EMPTY;
        for b in &boxes {
            bounds = bounds.union(*b);
        }
        for s in &spheres {
            let r = v3(s.radius, s.radius, s.radius);
            bounds = bounds.union(Aabb { min: s.center - r, max: s.center + r });
        }
        let rec = ModelRec {
            id,
            tri_start: self.tris.len() as u32,
            tri_count: tris.len() as u32,
            node_start: self.nodes.len() as u32,
            node_count: if tris.is_empty() { 0 } else { nodes.len() as u32 },
            sphere_start: self.spheres.len() as u32,
            sphere_count: spheres.len() as u32,
            _pad: 0,
            bounds,
        };
        self.tris.extend(order.iter().map(|&i| tris[i as usize]));
        if !tris.is_empty() {
            self.nodes.extend(nodes);
        }
        self.spheres.extend(spheres);
        let idx = self.models.len() as u32;
        self.models.push(rec);
        let old = self.by_id.insert(id, idx);
        // a replacement collision (or the first one for live objects of that id): re-point the instances of that id
        self.repoint(id, old, Some(idx));
        self.names.insert(id, name.to_string());
        Some(idx)
    }

    pub fn source_index(&mut self, name: &str) -> u32 {
        if let Some(i) = self.sources.iter().position(|s| s == name) {
            return i as u32;
        }
        self.sources.push(name.to_string());
        (self.sources.len() - 1) as u32
    }

    /// Place model `id` with transform `xf`. Returns false when the model has no collision.
    pub fn add_instance(&mut self, id: i32, xf: Affine, interior: i32, source: u32, flags: u32) -> bool {
        let Some(&m) = self.by_id.get(&id) else { return false };
        let bounds = transform_bounds(&xf, self.models[m as usize].bounds);
        self.instances.push(InstRec { model: m, interior, flags, source, xf, inv: xf.inverse(), bounds });
        true
    }

    /// Flag base-game instances of `model` (-1 = any) whose position lies within `radius` of `pos`, as MTA's
    /// removeWorldModel and SA-MP's RemoveBuildingForPlayer do. Returns how many were removed.
    pub fn remove(&mut self, model: i32, pos: Vec3, radius: f32, interior: i32) -> usize {
        let mut n = 0;
        let r2 = radius * radius;
        for i in self.instances.iter_mut() {
            if i.flags & INST_BASE == 0 || i.flags & INST_REMOVED != 0 {
                continue;
            }
            if model != -1 && self.models[i.model as usize].id != model {
                continue;
            }
            if interior != -1 && i.interior != interior {
                continue;
            }
            let d = i.xf.t - pos;
            if d.dot(d) <= r2 {
                i.flags |= INST_REMOVED;
                n += 1;
            }
        }
        n
    }

    /// Point every instance of game id `id` (`old`: the model index the id had, if any) at model `new`, or leave
    /// it without collision when `new` is None.
    pub(crate) fn repoint(&mut self, id: i32, old: Option<u32>, new: Option<u32>) {
        // map instances by the id of the model they point at (kept while they have no collision); only when the id
        // had or has instances to find, so first definitions while an overlay loads cost nothing
        if old.is_some() || new.is_some() && self.models.iter().filter(|m| m.id == id).count() > 1 {
            for ii in 0..self.instances.len() {
                let i = &self.instances[ii];
                if i.flags & (INST_DYN | INST_FREE) == 0 && self.models[i.model as usize].id == id && (Some(i.model) != new || i.flags & INST_NOCOL != 0) {
                    self.set_model_index(ii as u32, new);
                }
            }
        }
        if self.live.slots.is_empty() {
            return;
        }
        let live: Vec<u32> = self.live.slots.values().filter(|s| s.row.model == id).map(|s| s.inst).collect();
        for ii in live {
            self.set_model_index(ii, new);
        }
    }

    /// Give instance `ii` model index `m` (None: no collision), updating its bounds and BVH membership.
    pub(crate) fn set_model_index(&mut self, ii: u32, m: Option<u32>) {
        let i = &mut self.instances[ii as usize];
        match m {
            Some(m) => {
                i.model = m;
                i.flags &= !INST_NOCOL;
                i.bounds = transform_bounds(&i.xf, self.models[m as usize].bounds);
            }
            None => i.flags |= INST_NOCOL,
        }
        self.mark_dirty(ii);
    }

    /// Take instance `ii` out of both BVHs (its bounds changed): it is tested on its own until the next rebuild.
    pub(crate) fn mark_dirty(&mut self, ii: u32) {
        let f = &mut self.instances[ii as usize].flags;
        *f &= !TREE_BITS;
        if *f & INST_PENDING == 0 {
            *f |= INST_PENDING;
            self.pending.push(ii);
        }
    }

    /// Fold pending instances into the recent BVH once there are too many, and everything into the live BVH once
    /// the recent one grows too big.
    pub(crate) fn settle(&mut self) {
        if self.pending.len() > PENDING_MAX {
            self.rebuild_recent();
        }
    }

    fn clear_pending(&mut self) {
        for &ii in &self.pending {
            self.instances[ii as usize].flags &= !INST_PENDING;
        }
        self.pending.clear();
    }

    /// Rebuild the recent BVH over its instances plus the pending ones (or the live BVH, when that makes it too big).
    pub fn rebuild_recent(&mut self) {
        const GONE: u32 = INST_FREE | INST_NOCOL | INST_REMOVED;
        let mut set: Vec<u32> = Vec::with_capacity(self.recent_order.len() + self.pending.len());
        for &ii in &self.recent_order {
            let f = self.instances[ii as usize].flags;
            if f & INST_TREE_RECENT != 0 && f & GONE == 0 {
                set.push(ii);
            }
        }
        for &ii in &self.pending {
            if self.instances[ii as usize].flags & (TREE_BITS | GONE) == 0 {
                set.push(ii);
            }
        }
        if set.len() > RECENT_MAX {
            self.rebuild_dyn();
            return;
        }
        self.clear_pending();
        for &ii in &self.recent_order {
            self.instances[ii as usize].flags &= !INST_TREE_RECENT;
        }
        for &ii in &set {
            self.instances[ii as usize].flags |= INST_TREE_RECENT;
        }
        let boxes: Vec<Aabb> = set.iter().map(|&i| self.instances[i as usize].bounds).collect();
        let (nodes, order) = bvh::build(&boxes);
        self.recent_order = order.iter().map(|&o| set[o as usize]).collect();
        self.recent_tlas = if set.is_empty() { Vec::new() } else { nodes };
    }

    /// Rebuild the live BVH over every instance that is not in the static one, emptying the recent BVH and `pending`.
    pub fn rebuild_dyn(&mut self) {
        self.clear_pending();
        self.live.dead_in_tree = 0;
        self.recent_tlas.clear();
        self.recent_order.clear();
        let mut set = Vec::new();
        for (ii, i) in self.instances.iter_mut().enumerate() {
            i.flags &= !(INST_TREE_DYN | INST_TREE_RECENT);
            if i.flags & (INST_TREE_STATIC | INST_FREE | INST_NOCOL | INST_REMOVED) == 0 {
                i.flags |= INST_TREE_DYN;
                set.push(ii as u32);
            }
        }
        if set.is_empty() {
            self.dyn_tlas.clear();
            self.dyn_order.clear();
            return;
        }
        let boxes: Vec<Aabb> = set.iter().map(|&i| self.instances[i as usize].bounds).collect();
        let (nodes, order) = bvh::build(&boxes);
        self.dyn_tlas = nodes;
        self.dyn_order = order.iter().map(|&o| set[o as usize]).collect();
    }

    /// (Re)build the static top-level BVH over the map's instances (and the live one over live objects). Call after
    /// adding or removing map instances.
    pub fn finish(&mut self) {
        let mut live = Vec::with_capacity(self.instances.len());
        for (ii, i) in self.instances.iter_mut().enumerate() {
            i.flags &= !TREE_BITS;
            if i.flags & (INST_REMOVED | INST_DYN | INST_FREE | INST_NOCOL) == 0 {
                i.flags |= INST_TREE_STATIC;
                live.push(ii as u32);
            }
        }
        let boxes: Vec<Aabb> = live.iter().map(|&i| self.instances[i as usize].bounds).collect();
        let (nodes, order) = bvh::build(&boxes);
        self.tlas = nodes;
        self.tlas_order = order.iter().map(|&o| live[o as usize]).collect();
        self.rebuild_dyn();
    }

    /// The top-level BVHs with their instance order and the flag an instance carries while it is current in it.
    #[inline]
    fn trees(&self) -> [(&[Node], &[u32], u32); 3] {
        [(&self.tlas, &self.tlas_order, INST_TREE_STATIC), (&self.dyn_tlas, &self.dyn_order, INST_TREE_DYN), (&self.recent_tlas, &self.recent_order, INST_TREE_RECENT)]
    }

    /// Every instance a query may hit whose bounds the segment `o + t*d` (t in [0, *tmax]) may cross; `f(ii, tmax)`
    /// may shrink `tmax`. Instances come from the static, live and recent BVHs and the pending list.
    #[inline]
    pub fn ray_candidates(&self, o: Vec3, d: Vec3, tmax: &mut f32, mut f: impl FnMut(u32, &mut f32)) {
        for (nodes, order, bit) in self.trees() {
            if order.is_empty() {
                continue;
            }
            bvh::traverse(nodes, o, d, tmax, |first, count, tmax| {
                for &ii in &order[first as usize..(first + count) as usize] {
                    if self.instances[ii as usize].flags & (bit | INST_SKIP) == bit {
                        f(ii, tmax);
                    }
                }
            });
        }
        if !self.pending.is_empty() {
            let inv = v3(1.0 / d.x, 1.0 / d.y, 1.0 / d.z);
            for &ii in &self.pending {
                let i = &self.instances[ii as usize];
                if i.flags & (TREE_BITS | INST_SKIP) == 0 && i.bounds.ray(o, inv, *tmax).is_some() {
                    f(ii, tmax);
                }
            }
        }
    }

    /// Every instance a query may hit whose bounds overlap `q`.
    pub fn box_candidates(&self, q: Aabb, mut f: impl FnMut(u32)) {
        for (nodes, order, bit) in self.trees() {
            if order.is_empty() {
                continue;
            }
            bvh::query_box(nodes, q, |first, count| {
                for &ii in &order[first as usize..(first + count) as usize] {
                    let i = &self.instances[ii as usize];
                    if i.flags & (bit | INST_SKIP) == bit && overlaps(&i.bounds, &q) {
                        f(ii);
                    }
                }
            });
        }
        for &ii in &self.pending {
            let i = &self.instances[ii as usize];
            if i.flags & (TREE_BITS | INST_SKIP) == 0 && overlaps(&i.bounds, &q) {
                f(ii);
            }
        }
    }

    pub fn material_name(&self, m: u32) -> &str {
        self.materials.get(m as usize).map(|s| s.as_str()).unwrap_or("")
    }

    /// Instances queries can hit.
    pub fn live_instances(&self) -> usize {
        self.instances.iter().filter(|i| i.flags & INST_SKIP == 0 && i.flags & (TREE_BITS | INST_PENDING) != 0).count()
    }
}

#[inline]
fn overlaps(b: &Aabb, q: &Aabb) -> bool {
    b.min.x <= q.max.x && b.max.x >= q.min.x && b.min.y <= q.max.y && b.max.y >= q.min.y && b.min.z <= q.max.z && b.max.z >= q.min.z
}

pub fn transform_bounds(xf: &Affine, b: Aabb) -> Aabb {
    let mut out = Aabb::EMPTY;
    for i in 0..8 {
        let p = v3(if i & 1 != 0 { b.max.x } else { b.min.x }, if i & 2 != 0 { b.max.y } else { b.min.y }, if i & 4 != 0 { b.max.z } else { b.min.z });
        out.grow(xf.point(p));
    }
    out
}
