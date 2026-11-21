//! Live edits of a loaded world: objects with handles (added, moved, re-modelled, removed), world model removals
//! and restores, and replaced collisions, applied at once and kept as state so they can be replayed onto a freshly
//! built world (see `shared::reload`).
//!
//! Live objects go in `World::instances` like map instances, flagged `INST_DYN`. A changed instance leaves the
//! top-level BVHs and is tested on its own (`World::pending`) until the live BVH is rebuilt over every instance that
//! is not in the static one, so an edit costs microseconds and the static map's BVH is never rebuilt.

use crate::euler;
use crate::math::{Aabb, Vec3};
use crate::world::{transform_bounds, InstRec, World, INST_BASE, INST_DYN, INST_FREE, INST_LREMOVED, INST_NOCOL, INST_PENDING, INST_SKIP, INST_TREE_DYN, PENDING_MAX};
use std::collections::HashMap;
use std::sync::Arc;

/// Who made an edit (a server's script VM, for instance), so its edits can be dropped together. 0 = nobody.
pub type Owner = usize;

/// A live object as given: game model id, position, MTA object rotation in degrees (as in an MTA `.map`), interior
/// and scale (MTA's setObjectScale, per local axis).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectRow {
    pub model: i32,
    pub pos: [f32; 3],
    pub rot: [f32; 3],
    pub interior: i32,
    pub scale: [f32; 3],
}

impl ObjectRow {
    /// An unscaled object.
    pub fn new(model: i32, pos: [f32; 3], rot: [f32; 3], interior: i32) -> ObjectRow {
        ObjectRow { model, pos, rot, interior, scale: [1.0; 3] }
    }
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub row: ObjectRow,
    /// Index into `World::instances`.
    pub inst: u32,
    pub owner: Owner,
}

/// A world model removal, as MTA's removeWorldModel: base-game instances of `model` (-1 = any) whose position lies
/// within `radius` of `pos`, in `interior` (-1 = any).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Removal {
    pub model: i32,
    pub pos: [f32; 3],
    pub radius: f32,
    pub interior: i32,
    pub owner: Owner,
}

#[derive(Debug, Clone)]
pub struct ColOverride {
    /// The `.col` file as given.
    pub data: Arc<Vec<u8>>,
    pub owner: Owner,
    /// The model index the id had before it was first replaced (None: it had no collision).
    pub original: Option<u32>,
}

#[derive(Debug, Default, Clone)]
pub struct Live {
    /// Live objects by handle.
    pub slots: HashMap<u32, Slot>,
    /// The next handle to give out (handles start at 1 and are never reused).
    pub next_handle: u32,
    pub removals: Vec<Removal>,
    pub cols: HashMap<i32, ColOverride>,
    /// Instance indices of removed live objects, for reuse.
    pub free: Vec<u32>,
    /// Removed objects still listed in the live BVH (skipped by queries until it is rebuilt).
    pub dead_in_tree: usize,
}

/// One live edit, as recorded to replay onto another world.
#[derive(Debug, Clone)]
pub enum Op {
    Add { owner: Owner, rows: Vec<(u32, ObjectRow)> },
    Move { handle: u32, pos: [f32; 3], rot: [f32; 3] },
    Scale { handle: u32, scale: [f32; 3] },
    SetModel { handle: u32, model: i32 },
    Remove { handle: u32 },
    RemoveWorld(Removal),
    RestoreWorld { model: i32, radius: f32, pos: [f32; 3], interior: i32 },
    ReplaceCol { id: i32, data: Arc<Vec<u8>>, owner: Owner },
    RestoreCol { id: i32 },
    DropOwner(Owner),
    NextHandle(u32),
}

/// Counts for logs and stats.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveStats {
    pub objects: usize,
    pub objects_without_collision: usize,
    pub removals: usize,
    pub removed_instances: usize,
    pub cols: usize,
    pub pending: usize,
    pub dyn_tree: usize,
}

