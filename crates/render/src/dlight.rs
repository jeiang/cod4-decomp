// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD (https://github.com/KisakCOD/KisakCOD, GPL-3.0): gfx_d3d/r_scene.cpp
// (R_AddOmniLightToScene, R_AddSpotLightToScene), gfx_d3d/r_light.cpp (R_GetPointLightPartitions,
// R_MostImportantLights, R_LightImportanceGreaterEqual, the BSP light-surface box tests). Original code by
// Infinity Ward / Activision, and the KisakCOD authors.
//! Dynamic lights: the omni and spot lights the effects add each frame (muzzle flashes, explosions, fire), which
//! brighten the surfaces inside their radius on top of the lit pass. This module holds what a frame decides about
//! them on the CPU: the cone of a spot light, which of the added lights are drawn (the `r_dlightLimit` most
//! important of those in view), and which surfaces a light reaches.

use crate::codeconst::LightConsts;
use crate::cull::Frustum;
use glam::Vec3;

/// The most lights a frame takes; later ones are dropped (`MAX_ADDED_DLIGHTS`).
pub const MAX_ADDED: usize = 32;
/// The most lights drawn at once whatever `r_dlightLimit` says.
pub const MAX_VISIBLE: usize = 4;

/// The cone of a spot light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotCone {
    /// The light's `dir`, as the lit spot technique reads it: pointing back at the light.
    pub dir: Vec3,
    pub cos_inner: f32,
    pub cos_outer: f32,
    /// How far the apex was moved back from the emitter: where the shadow map's near plane goes.
    pub near: f32,
}

/// A light the effects add for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynLight {
    pub origin: Vec3,
    /// Linear colour, 0..1 per channel (spot lights carry the brightness scale).
    pub color: Vec3,
    pub radius: f32,
    pub spot: Option<SpotCone>,
}

/// `r_spotLightStartRadius`, `r_spotLightEndRadius`, `r_spotLightFovInnerFraction` and `r_spotLightBrightness`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotParams {
    pub start_radius: f32,
    pub end_radius: f32,
    pub fov_inner_fraction: f32,
    pub brightness: f32,
}

impl Default for SpotParams {
    fn default() -> Self {
        SpotParams {
            start_radius: 36.0,
            end_radius: 196.0,
            fov_inner_fraction: 0.7,
            brightness: 14.0,
        }
    }
}

impl DynLight {
    /// `R_AddOmniLightToScene`; `None` for a light with no radius.
    pub fn omni(origin: Vec3, radius: f32, color: Vec3) -> Option<DynLight> {
        (radius > 0.0).then_some(DynLight {
            origin,
            color,
            radius,
            spot: None,
        })
    }

    /// `R_AddSpotLightToScene`: a spot light shining from `origin` along `dir` for `radius` units, as wide at its start
    /// as `params` say and as wide at its end. The cone's apex lies behind the origin, where it would be a point.
    pub fn spot(
        origin: Vec3,
        dir: Vec3,
        radius: f32,
        color: Vec3,
        params: &SpotParams,
    ) -> Option<DynLight> {
        if radius <= 0.0 {
            return None;
        }
        let dir = dir.try_normalize()?;
        let start = params.start_radius.max(0.0);
        // The end circle must be wider than the start and inside the light's reach, as the dvar handlers keep it.
        let mut end = params.end_radius;
        if start >= end {
            end = start + 0.1;
        }
        if end >= start + radius {
            end = start + radius - 0.1;
        }
        let outer = ((end - start) / radius).atan();
        if outer <= 0.0 {
            return None;
        }
        let inner = outer * params.fov_inner_fraction;
        let offset = start / outer.tan();
        let back = -dir;
        Some(DynLight {
            origin: origin + back * offset,
            color: color * params.brightness,
            radius: radius + offset,
            spot: Some(SpotCone {
                dir: back,
                cos_inner: inner.cos(),
                cos_outer: outer.cos(),
                near: offset,
            }),
        })
    }

    /// The constants the lit omni and spot techniques read.
    pub fn consts(&self, attenuation_width: f32, lookup_start: i32) -> LightConsts {
        let (scale, bias, dir) = match &self.spot {
            Some(c) => {
                let s = 1.0 / (c.cos_inner - c.cos_outer).max(1e-4);
                (s, -s * c.cos_outer, c.dir)
            }
            None => (0.0, 0.0, Vec3::ZERO),
        };
        LightConsts {
            origin: self.origin,
            dir,
            color: self.color,
            radius: self.radius,
            spot_factors: [scale, bias, 1.0, 0.0],
            falloff_placement: [
                attenuation_width * (1.0 / 512.0),
                0.0,
                lookup_start as f32 * (1.0 / 512.0),
                0.0,
            ],
        }
    }

