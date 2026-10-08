// SPDX-License-Identifier: GPL-3.0-or-later
//! The hip-fire crosshair: the weapon's centre dot and four ticks that move out with the weapon's spread.
//!
//! The weapon file names the two materials, their sizes (in the 640x480 menu space), the smallest distance of a tick
//! from the centre and where on its own length a tick sits. The spread is the one the server shoots with
//! ([`sim::weapon::fire::aim_spread_degrees`]), turned into screen distance by the vertical field of view, so what
//! the ticks enclose is where the bullets go. It fades with the spread (a moving or just-fired weapon is less
//! certain) and away as the sights come up, where the sights themselves are the aim.

use assets::zone::weapon::WeaponDef;
use render::TextureCache;
use render::ui2d::Ui2d;
use std::sync::Arc;

/// The ticks never fade out completely.
const MIN_ALPHA: f32 = 0.5;
/// Half the 480-high menu space: the spread's tangent times this over the vertical field of view's is the distance.
const HALF_HEIGHT: f32 = 240.0;

/// The numbers of the weapon file the layout uses.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// Sizes of the centre and the ticks in virtual units; `None` where the weapon has no such picture.
    pub center: Option<f32>,
    pub side: Option<f32>,
    pub min_ofs: f32,
    pub side_pos: f32,
}

impl Style {
    pub fn of(w: &WeaponDef) -> Self {
        Self {
            center: w
                .reticle_center
                .is_some()
                .then_some(w.reticle_center_size as f32),
            side: w
                .reticle_side
                .is_some()
                .then_some(w.reticle_side_size as f32),
            min_ofs: w.reticle_min_ofs as f32,
            side_pos: w.hip_reticle_side_pos,
        }
    }
}

/// What the crosshair needs from the player's state this frame.
#[derive(Clone)]
pub struct Reticle {
    pub weapon: Arc<WeaponDef>,
    /// The spread the next bullet would be fired with, degrees.
    pub spread_deg: f32,
    /// `aim_spread_scale`: 0 when steady and aimed, 255 when the spread is at its widest.
    pub spread_scale: f32,
    /// How far the sights are up, 0 at the hip and 1 aimed.
    pub ads: f32,
    /// Tangent of half the vertical field of view.
    pub tan_half_fov_y: f32,
}

/// One picture of the crosshair: where (virtual units, centre of the crosshair at the origin, y down), how it is
/// turned (degrees clockwise) and whether it is a tick or the centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub rect: [f32; 4],
    pub degrees: f32,
    pub tick: bool,
}

/// The pieces and their opacity; nothing when the crosshair is not drawn.
pub fn layout(r: &Reticle) -> (f32, Vec<Piece>) {
    layout_of(
        &Style::of(&r.weapon),
        r.spread_deg,
        r.spread_scale,
        r.ads,
        r.tan_half_fov_y,
    )
}

fn layout_of(
    w: &Style,
    spread_deg: f32,
    spread_scale: f32,
    ads: f32,
    tan_half_fov_y: f32,
) -> (f32, Vec<Piece>) {
    let alpha = (1.0 - spread_scale / 255.0).max(MIN_ALPHA) * (1.0 - ads);
    if alpha < 0.01 || tan_half_fov_y <= 0.0 {
        return (0.0, Vec::new());
    }
    let mut out = Vec::with_capacity(5);
    if let Some(c) = w.center {
        out.push(Piece {
            rect: [-c * 0.5, -c * 0.5, c, c],
            degrees: 0.0,
            tick: false,
        });
    }
    if let Some(s) = w.side {
        let reach = (spread_deg.to_radians().tan() * HALF_HEIGHT / tan_half_fov_y).max(w.min_ofs);
        // From the tick's own length: the file says how far in from its inner end the spread is measured.
        let off = reach - w.side_pos * s;
        // The tick image is drawn pointing up: the top one as it is, the others turned.
        for (x, y, degrees) in [
            (-s * 0.5, -s - off, 0.0),
            (off, -s * 0.5, 90.0),
            (-s * 0.5, off, 180.0),
            (-s - off, -s * 0.5, 270.0),
        ] {
            out.push(Piece {
                rect: [x, y, s, s],
                degrees,
                tick: true,
            });
        }
    }
    (alpha, out)
}

/// Queues the crosshair on `g` for a window of `size` pixels (the 480-high menu space fills its height).
pub fn draw(g: &mut Ui2d, cache: &mut TextureCache, r: &Reticle, size: (u32, u32)) {
    let (alpha, pieces) = layout(r);
    let unit = size.1 as f32 / 480.0;
    let (cx, cy) = (size.0 as f32 * 0.5, size.1 as f32 * 0.5);
    for p in pieces {
        let Some(mat) = (if p.tick {
            r.weapon.reticle_side.as_deref()
        } else {
            r.weapon.reticle_center.as_deref()
        }) else {
            continue;
        };
        let img = g.image(cache, mat);
        let [x, y, w, h] = p.rect.map(|v| v * unit);
        let rect = [cx + x, cy + y, w, h];
        g.quad_rot(
            &img,
            rect,
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, alpha],
            p.degrees.to_radians(),
            [rect[0] + w * 0.5, rect[1] + h * 0.5],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const STYLE: Style = Style {
        center: Some(8.0),
        side: Some(16.0),
        min_ofs: 4.0,
        side_pos: 0.5,
    };

    /// 60 degrees vertical.
    fn lay(spread_deg: f32, scale: f32, ads: f32) -> (f32, Vec<Piece>) {
        layout_of(&STYLE, spread_deg, scale, ads, 30f32.to_radians().tan())
    }

    fn top_gap(r: &(f32, Vec<Piece>)) -> f32 {
        let p = &r.1;
        -(p.iter().find(|p| p.tick && p.degrees == 0.0).unwrap().rect[1] + 16.0)
    }

    #[test]
    fn a_steady_weapon_draws_the_centre_and_four_ticks_at_the_least_distance() {
        let r = lay(0.0, 0.0, 0.0);
        let (alpha, pieces) = (r.0, r.1.clone());
        assert_eq!(pieces.len(), 5);
        assert!((alpha - 1.0).abs() < 1e-6);
        // The minimum offset (4) less half a tick (8) puts the tick's inner end 4 units out of the centre.
        assert!((top_gap(&r) - -4.0).abs() < 1e-4, "{}", top_gap(&r));
        let turns: Vec<f32> = pieces
            .iter()
            .filter(|p| p.tick)
            .map(|p| p.degrees)
            .collect();
        assert_eq!(turns, [0.0, 90.0, 180.0, 270.0]);
    }

    #[test]
    fn the_ticks_enclose_the_cone_the_bullets_go_through() {
        // 6 degrees at a 60 degree field: tan(6) / tan(30) * 240 virtual units from the centre.
        let r = lay(6.0, 128.0, 0.0);
        let want = 6f32.to_radians().tan() / 30f32.to_radians().tan() * 240.0;
        assert!((top_gap(&r) + 8.0 - want).abs() < 0.01, "{}", top_gap(&r));
        // Wider spread, wider crosshair; and it fades a little.
        assert!(top_gap(&lay(8.0, 128.0, 0.0)) > top_gap(&r));
        let alpha = r.0;
        assert!((MIN_ALPHA..1.0).contains(&alpha));
    }

    #[test]
    fn the_sights_take_the_crosshair_away() {
        assert!(lay(1.0, 0.0, 1.0).1.is_empty());
        let half = lay(1.0, 0.0, 0.5).0;
        assert!((half - 0.5).abs() < 1e-6);
    }
}
