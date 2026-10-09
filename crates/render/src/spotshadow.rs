// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD (https://github.com/KisakCOD/KisakCOD, GPL-3.0): gfx_d3d/r_spotshadow.cpp
// (R_SetViewParmsForLight, R_GetSpotShadowLookupMatrix, R_AddSpotShadowsForLight), gfx_d3d/r_primarylights.cpp
// (R_ShadowedSpotLightScore, R_AddShadowedLightToShadowHistory, R_FadeOutShadowHistoryEntries). Original code by
// Infinity Ward / Activision, and the KisakCOD authors.
//! Spot light shadow maps: the lights that get one, how they fade in and out, and the matrices that render and read
//! them. The maps share one depth atlas of [`TILES`] square tiles, [`TILE`] texels each, stacked top to bottom.

use glam::{Mat4, Vec3, Vec4};

/// Edge of one shadow tile, in texels.
pub const TILE: u32 = 512;
/// Tiles in the atlas: the most spot shadows at once (`R_SPOTSHADOW_TILE_COUNT`).
pub const TILES: u32 = 4;
/// `spotShadowmapPixelAdjust` for 512 texel tiles in a four tile atlas.
pub const PIXEL_ADJUST: [f32; 4] = [1.0 / 2048.0, 1.0 / 4096.0, 1.0 / 1024.0, -1.0 / 8192.0];
/// `sm_polygonOffsetBias * 0.25` and `sm_polygonOffsetScale`, the depth bias of the map build.
pub const POLYGON_OFFSET: [f32; 4] = [0.125, 2.0, 0.0, 0.0];
/// `sm_lightScore_eyeProjectDist` and `sm_lightScore_spotProjectFrac`.
const SCORE_EYE_DIST: f32 = 64.0;
const SCORE_SPOT_FRAC: f32 = 0.125;

/// What the importance score of a spot light needs of it.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub origin: Vec3,
    /// The light's `dir`: pointing back at the light.
    pub dir: Vec3,
    pub radius: f32,
    pub color: Vec3,
}

/// `R_ShadowedSpotLightScore`: bright, large lights whose lit spot is near the middle of the view score high.
pub fn score(eye: Vec3, forward: Vec3, l: &Candidate) -> f32 {
    let eye_ref = eye + forward * SCORE_EYE_DIST;
    let to_light = l.origin - eye_ref;
    let focus = to_light + l.dir * (-l.radius * SCORE_SPOT_FRAC);
    let intensity = l.color.dot(Vec3::new(0.2989, 0.587, 0.114));
    l.radius * intensity / (focus.length() + 1.0)
}

/// World position to clip space of a light's shadow map: from the light along its shine direction, wide enough for the
/// outer cone, from `1 + near_bias` to the light's radius. `dir` points back at the light, like the light's constant.
pub fn view_proj(origin: Vec3, dir: Vec3, cos_outer: f32, radius: f32, near_bias: f32) -> Mat4 {
    let forward = -dir.normalize();
    let up = if forward.z.abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Z
    };
    // Game space: x forward, y left, z up. The camera's x is to the right, y up and z forward.
    let cx = forward.cross(up).normalize();
    let cy = cx.cross(forward);
    let rot = Mat4::from_cols(
        Vec4::new(cx.x, cy.x, forward.x, 0.0),
        Vec4::new(cx.y, cy.y, forward.y, 0.0),
        Vec4::new(cx.z, cy.z, forward.z, 0.0),
        Vec4::W,
    );
    let tan_half = (1.0 - cos_outer * cos_outer).max(0.0).sqrt() / cos_outer;
    let (zn, zf) = (1.0 + near_bias, radius);
    let proj = Mat4::from_cols(
        Vec4::new(1.0 / tan_half, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 1.0 / tan_half, 0.0, 0.0),
        Vec4::new(0.0, 0.0, zf / (zf - zn), 1.0),
        Vec4::new(0.0, 0.0, -zn * zf / (zf - zn), 0.0),
    );
    proj * rot * Mat4::from_translation(-origin)
}