    /// Whether the light reaches the box: its sphere does, and for a spot light its cone does too.
    pub fn reaches_box(&self, mins: Vec3, maxs: Vec3) -> bool {
        if box_dist_sq(self.origin, mins, maxs) > self.radius * self.radius {
            return false;
        }
        match &self.spot {
            Some(c) => cone_reaches_box(self.origin, c, mins, maxs),
            None => true,
        }
    }

    /// Whether the light reaches the sphere.
    pub fn reaches_sphere(&self, centre: Vec3, radius: f32) -> bool {
        self.reaches_box(centre - Vec3::splat(radius), centre + Vec3::splat(radius))
            && centre.distance_squared(self.origin) <= (self.radius + radius).powi(2)
    }
}

/// Squared distance from `p` to the box (zero inside).
pub fn box_dist_sq(p: Vec3, mins: Vec3, maxs: Vec3) -> f32 {
    let d = (mins - p).max(p - maxs).max(Vec3::ZERO);
    d.length_squared()
}

/// Whether the cone with its apex at `apex`, opening along `-c.dir` (a spot light's `dir` points back at the light),
/// can touch the box: tested against the box's bounding sphere, so it errs on the side of keeping a surface.
fn cone_reaches_box(apex: Vec3, c: &SpotCone, mins: Vec3, maxs: Vec3) -> bool {
    let centre = (mins + maxs) * 0.5;
    let r = (maxs - mins).length() * 0.5;
    let to = centre - apex;
    let dist = to.length();
    if dist <= r {
        return true;
    }
    let axis = -c.dir;
    let cos_to = axis.dot(to) / dist;
    // The sphere subtends asin(r / dist) around its centre direction.
    let angle_to = cos_to.clamp(-1.0, 1.0).acos();
    let spread = (r / dist).clamp(0.0, 1.0).asin();
    angle_to - spread <= c.cos_outer.clamp(-1.0, 1.0).acos()
}

