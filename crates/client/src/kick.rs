// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame_mp/cg_view_mp.cpp` (`CG_KickAngles`) and
// `cgame/cg_weapons.cpp` (`CG_FireWeapon`).
//! The kick of the player's own shots: the view kick angles (`cg_s::kickAVel`, `kickAngles`) and the speed the gun
//! model's recoil spring is given (`vGunSpeed`, spent by `viewmodel`).
//!
//! A shot the predicted player state fires sets the view's angular velocity ([`sim::weapon::fire::fire_recoil`]);
//! the angles it integrates to spring back towards zero at the weapon file's centering speed. The kick angles are
//! added to the angles of the command sent to the server, so recoil moves where shots land and the server agrees
//! with the prediction, and to the camera. They never enter the player's own look angles.

use fx::Rng;
use sim::pm::{PlayerState, ev};
use sim::weapon::WeaponInfo;
use sim::weapon::fire::fire_recoil;

/// The integration step of the kick spring, seconds.
const STEP: f32 = 0.005;
/// The centering speed with no weapon, degrees per second squared.
const NO_WEAPON_CENTER: f32 = 2400.0;
/// The largest kick angle on any axis, degrees.
const MAX_KICK: f32 = 10.0;

#[derive(Debug, Clone)]
pub struct Kick {
    /// Angular velocity of the view kick, degrees per second: pitch (negative is up), yaw, roll.
    vel: [f32; 3],
    /// The view kick, degrees.
    angles: [f32; 3],
    /// Speed the shots gave the gun's pitch and yaw spring, not yet handed to the view model.
    gun: [f32; 2],
    rng: Rng,
}

impl Default for Kick {
    fn default() -> Self {
        Kick {
            vel: [0.0; 3],
            angles: [0.0; 3],
            gun: [0.0; 2],
            rng: Rng::new(0x4B1C),
        }
    }
}

impl Kick {
    /// The view kick, degrees: added to the cmd angles and the camera.
    pub fn angles(&self) -> [f32; 3] {
        self.angles
    }

    /// The gun speed the shots since the last call added.
    pub fn take_gun_speed(&mut self) -> [f32; 2] {
        std::mem::take(&mut self.gun)
    }

    /// Drops the kick (a player who is dead or not the one drawn has none).
    pub fn clear(&mut self) {
        self.vel = [0.0; 3];
        self.angles = [0.0; 3];
        self.gun = [0.0; 2];
        self.idle = [0.0; 2];
    }

    /// Kicks for each shot among `events` (`CG_FireWeapon` of the own player): the predicted events no earlier
    /// frame delivered, so a shot kicks once however often the prediction replays it.
    pub fn shots(&mut self, events: &[(u8, u8)], ps: &PlayerState, info: &WeaponInfo) {
        for &(e, _) in events {
            if matches!(e, ev::FIRE_WEAPON | ev::FIRE_WEAPON_LASTSHOT) {
                let r = fire_recoil(info, ps, || self.rng.f());
                self.vel = r.view_kick;
                self.gun[0] += r.gun_kick[0];
                self.gun[1] += r.gun_kick[1];
            }
        }
    }

    /// Advances the spring by `dt` seconds (`CG_KickAngles`). `center` is the held weapon's hip and ADS centering
    /// speeds, `None` for no weapon.
    pub fn step(&mut self, dt: f32, ads: f32, center: Option<[f32; 2]>) {
        let mut left = dt;
        while left > 0.0 {
            let ft = left.min(STEP);
            left -= ft;
            for i in 0..3 {
                self.step_axis(i, ft, ads, center);
            }
        }
    }

    fn step_axis(&mut self, i: usize, ft: f32, ads: f32, center: Option<[f32; 2]>) {
        let (vel, angle) = (&mut self.vel[i], &mut self.angles[i]);
        if *vel == 0.0 && *angle == 0.0 {
            return;
        }
        if *angle != 0.0 {
            // Towards zero.
            let toward = if *angle <= 0.0 { 1.0 } else { -1.0 };
            let speed = match center {
                Some([hip, _]) if ads <= 0.5 => hip,
                Some([_, ads]) => ads,
                None => NO_WEAPON_CENTER,
            };
            *vel += toward * speed * ft;
        }
        let mut change = *vel * ft;
        // Moving back towards zero is slowed.
        if *angle * change < 0.0 {
            change *= 0.06;
        }
        if (*angle + change) * *angle < 0.0 {
            *angle = 0.0;
            *vel = 0.0;
            return;
        }
        *angle += change;
        if *angle == 0.0 {
            *vel = 0.0;
        } else if angle.abs() > MAX_KICK {
            *angle = MAX_KICK.copysign(*angle);
            *vel = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rifle() -> WeaponInfo {
        WeaponInfo {
            hip_view_kick_pitch: [4.0, 6.0],
            hip_gun_kick_pitch: [1.0, 2.0],
            ..WeaponInfo::default()
        }
    }

    const SHOT: [(u8, u8); 1] = [(ev::FIRE_WEAPON, 0)];

    fn pitch_kicks(k: &mut Kick, info: &WeaponInfo, events: &[(u8, u8)]) -> f32 {
        k.shots(events, &PlayerState::default(), info);
        k.vel[0]
    }

    #[test]
    fn a_shot_kicks_the_view_up_and_it_settles_back_to_zero() {
        let mut k = Kick::default();
        let info = rifle();
        assert!(pitch_kicks(&mut k, &info, &SHOT) < 0.0);
        let d = Some([600.0; 2]);
        let mut peak = 0.0f32;
        for _ in 0..30 {
            k.step(0.016, 0.0, d);
            peak = peak.min(k.angles()[0]);
        }
        assert!(peak < 0.0, "the view went up");
        for _ in 0..600 {
            k.step(0.016, 0.0, d);
        }
        assert_eq!(k.angles(), [0.0; 3], "and recovered");
    }

    #[test]
    fn the_spring_frame_rate_does_not_change_where_it_ends() {
        let d = Some([500.0; 2]);
        let run = |dt: f32, n: usize| {
            let mut k = Kick {
                vel: [-120.0, 30.0, -15.0],
                ..Kick::default()
            };
            for _ in 0..n {
                k.step(dt, 0.0, d);
            }
            k.angles
        };
        let (a, b) = (run(0.005, 40), run(0.020, 10));
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-3, "{a:?} {b:?}");
        }
    }

    #[test]
    fn the_kick_is_capped() {
        let mut k = Kick {
            vel: [-100_000.0, 0.0, 0.0],
            ..Kick::default()
        };
        k.step(0.05, 0.0, None);
        assert!(k.angles()[0] >= -MAX_KICK);
    }
}
