// SPDX-License-Identifier: GPL-3.0-only
//! Firing helpers for the game side: the spread a shot is fired with, the bullet directions of
//! one trigger pull, and the view/gun kick of a shot. The muzzle origin and the view basis come
//! from the server.
//!
//! Fact source: `iw3mp.exe` 1.7 `FireWeapon`, `Bullet_Fire`, `Bullet_Endpos`,
//! `Bullet_RandomDir`, `G_GoodRandomFloat`, `BG_GetSpreadForWeapon`, `BG_WeaponFireRecoil`.
//! Bullets are the one thing the server rolls dice for: the seed is the game time plus the
//! pellet number, so a shot is reproducible from `(time, pellet)`.

use super::info::{WeaponClass, WeaponInfo};
use super::params::{WeaponParams, perk};
use crate::Vec3;
use crate::pm::math;
use crate::pm::{PlayerState, SpreadOverrideState};

/// Range of a bullet that is not a shotgun pellet (`Bullet_Fire`).
pub const BULLET_RANGE: f32 = 8192.0;

/// One step of the generator's linear congruential core.
fn lcg_step(state: i32) -> i32 {
    const A: i32 = 16807;
    const Q: i32 = 127_773;
    const R: i32 = 2836;
    let next = A * (state % Q) - R * (state / Q);
    if next < 0 { next + i32::MAX } else { next }
}

/// `G_GoodRandomFloat`: a float in `[0, 1)` from the seed, which it advances. The generator is
/// a shuffled minimal-standard generator that re-primes itself on every call, so the result is a
/// pure function of the seed; `seed` becomes the next one.
pub fn good_random_float(seed: &mut i32) -> f32 {
    let mut state = seed.wrapping_neg();
    let mut table = [0i32; 32];
    for round in (0..40).rev() {
        state = lcg_step(state);
        if round < 32 {
            table[round] = state;
        }
    }
    state = lcg_step(state);
    *seed = state;
    let pick = (table[0] / 0x400_0000) as usize;
    let value = table[pick & 31] as f32 * (1.0 / 2_147_483_648.0);
    value.min(1.0 - f32::EPSILON)
}

/// `Bullet_RandomDir`: the unit-disc direction offset for a seed: an angle and a radius, both
/// uniform.
pub fn bullet_random_dir(seed: i32) -> [f32; 2] {
    let mut seed = seed;
    let theta = good_random_float(&mut seed) * 360.0;
    let r = good_random_float(&mut seed);
    let (s, c) = math::sincos_deg(theta);
    [r * c, r * s]
}

/// The right and up offsets, in units, at `range` for a cone of `spread_deg` degrees
/// (`Bullet_Endpos`).
pub fn bullet_spread_offsets(spread_deg: f32, range: f32, seed: i32) -> [f32; 2] {
    let (s, c) = math::sincos_deg(spread_deg);
    let offset = s / c * range;
    let [x, y] = bullet_random_dir(seed);
    [x * offset, y * offset]
}

/// The view basis a shot is aimed along.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimBasis {
    pub origin: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}

impl AimBasis {
    /// The basis for view angles (pitch, yaw, roll in degrees) at `origin`.
    pub fn from_angles(origin: Vec3, angles: &Vec3) -> Self {
        let (forward, right, up) = math::angle_vectors(angles);
        Self {
            origin,
            forward,
            right,
            up,
        }
    }
}

/// One bullet of a trigger pull.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BulletShot {
    pub start: Vec3,
    pub end: Vec3,
    /// Unit direction from `start` to `end`.
    pub dir: Vec3,
}

/// `Bullet_Endpos`: the end point and direction of a bullet fired with `spread_deg` at `range`.
pub fn bullet_endpos(aim: &AimBasis, spread_deg: f32, range: f32, seed: i32) -> BulletShot {
    let [right, up] = bullet_spread_offsets(spread_deg, range, seed);
    let mut end = math::mad(&aim.origin, range, &aim.forward);
    end = math::mad(&end, right, &aim.right);
    end = math::mad(&end, up, &aim.up);
    let mut dir = math::sub(&end, &aim.origin);
    math::normalize(&mut dir);
    BulletShot {
        start: aim.origin,
        end,
        dir,
    }
}

