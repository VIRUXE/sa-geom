//! Minimal f32 vector/matrix math (no SIMD types, so every struct is plain-old-data and serialisable as is).

use bytemuck::{Pod, Zeroable};
use std::ops::{Add, Mul, Neg, Sub};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Pod, Zeroable)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub const fn v3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3 { x, y, z }
}

impl Vec3 {
    pub const ZERO: Vec3 = v3(0.0, 0.0, 0.0);
    pub fn from_arr(a: [f32; 3]) -> Vec3 {
        v3(a[0], a[1], a[2])
    }
    pub fn arr(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: Vec3) -> Vec3 {
        v3(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    pub fn len(self) -> f32 {
        self.dot(self).sqrt()
    }
    pub fn normalize(self) -> Vec3 {
        let l = self.len();
        if l > 1e-12 {
            self * (1.0 / l)
        } else {
            self
        }
    }
    pub fn min(self, o: Vec3) -> Vec3 {
        v3(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }
    pub fn max(self, o: Vec3) -> Vec3 {
        v3(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }
    pub fn get(self, i: usize) -> f32 {
        match i {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f32) -> Vec3 {
        v3(self.x * s, self.y * s, self.z * s)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        v3(-self.x, -self.y, -self.z)
    }
}

/// Axis-aligned box.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: v3(f32::MAX, f32::MAX, f32::MAX), max: v3(f32::MIN, f32::MIN, f32::MIN) };
    pub fn grow(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    pub fn union(self, o: Aabb) -> Aabb {
        Aabb { min: self.min.min(o.min), max: self.max.max(o.max) }
    }
    pub fn center(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
    pub fn is_valid(self) -> bool {
        self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z
    }
    pub fn area(self) -> f32 {
        if !self.is_valid() {
            return 0.0;
        }
        let d = self.max - self.min;
        2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
    }
    pub fn contains_sphere(self, c: Vec3, r: f32) -> bool {
        // sphere/box overlap
        let q = c.max(self.min).min(self.max);
        (q - c).dot(q - c) <= r * r
    }
    /// Slab test for the ray `o + t*d`, t in [0, tmax]; `inv` = 1/d per axis. Returns entry t.
    #[inline]
    pub fn ray(self, o: Vec3, inv: Vec3, tmax: f32) -> Option<f32> {
        let t1 = (self.min - o).mul_v(inv);
        let t2 = (self.max - o).mul_v(inv);
        let tmin_v = t1.min(t2);
        let tmax_v = t1.max(t2);
        let tn = tmin_v.x.max(tmin_v.y).max(tmin_v.z).max(0.0);
        let tf = tmax_v.x.min(tmax_v.y).min(tmax_v.z).min(tmax);
        if tn <= tf {
            Some(tn)
        } else {
            None
        }
    }
}

impl Vec3 {
    #[inline]
    pub fn mul_v(self, o: Vec3) -> Vec3 {
        v3(self.x * o.x, self.y * o.y, self.z * o.z)
    }
}

/// Affine transform: 3x3 linear part as columns (right, forward, up in world space) plus translation.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Affine {
    pub x: Vec3,
    pub y: Vec3,
    pub z: Vec3,
    pub t: Vec3,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { x: v3(1.0, 0.0, 0.0), y: v3(0.0, 1.0, 0.0), z: v3(0.0, 0.0, 1.0), t: Vec3::ZERO };
    pub fn point(&self, p: Vec3) -> Vec3 {
        self.x * p.x + self.y * p.y + self.z * p.z + self.t
    }
    pub fn vector(&self, v: Vec3) -> Vec3 {
        self.x * v.x + self.y * v.y + self.z * v.z
    }
    /// Rows of the linear part.
    fn rows(&self) -> [Vec3; 3] {
        [v3(self.x.x, self.y.x, self.z.x), v3(self.x.y, self.y.y, self.z.y), v3(self.x.z, self.y.z, self.z.z)]
    }
    pub fn inverse(&self) -> Affine {
        // inverse of a general 3x3 via cofactors
        let [r0, r1, r2] = self.rows();
        let c0 = r1.cross(r2);
        let c1 = r2.cross(r0);
        let c2 = r0.cross(r1);
        let det = r0.dot(c0);
        let id = if det.abs() > 1e-20 { 1.0 / det } else { 0.0 };
        // inverse = (1/det) * [c0 c1 c2] as columns
        let inv = Affine { x: c0 * id, y: c1 * id, z: c2 * id, t: Vec3::ZERO };
        let t = -inv.vector(self.t);
        Affine { t, ..inv }
    }
    /// Normal transform (inverse transpose of the linear part) applied to a local normal.
    pub fn normal(&self, inv: &Affine, n: Vec3) -> Vec3 {
        // (M^-1)^T n = rows of M^-1 dotted... i.e. columns of inv as rows
        v3(inv.x.dot(n), inv.y.dot(n), inv.z.dot(n)).normalize()
    }
    pub fn from_quat(q: Quat, t: Vec3) -> Affine {
        let Quat { x, y, z, w } = q;
        Affine {
            x: v3(1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w)),
            y: v3(2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w)),
            z: v3(2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y)),
            t,
        }
    }
    pub fn scaled(self, s: Vec3) -> Affine {
        Affine { x: self.x * s.x, y: self.y * s.y, z: self.z * s.z, t: self.t }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quat {
    pub fn conj(self) -> Quat {
        Quat { x: -self.x, y: -self.y, z: -self.z, w: self.w }
    }
    pub fn normalize(self) -> Quat {
        let l = (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt();
        if l < 1e-12 {
            return Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        }
        Quat { x: self.x / l, y: self.y / l, z: self.z / l, w: self.w / l }
    }
    /// Quaternion of an orthonormal rotation (columns x, y, z).
    pub fn from_basis(x: Vec3, y: Vec3, z: Vec3) -> Quat {
        let (m00, m11, m22) = (x.x, y.y, z.z);
        let tr = m00 + m11 + m22;
        // element m[row][col]: col vectors x,y,z
        let (m01, m02, m10, m12, m20, m21) = (y.x, z.x, x.y, z.y, x.z, y.z);
        let q = if tr > 0.0 {
            let s = (tr + 1.0).sqrt() * 2.0;
            Quat { w: 0.25 * s, x: (m21 - m12) / s, y: (m02 - m20) / s, z: (m10 - m01) / s }
        } else if m00 > m11 && m00 > m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
            Quat { w: (m21 - m12) / s, x: 0.25 * s, y: (m01 + m10) / s, z: (m02 + m20) / s }
        } else if m11 > m22 {
            let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
            Quat { w: (m02 - m20) / s, x: (m01 + m10) / s, y: 0.25 * s, z: (m12 + m21) / s }
        } else {
            let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
            Quat { w: (m10 - m01) / s, x: (m02 + m20) / s, y: (m12 + m21) / s, z: 0.25 * s }
        };
        q.normalize()
    }
}