fn affine(row: &ObjectRow) -> crate::math::Affine {
    let xf = euler::object_affine(Vec3::from_arr(row.pos), row.rot);
    if row.scale == [1.0; 3] {
        xf
    } else {
        xf.scaled(Vec3::from_arr(row.scale))
    }
}

fn removal_hits(r: &Removal, model_id: i32, i: &InstRec) -> bool {
    if r.model != -1 && r.model != model_id {
        return false;
    }
    if r.interior != -1 && r.interior != i.interior {
        return false;
    }
    let d = i.xf.t - Vec3::from_arr(r.pos);
    d.dot(d) <= r.radius * r.radius
}

impl World {
    /// Whether game id `id` has collision in this world.
    pub fn has_collision(&self, id: i32) -> bool {
        self.by_id.contains_key(&id)
    }

    fn alloc_handle(&mut self) -> u32 {
        self.live.next_handle = self.live.next_handle.max(1);
        let h = self.live.next_handle;
        self.live.next_handle += 1;
        h
    }

    /// Place a live object under `handle`, with or without collision for its model.
    fn insert_object(&mut self, handle: u32, row: ObjectRow, owner: Owner) {
        if self.live.slots.contains_key(&handle) {
            self.remove_object(handle);
        }
        self.live.next_handle = self.live.next_handle.max(handle + 1);
        let xf = affine(&row);
        let m = self.by_id.get(&row.model).copied();
        let source = self.source_index("live");
        let rec = InstRec {
            model: m.unwrap_or(0),
            interior: row.interior,
            flags: INST_DYN | if m.is_none() { INST_NOCOL } else { 0 },
            source,
            xf,
            inv: xf.inverse(),
            bounds: m.map(|m| transform_bounds(&xf, self.models[m as usize].bounds)).unwrap_or(Aabb::EMPTY),
        };
        let ii = match self.live.free.pop() {
            Some(ii) => {
                // keep the pending mark: the index may still be listed in `pending`
                let keep = self.instances[ii as usize].flags & INST_PENDING;
                self.instances[ii as usize] = InstRec { flags: rec.flags | keep, ..rec };
                ii
            }
            None => {
                self.instances.push(rec);
                (self.instances.len() - 1) as u32
            }
        };
        self.mark_dirty(ii);
        self.live.slots.insert(handle, Slot { row, inst: ii, owner });
    }

    /// Add live objects in one go. Rows whose model has no collision are skipped (None). Returns each row's handle
    /// and the op that replays the batch.
    pub fn add_objects(&mut self, rows: &[ObjectRow], owner: Owner) -> (Vec<Option<u32>>, Op) {
        let mut out = Vec::with_capacity(rows.len());
        let mut done = Vec::with_capacity(rows.len());
        for row in rows {
            if !self.has_collision(row.model) {
                out.push(None);
                continue;
            }
            let h = self.alloc_handle();
            self.insert_object(h, *row, owner);
            done.push((h, *row));
            out.push(Some(h));
        }
        if self.pending.len() > PENDING_MAX {
            self.rebuild_dyn();
        }
        (out, Op::Add { owner, rows: done })
    }

    /// Add one live object; None when its model has no collision.
    pub fn add_object(&mut self, row: ObjectRow, owner: Owner) -> (Option<u32>, Op) {
        let (h, op) = self.add_objects(&[row], owner);
        (h[0], op)
    }

    pub fn move_object(&mut self, handle: u32, pos: [f32; 3], rot: [f32; 3]) -> bool {
        self.change_object(handle, |r| {
            r.pos = pos;
            r.rot = rot;
        })
    }

    /// Scale a live object along its own axes (1 = as modelled).
    pub fn scale_object(&mut self, handle: u32, scale: [f32; 3]) -> bool {
        self.change_object(handle, |r| r.scale = scale)
    }

