// SPDX-License-Identifier: GPL-3.0-or-later
//! The two-partition sun shadow map: where the near and far partitions sit in the 1024x2048 map, the matrices that
//! render casters into them, and the lookup constants the stock lit-sun-shadow pixel shaders read.
//!
//! The pixel shaders fix the layout. The lookup matrix maps a position to `(u, v, depth, w)`; the near partition
//! occupies `v` in `[0, 0.5)` of the map, the far one `[0.5, 1)`, and a lookup position moves from one to the other with
//! `uv_far = uv_near * switch.w + switch.xy`. `w` is the distance along the view direction in units of the near
//! partition's reach: below 0.75 only the near partition is read, up to 1 the two blend, beyond 1 only the far one
//! counts, and past the far partition's edge (`shadowmapScale`) the shader falls back to the baked sun visibility.
//!
//! Each partition is an orthographic projection along the light, `sample * 1024` world units wide (the far partition
//! is [`RATIO`] times coarser). Both are anchored to a world grid the size of the far texel so the shadows do not
//! swim as the camera moves.

use glam::{Mat4, Vec3, Vec4};

/// World units per texel of the near partition.
pub const SAMPLE_NEAR: f32 = 0.25;
/// How much coarser the far partition is.
pub const RATIO: f32 = 4.0;
/// Edge of one partition, in texels.
pub const SIZE: u32 = 1024;
/// Shadow map height: the two partitions stacked.
pub const HEIGHT: u32 = 2 * SIZE;
/// `sm_polygonOffsetBias` and `sm_polygonOffsetScale`.
const OFFSET_BIAS: f32 = 0.5;
const OFFSET_SCALE: f32 = 2.0;

/// One partition: its render matrix and the constant that biases its depth.
#[derive(Clone, Copy, Debug)]
pub struct Partition {
    /// World position to clip space of the partition's own 1024x1024 viewport.
    pub view_proj: Mat4,
    /// World units across the partition.
    pub extent: f32,
    /// Depth bias constants for `shadowmapPolygonOffset`, depth-texture and colour-texture variants.
    pub offset_depth: [f32; 4],
    pub offset_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct SunShadow {
    pub partitions: [Partition; 2],
    /// World position (absolute) to `(u, v, depth, w)`.
    pub lookup: Mat4,
    /// `shadowmapSwitchPartition`.
    pub switch_partition: [f32; 4],
    /// `shadowmapScale`.
    pub scale: [f32; 4],
    /// Unit vectors of the light space: along the light, right and up.
    pub axes: [Vec3; 3],
}

/// The camera as the shadow fit needs it.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub forward: Vec3,
    /// Tangent of half the horizontal and the vertical field of view.
    pub tan_half: [f32; 2],
    pub z_near: f32,
}

