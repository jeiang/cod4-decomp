// SPDX-License-Identifier: GPL-3.0-or-later
//! Quaternion helpers in the engine's `[x, y, z, w]` layout.

use crate::Vec3;
use crate::pm::math::sincos_deg;

pub type Quat = [f32; 4];

pub const IDENTITY: Quat = [0.0, 0.0, 0.0, 1.0];

/// Hamilton product `a * b` (apply `b`, then `a`).
#[inline]
pub fn mul(a: &Quat, b: &Quat) -> Quat {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

#[inline]
pub fn conj(a: &Quat) -> Quat {
    [-a[0], -a[1], -a[2], a[3]]
}

pub fn normalize(q: &Quat) -> Quat {
    let l2 = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
    if l2 == 0.0 {
        return IDENTITY;
    }
    let inv = 1.0 / l2.sqrt();
    [q[0] * inv, q[1] * inv, q[2] * inv, q[3] * inv]
}

/// Rotation matrix columns (the images of the x, y, z basis vectors) of a unit quaternion.
pub fn axes(q: &Quat) -> [Vec3; 3] {
    let [x, y, z, w] = *q;
    let (xx, yy, zz) = (2.0 * x * x, 2.0 * y * y, 2.0 * z * z);
    let (xy, xz, yz) = (2.0 * x * y, 2.0 * x * z, 2.0 * y * z);
    let (wx, wy, wz) = (2.0 * w * x, 2.0 * w * y, 2.0 * w * z);
    [
        [1.0 - (yy + zz), xy + wz, xz - wy],
        [xy - wz, 1.0 - (xx + zz), yz + wx],
        [xz + wy, yz - wx, 1.0 - (xx + yy)],
    ]
}

/// `q * v` for a unit quaternion.
#[inline]
pub fn rotate(q: &Quat, v: &Vec3) -> Vec3 {
    let a = axes(q);
    [
        v[0] * a[0][0] + v[1] * a[1][0] + v[2] * a[2][0],
        v[0] * a[0][1] + v[1] * a[1][1] + v[2] * a[2][1],
        v[0] * a[0][2] + v[1] * a[1][2] + v[2] * a[2][2],
    ]
}

/// `q^-1 * v` for a unit quaternion.
#[inline]
pub fn rotate_inv(q: &Quat, v: &Vec3) -> Vec3 {
    let a = axes(q);
    [
        v[0] * a[0][0] + v[1] * a[0][1] + v[2] * a[0][2],
        v[0] * a[1][0] + v[1] * a[1][1] + v[2] * a[1][2],
        v[0] * a[2][0] + v[1] * a[2][1] + v[2] * a[2][2],
    ]
}

/// Rotation for `[pitch, yaw, roll]` degrees (`DObjSetAngles`): yaw about z, then pitch about
/// the rotated y axis, then roll about the rotated x axis.
pub fn from_angles(angles: &Vec3) -> Quat {
    let (ys, yc) = sincos_deg(angles[1] * 0.5);
    let (ps, pc) = sincos_deg(angles[0] * 0.5);
    let (rs, rc) = sincos_deg(angles[2] * 0.5);
    let t = [-ps * ys, ps * yc, pc * ys, pc * yc];
    [
        rs * t[3] + rc * t[0],
        rc * t[1] + rs * t[2],
        -rs * t[1] + rc * t[2],
        rc * t[3] - rs * t[0],
    ]
}
