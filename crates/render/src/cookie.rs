// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD (https://github.com/KisakCOD/KisakCOD, GPL-3.0): gfx_d3d/r_shadowcookie.cpp
// (R_GenerateShadowCookies, R_AddShadowCookie, R_EmitShadowCookieSurfs), gfx_d3d/rb_shadowcookie.cpp
// (RB_DrawShadowCookies, RB_SetShadowCookie), gfx_d3d/r_scene.cpp (R_DynamicShadowType). Original code by Infinity
// Ward / Activision, and the KisakCOD authors.
//! Shadow cookies: the blob shadows of players and other dynamic models while shadow maps are off (`sm_enable 0`,
//! `sc_enable 1`). Each caster is drawn as a silhouette seen from the sun into a small square tile of a cookie atlas;
//! the surfaces around it then darken where the cookie covers them.

use glam::{Mat4, Vec3, Vec4};

/// Edge of one cookie, in texels. The silhouette is drawn one texel inside so the edge stays clear.
pub const TILE: u32 = 128;
/// Cookies per frame at most: one atlas tile each. (`sc_count` allows 24; the picture of a frame this crowded is
/// worth less than its passes cost.)
pub const MAX: usize = 8;

/// What the planner needs to know of a caster.
#[derive(Clone, Copy, Debug)]
pub struct Caster {
    pub origin: Vec3,
    pub radius: f32,
}

/// One cookie: where it is, how to draw it and how to look it up.
#[derive(Clone, Copy, Debug)]
pub struct Cookie {
    /// Index into the caster list given to [`plan`].
    pub caster: usize,
    pub centre: Vec3,
    pub radius: f32,
    /// World position to clip space of the cookie's tile.
    pub view_proj: Mat4,
    /// World position to `(u, v, depth, w)` of the cookie's tile of the atlas.
    pub lookup: Mat4,
}

/// The cookies of the `max` casters nearest the eye. `to_sun` points from the scene toward the sun.
pub fn plan(casters: &[Caster], eye: Vec3, to_sun: Vec3, max: usize) -> Vec<Cookie> {
    let mut order: Vec<usize> = (0..casters.len())
        .filter(|&i| casters[i].radius > 0.0)
        .collect();
    order.sort_by(|&a, &b| {
        let d = |i: usize| casters[i].origin.distance(eye) - casters[i].radius;
        d(a).total_cmp(&d(b))
    });
    order.truncate(max.min(MAX));
    order
        .into_iter()
        .enumerate()
        .map(|(tile, i)| {
            let c = &casters[i];
            let vp = view_proj(c.origin, c.radius, to_sun);
            Cookie {
                caster: i,
                centre: c.origin,
                radius: c.radius,
                view_proj: vp,
                lookup: lookup(&vp, tile as u32, MAX as u32),
            }
        })
        .collect()
}

/// Orthographic projection along the light, `2 * radius` wide around `centre`; depth runs `radius` either side.
pub fn view_proj(centre: Vec3, radius: f32, to_sun: Vec3) -> Mat4 {
    let along = -to_sun.normalize();
    let up = if along.z.abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Z
    };
    let right = along.cross(up).normalize();
    let up = right.cross(along);
    let k = 1.0 / radius;
    // clip.x = (p - c) . right / r; clip.y = (p - c) . up / r; depth = ((p - c) . along + r) / 2r.
    let rows = [
        Vec4::new(
            right.x * k,
            right.y * k,
            right.z * k,
            -centre.dot(right) * k,
        ),
        Vec4::new(up.x * k, up.y * k, up.z * k, -centre.dot(up) * k),
        Vec4::new(
            along.x * k * 0.5,
            along.y * k * 0.5,
            along.z * k * 0.5,
            0.5 - centre.dot(along) * k * 0.5,
        ),
        Vec4::W,
    ];
    Mat4::from_cols(
        Vec4::new(rows[0].x, rows[1].x, rows[2].x, rows[3].x),
        Vec4::new(rows[0].y, rows[1].y, rows[2].y, rows[3].y),
        Vec4::new(rows[0].z, rows[1].z, rows[2].z, rows[3].z),
        Vec4::new(rows[0].w, rows[1].w, rows[2].w, rows[3].w),
    )
}

