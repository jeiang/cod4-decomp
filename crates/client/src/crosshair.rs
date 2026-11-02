// SPDX-License-Identifier: GPL-3.0-only
// Crosshair visibility, colour and the cooked grenade's centre follow KisakCOD cgame/cg_draw_reticles.cpp
// (`AllowedToDrawCrosshair`, `CG_CalcCrosshairColor`, `CG_DrawReticleCenter`; GPL-3.0; copyright the KisakCOD
// contributors and Activision).
//! The hip-fire crosshair: the weapon's centre dot and four ticks that move out with the weapon's spread.
//!
//! The weapon file names the two materials, their sizes (in the 640x480 menu space), the smallest distance of a tick
//! from the centre and where on its own length a tick sits. The spread is the one the server shoots with
//! ([`sim::weapon::fire::aim_spread_degrees`]), turned into screen distance by the vertical field of view, so what
//! the ticks enclose is where the bullets go. It fades with the spread (a moving or just-fired weapon is less
//! certain) and away as the sights come up, where the sights themselves are the aim. Over a player it takes their
//! team's colour, and a cooked grenade's centre dot grows with the fuse.

use assets::zone::weapon::WeaponDef;
use render::TextureCache;
use render::ui2d::Ui2d;
use std::sync::Arc;

/// The stock `cg_crosshairAlpha` and `cg_crosshairAlphaMin`: the ticks never fade out completely.
pub const ALPHA: f32 = 1.0;
pub const MIN_ALPHA: f32 = 0.5;
/// Half the 480-high menu space: the spread's tangent times this over the vertical field of view's is the distance.
const HALF_HEIGHT: f32 = 240.0;

/// KisakCOD MP 1.8 `CG_DrawCrosshair` / `AllowedToDrawCrosshair` (115-197, 601-627).
/// These weapon-state values also apply to CoD4 MP 1.7; no 1.8-only states are used.
/// Reload start/end/interruption and offhand states deliberately remain allowed.
pub fn allowed(ps: &sim::pm::PlayerState, draw_gun: bool) -> bool {
    use sim::pm::weapon_state as ws;
    !(matches!(
        ps.weapon_state,
        ws::RAISING
            | ws::RAISING_ALTSWITCH
            | ws::DROPPING
            | ws::DROPPING_QUICK
            | ws::RELOADING
            | ws::MELEE_INIT
            | ws::MELEE_FIRE
            | ws::MELEE_END
    ) || ps.weapon_pos_frac == 1.0 && draw_gun)
}

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
    /// `cg_crosshairAlpha` and `cg_crosshairAlphaMin` (`CG_DrawCrosshair`: the alpha is scaled by the first and
    /// raised to the second); cheat dvars, stock 1 and 0.5.
    pub alpha_scale: f32,
    pub alpha_min: f32,
    /// The predicted fuse of a cooked grenade, ms (`grenadeTimeLeft`); 0 when none is cooking.
    pub grenade_time_left: i32,
    /// The weapon in hand has `crosshairColorChange`: the reticle may take a target's team colour.
    pub color_change: bool,
    /// The tint the reticle is drawn in, white until the HUD sets it from the crosshair's target ([`color`]).
    pub color: [f32; 3],
}

/// What tints the crosshair over a player (`CG_CalcCrosshairColor`): `cg_crosshairEnemyColor`, the name fade-ins
/// the target must have been held for, and the `g_TeamColor_MyTeam` and `g_TeamColor_EnemyTeam` colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub enemy_color: bool,
    pub friendly_fade_in: i32,
    pub enemy_fade_in: i32,
    pub my_team: [f32; 3],
    pub enemy_team: [f32; 3],
}

/// The reticle's colour: the enemy team's once an enemy in range has been under it for `enemy_fade_in` ms, the
/// viewer's team's once a teammate has for `friendly_fade_in`, otherwise white. `weap_flags` are the predicted
/// state's crosshair bits, `held` the target's `crosshairClientLastTime - crosshairClientStartTime`.
pub fn color(color_change: bool, weap_flags: u32, held: i32, t: &Tint) -> [f32; 3] {
    use sim::pm::wf;
    if !color_change {
        [1.0; 3]
    } else if weap_flags & wf::FRIEND_IN_CROSSHAIR != 0 {
        if held < t.friendly_fade_in {
            [1.0; 3]
        } else {
            t.my_team
        }
    } else if weap_flags & wf::ENEMY_IN_CROSSHAIR != 0 && t.enemy_color && held >= t.enemy_fade_in {
        t.enemy_team
    } else {
        [1.0; 3]
    }
}

