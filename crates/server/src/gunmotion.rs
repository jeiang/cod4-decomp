// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `game_mp/g_active_mp.cpp`
// (`ClientThink_real`) and `game/g_weapon.cpp` (`CalcMuzzlePoints`).
//! Where a player's shots go: the view's own effects (a hit's kick, a scope's sway and bob) and, aimed down sights,
//! the gun's angles (idle breathing, bob, damage kick, the recoil spring, view-delta sway) composed into the
//! player's angles. The springs advance once per usercmd, before the movement, like the original; the view effects
//! are [`sim::weapon::gun::ViewFx`], the same the client draws its camera with.
//!
//! The gun's recoil spring is rolled separately on the server and on the client, as in the original, so the gun the
//! server aims an aimed shot along kicks in its own direction after each shot.

use sim::Vec3;
use sim::pm::bob::bob_speed;
use sim::weapon::WeaponInfo;
use sim::weapon::gun::{GunFrame, GunState, ViewFx, shell_shock_sway_scale};

use crate::client::Client;
use crate::tags::{angles_to_axis, axis_to_angles, mul3};

#[derive(Debug, Clone, Default)]
pub struct GunMotion {
    pub state: GunState,
    pub fx: ViewFx,
    /// The level time the shell shock ends at, 0 for none.
    pub shock_end: i32,
    /// The angles of the shots of the command being run (`fGunPitch`, `fGunYaw`); `None` before the first.
    aim: Option<Vec3>,
}

impl Client {
    /// Advances the gun's springs and the view's effects for one usercmd of `msec`, from the state before the
    /// command's movement, and works out the aim of the shots it fires.
    pub fn gun_step(&mut self, info: Option<&WeaponInfo>, msec: i32) {
        let ps = &self.ps;
        let m = &mut self.gun;
        let Some(info) = info else {
            m.aim = None;
            return;
        };
        let p = &info.gun;
        m.fx.observe(ps);
        m.fx.step(ps, p);
        let time = ps.command_time;
        let xyspeed = bob_speed(ps, time);
        let view = compose_view(ps.viewangles, m.fx.view_angles(ps, p, xyspeed));
        let ss = shell_shock_sway_scale(p.sway_shell_shock_scale, m.shock_end - time);
        m.state.sway(ps, p, ss, msec);
        let angles = m.state.weapon_angles(&GunFrame {
            ps,
            p,
            xyspeed,
            frametime: msec as f32 * 0.001,
            time,
            hit: m.fx.hit,
        });
        let follows_gun = p.aim_down_sight && ps.weapon_pos_frac != 0.0 && !p.overlay_reticle;
        m.aim = Some(compose(view, follows_gun.then_some(angles)));
    }

    /// `client->fGunPitch`/`fGunYaw`: the angles shots leave along.
    pub fn fire_angles(&self) -> Vec3 {
        self.gun.aim.unwrap_or(self.ps.viewangles)
    }
}

fn compose_view(mut view: Vec3, effects: Vec3) -> Vec3 {
    for (a, v) in view.iter_mut().zip(effects) {
        *a += v;
    }
    view
}

/// The view turned by the gun's angles when the shot follows the gun.
fn compose(view: Vec3, gun: Option<Vec3>) -> Vec3 {
    match gun {
        Some(g) => axis_to_angles(&mul3(&angles_to_axis(g), &angles_to_axis(view))),
        None => view,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::pm::PlayerState;
    use sim::weapon::fire::AimBasis;
    use sim::weapon::gun::GunParams;

    fn client(ads: f32) -> Client {
        let mut c = Client::new(0, false, "t".into());
        c.ps = PlayerState {
            viewangles: [10.0, 90.0, 0.0],
            weapon_pos_frac: ads,
            command_time: 1000,
            ..PlayerState::default()
        };
        c
    }

    fn rifle() -> WeaponInfo {
        WeaponInfo {
            gun: GunParams {
                aim_down_sight: true,
                gun_max_pitch: 8.0,
                gun_max_yaw: 8.0,
                gun_kick_accel: [50.0; 2],
                gun_kick_speed_max: [400.0; 2],
                gun_kick_speed_decay: [3.0; 2],
                gun_kick_static_decay: [20.0; 2],
                ..GunParams::default()
            },
            ..WeaponInfo::default()
        }
    }

    fn forward(c: &Client) -> [f32; 3] {
        AimBasis::from_angles([0.0; 3], &c.fire_angles()).forward
    }

    #[test]
    fn an_aimed_shot_follows_the_gun_while_the_spring_is_kicked_and_a_hip_shot_does_not() {
        let info = rifle();
        let mut still = client(1.0);
        still.gun_step(Some(&info), 8);
        let rest = forward(&still);
        let mut kicked = client(1.0);
        kicked.gun.state.kick([-300.0, 0.0]);
        kicked.gun_step(Some(&info), 8);
        let moved = forward(&kicked);
        let dot: f32 = rest.iter().zip(moved).map(|(a, b)| a * b).sum();
        assert!(dot < 0.9999, "the kicked gun moved the shot: {dot}");
        let mut hip = client(0.0);
        hip.gun.state.kick([-300.0, 0.0]);
        hip.gun_step(Some(&info), 8);
        assert_eq!(
            hip.fire_angles(),
            hip.ps.viewangles,
            "the hip shot is along the view"
        );
    }

    #[test]
    fn a_weapon_without_info_shoots_along_the_view() {
        let mut c = client(1.0);
        c.gun_step(None, 8);
        assert_eq!(c.fire_angles(), c.ps.viewangles);
    }

    #[test]
    fn composing_the_gun_into_the_view_turns_the_shot_by_the_gun_angles() {
        let view = [10.0, 90.0, 0.0];
        assert_eq!(compose(view, None), view);
        let a = compose(view, Some([-3.0, 0.0, 0.0]));
        assert!(
            (a[0] - 7.0).abs() < 1e-3 && (a[1] - 90.0).abs() < 1e-3,
            "{a:?}"
        );
    }
}