    fn change_object(&mut self, handle: u32, f: impl FnOnce(&mut ObjectRow)) -> bool {
        let Some(slot) = self.live.slots.get_mut(&handle) else { return false };
        f(&mut slot.row);
        let (ii, row) = (slot.inst, slot.row);
        let xf = affine(&row);
        let i = &mut self.instances[ii as usize];
        i.xf = xf;
        i.inv = xf.inverse();
        if i.flags & INST_NOCOL == 0 {
            i.bounds = transform_bounds(&xf, self.models[i.model as usize].bounds);
        }
        self.mark_dirty(ii);
        self.settle();
        true
    }

    /// Give a live object another model. None: no such handle; Some(false): the model has no collision (the object
    /// keeps its handle and gets collision once one is loaded for that id).
    pub fn set_object_model(&mut self, handle: u32, model: i32) -> Option<bool> {
        let slot = self.live.slots.get_mut(&handle)?;
        slot.row.model = model;
        let ii = slot.inst;
        let m = self.by_id.get(&model).copied();
        self.set_model_index(ii, m);
        self.settle();
        Some(m.is_some())
    }

    pub fn remove_object(&mut self, handle: u32) -> bool {
        let Some(slot) = self.live.slots.remove(&handle) else { return false };
        let i = &mut self.instances[slot.inst as usize];
        if i.flags & INST_TREE_DYN != 0 {
            self.live.dead_in_tree += 1;
        }
        i.flags = INST_FREE | (i.flags & INST_PENDING);
        self.live.free.push(slot.inst);
        // a live BVH mostly made of removed objects slows every query down
        if self.live.dead_in_tree > 1024 && self.live.dead_in_tree * 2 > self.dyn_order.len() {
            self.rebuild_dyn();
        }
        true
    }

    /// Hide base-game instances matching `r` (until a restore). Returns how many were hidden now.
    pub fn remove_world_model(&mut self, r: Removal) -> usize {
        let mut n = 0;
        for ii in 0..self.instances.len() {
            let i = &self.instances[ii];
            if i.flags & INST_BASE == 0 || i.flags & INST_LREMOVED != 0 {
                continue;
            }
            if removal_hits(&r, self.models[i.model as usize].id, i) {
                self.instances[ii].flags |= INST_LREMOVED;
                n += 1;
            }
        }
        self.live.removals.push(r);
        n
    }

    /// Undo the removals of `model` (-1 = any) whose centre lies within `radius` of `pos` and whose interior is
    /// `interior` (-1 = any), as MTA's restoreWorldModel. Returns how many instances came back.
    pub fn restore_world_model(&mut self, model: i32, radius: f32, pos: [f32; 3], interior: i32) -> usize {
        let p = Vec3::from_arr(pos);
        let (gone, kept): (Vec<Removal>, Vec<Removal>) = self.live.removals.iter().partition(|r| {
            let d = Vec3::from_arr(r.pos) - p;
            (model == -1 || r.model == model) && (interior == -1 || r.interior == interior) && d.dot(d) <= radius * radius
        });
        self.live.removals = kept;
        if gone.is_empty() {
            return 0;
        }
        self.recheck_removed(Some(&gone))
    }

    /// Show again every hidden instance no remaining removal covers (only those `gone` covered, when given).
    fn recheck_removed(&mut self, gone: Option<&[Removal]>) -> usize {
        let mut n = 0;
        for ii in 0..self.instances.len() {
            let i = &self.instances[ii];
            if i.flags & INST_LREMOVED == 0 {
                continue;
            }
            let id = self.models[i.model as usize].id;
            if gone.is_some_and(|g| !g.iter().any(|r| removal_hits(r, id, i))) {
                continue;
            }
            if !self.live.removals.iter().any(|r| removal_hits(r, id, i)) {
                self.instances[ii].flags &= !INST_LREMOVED;
                n += 1;
            }
        }
        n
    }

