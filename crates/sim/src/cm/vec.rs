// SPDX-License-Identifier: GPL-3.0-only
//! Three-float helpers for the collision code.

use crate::Vec3;

#[inline]
pub(super) fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub(super) fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub(super) fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub(super) fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub(super) fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

/// `a + b * s`.
#[inline]
pub(super) fn mad(a: Vec3, s: f32, b: Vec3) -> Vec3 {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

#[inline]
pub(super) fn len_sq(a: Vec3) -> f32 {
    dot(a, a)
}

/// Normalizes `a` and returns it with its original length (zero stays zero).
#[inline]
pub(super) fn normalize(a: Vec3) -> (Vec3, f32) {
    let len = len_sq(a).sqrt();
    if len == 0.0 {
        (a, 0.0)
    } else {
        (scale(a, 1.0 / len), len)
    }
}

#[inline]
pub(super) fn xyz(p: [f32; 4]) -> Vec3 {
    [p[0], p[1], p[2]]
}

/// Component-wise `a + (b - a) * t` over a 4-vector (xyz plus the running trace fraction).
#[inline]
pub(super) fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        (b[0] - a[0]) * t + a[0],
        (b[1] - a[1]) * t + a[1],
        (b[2] - a[2]) * t + a[2],
        (b[3] - a[3]) * t + a[3],
    ]
}

/// Rows of the rotation an entity's `angles` describe (forward, left, up): `row . p` takes a
/// world offset into entity space, `row[i] * v[i]` summed over rows takes it back.
pub(super) fn angles_to_axis(angles: Vec3) -> [Vec3; 3] {
    let (sy, cy) = angles[1].to_radians().sin_cos();
    let (sp, cp) = angles[0].to_radians().sin_cos();
    let (sr, cr) = angles[2].to_radians().sin_cos();
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - sy * cr, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

/// `axis * p`: world to entity space.
#[inline]
pub(super) fn rotate_in(axis: &[Vec3; 3], p: Vec3) -> Vec3 {
    [dot(p, axis[0]), dot(p, axis[1]), dot(p, axis[2])]
}

/// `axisᵀ * p`: entity to world space.
#[inline]
pub(super) fn rotate_out(axis: &[Vec3; 3], p: Vec3) -> Vec3 {
    [
        p[0] * axis[0][0] + p[1] * axis[1][0] + p[2] * axis[2][0],
        p[0] * axis[0][1] + p[1] * axis[1][1] + p[2] * axis[2][1],
        p[0] * axis[0][2] + p[1] * axis[1][2] + p[2] * axis[2][2],
    ]
}

/// `a + (b - a) * t`.
#[inline]
pub(super) fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    [
        (b[0] - a[0]) * t + a[0],
        (b[1] - a[1]) * t + a[1],
        (b[2] - a[2]) * t + a[2],
    ]
}
