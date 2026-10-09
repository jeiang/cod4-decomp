// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame_mp/cg_view_mp.cpp`
// (`CG_UpdateSceneDepthOfField`, `CG_UpdateAdsDof`, `CG_UpdateAdsDofValue`, `CG_UpdateHelicopterKillCamDof`,
// `CG_UpdateAirstrikeKillCamDof`) and `cgame/cg_weapons.cpp` (the view model's range).
//! The depth of field of the picture: the blur the scripts ask for (`setdepthoffield`), the blur of aiming down the
//! sights, the kill cams' and the first-person weapon's own.
//!
//! Distances are world units from the eye and blur radii virtual 640x480 pixels, as [`render::Dof`] has them.

use render::Dof;
use sim::pm::{PlayerState, PmType};

/// The farthest the aim-down-sights blur looks for something to focus on.
pub const ADS_TRACE: f32 = 8192.0;

/// A hip-fired view blurs nothing: the near blur is the weakest the renderer accepts and the far blur is off.
const REST: Dof = Dof {
    view_model_start: 0.0,
    view_model_end: 0.0,
    near_start: 0.0,
    near_end: 0.0,
    far_start: 5000.0,
    far_end: 5000.0,
    near_blur: 6.0,
    far_blur: 0.0,
};

/// `CG_UpdateAdsDofValue`: `current` moved toward `target` by half the gap (at least one unit), no faster than
/// `max_change` per 50 ms of `dt` seconds.
fn approach(current: f32, target: f32, max_change: f32, dt: f32) -> f32 {
    let cap = max_change / 0.05 * dt;
    let half = (target - current).abs() * 0.5;
    let step = if cap >= half { half.max(1.0) } else { cap };
    if target >= current {
        target.min(current + step)
    } else {
        target.max(current - step)
    }
}

/// The blur of aiming down the sights, which eases to its focus while the weapon is fully aimed.
#[derive(Debug, Clone, Copy)]
pub struct AdsDof {
    current: Dof,
}

impl Default for AdsDof {
    fn default() -> Self {
        Self { current: REST }
    }
}

impl AdsDof {
    /// `CG_UpdateAdsDof`: the blur for a view with the weapon `frac` of the way to the sights (0 hip, 1 aimed) and
    /// something `trace` units ahead of the eye (`ADS_TRACE` when nothing is). The original focuses on the players on
    /// screen; with no list of them here, the focus is the middle of the screen's distance alone: sharp from just
    /// in front of what the sights point at out to 2500 units, closer than 256 and farther blurred.
    pub fn update(&mut self, ps: &PlayerState, trace: f32, dt: f32) -> Dof {
        let frac = ps.weapon_pos_frac;
        let dead = ps.pm_type >= PmType::Dead;
        if frac == 0.0 && !dead {
            self.current = REST;
            return REST;
        }
        let (mut near_end, mut far_start): (f32, f32) = (256.0, 2500.0);
        if trace < near_end {
            near_end = trace - 30.0;
        }
        near_end = near_end.max(1.0);
        far_start = far_start.max(trace);
        let (near_start, far_end) = (1.0, far_start * 4.0);
        let (near_blur, far_blur) = (6.0, 0.0);
        let c = &mut self.current;
        if frac == 1.0 || dead {
            c.near_start = approach(c.near_start, near_start, 50.0, dt);
            c.near_end = approach(c.near_end, near_end, 50.0, dt);
            c.far_start = approach(c.far_start, far_start, 400.0, dt);
            c.far_end = approach(c.far_end, far_end, 400.0, dt);
            c.near_blur = approach(c.near_blur, near_blur, 0.1, dt);
            c.far_blur = approach(c.far_blur, far_blur, 0.1, dt);
        } else {
            let mix = |aimed: f32, hip: f32| frac * aimed + (1.0 - frac) * hip;
            c.near_start = mix(near_start, REST.near_start);
            c.near_end = mix(near_end, REST.near_end);
            c.far_start = mix(far_start, REST.far_start);
            c.far_end = mix(far_end, REST.far_end);
            c.near_blur = mix(near_blur, REST.near_blur);
            c.far_blur = mix(far_blur, REST.far_blur);
        }
        *c
    }
}

/// `CG_UpdateSceneDepthOfField`: the blur the scripts gave `snap`'s player, if they gave a range (all four distances
/// zero hands the view to the aim-down-sights blur). The first-person weapon's range is [`view_model`]'s.
pub fn scripted(snap: &PlayerState) -> Option<Dof> {
    ([
        snap.dof_near_start,
        snap.dof_near_end,
        snap.dof_far_start,
        snap.dof_far_end,
    ] != [0.0; 4])
        .then_some(Dof {
            near_start: snap.dof_near_start,
            near_end: snap.dof_near_end,
            far_start: snap.dof_far_start,
            far_end: snap.dof_far_end,
            near_blur: snap.dof_near_blur,
            far_blur: snap.dof_far_blur,
            ..REST
        })
}

