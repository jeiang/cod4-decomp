// SPDX-License-Identifier: GPL-3.0-or-later
//! Root motion of an animation between two times (`XAnimGetRelDelta`): what `getmovedelta`
//! and `getangledelta` report.

use assets::zone::xanim::{DeltaPart, Indices, Quat, Trans, TransFrames, XAnimParts};

/// The root-motion track of an animation.
#[derive(Debug)]
pub struct RootMotion {
    pub looping: bool,
    pub num_frames: u16,
    pub delta: DeltaPart,
}

impl RootMotion {
    pub fn new(x: &XAnimParts) -> Option<Self> {
        Some(Self {
            looping: x.looping,
            num_frames: x.num_frames,
            delta: x.has_delta.then(|| x.delta.clone()).flatten()?,
        })
    }
}

/// Quaternion track values are 16-bit fixed point pairs; their product scale.
const ROT_SCALE: f32 = 1.0 / (32768.0 * 32768.0);

fn index_at(ix: &Indices, i: usize) -> u32 {
    match ix {
        Indices::None => 0,
        Indices::Byte(b) => u32::from(b[i]),
        Indices::Short(s) => u32::from(s[i]),
    }
}

/// `XAnim_GetTimeIndex`: the key frame at or before `frame_index` and how far the exact
/// frame position is toward the next one.
fn key_frame(ix: &Indices, size: usize, frames: f32, time: f32) -> (usize, f32) {
    let frame_frac = frames * time;
    let frame_index = frame_frac as u32;
    let (mut lo, mut hi) = (0usize, size);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if index_at(ix, mid) <= frame_index {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (a, b) = (index_at(ix, lo) as f32, index_at(ix, lo + 1) as f32);
    (lo, (frame_frac - a) / (b - a))
}

fn raw(t: &Trans, i: usize) -> [f32; 3] {
    match &t.frames {
        TransFrames::Byte(v) => v[i].map(f32::from),
        TransFrames::Short(v) => v[i].map(f32::from),
        TransFrames::None => [0.0; 3],
    }
}

fn scaled(t: &Trans, v: [f32; 3]) -> [f32; 3] {
    [
        t.extent[0] * v[0] + t.mins[0],
        t.extent[1] * v[1] + t.mins[1],
        t.extent[2] * v[2] + t.mins[2],
    ]
}

fn pos_at(t: &Trans, frames: f32, time: f32, entire: bool) -> [f32; 3] {
    if t.size == 0 {
        return t.frame0;
    }
    let size = usize::from(t.size);
    if entire {
        return scaled(t, raw(t, size));
    }
    let (i, frac) = key_frame(&t.indices, size, frames, time);
    let (a, b) = (raw(t, i), raw(t, i + 1));
    scaled(
        t,
        [
            frac * (b[0] - a[0]) + a[0],
            frac * (b[1] - a[1]) + a[1],
            frac * (b[2] - a[2]) + a[2],
        ],
    )
}

fn rot_at(q: &Quat, frames: f32, time: f32, entire: bool) -> [f32; 2] {
    if q.size == 0 {
        return q.frame0.map(f32::from);
    }
    let size = usize::from(q.size);
    if entire {
        return q.frames[size].map(f32::from);
    }
    let (i, frac) = key_frame(&q.indices, size, frames, time);
    let (a, b) = (q.frames[i], q.frames[i + 1]);
    [
        frac * f32::from(b[0] - a[0]) + f32::from(a[0]),
        frac * f32::from(b[1] - a[1]) + f32::from(a[1]),
    ]
}

/// The yaw rotation as `(sin-ish, cos-ish)` and the translation in the animation's start
/// frame, for the part of the animation between `t1` and `t2` (both in `[0, 1]`).
pub fn rel_delta(a: &RootMotion, t1: f32, t2: f32) -> ([f32; 2], [f32; 3]) {
    let d = &a.delta;
    let frames = f32::from(a.num_frames);
    let entire = |t: f32| t == 1.0 || a.num_frames == 0;
    let rot = |t: f32| {
        d.quat
            .as_ref()
            .map_or([0.0, 32767.0], |q| rot_at(q, frames, t, entire(t)))
    };
    let pos = |t: f32| {
        d.trans
            .as_ref()
            .map_or([0.0; 3], |p| pos_at(p, frames, t, entire(t)))
    };
    let (q1, q2) = (rot(t1), rot(t2));
    let (p1, mut p2) = (pos(t1), pos(t2));
    if a.looping
        && t1 > t2
        && let Some(t) = &d.trans
        && t.size != 0
    {
        let (from, to) = (raw(t, 0), raw(t, usize::from(t.size)));
        for i in 0..3 {
            p2[i] += t.extent[i] * (to[i] - from[i]);
        }
    }
    let mut rot = [
        (q2[0] * q1[1] - q2[1] * q1[0]) * ROT_SCALE,
        (q2[0] * q1[0] + q2[1] * q1[1]) * ROT_SCALE,
    ];
    let mut v = [p2[0] - p1[0], p2[1] - p1[1], p2[2] - p1[2]];
    // `TransformToQuatRefFrame`: the translation expressed in the start rotation's frame.
    let zz = q1[0] * q1[0];
    let r = q1[1] * q1[1] + zz;
    if r != 0.0 {
        let ra = 2.0 / r;
        let zza = zz * ra;
        let zw = q1[0] * q1[1] * ra;
        let x = (1.0 - zza) * v[0] + zw * v[1];
        v[1] -= zw * v[0] + zza * v[1];
        v[0] = x;
    }
    if rot == [0.0, 0.0] {
        rot = [0.0, 1.0];
    }
    (rot, v)
}

/// `RotationToYaw`.
pub fn rotation_to_yaw(rot: [f32; 2]) -> f32 {
    let zz = rot[0] * rot[0];
    let r = rot[1] * rot[1] + zz;
    if r == 0.0 {
        return 0.0;
    }
    let ra = 2.0 / r;
    (ra * (rot[1] * rot[0])).atan2(1.0 - ra * zz).to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(frames: u16, trans: Trans, looping: bool) -> RootMotion {
        RootMotion {
            looping,
            num_frames: frames,
            delta: DeltaPart {
                trans: Some(trans),
                quat: None,
            },
        }
    }

    fn walk() -> Trans {
        // Four frames, x from 0 to 30 in steps of 10 (extent 10 per raw step).
        Trans {
            size: 3,
            small: true,
            frame0: [0.0; 3],
            mins: [0.0; 3],
            extent: [10.0, 0.0, 0.0],
            indices: Indices::Byte(vec![0, 1, 2, 3]),
            frames: TransFrames::Byte(vec![[0, 0, 0], [1, 0, 0], [2, 0, 0], [3, 0, 0]]),
        }
    }

    #[test]
    fn move_delta_is_the_distance_between_the_two_times() {
        let a = anim(3, walk(), false);
        let (_, whole) = rel_delta(&a, 0.0, 1.0);
        assert!((whole[0] - 30.0).abs() < 1e-3);
        let (_, half) = rel_delta(&a, 0.0, 0.5);
        assert!((half[0] - 15.0).abs() < 1e-3, "{half:?}");
        let (rot, _) = rel_delta(&a, 0.0, 1.0);
        assert!(rotation_to_yaw(rot).abs() < 1e-4);
    }

    #[test]
    fn looping_animation_wraps_when_the_end_time_is_earlier() {
        let a = anim(3, walk(), true);
        let (_, v) = rel_delta(&a, 0.5, 0.25);
        // 0.5 -> end is 15, start -> 0.25 is 7.5.
        assert!((v[0] - 22.5).abs() < 1e-3, "{v:?}");
    }
}
