// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `bgame/bg_weapons.cpp`
// (`BG_GetBobCycle`, `BG_GetVerticalBobFactor`, `BG_GetHorizontalBobFactor`) and `bgame/bg_misc.cpp`
// (`BG_GetPlayerViewOrigin`, `BG_GetSpeed`).
//! The walk bob of the eye and where the player's eye is: shared by the client's camera (which draws from it) and the
//! server (which starts shots from it).

use super::math::{add_lean_to_position, angle_vectors};
use super::{PlayerState, VIEW_CROUCH, VIEW_PRONE, pmf};

/// The bob dvars `BG_RegisterDvars` registers (cheat protected), with the stock multiplayer defaults: the most the
/// eye bobs and, per stance, the bob per unit of speed as (horizontal, vertical).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bob {
    /// `bg_bobMax`: the most the eye bobs, units (0 to 36).
    pub max: f32,
    /// `bg_bobAmplitudeStanding`.
    pub standing: [f32; 2],
    /// `bg_bobAmplitudeDucked`.
    pub ducked: [f32; 2],
    /// `bg_bobAmplitudeProne`.
    pub prone: [f32; 2],
    /// `bg_bobAmplitudeSprinting`.
    pub sprinting: [f32; 2],
}

impl Default for Bob {
    fn default() -> Self {
        Self {
            max: 8.0,
            standing: [0.007, 0.007],
            ducked: [0.0075, 0.0075],
            prone: [0.02, 0.005],
            sprinting: [0.02, 0.014],
        }
    }
}

impl Bob {
    /// The amplitude of the player's stance: (horizontal, vertical) per unit of bob speed.
    pub fn amplitude(&self, ps: &PlayerState) -> [f32; 2] {
        if ps.view_height_target == VIEW_PRONE {
            self.prone
        } else if ps.view_height_target == VIEW_CROUCH {
            self.ducked
        } else if ps.pm_flags & pmf::SPRINTING != 0 {
            self.sprinting
        } else {
            self.standing
        }
    }
}

/// The eye is never lower than this over the feet.
pub const MIN_EYE: f32 = 8.0;

/// `BG_GetBobCycle`: the player state's byte as a phase in radians.
pub fn bob_cycle(ps: &PlayerState) -> f32 {
    use std::f32::consts::TAU;
    f32::from(ps.bob_cycle) / 255.0 * TAU + TAU
}

/// `BG_GetSpeed`: the ground speed the bob is scaled by; on a ladder the climbing speed.
pub fn bob_speed(ps: &PlayerState, now: i32) -> f32 {
    if ps.pm_flags & pmf::LADDER == 0 {
        ps.velocity[0].hypot(ps.velocity[1])
    } else if now - ps.jump_time >= 500 {
        ps.velocity[2]
    } else {
        0.0
    }
}

/// `BG_GetVerticalBobFactor`: `max` caps the amplitude (`bg_bobMax` for the eye).
pub fn vertical_bob(ps: &PlayerState, bob: &Bob, cycle: f32, speed: f32, max: f32) -> f32 {
    let amp = (speed * bob.amplitude(ps)[1]).min(max);
    ((cycle * 4.0 + std::f32::consts::FRAC_PI_2).sin() * 0.2 + (cycle * 2.0).sin()) * 0.75 * amp
}

/// `BG_GetHorizontalBobFactor`.
pub fn horizontal_bob(ps: &PlayerState, bob: &Bob, cycle: f32, speed: f32, max: f32) -> f32 {
    cycle.sin() * (speed * bob.amplitude(ps)[0]).min(max)
}

