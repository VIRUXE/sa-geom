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
    /// Model name by game id, for models with collision.
    pub names: HashMap<i32, String>,
    /// Game model id -> index into `models` (the latest definition wins).
    pub by_id: HashMap<i32, u32>,
    /// Top-level BVH over live instances (built by `finish`), and its instance order.
    pub tlas: Vec<Node>,
    pub tlas_order: Vec<u32>,
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
        if let Some(old) = self.by_id.insert(id, idx) {
            // a replacement collision: re-point the instances of that id
            for i in self.instances.iter_mut().filter(|i| i.model == old) {
                i.model = idx;
                i.bounds = transform_bounds(&i.xf, bounds);
            }
        }
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

    /// (Re)build the top-level BVH over the live instances. Call after adding or removing instances.
    pub fn finish(&mut self) {
        let live: Vec<u32> = (0..self.instances.len() as u32).filter(|&i| self.instances[i as usize].flags & INST_REMOVED == 0).collect();
        let boxes: Vec<Aabb> = live.iter().map(|&i| self.instances[i as usize].bounds).collect();
        let (nodes, order) = bvh::build(&boxes);
        self.tlas = nodes;
        self.tlas_order = order.iter().map(|&o| live[o as usize]).collect();
    }

    pub fn material_name(&self, m: u32) -> &str {
        self.materials.get(m as usize).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn live_instances(&self) -> usize {
        self.tlas_order.len()
    }
}

pub fn transform_bounds(xf: &Affine, b: Aabb) -> Aabb {
    let mut out = Aabb::EMPTY;
    for i in 0..8 {
        let p = v3(if i & 1 != 0 { b.max.x } else { b.min.x }, if i & 2 != 0 { b.max.y } else { b.min.y }, if i & 4 != 0 { b.max.z } else { b.min.z });
        out.grow(xf.point(p));
    }
    out
}