impl SunShadow {
    /// `to_sun` points from the scene toward the sun; `mins`/`maxs` bound the world.
    pub fn new(cam: &Camera, to_sun: Vec3, mins: Vec3, maxs: Vec3) -> SunShadow {
        let along = -to_sun.normalize();
        let up = if along.x * along.x + along.y * along.y >= 0.1 {
            Vec3::Z
        } else {
            Vec3::X
        };
        let right = up.cross(along).normalize();
        let up = along.cross(right);
        // Light-space coordinates of a position: x grows with `-right`, y with `up`.
        let xy = |p: Vec3| (-p.dot(right), p.dot(up));

        // The near plane's corners and the eye, in light space, fix where the eye sits in the map.
        let cam_right = cam.forward.cross(Vec3::Z).normalize_or(Vec3::Y);
        let cam_up = cam_right.cross(cam.forward);
        let c = cam.eye + cam.forward * cam.z_near;
        let (hx, hy) = (cam.z_near * cam.tan_half[0], cam.z_near * cam.tan_half[1]);
        let corners = [
            c - cam_right * hx - cam_up * hy,
            c + cam_right * hx - cam_up * hy,
            c + cam_right * hx + cam_up * hy,
            c - cam_right * hx + cam_up * hy,
        ];
        let eye_xy = xy(cam.eye);
        let (mut lo, mut hi) = (eye_xy, eye_xy);
        for p in corners {
            let q = xy(p);
            lo = (lo.0.min(q.0), lo.1.min(q.1));
            hi = (hi.0.max(q.0), hi.1.max(q.1));
        }
        let rect = (hi.0 - lo.0).max(hi.1 - lo.1).max(1e-3);
        let to_texels = (SIZE as f32 - 1.0) / rect;
        // Eye position in texels of a partition, measured from the side away from where the rectangle extends so the
        // partition reaches as far as possible in the viewing direction.
        let px = if eye_xy.0 < (lo.0 + hi.0) * 0.5 {
            (eye_xy.0 - lo.0) * to_texels + 1.0
        } else {
            (SIZE as f32 - 1.0) - (hi.0 - eye_xy.0) * to_texels
        };
        let py = if eye_xy.1 > (lo.1 + hi.1) * 0.5 {
            (hi.1 - eye_xy.1) * to_texels + 1.0
        } else {
            (SIZE as f32 - 1.0) - (eye_xy.1 - lo.1) * to_texels
        };

        // Scene extent along the light, for the depth range.
        let (mut z0, mut z1) = (f32::MAX, f32::MIN);
        for i in 0..8 {
            let p = Vec3::new(
                if i & 1 == 0 { mins.x } else { maxs.x },
                if i & 2 == 0 { mins.y } else { maxs.y },
                if i & 4 == 0 { mins.z } else { maxs.z },
            );
            z0 = z0.min(p.dot(along));
            z1 = z1.max(p.dot(along));
        }
        let depth_scale = 1.0 / (z1 - z0 + 2.0);
        let depth_bias = -(z0 - 1.0) * depth_scale;

        // The snapped anchor is shared: a multiple of the far texel.
        let grid = SAMPLE_NEAR * RATIO;
        let snap = ((eye_xy.0 / grid).floor() * grid, (eye_xy.1 / grid).floor() * grid);
        let mut parts = [None; 2];
        let mut org = [(0.0f32, 0.0f32); 2];
        for k in 0..2 {
            let sample = SAMPLE_NEAR * RATIO.powi(k as i32);
            let extent = sample * SIZE as f32;
            // Texel of the snapped anchor, then its texture-space position.
            let tx = ((snap.0 - eye_xy.0) / sample + px).floor();
            let ty = (py - (snap.1 - eye_xy.1) / sample).floor();
            org[k] = (tx / SIZE as f32, ty / SIZE as f32);
            let k2 = 2.0 / extent;
            // clip.x = (X - snapX) * 2/extent + 2*orgU - 1, with X = -p.right
            let clip_x = Vec4::new(-right.x, -right.y, -right.z, 0.0) * k2
                + Vec4::new(0.0, 0.0, 0.0, -snap.0 * k2 + 2.0 * org[k].0 - 1.0);
            let clip_y = Vec4::new(up.x, up.y, up.z, 0.0) * k2
                + Vec4::new(0.0, 0.0, 0.0, -snap.1 * k2 + 1.0 - 2.0 * org[k].1);
            let depth = Vec4::new(along.x, along.y, along.z, 0.0) * depth_scale
                + Vec4::new(0.0, 0.0, 0.0, depth_bias);
            let view_proj = Mat4::from_cols(
                Vec4::new(clip_x.x, clip_y.x, depth.x, 0.0),
                Vec4::new(clip_x.y, clip_y.y, depth.y, 0.0),
                Vec4::new(clip_x.z, clip_y.z, depth.z, 0.0),
                Vec4::new(clip_x.w, clip_y.w, depth.w, 1.0),
            );
            parts[k] = Some(Partition {
                view_proj,
                extent,
                offset_depth: [OFFSET_BIAS * 0.25 * depth_scale, OFFSET_SCALE, 0.0, 0.0],
                offset_color: [OFFSET_BIAS * 4.0 * depth_scale, 0.0, 0.0, 0.0],
            });
        }
        let [Some(near), Some(far)] = parts else {
            unreachable!()
        };

        // Near-partition lookup: u = (clip.x + 1) / 2, v = (1 - clip.y) / 4, both from the near matrix rows.
        let r = |m: &Mat4, i: usize| m.row(i);
        let u = (r(&near.view_proj, 0) + Vec4::W) * 0.5;
        let v = (Vec4::W - r(&near.view_proj, 1)) * 0.25;
        let d = r(&near.view_proj, 2);
        // w: distance along the view direction in units of the near reach.
        let reach = cam.z_near * SAMPLE_NEAR * to_texels;
        let wv = cam.forward / reach;
        let w = Vec4::new(wv.x, wv.y, wv.z, -cam.eye.dot(wv));
        let lookup = Mat4::from_cols(
            Vec4::new(u.x, v.x, d.x, w.x),
            Vec4::new(u.y, v.y, d.y, w.y),
            Vec4::new(u.z, v.z, d.z, w.z),
            Vec4::new(u.w, v.w, d.w, w.w),
        );
        let ratio_inv = 1.0 / RATIO;
        SunShadow {
            partitions: [near, far],
            lookup,
            switch_partition: [
                org[1].0 - org[0].0 * ratio_inv,
                (org[1].1 - org[0].1 * ratio_inv + 1.0) * 0.5,
                0.0,
                ratio_inv,
            ],
            scale: [16.0, 32.0, 0.0, 0.0],
            axes: [along, right, up],
        }
    }