    /// Use the first model of the `.col` file `data` as the collision of game id `id`, for every instance of it now
    /// and later.
    pub fn replace_col(&mut self, id: i32, data: Arc<Vec<u8>>, owner: Owner) -> Result<(), String> {
        let models = sa_formats::col::parse_archive(&data).map_err(|e| e.to_string())?;
        let col = models.into_iter().next().ok_or("no collision model in the data")?;
        let original = match self.live.cols.get(&id) {
            Some(o) => o.original,
            None => self.by_id.get(&id).copied(),
        };
        let name = self.names.get(&id).cloned().unwrap_or_else(|| col.name.clone());
        if self.add_model(id, &name, &col).is_none() {
            // an empty model: the id has no collision (as an empty engineReplaceCOL)
            let cur = self.by_id.remove(&id);
            self.repoint(id, cur, None);
        }
        self.live.cols.insert(id, ColOverride { data, owner, original });
        self.settle();
        Ok(())
    }

    /// Return game id `id` to the collision it had before `replace_col` (none, for an id that had none).
    pub fn restore_col(&mut self, id: i32) -> bool {
        let Some(o) = self.live.cols.remove(&id) else { return false };
        let cur = self.by_id.get(&id).copied();
        match o.original {
            Some(m) => {
                self.by_id.insert(id, m);
            }
            None => {
                self.by_id.remove(&id);
            }
        }
        self.repoint(id, cur, o.original);
        self.settle();
        true
    }

    /// Drop every live edit `owner` made. Returns how many objects, removals and collisions went.
    pub fn drop_owner(&mut self, owner: Owner) -> usize {
        let handles: Vec<u32> = self.live.slots.iter().filter(|(_, s)| s.owner == owner).map(|(h, _)| *h).collect();
        let mut n = handles.len();
        for h in handles {
            self.remove_object(h);
        }
        let before = self.live.removals.len();
        self.live.removals.retain(|r| r.owner != owner);
        if self.live.removals.len() != before {
            n += before - self.live.removals.len();
            self.recheck_removed(None);
        }
        let ids: Vec<i32> = self.live.cols.iter().filter(|(_, c)| c.owner == owner).map(|(id, _)| *id).collect();
        n += ids.len();
        for id in ids {
            self.restore_col(id);
        }
        self.settle();
        n
    }

    /// Apply a recorded op (from another world). Objects whose model has no collision here keep their handle.
    pub fn apply(&mut self, op: &Op) {
        match op {
            Op::Add { owner, rows } => {
                for (h, row) in rows {
                    self.insert_object(*h, *row, *owner);
                }
                self.settle();
            }
            Op::Move { handle, pos, rot } => {
                self.move_object(*handle, *pos, *rot);
            }
            Op::Scale { handle, scale } => {
                self.scale_object(*handle, *scale);
            }
            Op::SetModel { handle, model } => {
                self.set_object_model(*handle, *model);
            }
            Op::Remove { handle } => {
                self.remove_object(*handle);
            }
            Op::RemoveWorld(r) => {
                self.remove_world_model(*r);
            }
            Op::RestoreWorld { model, radius, pos, interior } => {
                self.restore_world_model(*model, *radius, *pos, *interior);
            }
            Op::ReplaceCol { id, data, owner } => {
                let _ = self.replace_col(*id, data.clone(), *owner);
            }
            Op::RestoreCol { id } => {
                self.restore_col(*id);
            }
            Op::DropOwner(o) => {
                self.drop_owner(*o);
            }
            Op::NextHandle(n) => self.live.next_handle = self.live.next_handle.max(*n),
        }
    }

    /// The live state as ops that rebuild it on another world: collisions, then objects, then removals.
    pub fn live_ops(&self) -> Vec<Op> {
        let mut ops: Vec<Op> = Vec::new();
        let mut cols: Vec<(&i32, &ColOverride)> = self.live.cols.iter().collect();
        cols.sort_by_key(|(id, _)| **id);
        for (id, c) in cols {
            ops.push(Op::ReplaceCol { id: *id, data: c.data.clone(), owner: c.owner });
        }
        let mut by_owner: HashMap<Owner, Vec<(u32, ObjectRow)>> = HashMap::new();
        for (h, s) in &self.live.slots {
            by_owner.entry(s.owner).or_default().push((*h, s.row));
        }
        for (owner, mut rows) in by_owner {
            rows.sort_by_key(|(h, _)| *h);
            ops.push(Op::Add { owner, rows });
        }
        for r in &self.live.removals {
            ops.push(Op::RemoveWorld(*r));
        }
        ops.push(Op::NextHandle(self.live.next_handle));
        ops
    }

