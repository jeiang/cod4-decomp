// SPDX-License-Identifier: GPL-3.0-or-later
//! Damage rules for the game side: distance falloff, hit-location scaling, bullet penetration,
//! explosions and melee.
//!
//! Fact source: `iw3mp.exe` 1.7 `Bullet_GetDamage`, `G_GetWeaponHitLocationMultiplier`,
//! `G_DamageClient`, `Bullet_FirePenetrate`, `BG_AdvanceTrace`, `BG_GetSurfacePenetrationDepth`,
//! `G_RadiusDamage`, `Weapon_Melee_internal`. Arithmetic follows the original's int/double mixes
//! where they change a rounded damage value.

use super::info::{HITLOC_COUNT, PenetrateType, SURFACE_TYPES, WeaponClass, WeaponInfo, WeaponType};
use super::params::{WeaponParams, perk};
use crate::Vec3;
use crate::pm::math;

/// `hitLocation_t` as the scripts name them, in the order of
/// [`WeaponInfo::location_damage_multipliers`].
pub const HIT_LOCATION_NAMES: [&str; HITLOC_COUNT] = [
    "none",
    "helmet",
    "head",
    "neck",
    "torso_upper",
    "torso_lower",
    "right_arm_upper",
    "left_arm_upper",
    "right_arm_lower",
    "left_arm_lower",
    "right_hand",
    "left_hand",
    "right_leg_upper",
    "left_leg_upper",
    "right_leg_lower",
    "left_leg_lower",
    "right_foot",
    "left_foot",
    "gun",
];

pub const HITLOC_NONE: usize = 0;
pub const HITLOC_HELMET: usize = 1;
pub const HITLOC_HEAD: usize = 2;
pub const HITLOC_GUN: usize = 18;

/// `G_GetHitLocationIndexFromString`: the hit location for a script name; `None` for unknown.
pub fn hit_location_from_name(name: &str) -> Option<usize> {
    HIT_LOCATION_NAMES.iter().position(|n| *n == name)
}

/// `Bullet_GetDamage` without the penetration/body multiplier: full damage up to the maximum
/// damage range, the minimum damage from the minimum range, and a linear blend between.
pub fn damage_at_range(info: &WeaponInfo, dist: f32) -> i32 {
    falloff(
        info.damage,
        info.min_damage,
        info.max_damage_range,
        info.min_damage_range,
        dist,
    )
}

/// The same falloff over the weapon's `player_damage` / `min_player_damage` pair.
pub fn player_damage_at_range(info: &WeaponInfo, dist: f32) -> i32 {
    falloff(
        info.player_damage,
        info.min_player_damage,
        info.max_damage_range,
        info.min_damage_range,
        dist,
    )
}

fn falloff(base: i32, min: i32, max_range: f32, min_range: f32, dist: f32) -> i32 {
    let range = min_range - max_range;
    if base == min || range == 0.0 || dist < max_range {
        return base;
    }
    if min_range <= dist {
        return min;
    }
    let lerp = (dist - max_range) / range;
    (f64::from(lerp) * f64::from(min) + (1.0 - f64::from(lerp)) * f64::from(base)) as i32
}

/// The damage of one bullet after the penetration/body multiplier (`Bullet_GetDamage`).
pub fn bullet_damage(info: &WeaponInfo, dist: f32, multiplier: f32) -> i32 {
    (f64::from(damage_at_range(info, dist)) * f64::from(multiplier)) as i32
}

/// Damage multiplier of a bullet that already went through a player with a rifle-class bullet
/// (`Bullet_FireExtended`).
pub const RIFLE_BODY_DAMAGE_SCALE: f32 = 0.5;

/// `info.location_damage_multipliers[hit_location]`; 1.0 for an out-of-range location.
pub fn location_multiplier(info: &WeaponInfo, hit_location: usize) -> f32 {
    info.location_damage_multipliers
        .get(hit_location)
        .copied()
        .unwrap_or(1.0)
}

