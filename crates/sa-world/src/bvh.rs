//! Binned-SAH bounding volume hierarchy over AABBs, stored as a flat node array.

use crate::math::{Aabb, Vec3};
use bytemuck::{Pod, Zeroable};

/// A node: `count > 0` is a leaf over primitives `first..first+count` (in the reordered primitive order);
/// `count == 0` is an inner node whose children are `first` and `first + 1`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Node {
    pub bounds: Aabb,
    pub first: u32,
    pub count: u32,
}

const BINS: usize = 12;
const MAX_LEAF: usize = 4;

/// Build over `boxes`. Returns the nodes and the primitive order (node `first` indexes into it).
pub fn build(boxes: &[Aabb]) -> (Vec<Node>, Vec<u32>) {
    let mut order: Vec<u32> = (0..boxes.len() as u32).collect();
    let centers: Vec<Vec3> = boxes.iter().map(|b| b.center()).collect();
    let mut nodes = Vec::with_capacity(boxes.len() * 2 / MAX_LEAF + 1);
    if boxes.is_empty() {
        nodes.push(Node { bounds: Aabb::EMPTY, first: 0, count: 0 });
        return (nodes, order);
    }
    nodes.push(Node { bounds: Aabb::EMPTY, first: 0, count: boxes.len() as u32 });
    let mut stack = vec![0usize];
    while let Some(ni) = stack.pop() {
        let (first, count) = (nodes[ni].first as usize, nodes[ni].count as usize);
        let prims = &mut order[first..first + count];
        let mut bounds = Aabb::EMPTY;
        let mut cb = Aabb::EMPTY;
        for &p in prims.iter() {
            bounds = bounds.union(boxes[p as usize]);
            cb.grow(centers[p as usize]);
        }
        nodes[ni].bounds = bounds;
        if count <= MAX_LEAF {
            continue;
        }
        // best split over the 3 axes with binned SAH on centroids
        let mut best = (f32::MAX, 0usize, 0usize); // cost, axis, bin split
        for axis in 0..3 {
            let (lo, hi) = (cb.min.get(axis), cb.max.get(axis));
            if hi - lo < 1e-6 {
                continue;
            }
            let scale = BINS as f32 / (hi - lo);
            let mut bin_box = [Aabb::EMPTY; BINS];
            let mut bin_n = [0usize; BINS];
            for &p in prims.iter() {
                let b = (((centers[p as usize].get(axis) - lo) * scale) as usize).min(BINS - 1);
                bin_n[b] += 1;
                bin_box[b] = bin_box[b].union(boxes[p as usize]);
            }
            let mut left_area = [0f32; BINS];
            let mut left_n = [0usize; BINS];
            let (mut acc, mut n) = (Aabb::EMPTY, 0);
            for i in 0..BINS - 1 {
                acc = acc.union(bin_box[i]);
                n += bin_n[i];
                left_area[i] = acc.area();
                left_n[i] = n;
            }
            let (mut acc, mut n) = (Aabb::EMPTY, 0);
            for i in (1..BINS).rev() {
                acc = acc.union(bin_box[i]);
                n += bin_n[i];
                let cost = left_n[i - 1] as f32 * left_area[i - 1] + n as f32 * acc.area();
                if left_n[i - 1] > 0 && n > 0 && cost < best.0 {
                    best = (cost, axis, i);
                }
            }
        }
        let leaf_cost = count as f32 * bounds.area();
        let mid = if best.0 < f32::MAX && (best.0 < leaf_cost || count > 16) {
            let (axis, split) = (best.1, best.2);
            let (lo, hi) = (cb.min.get(axis), cb.max.get(axis));
            let scale = BINS as f32 / (hi - lo);
            partition(prims, |p| (((centers[p as usize].get(axis) - lo) * scale) as usize).min(BINS - 1) < split)
        } else if count > 16 {
            // degenerate centroids: split in half
            count / 2
        } else {
            continue;
        };
        if mid == 0 || mid == count {
            continue;
        }
        let left = nodes.len();
        nodes.push(Node { bounds: Aabb::EMPTY, first: first as u32, count: mid as u32 });
        nodes.push(Node { bounds: Aabb::EMPTY, first: (first + mid) as u32, count: (count - mid) as u32 });
        nodes[ni].first = left as u32;
        nodes[ni].count = 0;
        stack.push(left);
        stack.push(left + 1);
    }
    (nodes, order)
}

fn partition(v: &mut [u32], pred: impl Fn(u32) -> bool) -> usize {
    let mut i = 0;
    for j in 0..v.len() {
        if pred(v[j]) {
            v.swap(i, j);
            i += 1;
        }
    }
    i
}

/// Walk the tree along the segment `o + t*d`, t in [0, *tmax], nearest child first. `leaf(first, count, tmax)`
/// tests the primitives and may shrink `tmax`.
#[inline]
pub fn traverse(nodes: &[Node], o: Vec3, d: Vec3, tmax: &mut f32, mut leaf: impl FnMut(u32, u32, &mut f32)) {
    if nodes.is_empty() {
        return;
    }
    let inv = Vec3 { x: 1.0 / d.x, y: 1.0 / d.y, z: 1.0 / d.z };
    let mut stack = [0u32; 128];
    let mut sp = 0usize;
    if nodes[0].bounds.ray(o, inv, *tmax).is_none() {
        return;
    }
    stack[sp] = 0;
    sp += 1;
    while sp > 0 {
        sp -= 1;
        let n = &nodes[stack[sp] as usize];
        if n.count > 0 {
            leaf(n.first, n.count, tmax);
            continue;
        }
        let (a, b) = (n.first, n.first + 1);
        let ta = nodes[a as usize].bounds.ray(o, inv, *tmax);
        let tb = nodes[b as usize].bounds.ray(o, inv, *tmax);
        match (ta, tb) {
            (Some(x), Some(y)) => {
                // push far first so near is popped first
                let (near, far) = if x <= y { (a, b) } else { (b, a) };
                if sp + 2 <= stack.len() {
                    stack[sp] = far;
                    stack[sp + 1] = near;
                    sp += 2;
                }
            }
            (Some(_), None) => {
                stack[sp] = a;
                sp += 1;
            }
            (None, Some(_)) => {
                stack[sp] = b;
                sp += 1;
            }
            _ => {}
        }
    }
}

/// Every leaf whose box overlaps `q`.
pub fn query_box(nodes: &[Node], q: Aabb, mut leaf: impl FnMut(u32, u32)) {
    if nodes.is_empty() {
        return;
    }
    let overlaps = |b: &Aabb| b.min.x <= q.max.x && b.max.x >= q.min.x && b.min.y <= q.max.y && b.max.y >= q.min.y && b.min.z <= q.max.z && b.max.z >= q.min.z;
    let mut stack = vec![0u32];
    while let Some(i) = stack.pop() {
        let n = &nodes[i as usize];
        if !overlaps(&n.bounds) {
            continue;
        }
        if n.count > 0 {
            leaf(n.first, n.count);
        } else {
            stack.push(n.first);
            stack.push(n.first + 1);
        }
    }
}
