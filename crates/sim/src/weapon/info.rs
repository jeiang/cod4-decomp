// SPDX-License-Identifier: GPL-3.0-only
//! `WeaponInfo`: the part of a decoded [`WeaponDef`] that simulation reads, without the models,
//! effects, materials and sounds.
//!
//! Field meanings and the load-time fix-ups (`BG_SetupTransitionTimes`, `BG_CheckWeaponDamageRanges`)
//! are the original's; the enum value order is the original's `weapType_t`, `weapClass_t`,
//! `weapInventoryType_t`, `weapFireType_t`, `OffhandClass` and `PenetrateType`. Times are
//! milliseconds. Fact source: `iw3mp.exe` 1.7 weapon definition loader.

use super::gun::GunParams;
use crate::pm::WeaponMove;
use assets::zone::weapon::WeaponDef;

/// Number of hit locations (`hitLocation_t`).
pub const HITLOC_COUNT: usize = 19;
/// Surface types a bounce or penetration table has an entry for.
pub const SURFACE_TYPES: usize = 29;

/// Index of the melee charge animation in `WeaponDef::anims` (`WEAP_ANIM_MELEE_CHARGE`).
const ANIM_MELEE_CHARGE: usize = 8;

macro_rules! raw_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        #[repr(u8)]
        pub enum $name {
            #[default]
            $($(#[$vm])* $var),+
        }

        impl $name {
            /// Every value in file order.
            pub const ALL: &'static [Self] = &[$(Self::$var),+];

            /// The value for a decoded integer; out-of-range values (corrupt data) fall back to
            /// the first variant, as the original's zeroed definition does.
            pub fn from_raw(v: i32) -> Self {
                usize::try_from(v)
                    .ok()
                    .and_then(|i| Self::ALL.get(i).copied())
                    .unwrap_or_default()
            }

            /// The decoded integer.
            pub fn raw(self) -> i32 {
                self as i32
            }

            /// The string the scripts see.
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$var => $text),+
                }
            }
        }
    };
}

raw_enum! {
    /// `weapType_t` (`weapontype()`).
    WeaponType {
        Bullet = "bullet",
        Grenade = "grenade",
        Projectile = "projectile",
        Binoculars = "binoculars",
    }
}

raw_enum! {
    /// `weapClass_t` (`weaponclass()`).
    WeaponClass {
        Rifle = "rifle",
        Mg = "mg",
        Smg = "smg",
        Spread = "spread",
        Pistol = "pistol",
        Grenade = "grenade",
        RocketLauncher = "rocketlauncher",
        Turret = "turret",
        NonPlayer = "non-player",
        Item = "item",
    }
}

raw_enum! {
    /// `weapInventoryType_t` (`weaponinventorytype()`).
    InventoryType {
        Primary = "primary",
        Offhand = "offhand",
        Item = "item",
        AltMode = "altmode",
    }
}

raw_enum! {
    /// `weapFireType_t`; the names are the weapon file's.
    FireType {
        FullAuto = "Full Auto",
        SingleShot = "Single Shot",
        Burst2 = "2-Round Burst",
        Burst3 = "3-Round Burst",
        Burst4 = "4-Round Burst",
    }
}

raw_enum! {
    /// `OffhandClass`.
    OffhandClass {
        None = "None",
        Frag = "Frag Grenade",
        Smoke = "Smoke Grenade",
        Flash = "Flash Grenade",
    }
}

raw_enum! {
    /// `PenetrateType`: which column of the bullet penetration table the weapon uses.
    PenetrateType {
        None = "none",
        Small = "small",
        Medium = "medium",
        Large = "large",
    }
}

raw_enum! {
    /// `ImpactType`.
    ImpactType {
        None = "none",
        BulletSmall = "bullet_small",
        BulletLarge = "bullet_large",
        BulletAp = "bullet_ap",
        Shotgun = "shotgun",
        GrenadeBounce = "grenade_bounce",
        GrenadeExplode = "grenade_explode",
        RocketExplode = "rocket_explode",
        ProjectileDud = "projectile_dud",
    }
}

raw_enum! {
    /// `weapProjExposion_t`.
    ProjExplosion {
        Grenade = "grenade",
        Rocket = "rocket",
        Flashbang = "flashbang",
        None = "none",
        Dud = "dud",
        Smoke = "smoke",
        Heavy = "heavy explosive",
    }
}