/// `G_GetWeaponHitLocationMultiplier`: weapons that are not bullet weapons (and turrets) use the
/// gametype's table instead of their own.
pub fn weapon_hit_location_multiplier(
    info: Option<&WeaponInfo>,
    hit_location: usize,
    gametype_table: &[f32; HITLOC_COUNT],
) -> f32 {
    match info {
        Some(w) if w.weap_type == WeaponType::Bullet && w.weap_class != WeaponClass::Turret => {
            location_multiplier(w, hit_location)
        }
        _ => gametype_table.get(hit_location).copied().unwrap_or(1.0),
    }
}

/// `G_DamageClient`'s scaling: melee is not scaled by location, everything else is, and a hit
/// that rounds to nothing still does 1.
pub fn scale_for_location(damage: i32, multiplier: f32, melee: bool) -> i32 {
    let scaled = if melee {
        damage
    } else {
        (f64::from(multiplier) * f64::from(damage)) as i32
    };
    scaled.max(1)
}

/// Longest chain of surfaces one bullet penetrates (`Bullet_FirePenetrate`).
pub const MAX_PENETRATIONS: usize = 5;
/// Longest chain of entities one non-penetrating bullet passes through (`Bullet_FireExtended`).
pub const MAX_BULLET_EXTENSIONS: usize = 12;
/// Distance the bullet restarts past a penetrated surface.
pub const PENETRATION_ADVANCE: f32 = 0.135;
/// Distance the bullet restarts past a pane of glass.
pub const GLASS_ADVANCE: f32 = 0.135;
/// Distance of the backwards trace that measures the thickness of a penetrated surface.
pub const PENETRATION_REVERSE_ADVANCE: f32 = 0.01;
/// Thinnest thickness a penetration counts as.
pub const MIN_PENETRATION_THICKNESS: f32 = 1.0;

/// `Com_SurfaceTypeToName`: surface type 0 is `default`.
pub const SURFACE_TYPE_NAMES: [&str; SURFACE_TYPES] = [
    "default",
    "bark",
    "brick",
    "carpet",
    "cloth",
    "concrete",
    "dirt",
    "flesh",
    "foliage",
    "glass",
    "grass",
    "gravel",
    "ice",
    "metal",
    "mud",
    "paper",
    "plaster",
    "rock",
    "sand",
    "snow",
    "water",
    "wood",
    "asphalt",
    "ceramic",
    "plastic",
    "rubber",
    "cushion",
    "fruit",
    "paintedmetal",
];

/// The bullet penetration table (`info/bullet_penetration_mp`): how much material, in units,
/// each penetration class can pass through, per surface type.
#[derive(Debug, Clone, PartialEq)]
pub struct PenetrationTable {
    /// Indexed by [`PenetrateType`] then surface type; row 0 (`none`) is all zero.
    depth: [[f32; SURFACE_TYPES]; 4],
}

impl Default for PenetrationTable {
    fn default() -> Self {
        Self {
            depth: [[0.0; SURFACE_TYPES]; 4],
        }
    }
}

impl PenetrationTable {
    /// Parses the info string (`BULLET_PEN_TABLE\small_bark\20\small_brick\6...`). Missing or
    /// unparsable entries stay 0.
    pub fn parse(text: &str) -> Self {
        let mut table = Self::default();
        let mut tokens = text.trim().split('\\');
        tokens.next();
        while let (Some(key), Some(value)) = (tokens.next(), tokens.next()) {
            let Some((class, surface)) = key.split_once('_') else {
                continue;
            };
            let row = match class {
                "small" => PenetrateType::Small,
                "medium" => PenetrateType::Medium,
                "large" => PenetrateType::Large,
                _ => continue,
            };
            let Some(col) = SURFACE_TYPE_NAMES.iter().position(|n| *n == surface) else {
                continue;
            };
            if let Ok(v) = value.trim().parse::<f32>() {
                table.depth[row as usize][col] = v;
            }
        }
        table
    }