/// World position to `(u, v, depth, w)` of tile `index` of an atlas of `tiles` stacked tiles, inset by one texel so the
/// cookie's untouched border is what lookups outside it read.
pub fn lookup(view_proj: &Mat4, index: u32, tiles: u32) -> Mat4 {
    let n = tiles as f32;
    // The silhouette fills the inner TILE - 2 texels of the tile.
    let inner = (TILE as f32 - 2.0) / TILE as f32;
    let y_mid = (index as f32 + 0.5) / n;
    let (x_scale, y_scale) = (0.5 * inner, -0.5 * inner / n);
    let tile = Mat4::from_cols(
        Vec4::new(x_scale, 0.0, 0.0, 0.0),
        Vec4::new(0.0, y_scale, 0.0, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 0.0),
        Vec4::new(0.5, y_mid, 0.0, 1.0),
    );
    tile * *view_proj
}

/// The viewport `(x, y, w, h)` the silhouette of tile `index` is drawn in: the tile less a one texel border.
pub fn viewport(index: u32) -> [f32; 4] {
    [
        1.0,
        (index * TILE + 1) as f32,
        (TILE - 2) as f32,
        (TILE - 2) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(m: &Mat4, p: Vec3) -> Vec4 {
        let c = *m * p.extend(1.0);
        Vec4::new(c.x / c.w, c.y / c.w, c.z / c.w, c.w)
    }

    fn caster(x: f32) -> Caster {
        Caster {
            origin: Vec3::new(x, 0.0, 30.0),
            radius: 40.0,
        }
    }

    #[test]
    fn the_nearest_casters_get_the_tiles_in_order() {
        let casters = [caster(900.0), caster(100.0), caster(500.0), caster(-300.0)];
        let got = plan(&casters, Vec3::ZERO, Vec3::new(0.3, 0.2, 1.0), 3);
        assert_eq!(
            got.iter().map(|c| c.caster).collect::<Vec<_>>(),
            vec![1, 3, 2]
        );
        assert_eq!(plan(&casters, Vec3::ZERO, Vec3::Z, 0).len(), 0);
        let many: Vec<Caster> = (0..30).map(|i| caster(i as f32 * 10.0)).collect();
        assert_eq!(plan(&many, Vec3::ZERO, Vec3::Z, 99).len(), MAX);
    }

    #[test]
    fn a_casters_centre_is_the_middle_of_its_tile_and_its_shadow_falls_along_the_light() {
        let sun = Vec3::new(0.4, -0.3, 0.8).normalize();
        let c = caster(100.0);
        for (tile, cookie) in plan(&[c, caster(200.0)], Vec3::ZERO, sun, 2)
            .iter()
            .enumerate()
        {
            let at = project(&cookie.lookup, cookie.centre);
            assert!((at.x - 0.5).abs() < 1e-4, "{at:?}");
            let mid = (tile as f32 + 0.5) / MAX as f32;
            assert!((at.y - mid).abs() < 1e-4, "{at:?}");
            // Where the shadow lands, down the light, reads the same cookie texel.
            let lands = project(&cookie.lookup, cookie.centre - sun * 120.0);
            assert!((lands.x - at.x).abs() < 1e-4 && (lands.y - at.y).abs() < 1e-4);
            // Depth stays between the near and far plane over the caster.
            assert!((0.0..=1.0).contains(&at.z));
        }
    }

    #[test]
    fn the_caster_fills_the_tile_less_its_border() {
        let sun = Vec3::Z;
        let cookie = plan(&[caster(0.0)], Vec3::ZERO, sun, 1)[0];
        let edge = project(&cookie.lookup, cookie.centre + Vec3::Y * cookie.radius);
        let inner = (TILE as f32 - 2.0) / TILE as f32;
        assert!(
            ((edge.x - 0.5).abs() - 0.5 * inner).abs() < 1e-4,
            "{edge:?}"
        );
        let vp = viewport(1);
        assert_eq!(vp, [1.0, 129.0, 126.0, 126.0]);
    }
}