impl FireType {
    /// Rounds in one burst: 0 for full auto, 1 for single shot.
    pub fn burst_len(self) -> u8 {
        match self {
            Self::FullAuto => 0,
            Self::SingleShot => 1,
            Self::Burst2 => 2,
            Self::Burst3 => 3,
            Self::Burst4 => 4,
        }
    }

    /// The fire types that fire a fixed burst and then cool down (`WeaponUsesBurstCooldown`).
    pub fn is_burst(self) -> bool {
        matches!(self, Self::Burst2 | Self::Burst3 | Self::Burst4)
    }
}

/// What a mounted turret (`WEAPCLASS_TURRET`) reads from its weapon: the arcs a gunner may swing the gun through
/// (degrees from the way the turret faces), the stance a gunner is held in and the hint strings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurretDef {
    pub left_arc: f32,
    pub right_arc: f32,
    pub top_arc: f32,
    pub bottom_arc: f32,
    /// `stance`: 0 standing, 1 crouched, 2 prone.
    pub stance: i32,
    pub player_spread: f32,
    /// What the crosshair says to a player who could mount it, and to the one mounted.
    pub use_hint_string: Box<str>,
    pub drop_hint_string: Box<str>,
}

/// Everything simulation reads from a weapon definition.
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponInfo {
    /// Table index (1-based); 0 until the table assigns it.
    pub index: u16,
    /// `szInternalName`, e.g. `ak47_mp`.
    pub name: Box<str>,
    /// `szAltWeaponName`; empty when none.
    pub alt_weapon_name: Box<str>,
    /// Table index of the alternate fire mode weapon (0 = none).
    pub alt_weapon: u16,
    pub player_anim_type: i32,
    /// Bit `n` set: the definition has gun model `n`.
    pub gun_model_mask: u16,
    pub has_knife_model: bool,
    /// The weapon has a melee charge animation (`PM_WeaponHasChargeMelee`).
    pub has_melee_charge_anim: bool,

    pub weap_type: WeaponType,
    pub weap_class: WeaponClass,
    pub inventory_type: InventoryType,
    pub fire_type: FireType,
    pub offhand_class: OffhandClass,
    pub penetrate_type: PenetrateType,
    pub impact_type: ImpactType,
    pub proj_explosion: ProjExplosion,

    // Ammunition. Indices are assigned by the table: weapons with the same `ammo_name` share
    // `ammo_index`, those with the same `clip_name` share `clip_index`.
    pub start_ammo: i32,
    pub max_ammo: i32,
    pub clip_size: i32,
    pub shot_count: i32,
    pub ammo_index: u16,
    pub clip_index: u16,
    /// Shared ammo cap group, or `None`.
    pub shared_ammo_cap_index: Option<u16>,
    pub shared_ammo_cap: i32,
    pub ammo_name: Box<str>,
    pub clip_name: Box<str>,
    pub shared_ammo_cap_name: Box<str>,
    pub clip_only: bool,
    pub reload_ammo_add: i32,
    pub reload_start_add: i32,
    pub no_partial_reload: bool,
    /// `avoidDropCleanup`: the weapon is never the one freed to make room for a newer drop.
    pub avoid_drop_cleanup: bool,
    /// `iDropAmmoMin` and `iDropAmmoMax`: how much a weapon dropped without an owner holds.
    pub drop_ammo_min: i32,
    pub drop_ammo_max: i32,

    // Timing (ms).
    pub fire_delay: i32,
    pub fire_time: i32,
    pub rechamber_time: i32,
    pub rechamber_bolt_time: i32,
    pub hold_fire_time: i32,
    pub melee_delay: i32,
    pub melee_time: i32,
    pub melee_charge_delay: i32,
    pub melee_charge_time: i32,
    pub detonate_delay: i32,
    pub detonate_time: i32,
    pub reload_time: i32,
    pub reload_empty_time: i32,
    pub reload_add_time: i32,
    pub reload_start_time: i32,
    pub reload_start_add_time: i32,
    pub reload_end_time: i32,
    pub drop_time: i32,
    pub raise_time: i32,
    pub alt_drop_time: i32,
    pub alt_raise_time: i32,
    pub quick_drop_time: i32,
    pub quick_raise_time: i32,
    pub first_raise_time: i32,
    pub empty_raise_time: i32,
    pub empty_drop_time: i32,
    pub sprint_in_time: i32,
    pub sprint_out_time: i32,
    pub night_vision_wear_time: i32,
    pub night_vision_remove_time: i32,
    pub fuse_time: i32,
    pub ads_trans_in_time: i32,
    pub ads_trans_out_time: i32,
    pub position_reload_trans_time: i32,

    // Damage.
    pub damage: i32,
    pub min_damage: i32,
    pub player_damage: i32,
    pub min_player_damage: i32,
    pub melee_damage: i32,
    /// Distance the bullet starts losing damage at.
    pub max_damage_range: f32,
    /// Distance at which the bullet is down to the minimum damage.
    pub min_damage_range: f32,
    pub explosion_radius: i32,
    pub explosion_radius_min: i32,
    pub explosion_inner_damage: i32,
    pub explosion_outer_damage: i32,
    /// Degrees; the explosion only damages inside this cone (>= 180 means everywhere).
    pub damage_cone_angle: f32,
    pub location_damage_multipliers: [f32; HITLOC_COUNT],
    pub rifle_bullet: bool,
    pub armor_piercing: bool,

    // Behaviour flags.
    pub aim_down_sight: bool,
    pub bolt_action: bool,
    pub segmented_reload: bool,
    pub rechamber_while_ads: bool,
    pub ads_fire_only: bool,
    pub no_ads_when_mag_empty: bool,
    pub blocks_prone: bool,
    pub silenced: bool,
    pub freeze_movement_when_firing: bool,
    pub overlay_reticle: bool,
    pub hold_button_to_throw: bool,
    pub cook_off_hold: bool,
    pub has_detonator: bool,
    pub timed_detonation: bool,
    pub require_lockon_to_fire: bool,

    // Spread (degrees) and kick.
    pub hip_spread_stand_min: f32,
    pub hip_spread_ducked_min: f32,
    pub hip_spread_prone_min: f32,
    pub hip_spread_stand_max: f32,
    pub hip_spread_ducked_max: f32,
    pub hip_spread_prone_max: f32,
    pub hip_spread_decay_rate: f32,
    pub hip_spread_ducked_decay: f32,
    pub hip_spread_prone_decay: f32,
    pub hip_spread_fire_add: f32,
    pub hip_spread_turn_add: f32,
    pub hip_spread_move_add: f32,
    pub ads_spread: f32,
    pub ads_gun_kick_reduced_kick_bullets: i32,
    pub ads_gun_kick_reduced_kick_percent: f32,
    pub ads_gun_kick_pitch: [f32; 2],
    pub ads_gun_kick_yaw: [f32; 2],
    pub ads_view_kick_pitch: [f32; 2],
    pub ads_view_kick_yaw: [f32; 2],
    pub hip_gun_kick_reduced_kick_bullets: i32,
    pub hip_gun_kick_reduced_kick_percent: f32,
    pub hip_gun_kick_pitch: [f32; 2],
    pub hip_gun_kick_yaw: [f32; 2],
    pub hip_view_kick_pitch: [f32; 2],
    pub hip_view_kick_yaw: [f32; 2],

    // Movement.
    pub move_speed_scale: f32,
    pub ads_move_speed_scale: f32,
    pub sprint_duration_scale: f32,
    pub ads_zoom_fov: f32,
    /// `fOOPosAnimLength`: ADS blend per millisecond, in and out.
    pub oo_pos_anim_length: [f32; 2],

    // Projectiles and thrown weapons.
    pub projectile_speed: i32,
    pub projectile_speed_up: i32,
    pub projectile_speed_forward: i32,
    pub projectile_activate_dist: i32,
    pub proj_lifetime: f32,
    /// `timeToAccelerate`: seconds a rocket takes to reach `projectile_speed`.
    pub time_to_accelerate: f32,
    pub projectile_curvature: f32,
    /// `destabilizationRateTime`: seconds between a rocket's random course changes once it destabilises; 0 keeps it
    /// stable.
    pub destabilization_rate_time: f32,
    /// `destabilizationCurvatureMax`: the most a destabilised rocket's heading drifts per course change.
    pub destabilization_curvature_max: f32,
    /// `destabilizeDistance`: how far a rocket flies before it destabilises.
    pub destabilize_distance: i32,
    /// `guidedMissileType`: 0 none, 1 sidewinder, 2 hellfire, 3 javelin.
    pub guided_missile_type: i32,
    pub max_steering_accel: f32,
    /// `projIgnitionDelay`: milliseconds a javelin coasts before its motor lights.
    pub proj_ignition_delay: i32,
    pub proj_impact_explode: bool,
    pub stickiness: i32,
    pub parallel_bounce: [f32; SURFACE_TYPES],
    pub perpendicular_bounce: [f32; SURFACE_TYPES],

    /// What moves the gun in the player's hands.
    pub gun: GunParams,
    pub turret: TurretDef,
}

