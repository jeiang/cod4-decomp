// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `game_mp/g_active_mp.cpp`
// (`ClientThink_real`) and `game/g_weapon.cpp` (`CalcMuzzlePoints`).
//! Where a player's shots go: the view's own effects (a hit's kick, a scope's sway) and, aimed down sights, the gun's
//! angles (idle breathing, bob, damage kick, the recoil spring, view-delta sway) composed into the player's angles.
//! The springs advance once per usercmd on the server with the same arithmetic the client draws the gun with.

use sim::Vec3;
use sim::pm::bob::bob_speed;
use sim::weapon::gun::{GunFrame, GunParams, GunState};

use crate::client::Client;
use crate::tags::{angles_to_axis, axis_to_angles, mul3};

#[derive(Debug, Clone, Default)]
pub struct GunMotion {
    pub state: GunState,
    /// What the view adds to the player's angles, degrees.
    pub view: Vec3,
    /// The gun's angles relative to the view, when the shot follows the gun (aimed, no scope overlay).
    pub gun: Option<Vec3>,
}

impl Client {
    /// Advances the gun's springs for one usercmd of `msec` and works out the aim of the shots it fires.
    pub fn gun_step(&mut self, p: &GunParams, time: i32, msec: i32) {
        let ps = &self.ps;
        let f = GunFrame {
            ps,
            p,
            xyspeed: bob_speed(ps, time),
            frametime: msec as f32 * 0.001,
            time,
            damage_time: self.damage_time,
            v_dmg_pitch: self.v_dmg[0],
            v_dmg_roll: self.v_dmg[1],
        };
        let m = &mut self.gun;
        m.view = m.state.view_angles(&f);
        m.state.sway(ps, p, 1.0, msec);
        let angles = m.state.weapon_angles(&f);
        m.gun =
            (p.aim_down_sight && ps.weapon_pos_frac != 0.0 && !p.overlay_reticle).then_some(angles);
    }

    /// `client->fGunPitch`/`fGunYaw`: the angles shots leave along, from the player's angles after this
    /// command's movement.
    pub fn fire_angles(&self) -> Vec3 {
        compose(self.ps.viewangles, self.gun.view, self.gun.gun)
    }
}

/// The player's angles plus the view's effects, turned by the gun's angles when the shot follows the gun.
fn compose(mut view: Vec3, effects: Vec3, gun: Option<Vec3>) -> Vec3 {
    for (a, v) in view.iter_mut().zip(effects) {
        *a += v;
    }
    match gun {
        Some(g) => axis_to_angles(&mul3(&angles_to_axis(g), &angles_to_axis(view))),
        None => view,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shot_follows_the_view_and_its_effects_and_when_aimed_the_gun() {
        let view = [10.0, 90.0, 0.0];
        assert_eq!(compose(view, [0.0; 3], None), view);
        assert_eq!(compose(view, [2.0, 0.0, 0.0], None), [12.0, 90.0, 0.0]);
        // The gun kicked up 3 degrees of pitch: the shot is 3 degrees above the view, along the same yaw.
        let a = compose(view, [0.0; 3], Some([-3.0, 0.0, 0.0]));
        assert!(
            (a[0] - 7.0).abs() < 1e-3 && (a[1] - 90.0).abs() < 1e-3,
            "{a:?}"
        );
        let a = compose(view, [0.0; 3], Some([0.0, 4.0, 0.0]));
        assert!((a[1] - 94.0).abs() < 0.2, "{a:?}");
    }
}