    /// Replay `ops` and rebuild the live BVH once.
    pub fn apply_all(&mut self, ops: &[Op]) {
        for op in ops {
            self.apply(op);
        }
        self.rebuild_dyn();
    }

    pub fn live_stats(&self) -> LiveStats {
        LiveStats {
            objects: self.live.slots.len(),
            objects_without_collision: self.live.slots.values().filter(|s| self.instances[s.inst as usize].flags & INST_NOCOL != 0).count(),
            removals: self.live.removals.len(),
            removed_instances: self.instances.iter().filter(|i| i.flags & INST_LREMOVED != 0 && i.flags & (INST_SKIP & !INST_LREMOVED) == 0).count(),
            cols: self.live.cols.len(),
            pending: self.pending.len(),
            dyn_tree: self.dyn_order.len(),
        }
    }

    /// The live object `handle`, if any.
    pub fn object(&self, handle: u32) -> Option<&Slot> {
        self.live.slots.get(&handle)
    }
}


#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::math::{v3, Affine};
    use crate::query::Filter;

    /// A COL1 file with one box.
    pub fn col1_box(name: &str, min: [f32; 3], max: [f32; 3]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut n = [0u8; 22];
        n[..name.len()].copy_from_slice(name.as_bytes());
        body.extend_from_slice(&n);
        body.extend_from_slice(&0u16.to_le_bytes());
        let f = |b: &mut Vec<u8>, v: &[f32]| v.iter().for_each(|x| b.extend_from_slice(&x.to_le_bytes()));
        f(&mut body, &[1.0, 0.0, 0.0, 0.0]);
        f(&mut body, &min);
        f(&mut body, &max);
        body.extend_from_slice(&0u32.to_le_bytes()); // spheres
        body.extend_from_slice(&0u32.to_le_bytes()); // lines
        body.extend_from_slice(&1u32.to_le_bytes()); // boxes
        f(&mut body, &min);
        f(&mut body, &max);
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(&0u32.to_le_bytes()); // verts
        body.extend_from_slice(&0u32.to_le_bytes()); // faces
        let mut out = b"COLL".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn model(w: &mut World, id: i32, min: [f32; 3], max: [f32; 3]) {
        let col = sa_formats::col::parse_archive(&col1_box("m", min, max)).unwrap().remove(0);
        w.add_model(id, "m", &col).unwrap();
    }

    /// A flat 200 m ground slab (id 1, top at z 0) at the origin, a 2 m cube (id 2) as a base building at (50, 50)
    /// and model 3 (a 4 x 4 x 1 slab, top at z 1).
    pub fn small_world() -> World {
        let mut w = World::default();
        model(&mut w, 1, [-100.0, -100.0, -1.0], [100.0, 100.0, 0.0]);
        model(&mut w, 2, [-1.0, -1.0, 0.0], [1.0, 1.0, 2.0]);
        model(&mut w, 3, [-2.0, -2.0, 0.0], [2.0, 2.0, 1.0]);
        let src = w.source_index("test");
        w.add_instance(1, Affine::IDENTITY, 0, src, INST_BASE);
        w.add_instance(2, Affine { t: v3(50.0, 50.0, 0.0), ..Affine::IDENTITY }, 0, src, INST_BASE);
        w.finish();
        w
    }

    fn z(w: &World, x: f32, y: f32) -> Option<(f32, i32)> {
        w.ground(x, y, 100.0, 200.0, Filter::default()).map(|h| ((h.pos.z * 1000.0).round() / 1000.0, h.model))
    }

    fn row(model: i32, x: f32, y: f32, zz: f32) -> ObjectRow {
        ObjectRow::new(model, [x, y, zz], [0.0; 3], 0)
    }

    #[test]
    fn add_move_remove() {
        let mut w = small_world();
        assert_eq!(z(&w, 10.0, 10.0), Some((0.0, 1)));
        let (h, _) = w.add_object(row(3, 10.0, 10.0, 0.0), 0);
        let h = h.unwrap();
        assert_eq!(z(&w, 10.0, 10.0), Some((1.0, 3)));
        assert!(w.move_object(h, [20.0, 10.0, 5.0], [0.0, 0.0, 45.0]));
        assert_eq!(z(&w, 10.0, 10.0), Some((0.0, 1)));
        assert_eq!(z(&w, 20.0, 10.0), Some((6.0, 3)));
        // turned 45 degrees: a corner of the 4 m slab reaches 2.83 m along an axis
        assert_eq!(z(&w, 22.6, 10.0), Some((6.0, 3)));
        assert!(w.remove_object(h));
        assert!(!w.remove_object(h));
        assert_eq!(z(&w, 20.0, 10.0), Some((0.0, 1)));
        // a raycast and the box queries see live objects too
        let (h2, _) = w.add_object(row(3, -10.0, 0.0, 0.0), 0);
        assert!(w.raycast(v3(-20.0, 0.0, 0.5), v3(0.0, 0.0, 0.5), Filter::default()).is_some_and(|hit| (hit.pos.x + 12.0).abs() < 1e-3));
        let ii = w.object(h2.unwrap()).unwrap().inst;
        assert!(w.instances_near(v3(-10.0, 0.0, 0.5), 1.0).contains(&ii));
    }

    #[test]
    fn interiors_and_rotation() {
        let mut w = small_world();
        w.add_object(ObjectRow { interior: 5, ..row(3, 0.0, 0.0, 10.0) }, 0);
        assert_eq!(z(&w, 0.0, 0.0), Some((0.0, 1)));
        assert_eq!(w.ground(0.0, 0.0, 100.0, 200.0, Filter::interior(5)).map(|h| h.model), Some(3));
        // tipped 90 degrees about X: the slab stands 4 m tall
        w.add_object(ObjectRow { rot: [90.0, 0.0, 0.0], ..row(3, 30.0, 30.0, 0.0) }, 0);
        // (its local up turns to -Y, so it fills y 29..30)
        assert_eq!(z(&w, 30.0, 29.5), Some((2.0, 3)));
        assert_eq!(z(&w, 30.0, 30.5), Some((0.0, 1)));
        // scaled 3x along its own axes: 12 m wide, 3 m tall
        let h = w.add_object(row(3, -40.0, -40.0, 0.0), 0).0.unwrap();
        assert!(w.scale_object(h, [3.0, 3.0, 3.0]));
        assert_eq!(z(&w, -40.0, -40.0), Some((3.0, 3)));
        assert_eq!(z(&w, -45.0, -40.0), Some((3.0, 3)));
        assert!(w.move_object(h, [-40.0, -40.0, 1.0], [0.0; 3]));
        assert_eq!(z(&w, -45.0, -40.0), Some((4.0, 3)), "a move keeps the scale");
    }

    #[test]
    fn models_without_collision_and_replaced_cols() {
        let mut w = small_world();
        assert!(w.add_object(row(20001, 0.0, 0.0, 0.0), 0).0.is_none());
        let h = w.add_object(row(3, 0.0, 0.0, 0.0), 0).0.unwrap();
        assert_eq!(w.set_object_model(h, 20001), Some(false));
        assert_eq!(z(&w, 0.0, 0.0), Some((0.0, 1)));
        w.replace_col(20001, Arc::new(col1_box("x", [-1.0, -1.0, 0.0], [1.0, 1.0, 3.0])), 0).unwrap();
        assert_eq!(z(&w, 0.0, 0.0), Some((3.0, 20001)));
        // later objects of that id use it too
        let h2 = w.add_object(row(20001, 5.0, 0.0, 0.0), 0).0.unwrap();
        assert_eq!(z(&w, 5.0, 0.0), Some((3.0, 20001)));
        assert!(w.restore_col(20001));
        assert_eq!(z(&w, 0.0, 0.0), Some((0.0, 1)));
        assert_eq!(z(&w, 5.0, 0.0), Some((0.0, 1)));
        assert!(w.object(h2).is_some(), "handles outlive their collision");
        // a base model replaced and restored: map instances follow
        w.replace_col(2, Arc::new(col1_box("y", [-1.0, -1.0, 0.0], [1.0, 1.0, 7.0])), 0).unwrap();
        assert_eq!(z(&w, 50.0, 50.0), Some((7.0, 2)));
        w.replace_col(2, Arc::new(col1_box("y", [-1.0, -1.0, 0.0], [1.0, 1.0, 9.0])), 0).unwrap();
        assert_eq!(z(&w, 50.0, 50.0), Some((9.0, 2)));
        assert!(w.restore_col(2));
        assert_eq!(z(&w, 50.0, 50.0), Some((2.0, 2)));
        assert!(w.replace_col(4, Arc::new(vec![1, 2, 3]), 0).is_err());
        // an empty collision takes a model's collision away, until restored
        let mut empty = col1_box("e", [0.0; 3], [0.0; 3]);
        let nb = empty.len() - 8 - 4; // the box count sits before the box, its 28 bytes and the 2 counts
        empty.truncate(nb - 28);
        empty.extend_from_slice(&0u32.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        let size = (empty.len() - 8) as u32;
        empty[4..8].copy_from_slice(&size.to_le_bytes());
        w.replace_col(2, Arc::new(empty), 0).unwrap();
        assert_eq!(z(&w, 50.0, 50.0), Some((0.0, 1)));
        assert!(!w.has_collision(2));
        assert!(w.restore_col(2));
        assert_eq!(z(&w, 50.0, 50.0), Some((2.0, 2)));
    }

    #[test]
    fn world_model_removal() {
        let mut w = small_world();
        let h = w.add_object(row(2, 50.0, 50.0, 3.0), 0).0.unwrap();
        assert_eq!(z(&w, 50.0, 50.0), Some((5.0, 2)));
        assert_eq!(w.remove_world_model(Removal { model: 2, pos: [50.0, 50.0, 0.0], radius: 5.0, interior: -1, owner: 0 }), 1);
        // live objects stay (as createObject objects do under MTA's removeWorldModel)
        assert_eq!(z(&w, 50.0, 50.0), Some((5.0, 2)));
        w.remove_object(h);
        assert_eq!(z(&w, 50.0, 50.0), Some((0.0, 1)));
        // a removal elsewhere or of another model does nothing
        assert_eq!(w.remove_world_model(Removal { model: 2, pos: [0.0, 0.0, 0.0], radius: 5.0, interior: -1, owner: 0 }), 0);
        assert_eq!(w.restore_world_model(2, 1.0, [50.0, 50.0, 0.0], -1), 1);
        assert_eq!(z(&w, 50.0, 50.0), Some((2.0, 2)));
        // two overlapping removals: restoring one keeps the other in force
        w.remove_world_model(Removal { model: 2, pos: [50.0, 50.0, 0.0], radius: 5.0, interior: -1, owner: 0 });
        w.remove_world_model(Removal { model: -1, pos: [51.0, 50.0, 0.0], radius: 5.0, interior: 0, owner: 0 });
        assert_eq!(w.restore_world_model(2, 0.5, [50.0, 50.0, 0.0], -1), 0);
        assert_eq!(z(&w, 50.0, 50.0), Some((0.0, 1)));
    }

    /// Brute force: the highest top of every live object at (x, y), or the ground.
    fn expect(w: &World, x: f32, y: f32) -> f32 {
        let mut best = 0.0f32;
        for s in w.live.slots.values() {
            let i = &w.instances[s.inst as usize];
            if i.flags & INST_NOCOL == 0 && (x - s.row.pos[0]).abs() < 1.99 && (y - s.row.pos[1]).abs() < 1.99 {
                best = best.max(s.row.pos[2] + 1.0);
            }
        }
        best
    }

    #[test]
    fn many_edits_stay_consistent() {
        let mut w = small_world();
        let mut rng = 12345u32;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        let rows: Vec<ObjectRow> = (0..6000).map(|k| row(3, (k % 40) as f32 * 4.0 - 80.0, (k / 40) as f32 * 4.0 - 100.0, (k % 7) as f32)).collect();
        let (hs, _) = w.add_objects(&rows, 0);
        let hs: Vec<u32> = hs.into_iter().map(Option::unwrap).collect();
        for step in 0..12000 {
            let h = hs[next() as usize % hs.len()];
            match next() % 4 {
                0 | 1 => {
                    let (x, y) = ((next() % 160) as f32 - 80.0, (next() % 160) as f32 - 80.0);
                    w.move_object(h, [x, y, (next() % 9) as f32], [0.0; 3]);
                }
                2 => {
                    w.set_object_model(h, if next() % 2 == 0 { 3 } else { 999 });
                }
                _ => {
                    let r = w.object(h).map(|s| s.row).unwrap();
                    w.remove_object(h);
                    w.insert_object(h, r, 0);
                }
            }
            if step % 100 == 0 {
                for _ in 0..50 {
                    let (x, y) = ((next() % 170) as f32 - 85.0 + 0.5, (next() % 170) as f32 - 85.0 + 0.5);
                    let got = z(&w, x, y).map(|g| g.0).unwrap_or(-1.0);
                    assert!((got - expect(&w, x, y)).abs() < 1e-3, "step {step} at {x},{y}: {got} vs {}", expect(&w, x, y));
                }
            }
        }
        assert!(w.pending.len() <= PENDING_MAX);
        assert_eq!(w.live_stats().objects, 6000);
    }

    #[test]
    fn replay_onto_a_fresh_world() {
        let mut w = small_world();
        w.replace_col(20001, Arc::new(col1_box("x", [-1.0, -1.0, 0.0], [1.0, 1.0, 3.0])), 7).unwrap();
        let (hs, _) = w.add_objects(&[row(3, 0.0, 0.0, 0.0), row(20001, 10.0, 0.0, 0.0), row(3, -10.0, 0.0, 0.0)], 7);
        w.move_object(hs[2].unwrap(), [-20.0, 0.0, 0.0], [0.0; 3]);
        w.remove_object(hs[0].unwrap());
        w.remove_world_model(Removal { model: 2, pos: [50.0, 50.0, 0.0], radius: 5.0, interior: -1, owner: 7 });
        let mut fresh = small_world();
        fresh.apply_all(&w.live_ops());
        for (x, y) in [(0.0, 0.0), (10.0, 0.0), (-10.0, 0.0), (-20.0, 0.0), (50.0, 50.0)] {
            assert_eq!(z(&fresh, x, y), z(&w, x, y), "at {x},{y}");
        }
        assert_eq!(fresh.object(hs[2].unwrap()).unwrap().row.pos, [-20.0, 0.0, 0.0]);
        assert!(fresh.object(hs[0].unwrap()).is_none());
        // handles keep counting from where they were
        assert_eq!(fresh.add_object(row(3, 0.0, 0.0, 0.0), 0).0, w.add_object(row(3, 0.0, 0.0, 0.0), 0).0);
        // dropping the owner undoes all of it
        fresh.drop_owner(7);
        assert_eq!(z(&fresh, 10.0, 0.0), Some((0.0, 1)));
        assert_eq!(z(&fresh, -20.0, 0.0), Some((0.0, 1)));
        assert_eq!(z(&fresh, 50.0, 50.0), Some((2.0, 2)));
        assert!(!fresh.has_collision(20001));
        assert_eq!(fresh.live_stats().objects, 1);
    }
}