impl Default for WeaponInfo {
    /// A weapon with every value zero, i.e. an unconfigured definition.
    fn default() -> Self {
        Self {
            index: 0,
            name: "".into(),
            alt_weapon_name: "".into(),
            alt_weapon: 0,
            player_anim_type: 0,
            gun_model_mask: 1,
            has_knife_model: false,
            has_melee_charge_anim: false,
            weap_type: WeaponType::default(),
            weap_class: WeaponClass::default(),
            inventory_type: InventoryType::default(),
            fire_type: FireType::default(),
            offhand_class: OffhandClass::default(),
            penetrate_type: PenetrateType::default(),
            impact_type: ImpactType::default(),
            proj_explosion: ProjExplosion::default(),
            start_ammo: 0,
            max_ammo: 0,
            clip_size: 0,
            shot_count: 0,
            ammo_index: 0,
            clip_index: 0,
            shared_ammo_cap_index: None,
            shared_ammo_cap: 0,
            ammo_name: "".into(),
            clip_name: "".into(),
            shared_ammo_cap_name: "".into(),
            clip_only: false,
            reload_ammo_add: 0,
            reload_start_add: 0,
            no_partial_reload: false,
            avoid_drop_cleanup: false,
            drop_ammo_min: 0,
            drop_ammo_max: 0,
            fire_delay: 0,
            fire_time: 0,
            rechamber_time: 0,
            rechamber_bolt_time: 0,
            hold_fire_time: 0,
            melee_delay: 0,
            melee_time: 0,
            melee_charge_delay: 0,
            melee_charge_time: 0,
            detonate_delay: 0,
            detonate_time: 0,
            reload_time: 0,
            reload_empty_time: 0,
            reload_add_time: 0,
            reload_start_time: 0,
            reload_start_add_time: 0,
            reload_end_time: 0,
            drop_time: 0,
            raise_time: 0,
            alt_drop_time: 0,
            alt_raise_time: 0,
            quick_drop_time: 0,
            quick_raise_time: 0,
            first_raise_time: 0,
            empty_raise_time: 0,
            empty_drop_time: 0,
            sprint_in_time: 0,
            sprint_out_time: 0,
            night_vision_wear_time: 0,
            night_vision_remove_time: 0,
            fuse_time: 0,
            ads_trans_in_time: 0,
            ads_trans_out_time: 0,
            position_reload_trans_time: 0,
            damage: 0,
            min_damage: 0,
            player_damage: 0,
            min_player_damage: 0,
            melee_damage: 0,
            max_damage_range: DAMAGE_RANGE_UNLIMITED,
            min_damage_range: DAMAGE_RANGE_UNLIMITED_MIN,
            explosion_radius: 0,
            explosion_radius_min: 0,
            explosion_inner_damage: 0,
            explosion_outer_damage: 0,
            damage_cone_angle: 0.0,
            location_damage_multipliers: [1.0; HITLOC_COUNT],
            rifle_bullet: false,
            armor_piercing: false,
            aim_down_sight: false,
            bolt_action: false,
            segmented_reload: false,
            rechamber_while_ads: false,
            ads_fire_only: false,
            no_ads_when_mag_empty: false,
            blocks_prone: false,
            silenced: false,
            freeze_movement_when_firing: false,
            overlay_reticle: false,
            hold_button_to_throw: false,
            cook_off_hold: false,
            has_detonator: false,
            timed_detonation: false,
            require_lockon_to_fire: false,
            hip_spread_stand_min: 0.0,
            hip_spread_ducked_min: 0.0,
            hip_spread_prone_min: 0.0,
            hip_spread_stand_max: 0.0,
            hip_spread_ducked_max: 0.0,
            hip_spread_prone_max: 0.0,
            hip_spread_decay_rate: 0.0,
            hip_spread_ducked_decay: 0.0,
            hip_spread_prone_decay: 0.0,
            hip_spread_fire_add: 0.0,
            hip_spread_turn_add: 0.0,
            hip_spread_move_add: 0.0,
            ads_spread: 0.0,
            ads_gun_kick_reduced_kick_bullets: 0,
            ads_gun_kick_reduced_kick_percent: 0.0,
            ads_gun_kick_pitch: [0.0; 2],
            ads_gun_kick_yaw: [0.0; 2],
            ads_view_kick_pitch: [0.0; 2],
            ads_view_kick_yaw: [0.0; 2],
            hip_gun_kick_reduced_kick_bullets: 0,
            hip_gun_kick_reduced_kick_percent: 0.0,
            hip_gun_kick_pitch: [0.0; 2],
            hip_gun_kick_yaw: [0.0; 2],
            hip_view_kick_pitch: [0.0; 2],
            hip_view_kick_yaw: [0.0; 2],
            move_speed_scale: 1.0,
            ads_move_speed_scale: 0.0,
            sprint_duration_scale: 1.0,
            ads_zoom_fov: 0.0,
            oo_pos_anim_length: [1.0 / 300.0, 1.0 / 500.0],
            projectile_speed: 0,
            projectile_speed_up: 0,
            projectile_speed_forward: 0,
            projectile_activate_dist: 0,
            proj_lifetime: 0.0,
            time_to_accelerate: 0.0,
            projectile_curvature: 0.0,
            destabilization_rate_time: 0.0,
            destabilization_curvature_max: 0.0,
            destabilize_distance: 0,
            guided_missile_type: 0,
            max_steering_accel: 0.0,
            proj_ignition_delay: 0,
            proj_impact_explode: false,
            stickiness: 0,
            parallel_bounce: [0.0; SURFACE_TYPES],
            perpendicular_bounce: [0.0; SURFACE_TYPES],
            gun: GunParams::default(),
            turret: TurretDef::default(),
        }
    }
}

