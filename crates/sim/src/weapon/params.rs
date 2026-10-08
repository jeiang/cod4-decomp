// SPDX-License-Identifier: GPL-3.0-only
//! Weapon tunables (the original's dvars). `WeaponParams::default()` is a stock multiplayer
//! server; fact source: the dvar registrations in `BG_RegisterDvars` and `BG_RegisterPerks`.

/// Perk bits the weapon code reads (`playerState_t.perks`).
pub mod perk {
    /// `specialty_bulletaccuracy`: scales the aim spread (`perk_weapSpreadMultiplier`).
    pub const BULLET_ACCURACY: u32 = 0x2;
    /// `specialty_fastreload`: faster reloads (`perk_weapReloadMultiplier`).
    pub const FAST_RELOAD: u32 = 0x4;
    /// `specialty_rof`: faster fire and rechamber (`perk_weapRateMultiplier`).
    pub const RATE_OF_FIRE: u32 = 0x8;
    /// `specialty_bulletpenetration`: deeper penetration (`perk_bulletPenetrationMultiplier`).
    pub const BULLET_PENETRATION: u32 = 0x20;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponParams {
    /// `bg_aimSpreadMoveSpeedThreshold`: above this speed moving widens the spread.
    pub aim_spread_move_speed_threshold: f32,
    /// `player_burstFireCooldown` (seconds); 0 means the minimum 1 ms.
    pub burst_fire_cooldown: f32,
    /// `player_sustainAmmo`: firing does not use clip ammunition.
    pub sustain_ammo: bool,
    pub perk_weap_spread_multiplier: f32,
    pub perk_weap_reload_multiplier: f32,
    pub perk_weap_rate_multiplier: f32,
    pub perk_bullet_penetration_multiplier: f32,
    /// `player_meleeRange`, `player_meleeWidth`, `player_meleeHeight`.
    pub melee_range: f32,
    pub melee_width: f32,
    pub melee_height: f32,
    /// `bullet_penetrationEnabled`.
    pub bullet_penetration_enabled: bool,
}

impl Default for WeaponParams {
    fn default() -> Self {
        Self {
            aim_spread_move_speed_threshold: 11.0,
            burst_fire_cooldown: 0.2,
            sustain_ammo: false,
            perk_weap_spread_multiplier: 0.65,
            perk_weap_reload_multiplier: 0.5,
            perk_weap_rate_multiplier: 0.75,
            perk_bullet_penetration_multiplier: 2.0,
            melee_range: 64.0,
            melee_width: 10.0,
            melee_height: 10.0,
            bullet_penetration_enabled: true,
        }
    }
}