    /// `BG_GetSurfacePenetrationDepth`: 0 for the default surface and for weapons that cannot
    /// penetrate.
    pub fn depth(&self, class: PenetrateType, surface_type: usize) -> f32 {
        if surface_type == 0 {
            return 0.0;
        }
        self.depth
            .get(class as usize)
            .and_then(|row| row.get(surface_type))
            .copied()
            .unwrap_or(0.0)
    }

    /// The depth for a shooter, with the penetration perk.
    pub fn depth_for(
        &self,
        info: &WeaponInfo,
        surface_type: usize,
        perks: u32,
        params: &WeaponParams,
    ) -> f32 {
        let d = self.depth(info.penetrate_type, surface_type);
        if perks & perk::BULLET_PENETRATION != 0 {
            d * params.perk_bullet_penetration_multiplier
        } else {
            d
        }
    }
}

/// The damage multiplier left after passing through `thickness` of a surface with `max_depth`
/// of penetrability (`Bullet_FirePenetrate`): `multiplier - thickness / max_depth`. At or below
/// zero the bullet stops.
pub fn penetrate_multiplier(multiplier: f32, thickness: f32, max_depth: f32) -> f32 {
    multiplier - thickness.max(MIN_PENETRATION_THICKNESS) / max_depth
}

/// `BG_AdvanceTrace`: where a bullet restarts after a hit, `dist` past the surface along its
/// direction. Against the world the step is stretched by the angle of incidence; a grazing hit
/// (cosine below 0.125) steps the fixed `dist / 0.125` and reports false. Entities restart
/// exactly at the hit.
pub fn advance_trace(hit: Vec3, dir: &Vec3, normal: &Vec3, dist: f32, hit_world: bool) -> (Vec3, bool) {
    if hit_world && dist > 0.0 {
        let dot = -math::dot(normal, dir);
        if dot < 0.125 {
            return (math::mad(&hit, dist / 0.125, dir), false);
        }
        return (math::mad(&hit, dist / dot, dir), true);
    }
    (hit, true)
}

/// The damage an explosion deals at `dist` from its centre (`G_RadiusDamage`): the inner damage
/// at the centre falling linearly to the outer damage at the radius, nothing beyond it.
pub fn explosion_damage_at(info: &WeaponInfo, dist: f32) -> f32 {
    let radius = (info.explosion_radius as f32).max(1.0);
    if dist >= radius {
        return 0.0;
    }
    let inner = info.explosion_inner_damage as f32;
    let outer = info.explosion_outer_damage as f32;
    (inner - outer) * (1.0 - dist / radius) + outer
}

/// The cosine an explosion's damage cone is tested against: `-1` (everything) unless the weapon
/// limits damage to a cone narrower than 180 degrees.
pub fn explosion_cone_cos(info: &WeaponInfo) -> f32 {
    if info.damage_cone_angle - 180.0 < 0.0 {
        let (_, c) = math::sincos_deg(info.damage_cone_angle);
        c
    } else {
        -1.0
    }
}

