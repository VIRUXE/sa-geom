//! GTA object Euler angles, as used by SA-MP/open.mp `CreateObject` and MTA objects.
//!
//! The formulas are those of open.mp's `GTAQuat` (open.mp-sdk `include/gtaquat.hpp`, after IllidanS4's i_quat):
//! degrees -> the game's quaternion and back. The game's quaternion is the conjugate of the object's rotation
//! (the same convention as IPL files), so the world matrix uses its conjugate.

use crate::math::{Affine, Quat, Vec3};

/// The game's quaternion for Euler angles in degrees.
pub fn game_quat(deg: [f32; 3]) -> Quat {
    let r = deg.map(|d| (d.to_radians()) * -0.5);
    let (cx, cy, cz) = (r[0].cos(), r[1].cos(), r[2].cos());
    let (sx, sy, sz) = (r[0].sin(), r[1].sin(), r[2].sin());
    Quat {
        w: cx * cy * cz + sx * sy * sz,
        x: cx * sy * sz + sx * cy * cz,
        y: cx * sy * cz - sx * cy * sz,
        z: cx * cy * sz - sx * sy * cz,
    }
}

/// Euler angles in degrees, each in [0, 360), from the game's quaternion.
pub fn game_euler(q: Quat) -> [f32; 3] {
    const EPS: f32 = 0.000_002_026_557_9;
    let c = |v: f32| v.clamp(-1.0, 1.0);
    let temp = 2.0 * q.y * q.z - 2.0 * q.x * q.w;
    let (rx, ry, rz);
    if temp >= 1.0 - EPS {
        rx = 90.0;
        ry = -c(q.y).atan2(c(q.w)).to_degrees();
        rz = -c(q.z).atan2(c(q.w)).to_degrees();
    } else if -temp >= 1.0 - EPS {
        rx = -90.0;
        ry = -c(q.y).atan2(c(q.w)).to_degrees();
        rz = -c(q.z).atan2(c(q.w)).to_degrees();
    } else {
        rx = c(temp).asin().to_degrees();
        ry = -c(q.x * q.z + q.y * q.w).atan2(c(0.5 - q.x * q.x - q.y * q.y)).to_degrees();
        rz = -c(q.x * q.y + q.z * q.w).atan2(c(0.5 - q.x * q.x - q.z * q.z)).to_degrees();
    }
    [rx, ry, rz].map(|a| {
        let m = a.rem_euclid(360.0);
        if m >= 360.0 {
            0.0
        } else {
            m
        }
    })
}

/// World transform of an object at `pos` with GTA Euler angles `deg`.
pub fn object_affine(pos: Vec3, deg: [f32; 3]) -> Affine {
    Affine::from_quat(game_quat(deg).conj().normalize(), pos)
}

/// GTA Euler angles (degrees) of an orthonormal basis (columns right, forward, up).
pub fn basis_euler(x: Vec3, y: Vec3, z: Vec3) -> [f32; 3] {
    game_euler(Quat::from_basis(x, y, z).conj())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v3;
    use crate::query::place;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).len() < 1e-4
    }

    #[test]
    fn single_axes_are_right_handed() {
        // rz = 90: the object's forward (+Y) turns to -X, its right (+X) to +Y
        let m = object_affine(Vec3::ZERO, [0.0, 0.0, 90.0]);
        assert!(close(m.y, v3(-1.0, 0.0, 0.0)), "{:?}", m.y);
        assert!(close(m.x, v3(0.0, 1.0, 0.0)), "{:?}", m.x);
        // rx = 90: up (+Z) tips to -Y
        let m = object_affine(Vec3::ZERO, [90.0, 0.0, 0.0]);
        assert!(close(m.z, v3(0.0, -1.0, 0.0)), "{:?}", m.z);
    }

    #[test]
    fn euler_round_trip() {
        for &deg in &[[10.0, 20.0, 30.0], [350.0, 5.0, 270.0], [45.0, 300.0, 123.0], [0.0, 0.0, 0.0], [89.0, 10.0, 200.0]] {
            let m = object_affine(Vec3::ZERO, deg);
            let back = basis_euler(m.x, m.y, m.z);
            let m2 = object_affine(Vec3::ZERO, back);
            assert!(close(m.x, m2.x) && close(m.y, m2.y) && close(m.z, m2.z), "{deg:?} -> {back:?}");
        }
    }

    #[test]
    fn placement_rotation_reproduces_the_surface_basis() {
        let n = v3(0.2, -0.3, 0.93).normalize();
        for yaw in [0.0f32, 37.0, 90.0, 200.0] {
            let p = place(Vec3::ZERO, n, yaw, 0.0);
            let m = object_affine(Vec3::ZERO, p.rot);
            assert!(close(m.z, n), "up {:?} vs {:?}", m.z, n);
            assert!(close(m.y, p.forward) && close(m.x, p.right));
            // forward stays in the vertical plane of the yaw heading
            let (s, c) = yaw.to_radians().sin_cos();
            assert!(v3(c, s, 0.0).dot(p.forward).abs() < 1e-4);
        }
    }
}
