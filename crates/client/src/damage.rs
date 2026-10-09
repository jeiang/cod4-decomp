// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame/cg_playerstate.cpp`
// (`CG_DamageFeedback`), `cgame/cg_draw_indicators.cpp` (`CG_DrawFlashDamage`, `CG_DrawDamageDirectionIndicators`)
// and `cgame/cg_view.cpp` (the damage kick of the view).
//! What the player's own screen does when a hit lands: the view turns toward the blow and settles back, the screen
//! flashes red, and a wedge points at where the blow came from for a couple of seconds.
//!
//! The server announces a hit by counting up `damage_event` and setting `damage_count` (the percent of the health
//! it took), `damage_yaw` and `damage_pitch` (where it came from; see [`sim::pm::damage`]). Each change of the
//! event is seen once.

use fx::Rng;
use sim::pm::PlayerState;
use sim::pm::damage::{VIEW_KICK_MS, direction_of, view_kick};
use sim::pm::math::{angle_normalize_360, vec_to_yaw};

/// Hits whose wedges can be on screen at once.
const SLOTS: usize = 8;
/// `cg_hudDamageIconTime`: how long a wedge stays, ms.
const ICON_MS: i32 = 2000;
/// The wedge is drawn this wide and high, and this far from the screen centre, in 640x480 menu units
/// (`cg_hudDamageIconWidth`, `Height` and `Offset`).
pub const ICON_SIZE: [f32; 2] = [128.0, 64.0];
pub const ICON_OFFSET: f32 = 128.0;
/// The material of the wedge.
pub const ICON_MATERIAL: &str = "hit_direction";

#[derive(Debug, Clone, Copy, Default)]
struct Wedge {
    time: i32,
    /// Degrees: the way the blow travelled, blurred a little so repeated hits do not stack exactly.
    yaw: f32,
}

#[derive(Debug, Clone)]
pub struct DamageView {
    /// `damage_event` of the state as of the last look; `None` before the first.
    seen: Option<u8>,
    /// Whose state was looked at last: another player's events are not this one's to compare with.
    who: u16,
    /// The kick of the last hit, degrees at full strength: how strong the red flash is.
    pitch: f32,
    /// The server time the red flash ends at, and the server time of the last hit (0: none yet).
    flash_end: i32,
    hit_time: i32,
    wedges: [Wedge; SLOTS],
    rng: Rng,
}

impl Default for DamageView {
    fn default() -> Self {
        DamageView {
            seen: None,
            who: u16::MAX,
            pitch: 0.0,
            flash_end: 0,
            hit_time: 0,
            wedges: [Wedge::default(); SLOTS],
            rng: Rng::new(0xDA3A),
        }
    }
}

/// What the HUD draws of the hits this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DamageHud {
    /// Opacity of the red screen flash, 0 for none.
    pub flash: f32,
    /// Each wedge: degrees clockwise to turn the icon (which points down at zero) about the screen centre, and its
    /// opacity.
    pub wedges: Vec<(f32, f32)>,
}

impl DamageView {
    /// Forgets every hit (the view is not the own living player's).
    pub fn clear(&mut self) {
        *self = DamageView {
            rng: self.rng.clone(),
            who: self.who,
            ..DamageView::default()
        };
    }

    /// Takes in a hit that `ps` newly shows (`CG_TransitionPlayerState` and `CG_DamageFeedback`). `now` is the server
    /// time, `view` the view's pitch and yaw in degrees, which the kick and the wedge are worked out against.
    pub fn look(&mut self, ps: &PlayerState, now: i32, view: [f32; 2]) {
        if self.who != ps.client_num {
            // Another player's state (a spectator following someone else): learn it, do not kick for it.
            self.clear();
            self.who = ps.client_num;
        }
        let event = ps.damage_event;
        let Some(last) = self.seen.replace(event) else {
            return;
        };
        if event == last || ps.damage_count == 0 {
            return;
        }
        let kick = view_kick(ps.damage_count);
        match direction_of(ps.damage_pitch, ps.damage_yaw) {
            None => {
                self.pitch = -kick;
            }
            Some(dir) => {
                let (forward, _, _) = sim::pm::math::angle_vectors(&[view[0], view[1], 0.0]);
                let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                self.pitch = kick * dot(dir, forward);
                // The oldest slot is the one given up.
                let slot = (0..SLOTS).min_by_key(|&i| self.wedges[i].time).unwrap_or(0);
                let yaw = vec_to_yaw(&dir);
                self.wedges[slot] = Wedge {
                    time: now,
                    yaw: angle_normalize_360((self.rng.f() - 0.5) * 20.0 + yaw),
                };
            }
        }
        self.flash_end = now + VIEW_KICK_MS;
        self.hit_time = now;
    }