/// `BG_GetPlayerViewOrigin`: the eye with its bob (capped at `bg_bobMax`) and lean, never lower than [`MIN_EYE`]
/// over the feet. `time` is the clock the ladder check of the bob speed reads (`ps.command_time` on the server).
pub fn view_origin(ps: &PlayerState, time: i32, bob: &Bob) -> [f32; 3] {
    let mut o = [
        ps.origin[0],
        ps.origin[1],
        ps.origin[2] + ps.view_height_current,
    ];
    let (cycle, speed) = (bob_cycle(ps), bob_speed(ps, time));
    o[2] += vertical_bob(ps, bob, cycle, speed, bob.max);
    let side = horizontal_bob(ps, bob, cycle, speed, bob.max);
    let (_, right, _) = angle_vectors(&ps.viewangles);
    for (o, r) in o.iter_mut().zip(right) {
        *o += side * r;
    }
    add_lean_to_position(&mut o, ps.viewangles[1], ps.leanf, 16.0, 20.0);
    o[2] = o[2].max(ps.origin[2] + MIN_EYE);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> PlayerState {
        PlayerState {
            origin: [10.0, 20.0, 30.0],
            view_height_current: 60.0,
            view_height_target: super::super::VIEW_STAND,
            velocity: [190.0, 0.0, 0.0],
            ..PlayerState::default()
        }
    }

    #[test]
    fn standing_still_the_eye_is_the_view_height_over_the_feet() {
        let ps = PlayerState {
            velocity: [0.0; 3],
            ..running()
        };
        assert_eq!(view_origin(&ps, 0, &Bob::default()), [10.0, 20.0, 90.0]);
    }

    #[test]
    fn running_bobs_the_eye_within_the_cap_and_a_lean_shifts_it_sideways() {
        let mut ps = running();
        let bob = Bob::default();
        let max = bob.max;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for c in 0..=255u8 {
            ps.bob_cycle = c;
            let z = view_origin(&ps, 0, &bob)[2];
            lo = lo.min(z);
            hi = hi.max(z);
        }
        assert!(hi - lo > 0.5 && hi - lo < 2.0 * max, "{lo} {hi}");
        assert!(lo >= 30.0 + 60.0 - max && hi <= 30.0 + 60.0 + max);
        ps.leanf = 0.5;
        ps.velocity = [0.0; 3];
        let o = view_origin(&ps, 0, &bob);
        // Facing +x, leaning right moves the eye along -y.
        assert!((o[1] - 20.0 + 15.0).abs() < 0.5, "{o:?}");
    }

    /// The span of the eye's height over one bob cycle.
    fn bob_span(ps: &mut PlayerState, bob: &Bob) -> f32 {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for c in 0..=255u8 {
            ps.bob_cycle = c;
            let z = view_origin(ps, 0, bob)[2];
            lo = lo.min(z);
            hi = hi.max(z);
        }
        hi - lo
    }

    #[test]
    fn the_bob_dvars_scale_and_cap_the_eye_bob() {
        let mut ps = running();
        let stock = bob_span(&mut ps, &Bob::default());
        // `bg_bobMax 0`: the eye does not bob.
        let still = Bob {
            max: 0.0,
            ..Bob::default()
        };
        assert_eq!(bob_span(&mut ps, &still), 0.0);
        // Twice the standing amplitude, still under the cap, bobs twice as far.
        let double = Bob {
            standing: [0.014, 0.014],
            ..Bob::default()
        };
        let doubled = bob_span(&mut ps, &double);
        assert!((doubled - 2.0 * stock).abs() < 0.01, "{stock} {doubled}");
        // A small cap holds it.
        let capped = Bob {
            max: 0.5,
            standing: [1.0, 1.0],
            ..Bob::default()
        };
        assert!(bob_span(&mut ps, &capped) <= 2.0 * 0.5 * 0.75 * 1.2 + 1e-3);
    }

    #[test]
    fn the_eye_is_never_lower_than_a_step_over_the_feet() {
        let ps = PlayerState {
            view_height_current: 2.0,
            velocity: [0.0; 3],
            ..running()
        };
        assert_eq!(view_origin(&ps, 0, &Bob::default())[2], 30.0 + MIN_EYE);
    }
}
