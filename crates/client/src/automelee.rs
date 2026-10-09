// SPDX-License-Identifier: GPL-3.0-only
// Target selection and the view pull follow KisakCOD (GPL-3.0): aim_assist/aim_assist.cpp (AimAssist_UpdateScreenTargets,
// AimAssist_ConvertToClipBounds, AimAssist_GetBestTarget, AimAssist_ApplyMeleeCharge, AimAssist_ApplyAutoMelee) and
// aim_assist/aim_target_mp.cpp; copyright holders of the Call of Duty 4 source reconstruction and its contributors.
//! Melee aim assist (`aim_automelee_enabled`, on in multiplayer). A knife swing at an enemy near the middle of the
//! screen lunges toward them (`usercmd.meleeChargeYaw/Dist`, which `PM_MeleeChargeStart` consumes) and the view is
//! pulled gently onto them while the swing winds up.
//!
//! The targets are the enemies' bounding boxes projected to the screen; the best is the one nearest the crosshair that
//! is within range and overlaps the middle of the screen.

use glam::Vec3;
use sim::pm::math;

/// `aim_automelee_range`: how far away a target may be.
pub const RANGE: f32 = 128.0;
/// `aim_automelee_region_width/height` as a fraction of the screen: 320x240 of 640x480.
const REGION: [f32; 2] = [0.5, 0.5];
/// `aim_automelee_lerp`: how fast the view converges (per second, of the remaining angle).
const LERP: f32 = 40.0;
/// `aim_target_sentient_radius`: half the width of a player's box.
const RADIUS: f32 = 10.0;
/// What a player is shot at: how far up the box the chest is.
const CHEST: f32 = 0.75;

/// An enemy that could be the target of a swing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub number: u16,
    /// The feet.
    pub origin: Vec3,
    /// How tall the body is in its stance.
    pub height: f32,
}

/// A target as seen from the view (`AimScreenTarget`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub number: u16,
    /// Where the swing is aimed: the chest.
    pub aim: Vec3,
    /// The squared distance from the player's feet to the target's (`worldDistSqr`).
    pub dist_sqr: f32,
    /// The target's box on the screen, in clip space (-1..1 each way).
    pub mins: [f32; 2],
    pub maxs: [f32; 2],
    /// The squared distance of the box's centre from the middle of the screen.
    pub cross_sqr: f32,
}

/// What the camera is: where, which way (pitch, yaw, roll in degrees) and how wide it sees (tangents of the half
/// angles).
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: Vec3,
    pub feet: Vec3,
    pub angles: [f32; 3],
    pub tan_half_fov: [f32; 2],
}