/// What the cooked fuse adds to the centre's size (`CG_DrawReticleCenter`): a cook-off-hold grenade's dot is a unit
/// larger per 100 ms left in the fuse's current second, so it shrinks and jumps back once a second while held.
pub fn cook_growth(w: &WeaponDef, grenade_time_left: i32) -> f32 {
    /// `WEAPTYPE_GRENADE`.
    const GRENADE: i32 = 1;
    growth(
        w.weap_type == GRENADE && w.cook_off_hold != 0,
        grenade_time_left,
    )
}

fn growth(cooks: bool, grenade_time_left: i32) -> f32 {
    if cooks && grenade_time_left != 0 {
        (grenade_time_left % 1000) as f32 / 100.0
    } else {
        0.0
    }
}

/// One picture of the crosshair: where (virtual units, centre of the crosshair at the origin, y down), how it is
/// turned (degrees clockwise) and whether it is a tick or the centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub rect: [f32; 4],
    pub degrees: f32,
    pub tick: bool,
}

/// The centre picture's size in virtual units this frame, the cooked fuse's growth included; `None` without one.
pub fn center_size(r: &Reticle) -> Option<f32> {
    Style::of(&r.weapon)
        .center
        .map(|c| c + cook_growth(&r.weapon, r.grenade_time_left))
}

/// The pieces and their opacity; nothing when the crosshair is not drawn.
pub fn layout(r: &Reticle) -> (f32, Vec<Piece>) {
    let style = Style {
        center: center_size(r),
        ..Style::of(&r.weapon)
    };
    layout_of(
        &style,
        r.spread_deg,
        r.spread_scale,
        r.ads,
        r.tan_half_fov_y,
        (r.alpha_scale, r.alpha_min),
    )
}

/// The reticle's opacity before screen geometry is known (`CG_CalcCrosshairColor`).
pub fn alpha(r: &Reticle) -> f32 {
    alpha_of(r.spread_scale, r.ads, (r.alpha_scale, r.alpha_min))
}

/// What the crosshair came to on screen, as `cgis=crosshair_alpha` reports it: the drawn opacity to two places, or
/// `hidden` when there was no reticle to draw (refused by [`allowed`], such as fully aimed with the gun drawn, or
/// dead, flashbanged, `cg_drawCrosshair 0`).
pub fn shown(alpha: Option<f32>) -> String {
    alpha.map_or_else(|| "hidden".to_owned(), |a| format!("{a:.2}"))
}

fn alpha_of(spread_scale: f32, ads: f32, (alpha_scale, alpha_min): (f32, f32)) -> f32 {
    ((1.0 - ads) * alpha_scale * (1.0 - spread_scale / 255.0)).max(alpha_min)
}