/// The pixels `(x, y, width, height)` of a `size` target that the light's bounding box can reach: the scissor of its
/// pass. `clip_from_world` takes eye-relative positions to clip space (`View::clip_from_world` with the eye offset
/// applied by the caller: pass the light's origin relative to the eye). The whole target when the box reaches the eye's
/// plane, `None` when it is off screen.
pub fn screen_rect(
    clip_from_eye: &glam::Mat4,
    rel_origin: Vec3,
    radius: f32,
    size: (u32, u32),
) -> Option<[u32; 4]> {
    let full = [0, 0, size.0, size.1];
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for i in 0..8 {
        let corner = rel_origin
            + Vec3::new(
                if i & 1 == 0 { -radius } else { radius },
                if i & 2 == 0 { -radius } else { radius },
                if i & 4 == 0 { -radius } else { radius },
            );
        let c = *clip_from_eye * corner.extend(1.0);
        if c.w <= 1e-3 {
            return Some(full);
        }
        for (k, v) in [c.x / c.w, c.y / c.w].into_iter().enumerate() {
            lo[k] = lo[k].min(v);
            hi[k] = hi[k].max(v);
        }
    }
    if hi[0] < -1.0 || lo[0] > 1.0 || hi[1] < -1.0 || lo[1] > 1.0 {
        return None;
    }
    let to_px = |ndc: f32, n: u32| (ndc.clamp(-1.0, 1.0) + 1.0) * 0.5 * n as f32;
    let x0 = to_px(lo[0], size.0).floor() as u32;
    let x1 = (to_px(hi[0], size.0).ceil() as u32).min(size.0);
    // NDC y points up, pixel rows down.
    let y0 = (size.1 as f32 - to_px(hi[1], size.1)).floor().max(0.0) as u32;
    let y1 = ((size.1 as f32 - to_px(lo[1], size.1)).ceil() as u32).min(size.1);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// `R_LightImportanceGreaterEqual`: spot lights beat omni lights; otherwise the light that covers more of the view
/// (radius over distance) is the more important.
fn importance_ge(a: &DynLight, b: &DynLight, eye: Vec3) -> bool {
    if a.spot.is_some() != b.spot.is_some() {
        return a.spot.is_some();
    }
    let (ra, rb) = (a.radius * a.radius, b.radius * b.radius);
    let (da, db) = (eye.distance_squared(a.origin), eye.distance_squared(b.origin));
    rb * da <= ra * db
}

/// The lights to draw, most important first: those whose sphere is in `frustum` (`R_CullDynamicPointLightsInCameraView`),
/// from at most the first [`MAX_ADDED`] added, at most `limit` (clamped to [`MAX_VISIBLE`]) of them.
pub fn select(lights: &[DynLight], eye: Vec3, frustum: &Frustum, limit: usize) -> Vec<usize> {
    let limit = limit.min(MAX_VISIBLE);
    if limit == 0 {
        return Vec::new();
    }
    let mut visible: Vec<usize> = (0..lights.len().min(MAX_ADDED))
        .filter(|&i| {
            let l = &lights[i];
            !frustum.culls(l.origin - Vec3::splat(l.radius), l.origin + Vec3::splat(l.radius))
        })
        .collect();
    // A stable insertion sort by the pairwise importance: at most 32 lights.
    for i in 1..visible.len() {
        let mut j = i;
        while j > 0
            && !importance_ge(&lights[visible[j - 1]], &lights[visible[j]], eye)
        {
            visible.swap(j - 1, j);
            j -= 1;
        }
    }
    visible.truncate(limit);
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    /// A frustum of everything with `x < 1000` in front of the eye at the origin looking along +x.
    fn view() -> Frustum {
        let near_far = [Vec4::new(1.0, 0.0, 0.0, 0.0), Vec4::new(-1.0, 0.0, 0.0, 1000.0)];
        let sides = [
            Vec4::new(1.0, 1.0, 0.0, 0.0),
            Vec4::new(1.0, -1.0, 0.0, 0.0),
            Vec4::new(1.0, 0.0, 1.0, 0.0),
            Vec4::new(1.0, 0.0, -1.0, 0.0),
        ];
        Frustum {
            planes: near_far
                .into_iter()
                .chain(sides)
                .map(|p| p / p.truncate().length())
                .collect(),
        }
    }

    fn omni(at: [f32; 3], radius: f32) -> DynLight {
        DynLight::omni(Vec3::from(at), radius, Vec3::ONE).unwrap()
    }

    #[test]
    fn a_light_with_no_radius_is_not_a_light() {
        assert!(DynLight::omni(Vec3::ZERO, 0.0, Vec3::ONE).is_none());
        assert!(DynLight::spot(Vec3::ZERO, Vec3::X, 0.0, Vec3::ONE, &SpotParams::default()).is_none());
    }

    #[test]
    fn lights_out_of_view_are_dropped_and_the_rest_ordered_by_how_much_of_the_view_they_cover() {
        let lights = [
            omni([100.0, 0.0, 0.0], 50.0),    // close, small
            omni([-500.0, 0.0, 0.0], 100.0),  // behind the eye
            omni([400.0, 0.0, 0.0], 400.0),   // far, big: radius/dist = 1
            omni([200.0, 0.0, 0.0], 50.0),    // radius/dist = 0.25
            omni([100.0, 0.0, 0.0], 120.0),   // radius/dist = 1.2
        ];
        let got = select(&lights, Vec3::ZERO, &view(), 4);
        assert_eq!(got, vec![4, 2, 0, 3]);
    }

    #[test]
    fn the_limit_keeps_the_most_important_and_zero_draws_none() {
        let lights = [
            omni([100.0, 0.0, 0.0], 50.0),
            omni([100.0, 0.0, 0.0], 120.0),
            omni([300.0, 0.0, 0.0], 50.0),
        ];
        assert_eq!(select(&lights, Vec3::ZERO, &view(), 1), vec![1]);
        assert_eq!(select(&lights, Vec3::ZERO, &view(), 2), vec![1, 0]);
        assert!(select(&lights, Vec3::ZERO, &view(), 0).is_empty());
        // Whatever r_dlightLimit says, four is the most.
        let many: Vec<DynLight> = (0..10).map(|i| omni([100.0 + i as f32, 0.0, 0.0], 50.0)).collect();
        assert_eq!(select(&many, Vec3::ZERO, &view(), 99).len(), MAX_VISIBLE);
    }

    #[test]
    fn a_light_ahead_scissors_to_the_part_of_the_screen_it_covers() {
        use glam::{Mat4, Vec4};
        // x forward -> clip w; y left -> clip x (inverted); z up -> clip y. Aspect 1, 90 degree view.
        let m = Mat4::from_cols(
            Vec4::new(0.0, 0.0, 1.0, 1.0),
            Vec4::new(-1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::ZERO,
        );
        let size = (200, 100);
        // Ahead, 100 away, radius 10: a small box around the middle.
        let r = screen_rect(&m, Vec3::new(100.0, 0.0, 0.0), 10.0, size).unwrap();
        assert!(r[0] > 80 && r[0] + r[2] < 120, "{r:?}");
        assert!(r[1] > 40 && r[1] + r[3] < 60, "{r:?}");
        // Above the middle: the rectangle sits higher up the screen (smaller y).
        let up = screen_rect(&m, Vec3::new(100.0, 0.0, 50.0), 10.0, size).unwrap();
        assert!(up[1] + up[3] < r[1]);
        // Off to the side beyond the view.
        assert!(screen_rect(&m, Vec3::new(100.0, 500.0, 0.0), 10.0, size).is_none());
        // Around the eye: everything.
        assert_eq!(
            screen_rect(&m, Vec3::new(5.0, 0.0, 0.0), 50.0, size),
            Some([0, 0, 200, 100])
        );
    }

    #[test]
    fn a_spot_light_outranks_any_omni_light() {
        let spot = DynLight::spot(
            Vec3::new(300.0, 0.0, 0.0),
            -Vec3::X,
            80.0,
            Vec3::ONE,
            &SpotParams::default(),
        )
        .unwrap();
        let lights = [omni([100.0, 0.0, 0.0], 400.0), spot];
        assert_eq!(select(&lights, Vec3::ZERO, &view(), 1), vec![1]);
    }

    #[test]
    fn only_the_first_thirty_two_added_lights_are_considered() {
        let mut lights: Vec<DynLight> = (0..MAX_ADDED)
            .map(|_| omni([100.0, 0.0, 0.0], 10.0))
            .collect();
        lights.push(omni([100.0, 0.0, 0.0], 5000.0));
        let got = select(&lights, Vec3::ZERO, &view(), 4);
        assert!(!got.contains(&MAX_ADDED));
    }

    #[test]
    fn a_spot_light_starts_as_wide_as_asked_and_its_apex_lies_behind_the_emitter() {
        let p = SpotParams::default();
        let l = DynLight::spot(Vec3::new(10.0, 0.0, 0.0), Vec3::X, 300.0, Vec3::ONE, &p).unwrap();
        let c = l.spot.unwrap();
        // The cone opens along the shine direction, and its dir points back.
        assert_eq!(c.dir, -Vec3::X);
        let outer = c.cos_outer.acos();
        assert!(((p.end_radius - p.start_radius) / 300.0 - outer.tan()).abs() < 1e-4);
        // At the emitter the cone is start_radius wide.
        let apex_to_emitter = Vec3::new(10.0, 0.0, 0.0).distance(l.origin);
        assert!((apex_to_emitter * outer.tan() - p.start_radius).abs() < 1e-3);
        assert!((l.radius - (300.0 + apex_to_emitter)).abs() < 1e-3);
        assert!(c.cos_inner > c.cos_outer);
        assert_eq!(l.color, Vec3::splat(p.brightness));
    }

    #[test]
    fn a_spot_end_radius_that_cannot_work_is_pulled_in() {
        let p = SpotParams {
            start_radius: 50.0,
            end_radius: 20.0,
            ..SpotParams::default()
        };
        let l = DynLight::spot(Vec3::ZERO, Vec3::X, 100.0, Vec3::ONE, &p).unwrap();
        let c = l.spot.unwrap();
        assert!(c.cos_outer < 1.0 && c.cos_inner > c.cos_outer);
    }

    #[test]
    fn an_omni_light_reaches_what_is_within_its_radius() {
        let l = omni([0.0, 0.0, 0.0], 100.0);
        assert!(l.reaches_box(Vec3::new(50.0, -5.0, -5.0), Vec3::new(60.0, 5.0, 5.0)));
        assert!(l.reaches_box(Vec3::new(99.0, -500.0, -500.0), Vec3::new(300.0, 500.0, 500.0)));
        assert!(!l.reaches_box(Vec3::new(101.0, -5.0, -5.0), Vec3::new(160.0, 5.0, 5.0)));
        // A box containing the light is reached.
        assert!(l.reaches_box(Vec3::splat(-1.0), Vec3::splat(1.0)));
    }

    #[test]
    fn a_spot_light_reaches_only_what_is_inside_its_cone() {
        let l = DynLight::spot(Vec3::ZERO, Vec3::X, 300.0, Vec3::ONE, &SpotParams::default()).unwrap();
        let along = (Vec3::new(150.0, -5.0, -5.0), Vec3::new(160.0, 5.0, 5.0));
        let beside = (Vec3::new(10.0, 180.0, -5.0), Vec3::new(20.0, 190.0, 5.0));
        let behind = (Vec3::new(-100.0, -5.0, -5.0), Vec3::new(-90.0, 5.0, 5.0));
        assert!(l.reaches_box(along.0, along.1));
        assert!(!l.reaches_box(beside.0, beside.1));
        assert!(!l.reaches_box(behind.0, behind.1));
    }

    #[test]
    fn spot_constants_scale_the_cone_edge_to_zero_and_the_core_to_one() {
        let l = DynLight::spot(Vec3::ZERO, Vec3::X, 300.0, Vec3::ONE, &SpotParams::default()).unwrap();
        let c = l.consts(32.0, 0);
        let c0 = l.spot.unwrap();
        let at = |cos: f32| cos * c.spot_factors[0] + c.spot_factors[1];
        assert!(at(c0.cos_outer).abs() < 1e-4);
        assert!((at(c0.cos_inner) - 1.0).abs() < 1e-4);
        assert_eq!(c.dir, c0.dir);
    }
}