/// The targets in view, the one nearest the crosshair first (`AimAssist_UpdateScreenTargets`).
pub fn screen_targets(view: &View, targets: &[Target]) -> Vec<Screen> {
    let (f, r, u) = math::angle_vectors(&view.angles);
    let (f, r, u) = (Vec3::from(f), Vec3::from(r), Vec3::from(u));
    let mut out: Vec<Screen> = targets
        .iter()
        .filter_map(|t| {
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            let mut any = false;
            for i in 0..8 {
                let corner = t.origin
                    + Vec3::new(
                        if i & 1 == 0 { -RADIUS } else { RADIUS },
                        if i & 2 == 0 { -RADIUS } else { RADIUS },
                        if i & 4 == 0 { 0.0 } else { t.height },
                    );
                // `AimAssist_XfmWorldPointToClipSpace`: a corner behind the eye is left out.
                let d = corner - view.eye;
                let depth = d.dot(f);
                if depth <= 0.0 {
                    continue;
                }
                any = true;
                let p = [
                    d.dot(r) / (depth * view.tan_half_fov[0]),
                    -d.dot(u) / (depth * view.tan_half_fov[1]),
                    1.0 - 1.0 / depth,
                ];
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            // `AimAssist_ConvertToClipBounds`: off the screen, or closer than the near plane, is not a target.
            if !any
                || lo[0] > 1.0
                || lo[1] > 1.0
                || lo[2] > 1.0
                || hi[0] < -1.0
                || hi[1] < -1.0
                || hi[2] < 0.0
            {
                return None;
            }
            let c = |v: f32| v.clamp(-1.0, 1.0);
            let (mins, maxs) = ([c(lo[0]), c(lo[1])], [c(hi[0]), c(hi[1])]);
            let (cx, cy) = ((mins[0] + maxs[0]) * 0.5, (mins[1] + maxs[1]) * 0.5);
            Some(Screen {
                number: t.number,
                aim: t.origin + Vec3::Z * (t.height * CHEST),
                dist_sqr: (t.origin - view.feet).length_squared(),
                mins,
                maxs,
                cross_sqr: cx * cx + cy * cy,
            })
        })
        .collect();
    out.sort_by(|a, b| a.cross_sqr.total_cmp(&b.cross_sqr));
    out
}

/// The nearest-the-crosshair target within `range` whose box overlaps the middle `region` of the screen
/// (`AimAssist_GetBestTarget`).
pub fn best(screens: &[Screen], range: f32, region: [f32; 2]) -> Option<&Screen> {
    screens.iter().find(|s| {
        s.dist_sqr <= range * range
            && region[0] >= s.mins[0]
            && s.maxs[0] >= -region[0]
            && region[1] >= s.mins[1]
            && s.maxs[1] >= -region[1]
    })
}

/// The lunge of a swing begun now (`AimAssist_ApplyMeleeCharge`): the yaw toward the target and how far away it is, or
/// nothing (distance 0) when there is none or it is within `melee_range` already.
pub fn charge(screens: &[Screen], eye: Vec3, melee_range: f32) -> (f32, u8) {
    match best(screens, RANGE, REGION) {
        Some(s) if s.dist_sqr >= melee_range * melee_range => {
            let to = s.aim - eye;
            (
                math::vec_to_yaw(&[to.x, to.y, 0.0]),
                s.dist_sqr.sqrt().min(255.0) as u8,
            )
        }
        _ => (0.0, 0),
    }
}

/// The view's pull onto a target while a swing winds up (`AimAssist_ApplyAutoMelee`).
#[derive(Default)]
pub struct AutoMelee {
    /// The target chosen at the start of the swing.
    target: Option<u16>,
    /// A swing's target has been looked for already.
    pressed: bool,
    /// The pitch and yaw the pull has brought the view to.
    at: [f32; 2],
}

impl AutoMelee {
    /// How far to turn the view this frame (pitch, yaw, degrees): while `meleeing` (the swing has begun) the view
    /// converges on the target picked when it began; once it ends the pull lets go.
    pub fn step(
        &mut self,
        meleeing: bool,
        screens: &[Screen],
        eye: Vec3,
        view: [f32; 2],
        dt: f32,
    ) -> [f32; 2] {
        if !meleeing {
            *self = Self::default();
            return [0.0; 2];
        }
        if !self.pressed {
            self.pressed = true;
            if let Some(s) = best(screens, RANGE, REGION) {
                self.target = Some(s.number);
                self.at = view;
            }
        }
        let Some(s) = self
            .target
            .and_then(|n| screens.iter().find(|s| s.number == n))
        else {
            self.target = None;
            return [0.0; 2];
        };
        let to = s.aim - eye;
        let goal = [
            -to.z.atan2(to.x.hypot(to.y)).to_degrees(),
            to.y.atan2(to.x).to_degrees(),
        ];
        let next = [0, 1].map(|i| track(goal[i], self.at[i], dt));
        let delta = [0, 1].map(|i| math::angle_delta(next[i], self.at[i]));
        self.at = next;
        delta
    }
}

/// `DiffTrackAngle`: `cur` a step of [`LERP`] toward `tgt` the short way round.
fn track(mut tgt: f32, cur: f32, dt: f32) -> f32 {
    while tgt - cur > 180.0 {
        tgt -= 360.0;
    }
    while tgt - cur < -180.0 {
        tgt += 360.0;
    }
    math::diff_track(tgt, cur, LERP, dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View {
            eye: Vec3::new(0.0, 0.0, 60.0),
            feet: Vec3::ZERO,
            angles: [0.0; 3],
            tan_half_fov: [1.0, 0.75],
        }
    }

    fn target(number: u16, x: f32, y: f32) -> Target {
        Target {
            number,
            origin: Vec3::new(x, y, 0.0),
            height: 70.0,
        }
    }

    #[test]
    fn the_target_nearest_the_crosshair_wins_and_the_far_and_the_aside_are_passed_over() {
        // Looking along +x: 100 ahead and a little aside, 100 ahead and further aside, 300 ahead (beyond the range).
        let t = [
            target(1, 100.0, 40.0),
            target(2, 100.0, 5.0),
            target(3, 300.0, 0.0),
        ];
        let s = screen_targets(&view(), &t);
        assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(best(&s, RANGE, REGION).map(|s| s.number), Some(2));
    }

    #[test]
    fn nobody_behind_or_far_to_the_side_is_a_target() {
        let t = [target(1, -100.0, 0.0), target(2, 20.0, 400.0)];
        let s = screen_targets(&view(), &t);
        assert!(best(&s, RANGE, REGION).is_none(), "{s:?}");
    }

    #[test]
    fn a_swing_lunges_only_at_a_target_beyond_arms_reach() {
        let t = [target(1, 100.0, 0.0)];
        let s = screen_targets(&view(), &t);
        let (yaw, dist) = charge(&s, view().eye, 64.0);
        assert_eq!(dist, 100);
        assert!(yaw.abs() < 0.01, "{yaw}");
        let near = screen_targets(&view(), &[target(1, 40.0, 0.0)]);
        assert_eq!(charge(&near, view().eye, 64.0), (0.0, 0));
        assert_eq!(charge(&[], view().eye, 64.0), (0.0, 0));
    }

    #[test]
    fn the_lunge_turns_toward_a_target_off_to_the_left() {
        let ahead_left = screen_targets(&view(), &[target(1, 100.0, 40.0)]);
        let (yaw, dist) = charge(&ahead_left, view().eye, 64.0);
        assert!(yaw > 10.0 && yaw < 30.0, "{yaw}");
        assert!((100..=110).contains(&dist), "{dist}");
    }

    #[test]
    fn the_pull_closes_on_the_target_picked_when_the_swing_began_and_lets_go_after() {
        let t = [target(1, 100.0, 40.0)];
        let s = screen_targets(&view(), &t);
        let mut pull = AutoMelee::default();
        let mut yaw = 0.0;
        for _ in 0..20 {
            let d = pull.step(true, &s, view().eye, [0.0, yaw], 0.01);
            assert!(d[1] >= 0.0, "turns toward the target (left, positive yaw)");
            yaw += d[1];
        }
        let want = (40.0f32 / 100.0).atan().to_degrees();
        assert!((yaw - want).abs() < 4.0, "turned {yaw}, target at {want}");
        // The swing ends: the pull lets go and forgets the target.
        assert_eq!(pull.step(false, &s, view().eye, [0.0, yaw], 0.01), [0.0; 2]);
        // A swing with nobody in view pulls nothing.
        assert_eq!(pull.step(true, &[], view().eye, [0.0, 0.0], 0.01), [0.0; 2]);
    }
}