/// `BG_CheckWeaponDamageRanges`: an unset (non-positive) max range becomes this.
pub const DAMAGE_RANGE_UNLIMITED: f32 = 999_999.0;
/// `BG_CheckWeaponDamageRanges`: an unset min range becomes this (just past the max).
pub const DAMAGE_RANGE_UNLIMITED_MIN: f32 = 999_999.1;

impl WeaponInfo {
    /// Copies what simulation needs out of a decoded definition. The table assigns `index`,
    /// `alt_weapon` and the ammo, clip and shared-cap indices afterwards.
    pub fn from_def(def: &WeaponDef) -> Self {
        let text = |n: &assets::zone::gfx::Name| -> Box<str> { n.as_deref().unwrap_or("").into() };
        let flag = |v: i32| v != 0;
        let mut gun_model_mask = 0u16;
        for (i, m) in def.gun_models.iter().take(16).enumerate() {
            if m.is_some() {
                gun_model_mask |= 1 << i;
            }
        }
        let has_melee_charge_anim = def
            .anims
            .get(ANIM_MELEE_CHARGE)
            .is_some_and(|n| n.as_deref().is_some_and(|s| !s.is_empty()));
        let oo_pos_anim_length =
            if def.oo_pos_anim_length[0] > 0.0 && def.oo_pos_anim_length[1] > 0.0 {
                def.oo_pos_anim_length
            } else {
                transition_rates(def.ads_trans_in_time, def.ads_trans_out_time)
            };
        let [max_damage_range, min_damage_range] =
            damage_ranges(def.max_damage_range, def.min_damage_range);
        let pair = |a: f32, b: f32| [a, b];
        Self {
            index: 0,
            name: text(&def.internal_name),
            alt_weapon_name: text(&def.alt_weapon_name),
            alt_weapon: 0,
            player_anim_type: def.player_anim_type,
            gun_model_mask,
            has_knife_model: def.knife_model.is_some(),
            has_melee_charge_anim,
            weap_type: WeaponType::from_raw(def.weap_type),
            weap_class: WeaponClass::from_raw(def.weap_class),
            inventory_type: InventoryType::from_raw(def.inventory_type),
            fire_type: FireType::from_raw(def.fire_type),
            offhand_class: OffhandClass::from_raw(def.offhand_class),
            penetrate_type: PenetrateType::from_raw(def.penetrate_type),
            impact_type: ImpactType::from_raw(def.impact_type),
            proj_explosion: ProjExplosion::from_raw(def.proj_explosion),
            start_ammo: def.start_ammo,
            max_ammo: def.max_ammo,
            clip_size: def.clip_size,
            shot_count: def.shot_count,
            ammo_index: 0,
            clip_index: 0,
            shared_ammo_cap_index: None,
            shared_ammo_cap: def.shared_ammo_cap,
            ammo_name: text(&def.ammo_name),
            clip_name: text(&def.clip_name),
            shared_ammo_cap_name: text(&def.shared_ammo_cap_name),
            clip_only: flag(def.clip_only),
            reload_ammo_add: def.reload_ammo_add,
            reload_start_add: def.reload_start_add,
            no_partial_reload: flag(def.no_partial_reload),
            avoid_drop_cleanup: flag(def.avoid_drop_cleanup),
            drop_ammo_min: def.drop_ammo_min,
            drop_ammo_max: def.drop_ammo_max,
            fire_delay: def.fire_delay,
            fire_time: def.fire_time,
            rechamber_time: def.rechamber_time,
            rechamber_bolt_time: def.rechamber_bolt_time,
            hold_fire_time: def.hold_fire_time,
            melee_delay: def.melee_delay,
            melee_time: def.melee_time,
            melee_charge_delay: def.melee_charge_delay,
            melee_charge_time: def.melee_charge_time,
            detonate_delay: def.detonate_delay,
            detonate_time: def.detonate_time,
            reload_time: def.reload_time,
            reload_empty_time: def.reload_empty_time,
            reload_add_time: def.reload_add_time,
            reload_start_time: def.reload_start_time,
            reload_start_add_time: def.reload_start_add_time,
            reload_end_time: def.reload_end_time,
            drop_time: def.drop_time,
            raise_time: def.raise_time,
            alt_drop_time: def.alt_drop_time,
            alt_raise_time: def.alt_raise_time,
            quick_drop_time: def.quick_drop_time,
            quick_raise_time: def.quick_raise_time,
            first_raise_time: def.first_raise_time,
            empty_raise_time: def.empty_raise_time,
            empty_drop_time: def.empty_drop_time,
            sprint_in_time: def.sprint_in_time,
            sprint_out_time: def.sprint_out_time,
            night_vision_wear_time: def.night_vision_wear_time,
            night_vision_remove_time: def.night_vision_remove_time,
            fuse_time: def.fuse_time,
            ads_trans_in_time: def.ads_trans_in_time,
            ads_trans_out_time: def.ads_trans_out_time,
            position_reload_trans_time: def.position_reload_trans_time,
            damage: def.damage,
            min_damage: def.min_damage,
            player_damage: def.player_damage,
            min_player_damage: def.min_player_damage,
            melee_damage: def.melee_damage,
            max_damage_range,
            min_damage_range,
            explosion_radius: def.explosion_radius,
            explosion_radius_min: def.explosion_radius_min,
            explosion_inner_damage: def.explosion_inner_damage,
            explosion_outer_damage: def.explosion_outer_damage,
            damage_cone_angle: def.damage_cone_angle,
            location_damage_multipliers: def.location_damage_multipliers,
            rifle_bullet: flag(def.rifle_bullet),
            armor_piercing: flag(def.armor_piercing),
            aim_down_sight: flag(def.aim_down_sight),
            bolt_action: flag(def.bolt_action),
            segmented_reload: flag(def.segmented_reload),
            rechamber_while_ads: flag(def.rechamber_while_ads),
            ads_fire_only: flag(def.ads_fire_only),
            no_ads_when_mag_empty: flag(def.no_ads_when_mag_empty),
            blocks_prone: flag(def.blocks_prone),
            silenced: flag(def.silenced),
            freeze_movement_when_firing: flag(def.freeze_movement_when_firing),
            overlay_reticle: flag(def.overlay_reticle),
            hold_button_to_throw: flag(def.hold_button_to_throw),
            cook_off_hold: flag(def.cook_off_hold),
            has_detonator: flag(def.has_detonator),
            timed_detonation: flag(def.timed_detonation),
            require_lockon_to_fire: flag(def.require_lockon_to_fire),
            hip_spread_stand_min: def.hip_spread_stand_min,
            hip_spread_ducked_min: def.hip_spread_ducked_min,
            hip_spread_prone_min: def.hip_spread_prone_min,
            hip_spread_stand_max: def.hip_spread_stand_max,
            hip_spread_ducked_max: def.hip_spread_ducked_max,
            hip_spread_prone_max: def.hip_spread_prone_max,
            hip_spread_decay_rate: def.hip_spread_decay_rate,
            hip_spread_ducked_decay: def.hip_spread_ducked_decay,
            hip_spread_prone_decay: def.hip_spread_prone_decay,
            hip_spread_fire_add: def.hip_spread_fire_add,
            hip_spread_turn_add: def.hip_spread_turn_add,
            hip_spread_move_add: def.hip_spread_move_add,
            ads_spread: def.ads_spread,
            ads_gun_kick_reduced_kick_bullets: def.ads_gun_kick_reduced_kick_bullets,
            ads_gun_kick_reduced_kick_percent: def.ads_gun_kick_reduced_kick_percent,
            ads_gun_kick_pitch: pair(def.ads_gun_kick_pitch_min, def.ads_gun_kick_pitch_max),
            ads_gun_kick_yaw: pair(def.ads_gun_kick_yaw_min, def.ads_gun_kick_yaw_max),
            ads_view_kick_pitch: pair(def.ads_view_kick_pitch_min, def.ads_view_kick_pitch_max),
            ads_view_kick_yaw: pair(def.ads_view_kick_yaw_min, def.ads_view_kick_yaw_max),
            hip_gun_kick_reduced_kick_bullets: def.hip_gun_kick_reduced_kick_bullets,
            hip_gun_kick_reduced_kick_percent: def.hip_gun_kick_reduced_kick_percent,
            hip_gun_kick_pitch: pair(def.hip_gun_kick_pitch_min, def.hip_gun_kick_pitch_max),
            hip_gun_kick_yaw: pair(def.hip_gun_kick_yaw_min, def.hip_gun_kick_yaw_max),
            hip_view_kick_pitch: pair(def.hip_view_kick_pitch_min, def.hip_view_kick_pitch_max),
            hip_view_kick_yaw: pair(def.hip_view_kick_yaw_min, def.hip_view_kick_yaw_max),
            move_speed_scale: def.move_speed_scale,
            ads_move_speed_scale: def.ads_move_speed_scale,
            sprint_duration_scale: def.sprint_duration_scale,
            ads_zoom_fov: def.ads_zoom_fov,
            oo_pos_anim_length,
            projectile_speed: def.projectile_speed,
            projectile_speed_up: def.projectile_speed_up,
            projectile_speed_forward: def.projectile_speed_forward,
            projectile_activate_dist: def.projectile_activate_dist,
            proj_lifetime: def.proj_lifetime,
            time_to_accelerate: def.time_to_accelerate,
            projectile_curvature: def.projectile_curvature,
            destabilization_rate_time: def.destabilization_rate_time,
            destabilization_curvature_max: def.destabilization_curvature_max,
            destabilize_distance: def.destabilize_distance,
            guided_missile_type: def.guided_missile_type,
            max_steering_accel: def.max_steering_accel,
            proj_ignition_delay: def.proj_ignition_delay,
            proj_impact_explode: flag(def.proj_impact_explode),
            stickiness: def.stickiness,
            parallel_bounce: def.parallel_bounce,
            perpendicular_bounce: def.perpendicular_bounce,
            gun: GunParams::from_def(def),
            turret: TurretDef {
                left_arc: def.left_arc,
                right_arc: def.right_arc,
                top_arc: def.top_arc,
                bottom_arc: def.bottom_arc,
                stance: def.stance,
                player_spread: def.player_spread,
                use_hint_string: text(&def.use_hint_string),
                drop_hint_string: text(&def.drop_hint_string),
            },
        }
    }