/// How many bullets one trigger pull sends and how far they fly (`Bullet_Fire`): spread-class
/// weapons (shotguns) send `shot_count` pellets that reach as far as the minimum damage range;
/// every other bullet weapon sends one bullet.
pub fn shot_pattern(info: &WeaponInfo) -> (usize, f32) {
    if info.weap_class == WeaponClass::Spread {
        (info.shot_count.max(0) as usize, info.min_damage_range)
    } else {
        (1, BULLET_RANGE)
    }
}

/// All bullets of one trigger pull, without allocating. `game_time` seeds the dice
/// (`Bullet_Fire`'s `randSeed`); pellet `i` uses `game_time + i`.
pub fn bullet_shots<'a>(
    info: &WeaponInfo,
    aim: &'a AimBasis,
    spread_deg: f32,
    game_time: i32,
) -> impl Iterator<Item = BulletShot> + 'a {
    let (count, range) = shot_pattern(info);
    (0..count).map(move |i| bullet_endpos(aim, spread_deg, range, game_time.wrapping_add(i as i32)))
}

/// `BG_GetSpreadForWeapon`: the least and greatest hip spread (degrees) for the player's stance
/// blend, honouring a script override and the spread perk.
pub fn spread_range(info: &WeaponInfo, ps: &PlayerState, params: &WeaponParams) -> (f32, f32) {
    let (mut min, mut max);
    if ps.spread_override_state == SpreadOverrideState::Enabled {
        min = ps.spread_override as f32;
        max = min;
    } else if ps.view_height_current <= 40.0 {
        let frac = (ps.view_height_current - 11.0) / 29.0;
        min = (info.hip_spread_ducked_min - info.hip_spread_prone_min) * frac
            + info.hip_spread_prone_min;
        max = (info.hip_spread_ducked_max - info.hip_spread_prone_max) * frac
            + info.hip_spread_prone_max;
    } else {
        let frac = (ps.view_height_current - 40.0) / 20.0;
        min = (info.hip_spread_stand_min - info.hip_spread_ducked_min) * frac
            + info.hip_spread_ducked_min;
        max = (info.hip_spread_stand_max - info.hip_spread_ducked_max) * frac
            + info.hip_spread_ducked_max;
    }
    if ps.spread_override_state == SpreadOverrideState::Resetting {
        max = ps.spread_override as f32;
    }
    if ps.perks & perk::BULLET_ACCURACY != 0 {
        min *= params.perk_weap_spread_multiplier;
        max *= params.perk_weap_spread_multiplier;
    }
    (min, max)
}

/// The spread (degrees) a shot is fired with (`FireWeapon`): the blend between the minimum and
/// maximum by `aim_spread_scale / 255`, or between the ADS spread and the maximum when fully
/// aimed down sights.
pub fn aim_spread_degrees(info: &WeaponInfo, ps: &PlayerState, params: &WeaponParams) -> f32 {
    let scale = (f64::from(ps.aim_spread_scale) / 255.0) as f32;
    let (min, max) = spread_range(info, ps, params);
    if ps.weapon_pos_frac == 1.0 {
        (max - info.ads_spread) * scale + info.ads_spread
    } else {
        (max - min) * scale + min
    }
}

/// The view and gun kick of one shot (`BG_WeaponFireRecoil`). `random` yields floats in `[0, 1)`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Recoil {
    /// Angular velocity added to the view (`kickAVel`): pitch (negative is up), yaw, roll.
    pub view_kick: [f32; 3],
    /// Speed added to the gun's pitch and yaw spring.
    pub gun_kick: [f32; 2],
}