/// World position to `(u, v, depth, w)` of tile `index` of the atlas: what the lit spot shadow shaders read as the shadow
/// lookup matrix.
pub fn lookup(view_proj: &Mat4, index: u32) -> Mat4 {
    let n = TILES as f32;
    let y1 = index as f32 / n;
    let y0 = 1.0 / n + y1;
    let (y_scale, y_shift) = ((y1 - y0) * 0.5, (y1 + y0) * 0.5);
    let tile = Mat4::from_cols(
        Vec4::new(0.5, 0.0, 0.0, 0.0),
        Vec4::new(0.0, y_scale, 0.0, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 0.0),
        Vec4::new(0.5, y_shift, 0.0, 1.0),
    );
    tile * *view_proj
}

/// The viewport `(x, y, w, h)` of tile `index` in the atlas.
pub fn viewport(index: u32) -> [f32; 4] {
    [0.0, (index * TILE) as f32, TILE as f32, TILE as f32]
}

/// A light that has a shadow map, or is fading out of having one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    /// The primary light's index, or a key the caller makes up for a light that is not one.
    pub id: u32,
    /// How strongly the shadow shows, 0 to 1.
    pub fade: f32,
}

/// `R_AddShadowedLightToShadowHistory` and `R_FadeOutShadowHistoryEntries`: the lights in `wanted` (best first) fade in
/// by `dt / fade_time`, those that are no longer wanted fade out and are dropped when almost gone; at most `max` lights
/// are kept, and a new one has to wait for room. A light that was already lit something the frame before fades in from
/// nothing; one that only just came into view starts at full strength. The tile of an entry is its position in
/// `history`.
pub fn update(
    history: &mut Vec<Entry>,
    wanted: &[u32],
    was_in_use: &[u32],
    dt: f32,
    fade_time: f32,
    max: usize,
) {
    let delta = if fade_time > 0.0 {
        (dt / fade_time).clamp(0.0, 1.0)
    } else {
        1.0
    };
    for e in history.iter_mut() {
        if wanted.contains(&e.id) {
            e.fade = (e.fade + delta).min(1.0);
        } else {
            e.fade -= delta;
        }
    }
    history.retain(|e| e.fade >= 0.01);
    for &id in wanted {
        if history.len() >= max.min(TILES as usize) {
            break;
        }
        if !history.iter().any(|e| e.id == id) {
            history.push(Entry {
                id,
                fade: if was_in_use.contains(&id) {
                    delta.max(0.01)
                } else {
                    1.0
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(m: &Mat4, p: Vec3) -> Vec4 {
        let c = *m * p.extend(1.0);
        Vec4::new(c.x / c.w, c.y / c.w, c.z / c.w, c.w)
    }

    fn cos(deg: f32) -> f32 {
        deg.to_radians().cos()
    }

    #[test]
    fn a_point_on_the_axis_lands_in_the_middle_of_its_tile_for_every_direction() {
        for dir in [
            Vec3::X,
            Vec3::Y,
            -Vec3::Z,
            Vec3::Z,
            Vec3::new(1.0, 1.0, 0.3).normalize(),
        ] {
            // `dir` points back at the light, so the light shines along -dir.
            let origin = Vec3::new(100.0, -50.0, 20.0);
            let vp = view_proj(origin, dir, cos(30.0), 400.0, 0.0);
            for tile in [0, 2] {
                let m = lookup(&vp, tile);
                let p = project(&m, origin - dir * 200.0);
                assert!((p.x - 0.5).abs() < 1e-4, "{dir:?} u {p:?}");
                let centre = (tile as f32 + 0.5) / TILES as f32;
                assert!((p.y - centre).abs() < 1e-4, "{dir:?} v {p:?}");
                assert!(p.z > 0.0 && p.z < 1.0, "{dir:?} depth {p:?}");
            }
        }
    }

    #[test]
    fn the_outer_cone_edge_maps_to_the_edge_of_the_tile_and_depth_runs_from_near_to_radius() {
        let origin = Vec3::ZERO;
        let vp = view_proj(origin, -Vec3::X, cos(30.0), 500.0, 0.0);
        let m = lookup(&vp, 1);
        // On the cone's edge, 100 along: 100 * tan(30) to the side.
        let side = project(&m, Vec3::new(100.0, 100.0 * 30f32.to_radians().tan(), 0.0));
        assert!(
            (side.x - 0.0).abs() < 1e-4 || (side.x - 1.0).abs() < 1e-4,
            "{side:?}"
        );
        let near = project(&m, Vec3::new(1.0, 0.0, 0.0));
        let far = project(&m, Vec3::new(500.0, 0.0, 0.0));
        assert!(
            near.z.abs() < 1e-4 && (far.z - 1.0).abs() < 1e-4,
            "{near:?} {far:?}"
        );
        // Outside the cone the lookup leaves the tile.
        let out = project(&m, Vec3::new(100.0, 90.0, 0.0));
        assert!(out.x < 0.0 || out.x > 1.0);
    }

    #[test]
    fn the_near_bias_moves_the_near_plane_out() {
        let vp = view_proj(Vec3::ZERO, -Vec3::X, cos(30.0), 500.0, 9.0);
        assert!(project(&lookup(&vp, 0), Vec3::new(10.0, 0.0, 0.0)).z.abs() < 1e-4);
    }

    #[test]
    fn a_bright_light_whose_spot_is_in_front_of_the_eye_outscores_a_dim_or_distant_one() {
        let eye = Vec3::ZERO;
        let l = |x: f32, y: f32, c: f32| Candidate {
            origin: Vec3::new(x, y, 0.0),
            dir: Vec3::X, // shines toward -x
            radius: 200.0,
            color: Vec3::splat(c),
        };
        // Lights above x = 200 shine toward the eye.
        let ahead = score(eye, Vec3::X, &l(260.0, 0.0, 1.0));
        let dim = score(eye, Vec3::X, &l(260.0, 0.0, 0.2));
        let aside = score(eye, Vec3::X, &l(260.0, 600.0, 1.0));
        assert!(ahead > dim && ahead > aside);
    }

    #[test]
    fn a_wanted_light_fades_in_over_the_fade_time_and_an_unwanted_one_fades_out_and_goes() {
        let mut h = Vec::new();
        update(&mut h, &[7], &[7], 0.25, 1.0, 4);
        assert_eq!(h.len(), 1);
        assert!((h[0].fade - 0.25).abs() < 1e-6);
        for _ in 0..3 {
            update(&mut h, &[7], &[7], 0.25, 1.0, 4);
        }
        assert_eq!(h[0].fade, 1.0);
        for _ in 0..3 {
            update(&mut h, &[], &[7], 0.25, 1.0, 4);
        }
        assert!((h[0].fade - 0.25).abs() < 1e-5);
        update(&mut h, &[], &[7], 0.25, 1.0, 4);
        assert!(h.is_empty());
    }

    #[test]
    fn no_more_lights_than_the_limit_get_a_map_and_the_best_keep_theirs() {
        let mut h = Vec::new();
        update(&mut h, &[1, 2, 3], &[1, 2, 3], 0.1, 1.0, 2);
        assert_eq!(h.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);
        // A better light waits while the others hold their maps, and takes the place of one that has faded out.
        update(&mut h, &[9, 1, 2], &[9, 1, 2], 0.1, 1.0, 2);
        assert_eq!(h.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);
        for _ in 0..3 {
            update(&mut h, &[9, 1], &[9, 1], 0.1, 1.0, 2);
        }
        assert_eq!(h.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 9]);
        // The atlas never holds more than its tiles.
        let mut big = Vec::new();
        update(&mut big, &[1, 2, 3, 4, 5, 6], &[], 0.1, 1.0, 99);
        assert_eq!(big.len(), TILES as usize);
    }

    #[test]
    fn a_light_that_just_came_into_view_has_its_shadow_at_once() {
        let mut h = Vec::new();
        update(&mut h, &[5], &[], 0.016, 1.0, 4);
        assert_eq!(h[0].fade, 1.0);
    }

    #[test]
    fn a_zero_fade_time_shows_the_shadow_at_once() {
        let mut h = Vec::new();
        update(&mut h, &[3], &[3], 0.016, 0.0, 4);
        assert_eq!(h[0].fade, 1.0);
    }
}