/// `Weapon_Melee_internal`: the weapon's melee damage plus up to 4 (`rand % 5`).
pub fn melee_damage(info: &WeaponInfo, rand: u32) -> i32 {
    info.melee_damage + (rand % 5) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weapon::fixtures::rifle;

    #[test]
    fn falloff_boundaries() {
        let w = rifle("ak47_mp", "ar"); // 40 -> 20 between 500 and 1000
        assert_eq!(damage_at_range(&w, 0.0), 40);
        assert_eq!(damage_at_range(&w, 499.9), 40);
        assert_eq!(damage_at_range(&w, 500.0), 40);
        assert_eq!(damage_at_range(&w, 750.0), 30);
        assert_eq!(damage_at_range(&w, 999.0), 20); // 20.04 -> truncated by the int cast: 39.96 - ...
        assert_eq!(damage_at_range(&w, 1000.0), 20);
        assert_eq!(damage_at_range(&w, 5000.0), 20);
    }

    #[test]
    fn falloff_rounds_toward_zero() {
        let w = rifle("ak47_mp", "ar");
        // 40 - 20 * (510 - 500) / 500 = 39.6 -> 39
        assert_eq!(damage_at_range(&w, 510.0), 39);
    }

    #[test]
    fn flat_damage_ignores_distance_and_degenerate_ranges() {
        let mut w = rifle("pistol_mp", "p");
        w.min_damage = w.damage;
        assert_eq!(damage_at_range(&w, 9000.0), 40);
        let mut w = rifle("x_mp", "p");
        w.min_damage_range = w.max_damage_range;
        assert_eq!(damage_at_range(&w, 9000.0), 40);
    }

    #[test]
    fn player_damage_uses_its_own_pair() {
        let mut w = rifle("ak47_mp", "ar");
        w.player_damage = 100;
        w.min_player_damage = 50;
        assert_eq!(player_damage_at_range(&w, 0.0), 100);
        assert_eq!(player_damage_at_range(&w, 750.0), 75);
        assert_eq!(player_damage_at_range(&w, 2000.0), 50);
    }

    #[test]
    fn bullet_damage_applies_the_multiplier_with_truncation() {
        let w = rifle("ak47_mp", "ar");
        assert_eq!(bullet_damage(&w, 0.0, 1.0), 40);
        assert_eq!(bullet_damage(&w, 0.0, RIFLE_BODY_DAMAGE_SCALE), 20);
        assert_eq!(bullet_damage(&w, 0.0, 0.33), 13);
    }

    #[test]
    fn hit_locations_scale_damage() {
        let mut w = rifle("ak47_mp", "ar");
        w.location_damage_multipliers[HITLOC_HEAD] = 4.0;
        w.location_damage_multipliers[HITLOC_GUN] = 0.0;
        assert_eq!(hit_location_from_name("head"), Some(HITLOC_HEAD));
        assert_eq!(hit_location_from_name("gun"), Some(HITLOC_GUN));
        assert_eq!(hit_location_from_name("knee"), None);
        assert_eq!(location_multiplier(&w, HITLOC_HEAD), 4.0);
        assert_eq!(location_multiplier(&w, 99), 1.0);
        assert_eq!(scale_for_location(40, 4.0, false), 160);
        assert_eq!(scale_for_location(40, 0.0, false), 1, "never below one");
        assert_eq!(scale_for_location(40, 4.0, true), 40, "melee is not scaled");
    }

    #[test]
    fn non_bullet_weapons_use_the_gametype_table() {
        let mut w = rifle("ak47_mp", "ar");
        w.location_damage_multipliers[HITLOC_HEAD] = 4.0;
        let mut table = [1.0; HITLOC_COUNT];
        table[HITLOC_HEAD] = 2.5;
        assert_eq!(weapon_hit_location_multiplier(Some(&w), HITLOC_HEAD, &table), 4.0);
        w.weap_type = WeaponType::Projectile;
        assert_eq!(weapon_hit_location_multiplier(Some(&w), HITLOC_HEAD, &table), 2.5);
        assert_eq!(weapon_hit_location_multiplier(None, HITLOC_HEAD, &table), 2.5);
        w.weap_type = WeaponType::Bullet;
        w.weap_class = WeaponClass::Turret;
        assert_eq!(weapon_hit_location_multiplier(Some(&w), HITLOC_HEAD, &table), 2.5);
    }

    #[test]
    fn penetration_table_parses_and_looks_up() {
        let t = PenetrationTable::parse(
            "BULLET_PEN_TABLE\\small_bark\\20\\small_brick\\6\\medium_wood\\28\\large_metal\\40\\bogus_wood\\9",
        );
        let bark = SURFACE_TYPE_NAMES.iter().position(|n| *n == "bark").unwrap();
        let wood = SURFACE_TYPE_NAMES.iter().position(|n| *n == "wood").unwrap();
        assert_eq!(t.depth(PenetrateType::Small, bark), 20.0);
        assert_eq!(t.depth(PenetrateType::Medium, wood), 28.0);
        assert_eq!(t.depth(PenetrateType::Small, wood), 0.0);
        assert_eq!(t.depth(PenetrateType::None, bark), 0.0);
        assert_eq!(t.depth(PenetrateType::Small, 0), 0.0, "default surface never penetrates");
        assert_eq!(t.depth(PenetrateType::Large, 99), 0.0);
    }

    #[test]
    fn penetration_perk_scales_depth() {
        let t = PenetrationTable::parse("X\\small_wood\\12");
        let wood = SURFACE_TYPE_NAMES.iter().position(|n| *n == "wood").unwrap();
        let mut w = rifle("ak47_mp", "ar");
        w.penetrate_type = PenetrateType::Small;
        let p = WeaponParams::default();
        assert_eq!(t.depth_for(&w, wood, 0, &p), 12.0);
        assert_eq!(t.depth_for(&w, wood, perk::BULLET_PENETRATION, &p), 24.0);
    }

    #[test]
    fn thickness_reduces_damage_by_its_share_of_the_depth() {
        assert_eq!(penetrate_multiplier(1.0, 6.0, 12.0), 0.5);
        assert_eq!(penetrate_multiplier(1.0, 0.2, 10.0), 0.9, "thinner than 1 counts as 1");
        assert!(penetrate_multiplier(0.5, 6.0, 12.0) <= 0.0);
    }

    #[test]
    fn advance_trace_stretches_with_the_incidence_angle() {
        let dir = [1.0, 0.0, 0.0];
        let (p, ok) = advance_trace([10.0, 0.0, 0.0], &dir, &[-1.0, 0.0, 0.0], 0.135, true);
        assert!(ok);
        assert!((p[0] - 10.135).abs() < 1e-6);
        // 45 degrees: the step is stretched by 1/cos.
        let n = [-std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2, 0.0];
        let (p, ok) = advance_trace([0.0; 3], &dir, &n, 0.135, true);
        assert!(ok);
        assert!((p[0] - 0.135 * std::f32::consts::SQRT_2).abs() < 1e-5);
        // Grazing: fixed step, reported as a failure.
        let (p, ok) = advance_trace([0.0; 3], &dir, &[-0.1, 0.99, 0.0], 0.135, true);
        assert!(!ok);
        assert!((p[0] - 0.135 / 0.125).abs() < 1e-6);
        // Entities restart at the hit.
        assert_eq!(advance_trace([3.0, 4.0, 5.0], &dir, &n, 0.135, false), ([3.0, 4.0, 5.0], true));
    }

    #[test]
    fn explosion_falls_linearly_to_the_outer_damage_at_the_radius() {
        let w = WeaponInfo {
            explosion_radius: 300,
            explosion_inner_damage: 130,
            explosion_outer_damage: 50,
            ..WeaponInfo::default()
        };
        assert_eq!(explosion_damage_at(&w, 0.0), 130.0);
        assert_eq!(explosion_damage_at(&w, 150.0), 90.0);
        assert!((explosion_damage_at(&w, 299.9) - 50.0).abs() < 0.1);
        assert_eq!(explosion_damage_at(&w, 300.0), 0.0);
        assert_eq!(explosion_damage_at(&w, 1000.0), 0.0);
    }

    #[test]
    fn explosion_radius_is_at_least_one_and_cones_default_open() {
        let mut w = WeaponInfo {
            explosion_radius: 0,
            explosion_inner_damage: 10,
            explosion_outer_damage: 0,
            damage_cone_angle: 180.0,
            ..WeaponInfo::default()
        };
        assert_eq!(explosion_damage_at(&w, 0.0), 10.0);
        assert_eq!(explosion_damage_at(&w, 1.0), 0.0);
        assert_eq!(explosion_cone_cos(&w), -1.0);
        w.damage_cone_angle = 60.0;
        assert!((explosion_cone_cos(&w) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn melee_adds_up_to_four() {
        let w = WeaponInfo {
            melee_damage: 135,
            ..WeaponInfo::default()
        };
        assert_eq!(melee_damage(&w, 0), 135);
        assert_eq!(melee_damage(&w, 7), 137);
        assert_eq!(melee_damage(&w, 9), 139);
    }
}