    /// Viewport of partition `k` in the 1024x2048 map: `(x, y, width, height)`.
    pub fn viewport(k: usize) -> [f32; 4] {
        [0.0, (k as u32 * SIZE) as f32, SIZE as f32, SIZE as f32]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(eye: Vec3, yaw: f32) -> Camera {
        Camera {
            eye,
            forward: Vec3::new(yaw.cos(), yaw.sin(), 0.0),
            tan_half: [0.839, 0.472],
            z_near: 4.0,
        }
    }

    const SUN: Vec3 = Vec3::new(-0.32, -0.55, 0.77);

    fn shadow(eye: Vec3, yaw: f32) -> SunShadow {
        SunShadow::new(
            &camera(eye, yaw),
            SUN,
            Vec3::new(-4000.0, -4000.0, 0.0),
            Vec3::new(4000.0, 4000.0, 2000.0),
        )
    }

    fn look(s: &SunShadow, p: Vec3) -> Vec4 {
        s.lookup * p.extend(1.0)
    }

    #[test]
    fn points_ahead_of_the_eye_land_in_the_near_partition_with_a_growing_weight() {
        let eye = Vec3::new(100.0, 200.0, 50.0);
        let s = shadow(eye, 0.4);
        let f = camera(eye, 0.4).forward;
        let mut last = -1.0;
        for dist in [10.0, 60.0, 120.0, 180.0] {
            let l = look(&s, eye + f * dist);
            assert!(l.x > 0.0 && l.x < 1.0, "u {l:?}");
            assert!(l.y > 0.0 && l.y < 0.5, "v {l:?}");
            assert!(l.z > 0.0 && l.z < 1.0, "depth {l:?}");
            assert!(l.w > last);
            last = l.w;
        }
        // The shader blends from 0.75 and reads only the far partition past 1.
        assert!(look(&s, eye + f * 10.0).w < 0.2);
        assert!(look(&s, eye + f * 1000.0).w > 1.0);
    }

    #[test]
    fn the_far_lookup_agrees_with_the_far_projection() {
        let s = shadow(Vec3::new(-300.0, 120.0, 80.0), 2.0);
        let sw = s.switch_partition;
        for p in [
            Vec3::new(-250.0, 100.0, 30.0),
            Vec3::new(-100.0, 400.0, 10.0),
            Vec3::new(200.0, -100.0, 70.0),
        ] {
            let l = look(&s, p);
            let (u, v) = (l.x * sw[3] + sw[0], l.y * sw[3] + sw[1]);
            let c = s.partitions[1].view_proj * p.extend(1.0);
            let (fu, fv) = ((c.x + 1.0) * 0.5, 0.5 + (1.0 - c.y) * 0.25);
            assert!((u - fu).abs() < 1e-4, "u {u} vs {fu}");
            assert!((v - fv).abs() < 1e-4, "v {v} vs {fv}");
            assert!((l.z - c.z).abs() < 1e-5);
        }
    }

    #[test]
    fn the_projection_is_texel_stable_while_the_eye_moves_inside_a_grid_cell() {
        let texel = |s: &SunShadow, p: Vec3| {
            let l = look(s, p);
            (l.x * SIZE as f32, l.y * 2.0 * SIZE as f32)
        };
        let p = Vec3::new(40.0, -30.0, 12.0);
        let a = texel(&shadow(Vec3::new(0.3, 0.2, 50.0), 0.0), p);
        let b = texel(&shadow(Vec3::new(0.35, 0.25, 50.0), 0.0), p);
        // The texel of a fixed point moves only by whole texels.
        let whole = |d: f32| (d - d.round()).abs() < 1e-2;
        assert!(whole(a.0 - b.0) && whole(a.1 - b.1), "{a:?} {b:?}");
        let c = texel(&shadow(Vec3::new(2.3, 0.2, 50.0), 0.0), p);
        assert!(whole(c.0 - a.0) && whole(c.1 - a.1), "{a:?} {c:?}");
        assert!((c.0 - a.0).abs() + (c.1 - a.1).abs() > 1.0);
    }
}
