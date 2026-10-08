// SPDX-License-Identifier: GPL-3.0-only
//! Model tags and the small matrix algebra entity linking and tag lookup need.
//!
//! A model's tags are its bones. [`Skeleton`] keeps the rest pose of every bone in model
//! space, which is what `gettagorigin`, `gettagangles`, `linkto` with a tag and `attach` read.
//! The ceiling of this module is the rest pose: a tag does not follow animations playing on
//! the entity until the skeleton slice supplies animated bone matrices (entities play
//! animations server-side only for their notetracks, see `anim.rs`).
//!
//! Matrices follow the original's layout: `[forward, left, up]` rows plus an origin row, row
//! vectors multiplied on the left, so `a * b` applies `a` first and then `b`.

use std::sync::Arc;

use assets::zone::xmodel::XModel;
use sim::Vec3;

/// Three axis rows (forward, left, up).
pub type Axis = [Vec3; 3];
/// An axis and an origin (`mat4x3`).
pub type Mat43 = [Vec3; 4];

pub const IDENTITY: Mat43 = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 0.0],
];

/// `AnglesToAxis` (pitch, yaw, roll in degrees).
pub fn angles_to_axis(a: Vec3) -> Axis {
    let (sp, cp) = sincos_deg(a[0]);
    let (sy, cy) = sincos_deg(a[1]);
    let (sr, cr) = sincos_deg(a[2]);
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

fn sincos_deg(deg: f32) -> (f32, f32) {
    let (s, c) = f64::from(deg).to_radians().sin_cos();
    (s as f32, c as f32)
}

fn atan2_deg(y: f32, x: f32) -> f32 {
    f64::from(y).atan2(f64::from(x)).to_degrees() as f32
}

/// `AxisToAngles`.
pub fn axis_to_angles(axis: &Axis) -> Vec3 {
    let fwd = axis[0];
    let (mut pitch, yaw);
    if fwd[0] == 0.0 && fwd[1] == 0.0 {
        yaw = 0.0;
        pitch = if -fwd[2] < 0.0 { 270.0 } else { 90.0 };
    } else {
        let y = atan2_deg(fwd[1], fwd[0]);
        yaw = if y < 0.0 { y + 360.0 } else { y };
        let flat = (fwd[0] * fwd[0] + fwd[1] * fwd[1]).sqrt();
        pitch = -atan2_deg(fwd[2], flat);
        if pitch < 0.0 {
            pitch += 360.0;
        }
    }
    // Bring the left vector into the frame where forward is +X; its tilt is the roll.
    let mut left = axis[1];
    let (s, c) = sincos_deg(-yaw);
    let t = c * left[0] - s * left[1];
    left[1] = s * left[0] + c * left[1];
    let (s, c) = sincos_deg(-pitch);
    left[0] = s * left[2] + c * t;
    left[2] = c * left[2] - s * t;
    let signed_pitch = if left[0] == 0.0 && left[1] == 0.0 {
        if left[2] > 0.0 { -90.0 } else { 90.0 }
    } else {
        -atan2_deg(left[2], (left[0] * left[0] + left[1] * left[1]).sqrt())
    };
    let roll = if left[1] >= 0.0 {
        -signed_pitch
    } else {
        signed_pitch + if signed_pitch >= 0.0 { -180.0 } else { 180.0 }
    };
    [pitch, yaw, roll]
}

pub fn mul3(a: &Axis, b: &Axis) -> Axis {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}

/// `MatrixTransformVector`: a row vector times the axis.
pub fn transform3(v: Vec3, m: &Axis) -> Vec3 {
    [
        v[0] * m[0][0] + v[1] * m[1][0] + v[2] * m[2][0],
        v[0] * m[0][1] + v[1] * m[1][1] + v[2] * m[2][1],
        v[0] * m[0][2] + v[1] * m[1][2] + v[2] * m[2][2],
    ]
}

/// `MatrixTransformVector43`.
pub fn transform43(v: Vec3, m: &Mat43) -> Vec3 {
    let r = transform3(v, &[m[0], m[1], m[2]]);
    [r[0] + m[3][0], r[1] + m[3][1], r[2] + m[3][2]]
}

/// `MatrixMultiply43`: `a` expressed in the frame `b`.
pub fn mul43(a: &Mat43, b: &Mat43) -> Mat43 {
    let m = mul3(&[a[0], a[1], a[2]], &[b[0], b[1], b[2]]);
    [m[0], m[1], m[2], transform43(a[3], b)]
}

pub fn transpose3(m: &Axis) -> Axis {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

/// `MatrixInverseOrthogonal43`.
pub fn inverse43(m: &Mat43) -> Mat43 {
    let t = transpose3(&[m[0], m[1], m[2]]);
    let o = transform3([-m[3][0], -m[3][1], -m[3][2]], &t);
    [t[0], t[1], t[2], o]
}

/// The frame of an entity: its angles and origin.
pub fn frame(origin: Vec3, angles: Vec3) -> Mat43 {
    let a = angles_to_axis(angles);
    [a[0], a[1], a[2], origin]
}

/// Rotation of a `DObjAnimMat` quaternion (`x y z w`, scaled by its `transWeight`).
fn quat_axis(q: [f32; 4], tw: f32) -> Axis {
    let s = [q[0] * tw, q[1] * tw, q[2] * tw];
    let (xx, xy, xz, xw) = (s[0] * q[0], s[0] * q[1], s[0] * q[2], s[0] * q[3]);
    let (yy, yz, yw) = (s[1] * q[1], s[1] * q[2], s[1] * q[3]);
    let (zz, zw) = (s[2] * q[2], s[2] * q[3]);
    [
        [1.0 - (yy + zz), xy + zw, xz - yw],
        [xy - zw, 1.0 - (xx + zz), yz + xw],
        [xz + yw, yz - xw, 1.0 - (xx + yy)],
    ]
}

/// What a server keeps of a model's bones: names and rest-pose matrices in model space.
#[derive(Debug)]
pub struct Skeleton {
    names: Vec<Arc<str>>,
    rest: Vec<Mat43>,
}

impl Skeleton {
    /// `strings` resolves the zone's script-string indices that name the bones.
    pub fn new(m: &XModel, strings: &[Option<Arc<str>>]) -> Self {
        let n = usize::from(m.num_bones);
        let names = (0..n)
            .map(|i| {
                m.bone_names
                    .get(i)
                    .and_then(|s| strings.get(usize::from(*s)).cloned().flatten())
                    .unwrap_or_else(|| Arc::from(""))
            })
            .collect();
        let rest = m
            .base_mat
            .iter()
            .take(n)
            .map(|b| {
                let a = quat_axis(b.quat, b.trans_weight);
                [a[0], a[1], a[2], b.trans]
            })
            .collect();
        Self { names, rest }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Name of bone `i` (`getpartname`).
    pub fn name(&self, i: usize) -> Option<&str> {
        self.names.get(i).map(|n| &**n)
    }

    /// `DObjGetBoneIndex`: bone names compare case-insensitively.
    pub fn bone_index(&self, tag: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(tag))
    }

    /// Rest pose of bone `i` in model space.
    pub fn rest(&self, i: usize) -> Option<&Mat43> {
        self.rest.get(i)
    }

    pub fn tag(&self, tag: &str) -> Option<&Mat43> {
        self.rest(self.bone_index(tag)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
    }

    #[test]
    fn angles_round_trip_through_axis() {
        for a in [
            [0.0, 0.0, 0.0],
            [10.0, 20.0, 30.0],
            [-45.0, 170.0, -80.0],
            [30.0, 270.0, 120.0],
        ] {
            let back = axis_to_angles(&angles_to_axis(a));
            let wrap = |v: f32| (v + 540.0).rem_euclid(360.0) - 180.0;
            assert!(
                close(
                    [wrap(a[0]), wrap(a[1]), wrap(a[2])],
                    [wrap(back[0]), wrap(back[1]), wrap(back[2])]
                ),
                "{a:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn yaw_rotates_forward_to_left() {
        let a = angles_to_axis([0.0, 90.0, 0.0]);
        assert!(close(a[0], [0.0, 1.0, 0.0]));
        assert!(close(a[1], [-1.0, 0.0, 0.0]));
    }

    #[test]
    fn inverse_undoes_a_frame() {
        let f = frame([10.0, -5.0, 3.0], [15.0, 80.0, 20.0]);
        let p = [7.0, 8.0, 9.0];
        let world = transform43(p, &f);
        assert!(close(transform43(world, &inverse43(&f)), p));
    }

    #[test]
    fn composition_applies_left_operand_first() {
        let child = frame([1.0, 0.0, 0.0], [0.0; 3]);
        let parent = frame([0.0, 0.0, 0.0], [0.0, 90.0, 0.0]);
        let world = mul43(&child, &parent);
        assert!(close(world[3], [0.0, 1.0, 0.0]));
    }
}
