// SPDX-License-Identifier: GPL-3.0-only
//! Vector and angle helpers for player movement.
//!
//! Determinism: every operation here is a plain IEEE-754 `+ - * /`, `sqrt`, `floor` or
//! round-half-even, which all targets (x86-64, ARM64, wasm32) evaluate bit-identically as long as
//! the compiler does not fuse multiply-add. Rust never contracts `a * b + c` on its own and this
//! module never calls `mul_add`. The trigonometric functions are the one place where `std`
//! defers to the platform libm, so they are implemented here from basic operations on `f64`
//! (range reduction plus a fixed polynomial) instead of calling `f32::sin` and friends.

use crate::Vec3;

pub const EQUAL_EPSILON: f32 = 0.001;
/// `DiffTrack`: moves `cur` toward `tgt` by `rate` times the remaining distance per second.
pub fn diff_track(tgt: f32, cur: f32, rate: f32, dt: f32) -> f32 {
    let err = tgt - cur;
    let step = rate * err * dt;
    if err.abs() <= EQUAL_EPSILON || step.abs() > err.abs() {
        tgt
    } else {
        cur + step
    }
}

/// `1 / 360` as the original multiplies angles by it before `floor` (double constant).
const INV_360: f32 = 0.002_777_777_8;

#[inline]
pub fn dot(a: &Vec3, b: &Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub fn add(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub fn sub(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub fn scale(a: &Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

/// `a + s * b`.
#[inline]
pub fn mad(a: &Vec3, s: f32, b: &Vec3) -> Vec3 {
    [a[0] + s * b[0], a[1] + s * b[1], a[2] + s * b[2]]
}

#[inline]
pub fn lerp(start: &Vec3, end: &Vec3, f: f32) -> Vec3 {
    [
        (end[0] - start[0]) * f + start[0],
        (end[1] - start[1]) * f + start[1],
        (end[2] - start[2]) * f + start[2],
    ]
}

#[inline]
pub fn cross(a: &Vec3, b: &Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub fn length_sq(a: &Vec3) -> f32 {
    dot(a, a)
}

#[inline]
pub fn length(a: &Vec3) -> f32 {
    length_sq(a).sqrt()
}

#[inline]
pub fn length2(a: &Vec3) -> f32 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}

/// Normalises in place and returns the old length; a zero vector stays zero.
#[inline]
pub fn normalize(v: &mut Vec3) -> f32 {
    let len = length(v);
    let inv = if len > 0.0 { 1.0 / len } else { 1.0 };
    v[0] *= inv;
    v[1] *= inv;
    v[2] *= inv;
    len
}

/// Normalises the XY part in place, returning the old 2D length.
#[inline]
pub fn normalize2(v: &mut Vec3) -> f32 {
    let len = length2(v);
    let inv = if len > 0.0 { 1.0 / len } else { 1.0 };
    v[0] *= inv;
    v[1] *= inv;
    len
}

/// Normalised copy and the original length.
#[inline]
pub fn normalize_to(v: &Vec3) -> (Vec3, f32) {
    let len = length(v);
    let inv = 1.0 / if len > 0.0 { len } else { 1.0 };
    ([v[0] * inv, v[1] * inv, v[2] * inv], len)
}

/// Round to nearest, ties to even (`cvtss2si`), as the original's snap helpers do.
#[inline]
pub fn snap(x: f32) -> f32 {
    x.round_ties_even()
}

#[inline]
pub fn snap_to_int(x: f32) -> i32 {
    x.round_ties_even() as i32
}

#[inline]
pub fn snap_vector(v: &mut Vec3) {
    v[0] = snap(v[0]);
    v[1] = snap(v[1]);
    v[2] = snap(v[2]);
}

/// `AngleDelta(a, b)`: the signed difference `a - b` wrapped into `[-180, 180)`.
pub fn angle_delta(a: f32, b: f32) -> f32 {
    let t = (a - b) * INV_360;
    (t - (t + 0.5).floor()) * 360.0
}

/// Wraps into `[0, 360)`.
pub fn angle_normalize_360(angle: f32) -> f32 {
    let t = angle * INV_360;
    let r = (t - t.floor()) * 360.0;
    if r - 360.0 < 0.0 { r } else { r - 360.0 }
}

/// Wraps into `[-180, 180)` the way view-angle updates do.
pub fn angle_wrap_180(angle: f32) -> f32 {
    let t = angle * INV_360;
    (t - (t + 0.5).floor()) * 360.0
}

// --- deterministic trigonometry ---------------------------------------------------------

const PIO2_1: f64 = 1.570_796_326_734_125_6;
const PIO2_1T: f64 = 6.077_100_506_506_192e-11;

/// `(sin x, cos x)` for `x` in radians, valid for |x| < ~1e6.
fn sin_cos(x: f64) -> (f64, f64) {
    let k = (x * core::f64::consts::FRAC_2_PI).round_ties_even();
    let r = (x - k * PIO2_1) - k * PIO2_1T;
    let r2 = r * r;
    let s = r
        * (1.0
            + r2 * (-1.0 / 6.0
                + r2 * (1.0 / 120.0
                    + r2 * (-1.0 / 5040.0
                        + r2 * (1.0 / 362_880.0
                            + r2 * (-1.0 / 39_916_800.0 + r2 * (1.0 / 6_227_020_800.0)))))));
    let c = 1.0
        + r2 * (-0.5
            + r2 * (1.0 / 24.0
                + r2 * (-1.0 / 720.0
                    + r2 * (1.0 / 40_320.0
                        + r2 * (-1.0 / 3_628_800.0 + r2 * (1.0 / 479_001_600.0))))));
    match (k as i64).rem_euclid(4) {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

fn atan_f64(x: f64) -> f64 {
    let neg = x < 0.0;
    let mut a = x.abs();
    let inv = a > 1.0;
    if inv {
        a = 1.0 / a;
    }
    // Two argument halvings bring |a| below tan(pi/16).
    a /= 1.0 + (1.0 + a * a).sqrt();
    a /= 1.0 + (1.0 + a * a).sqrt();
    let a2 = a * a;
    let mut p = 0.0;
    let mut k = 25.0;
    while k >= 1.0 {
        p = 1.0 / k - a2 * p;
        k -= 2.0;
    }
    let mut r = 4.0 * a * p;
    if inv {
        r = core::f64::consts::FRAC_PI_2 - r;
    }
    if neg { -r } else { r }
}

fn atan2_f64(y: f64, x: f64) -> f64 {
    use core::f64::consts::{FRAC_PI_2, PI};
    if x == 0.0 {
        return if y > 0.0 {
            FRAC_PI_2
        } else if y < 0.0 {
            -FRAC_PI_2
        } else {
            0.0
        };
    }
    let a = atan_f64(y / x);
    if x > 0.0 {
        a
    } else if y >= 0.0 {
        a + PI
    } else {
        a - PI
    }
}

const DEG2RAD: f64 = core::f64::consts::PI * 2.0 / 360.0;
const RAD2DEG: f64 = 180.0 / core::f64::consts::PI;

#[inline]
fn sin_cos_deg(deg: f32) -> (f32, f32) {
    let (s, c) = sin_cos(f64::from(deg) * DEG2RAD);
    (s as f32, c as f32)
}

/// `(sin, cos)` of an angle in degrees.
pub fn sincos_deg(deg: f32) -> (f32, f32) {
    sin_cos_deg(deg)
}

/// `acos(x)` in degrees.
pub fn acos_deg(x: f32) -> f32 {
    let x = f64::from(x).clamp(-1.0, 1.0);
    (atan2_f64((1.0 - x * x).sqrt(), x) * RAD2DEG) as f32
}

/// `AngleVectors`: forward, right, up for pitch/yaw/roll in degrees.
pub fn angle_vectors(angles: &Vec3) -> (Vec3, Vec3, Vec3) {
    let (sy, cy) = sin_cos_deg(angles[1]);
    let (sp, cp) = sin_cos_deg(angles[0]);
    let (sr, cr) = sin_cos_deg(angles[2]);
    (
        [cp * cy, cp * sy, -sp],
        [-sr * sp * cy + cr * sy, -sr * sp * sy + -cr * cy, -sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy + -sr * cy, cr * cp],
    )
}

/// `YawVectors2D`: the XY forward and right for a yaw.
pub fn yaw_vectors_2d(yaw: f32) -> ([f32; 2], [f32; 2]) {
    let (sy, cy) = sin_cos_deg(yaw);
    ([cy, sy], [sy, -cy])
}

pub fn vec_to_yaw(v: &Vec3) -> f32 {
    if v[1] == 0.0 && v[0] == 0.0 {
        return 0.0;
    }
    let yaw = (atan2_f64(f64::from(v[1]), f64::from(v[0])) * RAD2DEG) as f32;
    yaw + if yaw < 0.0 { 360.0 } else { 0.0 }
}

pub fn vec_to_pitch(v: &Vec3) -> f32 {
    if v[1] == 0.0 && v[0] == 0.0 {
        return if -v[2] < 0.0 { 270.0 } else { 90.0 };
    }
    let flat = f64::from(v[1] * v[1] + v[0] * v[0]).sqrt();
    let pitch = (atan2_f64(f64::from(v[2]), flat) * -RAD2DEG) as f32;
    pitch + if pitch < 0.0 { 360.0 } else { 0.0 }
}

/// `PitchForYawOnNormal`: the pitch of a surface along a yaw direction.
pub fn pitch_for_yaw_on_normal(yaw: f32, normal: &Vec3) -> f32 {
    let (s, c) = sin_cos_deg(yaw);
    if normal[2] == 0.0 {
        return 270.0;
    }
    let z = (normal[0] * c + normal[1] * s) / normal[2];
    (atan_f64(f64::from(z)) * RAD2DEG) as f32
}

/// Projects `p` onto the plane through the origin with unit `normal`.
pub fn project_point_on_plane(p: &Vec3, normal: &Vec3) -> Vec3 {
    let d = -dot(normal, p);
    mad(p, d, normal)
}

pub fn get_lean_fraction(f: f32) -> f32 {
    (2.0 - f.abs()) * f
}

pub fn un_get_lean_fraction(f: f32) -> f32 {
    1.0 - (1.0 - f).sqrt()
}

/// Shifts a viewpoint sideways for a lean.
pub fn add_lean_to_position(
    pos: &mut Vec3,
    view_yaw: f32,
    lean_frac: f32,
    view_roll: f32,
    lean_dist: f32,
) {
    if lean_frac != 0.0 {
        let lean = get_lean_fraction(lean_frac);
        let (_, right, _) = angle_vectors(&[0.0, view_yaw, view_roll * lean]);
        *pos = mad(pos, lean * lean_dist, &right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trig_matches_hand_values() {
        let (s, c) = sincos_deg(30.0);
        assert!((s - 0.5).abs() < 1e-7 && (c - 0.866_025_4).abs() < 1e-7);
        let (s, c) = sincos_deg(-135.0);
        assert!((s + core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-7);
        assert!((c + core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-7);
        assert!((acos_deg(0.5) - 60.0).abs() < 1e-4);
        assert!((vec_to_yaw(&[-1.0, 0.0, 0.0]) - 180.0).abs() < 1e-4);
        assert!((vec_to_yaw(&[0.0, -1.0, 0.0]) - 270.0).abs() < 1e-4);
        assert!((vec_to_pitch(&[1.0, 0.0, 1.0]) - 315.0).abs() < 1e-4);
    }

    #[test]
    fn angle_delta_wraps() {
        assert!((angle_delta(10.0, 350.0) - 20.0).abs() < 1e-4);
        assert!((angle_delta(350.0, 10.0) + 20.0).abs() < 1e-4);
        assert!((angle_normalize_360(-30.0) - 330.0).abs() < 1e-3);
    }

    #[test]
    fn angle_vectors_are_orthonormal() {
        let (f, r, u) = angle_vectors(&[20.0, 70.0, 0.0]);
        assert!(dot(&f, &r).abs() < 1e-6 && dot(&f, &u).abs() < 1e-6 && dot(&r, &u).abs() < 1e-6);
        assert!((length(&f) - 1.0).abs() < 1e-6);
    }
}