    /// The values player movement reads (`WeaponMove`). `ammo_in_clip` is the caller's: it
    /// depends on the player, not the weapon.
    pub fn weapon_move(&self) -> WeaponMove {
        WeaponMove {
            move_speed_scale: self.move_speed_scale,
            ads_move_speed_scale: self.ads_move_speed_scale,
            sprint_duration_scale: self.sprint_duration_scale,
            blocks_prone: self.blocks_prone,
            freeze_movement_when_firing: self.freeze_movement_when_firing,
            overlay_reticle: self.overlay_reticle,
            aim_down_sight: self.aim_down_sight,
            no_ads_when_mag_empty: self.no_ads_when_mag_empty,
            ammo_in_clip: 0,
            is_player: true,
            pos_blend_in_rate: self.oo_pos_anim_length[0],
            pos_blend_out_rate: self.oo_pos_anim_length[1],
            segmented_reload: self.segmented_reload,
            position_reload_trans_time: self.position_reload_trans_time,
            rechamber_while_ads: self.rechamber_while_ads,
            ads_fire_only: self.ads_fire_only,
        }
    }

    /// Whether weapon model variant `model` exists (`BG_CanPlayerHaveWeapon`'s `gunXModel[model]`).
    pub fn has_gun_model(&self, model: u8) -> bool {
        model < 16 && self.gun_model_mask & (1 << model) != 0
    }