/// The range the first-person weapon is blurred over: the scripts' (`setviewmodeldepthoffield`) when hip-fired, the
/// weapon's own (`adsDofStart`, `adsDofEnd`) when aimed.
pub fn view_model(ps: &PlayerState, ads: Option<(f32, f32)>) -> (f32, f32) {
    let (a_start, a_end) = ads.unwrap_or((ps.dof_viewmodel_start, ps.dof_viewmodel_end));
    let f = ps.weapon_pos_frac;
    (
        (a_start - ps.dof_viewmodel_start) * f + ps.dof_viewmodel_start,
        (a_end - ps.dof_viewmodel_end) * f + ps.dof_viewmodel_end,
    )
}

/// `CG_UpdateHelicopterKillCamDof` (the `cg_heliKillCam*Blur*` defaults): `distance` is from the point the camera
/// looks from to the victim, `pull` how far the camera hangs behind it.
pub fn helicopter_kill_cam(distance: f32, pull: f32) -> Dof {
    kill_cam(pull + distance, 100.0, 300.0, 0.0, 100.0)
}

/// `CG_UpdateAirstrikeKillCamDof`: `distance` from the camera to the victim.
pub fn airstrike_kill_cam(distance: f32) -> Dof {
    kill_cam(distance, 100.0, 300.0, 0.0, 100.0)
}

fn kill_cam(focus: f32, far_start: f32, far_dist: f32, near_start: f32, near_end: f32) -> Dof {
    Dof {
        view_model_start: 0.0,
        view_model_end: 0.0,
        near_start,
        near_end: focus - near_end,
        far_start: focus + far_start,
        far_end: focus + far_start + far_dist,
        near_blur: 4.0,
        far_blur: 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aiming(frac: f32) -> PlayerState {
        PlayerState {
            weapon_pos_frac: frac,
            ..PlayerState::default()
        }
    }

    #[test]
    fn a_hip_fired_view_is_sharp() {
        let mut ads = AdsDof::default();
        assert!(!ads.update(&aiming(0.0), 300.0, 0.016).active());
    }

    #[test]
    fn aiming_blurs_what_is_closer_and_what_is_farther_than_the_focus() {
        let mut ads = AdsDof::default();
        let d = ads.update(&aiming(0.5), 2000.0, 0.016);
        assert!(d.active());
        // Half way to the sights is half way between the hip and the aimed numbers.
        assert_eq!(d.near_end, 0.5 * 256.0);
        assert_eq!(d.far_start, 0.5 * 2500.0 + 0.5 * 5000.0);
        // Everything nearer than the wall in front of the sights is out of focus.
        let mut ads = AdsDof::default();
        let d = ads.update(&aiming(0.5), 100.0, 0.016);
        assert_eq!(d.near_end, 0.5 * 70.0);
    }

    #[test]
    fn a_fully_aimed_view_eases_to_its_focus_instead_of_jumping() {
        let mut ads = AdsDof::default();
        let first = ads.update(&aiming(1.0), 2000.0, 0.016);
        // From hip numbers (far start 5000) toward the aimed 2500, at most 400 per 50 ms.
        assert!(first.far_start < 5000.0 && first.far_start > 4800.0, "{first:?}");
        let mut d = first;
        for _ in 0..600 {
            d = ads.update(&aiming(1.0), 2000.0, 0.016);
        }
        assert_eq!((d.near_end, d.far_start, d.far_end), (256.0, 2500.0, 10000.0));
        assert_eq!((d.near_blur, d.far_blur), (6.0, 0.0));
    }

    #[test]
    fn the_scripts_range_is_used_and_zero_leaves_it_to_the_aim() {
        let mut ps = aiming(1.0);
        assert_eq!(scripted(&ps), None);
        (ps.dof_near_end, ps.dof_far_start, ps.dof_far_end) = (128.0, 512.0, 4000.0);
        (ps.dof_near_blur, ps.dof_far_blur) = (6.0, 1.8);
        let d = scripted(&ps).unwrap();
        assert_eq!((d.near_start, d.near_end, d.far_start, d.far_end), (0.0, 128.0, 512.0, 4000.0));
        assert_eq!((d.near_blur, d.far_blur), (6.0, 1.8));
        assert!(d.active());
    }

    #[test]
    fn the_weapon_blurs_over_its_own_range_as_it_is_aimed() {
        let mut ps = aiming(0.0);
        (ps.dof_viewmodel_start, ps.dof_viewmodel_end) = (4.0, 8.0);
        assert_eq!(view_model(&ps, Some((10.0, 20.0))), (4.0, 8.0));
        ps.weapon_pos_frac = 1.0;
        assert_eq!(view_model(&ps, Some((10.0, 20.0))), (10.0, 20.0));
        ps.weapon_pos_frac = 0.5;
        assert_eq!(view_model(&ps, Some((10.0, 20.0))), (7.0, 14.0));
    }

    #[test]
    fn the_kill_cams_focus_on_the_victim() {
        let d = airstrike_kill_cam(500.0);
        assert!(d.active());
        assert_eq!((d.near_end, d.far_start, d.far_end), (400.0, 600.0, 900.0));
        let h = helicopter_kill_cam(500.0, 1000.0);
        assert_eq!((h.near_end, h.far_start, h.far_end), (1400.0, 1600.0, 1900.0));
    }
}