    /// The flash and the wedges at `now` for a view looking along `view_yaw` degrees.
    pub fn hud(&self, now: i32, view_yaw: f32) -> DamageHud {
        let mut out = DamageHud::default();
        if self.flash_end > now {
            // The red is strongest for a hit that kicks the view up or down a lot, and fades with it.
            let left = (self.flash_end - now) as f32 * self.pitch / VIEW_KICK_MS as f32;
            out.flash = left.abs().min(5.0) / 5.0 * 0.7;
        }
        for w in &self.wedges {
            let t = now - w.time;
            if w.time != 0 && t > 0 && t < ICON_MS {
                // Fully there for the first half, then fading out.
                let alpha = (2.0 - 2.0 * t as f32 / ICON_MS as f32).min(1.0);
                out.wedges.push((view_yaw - w.yaw, alpha));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(ps: &mut PlayerState, percent: i32, from: Option<[f32; 3]>) {
        let (pitch, yaw) = sim::pm::damage::direction_bytes(from);
        ps.damage_event = ps.damage_event.wrapping_add(1);
        ps.damage_count = percent;
        ps.damage_pitch = pitch;
        ps.damage_yaw = yaw;
    }

    /// The screen position of the wedge's middle, in pixels right and down of the centre, as the icon is turned.
    fn wedge_at(turn_deg: f32) -> [f32; 2] {
        let r = ICON_OFFSET + ICON_SIZE[1] * 0.5;
        let (s, c) = turn_deg.to_radians().sin_cos();
        [-s * r, c * r]
    }

    #[test]
    fn a_hit_from_the_right_draws_the_wedge_on_the_right() {
        let mut d = DamageView::default();
        let mut ps = PlayerState::default();
        // Looking along +x (yaw 0); the shooter on the right (-y) drives the blow towards +y.
        d.look(&ps, 1000, [0.0, 0.0]);
        hit(&mut ps, 40, Some([0.0, 1.0, 0.0]));
        d.look(&ps, 1000, [0.0, 0.0]);
        let hud = d.hud(1100, 0.0);
        assert_eq!(hud.wedges.len(), 1);
        let [x, y] = wedge_at(hud.wedges[0].0);
        assert!(x > 50.0 && y.abs() < 40.0, "wedge at {x}, {y}");
        // A blow from behind (driving forward) puts it at the bottom.
        let mut e = DamageView::default();
        let mut ps = PlayerState::default();
        e.look(&ps, 1000, [0.0, 0.0]);
        hit(&mut ps, 40, Some([1.0, 0.0, 0.0]));
        e.look(&ps, 1000, [0.0, 0.0]);
        let [x, y] = wedge_at(e.hud(1100, 0.0).wedges[0].0);
        assert!(y > 50.0 && x.abs() < 40.0, "wedge at {x}, {y}");
    }

    #[test]
    fn a_hit_is_seen_once_and_everything_fades() {
        let mut d = DamageView::default();
        let mut ps = PlayerState::default();
        d.look(&ps, 0, [0.0; 2]);
        hit(&mut ps, 30, Some([1.0, 0.0, 0.0]));
        d.look(&ps, 500, [0.0; 2]);
        // The same state again is not another hit.
        d.look(&ps, 700, [0.0; 2]);
        assert_eq!(d.hit_time, 500);
        assert!(d.hud(600, 0.0).flash > 0.0);
        assert_eq!(d.hud(1100, 0.0).flash, 0.0);
        assert_eq!(d.hud(500 + ICON_MS, 0.0).wedges.len(), 0);
    }

    #[test]
    fn a_hit_with_no_direction_kicks_up_and_draws_no_wedge() {
        let mut d = DamageView::default();
        let mut ps = PlayerState::default();
        d.look(&ps, 0, [0.0; 2]);
        hit(&mut ps, 50, None);
        d.look(&ps, 100, [0.0; 2]);
        assert!(d.hud(200, 0.0).wedges.is_empty());
    }

    #[test]
    fn a_change_of_followed_player_is_not_a_hit() {
        let mut d = DamageView::default();
        let mut ps = PlayerState::default();
        d.look(&ps, 0, [0.0; 2]);
        // Now watching someone whose counter reads 7.
        ps.client_num = 3;
        ps.damage_event = 7;
        ps.damage_count = 40;
        d.look(&ps, 100, [0.0; 2]);
        assert_eq!(d.hit_time, 0);
        // Their next hit is shown.
        hit(&mut ps, 40, Some([1.0, 0.0, 0.0]));
        d.look(&ps, 200, [0.0; 2]);
        assert_eq!(d.hit_time, 200);
    }

    #[test]
    fn a_state_that_shows_no_damage_count_is_not_a_hit() {
        let mut d = DamageView::default();
        let mut ps = PlayerState::default();
        d.look(&ps, 0, [0.0; 2]);
        hit(&mut ps, 0, Some([1.0, 0.0, 0.0]));
        d.look(&ps, 10, [0.0; 2]);
        assert_eq!(d.hit_time, 0);
    }
}