    /// `PM_GetWeaponFireButton`: the button that fires the weapon (detonator grenades use the
    /// throw button).
    pub fn fire_button(&self) -> i32 {
        if self.weap_type == WeaponType::Grenade && self.has_detonator {
            crate::pm::button::THROW
        } else {
            crate::pm::button::ATTACK
        }
    }

    /// The bullet-per-shot count of one trigger pull: pellets for spread (shotgun) weapons.
    pub fn pellets(&self) -> i32 {
        if self.weap_class == WeaponClass::Spread {
            self.shot_count
        } else {
            1
        }
    }

    /// `weaponissemiauto`.
    pub fn is_semi_auto(&self) -> bool {
        self.fire_type == FireType::SingleShot
    }
}

/// `BG_SetupTransitionTimes`.
fn transition_rates(trans_in: i32, trans_out: i32) -> [f32; 2] {
    let rate = |t: i32, default: f32| {
        if t <= 0 {
            1.0 / default
        } else {
            (1.0 / f64::from(t)) as f32
        }
    };
    [rate(trans_in, 300.0), rate(trans_out, 500.0)]
}

/// `BG_CheckWeaponDamageRanges`.
fn damage_ranges(max: f32, min: f32) -> [f32; 2] {
    [
        if max <= 0.0 {
            DAMAGE_RANGE_UNLIMITED
        } else {
            max
        },
        if min <= 0.0 {
            DAMAGE_RANGE_UNLIMITED_MIN
        } else {
            min
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_names_match_the_original_tables() {
        assert_eq!(WeaponClass::from_raw(8).name(), "non-player");
        assert_eq!(WeaponClass::from_raw(6).name(), "rocketlauncher");
        assert_eq!(WeaponClass::from_raw(9).name(), "item");
        assert_eq!(InventoryType::from_raw(3).name(), "altmode");
        assert_eq!(WeaponType::from_raw(3).name(), "binoculars");
        assert_eq!(FireType::from_raw(1).name(), "Single Shot");
        assert_eq!(OffhandClass::from_raw(3).name(), "Flash Grenade");
        assert_eq!(WeaponClass::ALL.len(), 10);
    }

    #[test]
    fn out_of_range_raw_values_fall_back_to_first() {
        assert_eq!(WeaponClass::from_raw(-1), WeaponClass::Rifle);
        assert_eq!(WeaponClass::from_raw(10), WeaponClass::Rifle);
        assert_eq!(FireType::from_raw(99), FireType::FullAuto);
    }

    #[test]
    fn unset_damage_ranges_become_unlimited() {
        assert_eq!(
            damage_ranges(0.0, -1.0),
            [DAMAGE_RANGE_UNLIMITED, DAMAGE_RANGE_UNLIMITED_MIN]
        );
        assert_eq!(damage_ranges(500.0, 1000.0), [500.0, 1000.0]);
    }

    #[test]
    fn ads_blend_rates_default_without_transition_times() {
        let [i, o] = transition_rates(0, 0);
        assert_eq!(i, 1.0 / 300.0);
        assert_eq!(o, 1.0 / 500.0);
        assert_eq!(transition_rates(250, 100)[0], (1.0f64 / 250.0) as f32);
    }

    #[test]
    fn detonator_grenades_fire_with_the_throw_button() {
        let mut w = WeaponInfo {
            weap_type: WeaponType::Grenade,
            ..WeaponInfo::default()
        };
        assert_eq!(w.fire_button(), crate::pm::button::ATTACK);
        w.has_detonator = true;
        assert_eq!(w.fire_button(), crate::pm::button::THROW);
    }
}