pub fn fire_recoil(info: &WeaponInfo, ps: &PlayerState, mut random: impl FnMut() -> f32) -> Recoil {
    let pos = ps.weapon_pos_frac;
    let mut reduce = 1.0;
    if ps.weapon_restrict_kick_time > 0 {
        reduce = if pos == 1.0 {
            info.ads_gun_kick_reduced_kick_percent * 0.01
        } else {
            info.hip_gun_kick_reduced_kick_percent * 0.01
        };
    }
    let mut roll = |range: [f32; 2]| random() * (range[1] - range[0]) + range[0];
    let (view_pitch, view_yaw) = if pos == 1.0 {
        (roll(info.ads_view_kick_pitch), roll(info.ads_view_kick_yaw))
    } else {
        (roll(info.hip_view_kick_pitch), roll(info.hip_view_kick_yaw))
    };
    let view_pitch = view_pitch * reduce;
    let view_yaw = view_yaw * reduce;
    let (gun_pitch, gun_yaw) = if pos <= 0.0 {
        (roll(info.hip_gun_kick_pitch), roll(info.hip_gun_kick_yaw))
    } else {
        (roll(info.ads_gun_kick_pitch), roll(info.ads_gun_kick_yaw))
    };
    Recoil {
        view_kick: [-view_pitch, view_yaw, view_yaw * -0.5],
        gun_kick: [gun_pitch * reduce, gun_yaw * reduce],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weapon::fixtures::rifle;

    #[test]
    fn good_random_is_a_pure_function_of_the_seed() {
        let a = bullet_random_dir(1234);
        assert_eq!(a, bullet_random_dir(1234));
        assert_ne!(a, bullet_random_dir(1235));
        for seed in [-5, 0, 1, 99_999, 123_456_789, i32::MAX] {
            let [x, y] = bullet_random_dir(seed);
            assert!(
                x * x + y * y <= 1.0 + 1e-6,
                "seed {seed}: ({x}, {y}) outside the unit disc"
            );
        }
    }

    #[test]
    fn good_random_stays_in_unit_interval_and_covers_it() {
        let mut seed = 77;
        let mut lo = 1.0f32;
        let mut hi = 0.0f32;
        for _ in 0..2000 {
            let v = good_random_float(&mut seed);
            assert!((0.0..1.0).contains(&v));
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(lo < 0.05 && hi > 0.95, "poor coverage: {lo}..{hi}");
    }

    #[test]
    fn zero_spread_flies_straight() {
        let aim = AimBasis::from_angles([10.0, 20.0, 30.0], &[0.0, 90.0, 0.0]);
        let shot = bullet_endpos(&aim, 0.0, 1000.0, 7);
        assert!((shot.dir[1] - 1.0).abs() < 1e-6, "{:?}", shot.dir);
        assert!((shot.end[1] - 1020.0).abs() < 1e-3);
        assert_eq!(shot.start, [10.0, 20.0, 30.0]);
    }

    #[test]
    fn spread_bounds_the_deviation_from_the_aim() {
        let aim = AimBasis::from_angles([0.0; 3], &[0.0; 3]);
        let spread: f32 = 5.0;
        let limit = spread.to_radians().cos() - 1e-5;
        let mut widest: f32 = 1.0;
        for seed in 0..500 {
            let shot = bullet_endpos(&aim, spread, 8192.0, seed);
            let cos = math::dot(&shot.dir, &aim.forward);
            assert!(cos >= limit, "seed {seed} deviates {cos}");
            widest = widest.min(cos);
        }
        assert!(widest < 0.9999, "bullets never leave the axis: {widest}");
    }

    #[test]
    fn shotguns_send_every_pellet_to_the_min_damage_range() {
        let mut sg = rifle("shotgun_mp", "shells");
        sg.weap_class = WeaponClass::Spread;
        sg.shot_count = 8;
        sg.min_damage_range = 600.0;
        assert_eq!(shot_pattern(&sg), (8, 600.0));
        assert_eq!(shot_pattern(&rifle("ak47_mp", "ar")), (1, BULLET_RANGE));
        let aim = AimBasis::from_angles([0.0; 3], &[0.0; 3]);
        let shots: Vec<_> = bullet_shots(&sg, &aim, 6.0, 1000).collect();
        assert_eq!(shots.len(), 8);
        assert_ne!(shots[0].end, shots[1].end);
        assert_eq!(shots[3], bullet_endpos(&aim, 6.0, 600.0, 1003));
    }

    fn sample_weapon() -> WeaponInfo {
        WeaponInfo {
            hip_spread_stand_min: 4.0,
            hip_spread_stand_max: 8.0,
            hip_spread_ducked_min: 2.0,
            hip_spread_ducked_max: 6.0,
            hip_spread_prone_min: 1.0,
            hip_spread_prone_max: 3.0,
            ads_spread: 0.5,
            ..WeaponInfo::default()
        }
    }

    #[test]
    fn spread_blends_by_view_height() {
        let w = sample_weapon();
        let p = WeaponParams::default();
        let mut ps = PlayerState {
            view_height_current: 60.0,
            ..PlayerState::default()
        };
        assert_eq!(spread_range(&w, &ps, &p), (4.0, 8.0));
        ps.view_height_current = 40.0;
        assert_eq!(spread_range(&w, &ps, &p), (2.0, 6.0));
        ps.view_height_current = 11.0;
        assert_eq!(spread_range(&w, &ps, &p), (1.0, 3.0));
        ps.view_height_current = 50.0;
        assert_eq!(spread_range(&w, &ps, &p), (3.0, 7.0));
    }

    #[test]
    fn aim_spread_interpolates_and_ads_uses_the_ads_spread() {
        let w = sample_weapon();
        let p = WeaponParams::default();
        let mut ps = PlayerState {
            view_height_current: 60.0,
            aim_spread_scale: 0.0,
            ..PlayerState::default()
        };
        assert_eq!(aim_spread_degrees(&w, &ps, &p), 4.0);
        ps.aim_spread_scale = 255.0;
        assert_eq!(aim_spread_degrees(&w, &ps, &p), 8.0);
        ps.weapon_pos_frac = 1.0;
        assert_eq!(aim_spread_degrees(&w, &ps, &p), 8.0);
        ps.aim_spread_scale = 0.0;
        assert_eq!(aim_spread_degrees(&w, &ps, &p), 0.5);
    }

    #[test]
    fn spread_perk_and_override_apply() {
        let w = sample_weapon();
        let p = WeaponParams::default();
        let mut ps = PlayerState {
            view_height_current: 60.0,
            perks: perk::BULLET_ACCURACY,
            ..PlayerState::default()
        };
        let (min, max) = spread_range(&w, &ps, &p);
        assert!((min - 2.6).abs() < 1e-6 && (max - 5.2).abs() < 1e-6);
        ps.perks = 0;
        ps.spread_override_state = SpreadOverrideState::Enabled;
        ps.spread_override = 10;
        assert_eq!(spread_range(&w, &ps, &p), (10.0, 10.0));
        ps.spread_override_state = SpreadOverrideState::Resetting;
        ps.spread_override = 12;
        assert_eq!(spread_range(&w, &ps, &p), (4.0, 12.0));
    }

    #[test]
    fn recoil_is_reduced_right_after_the_first_shot() {
        let mut w = sample_weapon();
        w.hip_view_kick_pitch = [1.0, 3.0];
        w.hip_view_kick_yaw = [-1.0, 1.0];
        w.hip_gun_kick_pitch = [2.0, 2.0];
        w.hip_gun_kick_yaw = [0.0, 0.0];
        w.hip_gun_kick_reduced_kick_percent = 50.0;
        let mut ps = PlayerState::default();
        let full = fire_recoil(&w, &ps, || 1.0);
        assert_eq!(full.view_kick, [-3.0, 1.0, -0.5]);
        assert_eq!(full.gun_kick, [2.0, 0.0]);
        ps.weapon_restrict_kick_time = 100;
        let reduced = fire_recoil(&w, &ps, || 1.0);
        assert_eq!(reduced.view_kick, [-1.5, 0.5, -0.25]);
        assert_eq!(reduced.gun_kick, [1.0, 0.0]);
    }
}