fn layout_of(
    w: &Style,
    spread_deg: f32,
    spread_scale: f32,
    ads: f32,
    tan_half_fov_y: f32,
    alpha_cfg: (f32, f32),
) -> (f32, Vec<Piece>) {
    let alpha = alpha_of(spread_scale, ads, alpha_cfg);
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
            [r.color[0], r.color[1], r.color[2], alpha],
            p.degrees.to_radians(),
            [rect[0] + w * 0.5, rect[1] + h * 0.5],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_weapon_states_hide_only_the_named_actions() {
        use sim::pm::{PlayerState, weapon_state as ws};
        for state in 0..=ws::NIGHTVISION_REMOVE {
            let ps = PlayerState {
                weapon_state: state,
                ..PlayerState::default()
            };
            let hidden = [1, 2, 3, 4, 7, 12, 13, 14].contains(&state);
            assert_eq!(allowed(&ps, true), !hidden, "weapon state {state}");
        }
    }

    #[test]
    fn fully_aimed_crosshair_requires_hidden_viewmodel() {
        let mut ps = sim::pm::PlayerState::default();
        for frac in [0.0, 0.5, 0.99] {
            ps.weapon_pos_frac = frac;
            assert!(allowed(&ps, true));
        }
        ps.weapon_pos_frac = 1.0;
        assert!(!allowed(&ps, true));
        assert!(allowed(&ps, false));
    }
    const STYLE: Style = Style {
        center: Some(8.0),
        side: Some(16.0),
        min_ofs: 4.0,
        side_pos: 0.5,
    };

    /// 60 degrees vertical.
    fn lay(spread_deg: f32, scale: f32, ads: f32) -> (f32, Vec<Piece>) {
        layout_of(
            &STYLE,
            spread_deg,
            scale,
            ads,
            30f32.to_radians().tan(),
            (ALPHA, MIN_ALPHA),
        )
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
    fn the_minimum_alpha_applies_after_the_sights_scale() {
        let alpha = layout_of(&STYLE, 1.0, 0.0, 0.5, 30f32.to_radians().tan(), (0.0, 0.75)).0;
        assert!((alpha - 0.75).abs() < 1e-6);
    }

    #[test]
    fn the_sights_take_the_crosshair_away() {
        let gone = layout_of(&STYLE, 1.0, 0.0, 1.0, 30f32.to_radians().tan(), (1.0, 0.0));
        assert!(gone.1.is_empty());
        let half = lay(1.0, 0.0, 0.5).0;
        assert!((half - 0.5).abs() < 1e-6);
    }

    const TINT: Tint = Tint {
        enemy_color: true,
        friendly_fade_in: 0,
        enemy_fade_in: 250,
        my_team: [0.4, 0.6, 0.85],
        enemy_team: [0.75, 0.25, 0.25],
    };

    #[test]
    fn an_enemy_tints_the_crosshair_after_the_enemy_fade_in() {
        use sim::pm::wf::ENEMY_IN_CROSSHAIR as E;
        assert_eq!(color(true, E, 249, &TINT), [1.0; 3]);
        assert_eq!(color(true, E, 250, &TINT), TINT.enemy_team);
        assert_eq!(color(true, 0, 1000, &TINT), [1.0; 3]);
        // `cg_crosshairEnemyColor 0`, or a weapon without `crosshairColorChange`, stays white.
        let off = Tint {
            enemy_color: false,
            ..TINT
        };
        assert_eq!(color(true, E, 1000, &off), [1.0; 3]);
        assert_eq!(color(false, E, 1000, &TINT), [1.0; 3]);
    }

    #[test]
    fn a_teammate_tints_the_crosshair_after_the_friendly_fade_in_whatever_the_enemy_switch() {
        use sim::pm::wf::FRIEND_IN_CROSSHAIR as F;
        assert_eq!(color(true, F, 0, &TINT), TINT.my_team);
        let slow = Tint {
            friendly_fade_in: 100,
            enemy_color: false,
            ..TINT
        };
        assert_eq!(color(true, F, 99, &slow), [1.0; 3]);
        assert_eq!(color(true, F, 100, &slow), TINT.my_team);
        assert_eq!(color(false, F, 100, &slow), [1.0; 3]);
    }

    #[test]
    fn a_cooked_grenade_grows_the_centre_with_the_fuse_second() {
        assert_eq!(growth(true, 0), 0.0);
        assert_eq!(growth(true, 3500), 5.0);
        assert_eq!(growth(true, 2990), 9.9);
        assert_eq!(growth(true, 2000), 0.0);
        assert_eq!(growth(false, 3500), 0.0);
    }

    /// The `client-gfx` sequence: `cg_crosshairAlpha 0`, then `cg_crosshairAlphaMin 0.75` at the hip, then the
    /// sights fully up. Aimed with the gun drawn the stock gate hides the crosshair, which reports `hidden` rather
    /// than the floored alpha (#494); without the gun the floor still holds.
    #[test]
    fn the_report_shows_the_floor_at_the_hip_and_hidden_when_fully_aimed() {
        let tan = 30f32.to_radians().tan();
        let drawn = |ps: &sim::pm::PlayerState, draw_gun: bool, cfg| {
            shown(
                allowed(ps, draw_gun)
                    .then(|| layout_of(&STYLE, 1.0, 0.0, ps.weapon_pos_frac, tan, cfg).0),
            )
        };
        let mut ps = sim::pm::PlayerState::default();
        assert_eq!(drawn(&ps, true, (0.0, 0.0)), "0.00");
        assert_eq!(drawn(&ps, true, (0.0, 0.75)), "0.75");
        ps.weapon_pos_frac = 1.0;
        assert_eq!(drawn(&ps, true, (0.0, 0.75)), "hidden");
        assert_eq!(drawn(&ps, false, (0.0, 0.75)), "0.75");
    }
}
