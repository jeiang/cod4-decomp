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
    /// `specialty_holdbreath`: holds the breath longer (`perk_extraBreath`).
    pub const EXTRA_BREATH: u32 = 0x10;
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
    /// `player_breath_hold_time` (seconds): the longest a scoped weapon's breath is held. 0 turns holding off.
    pub breath_hold_time: f32,
    /// `player_breath_gasp_time`: how long the gasp lasts once the breath is out.
    pub breath_gasp_time: f32,
    /// `player_breath_fire_delay`: breath time a shot costs.
    pub breath_fire_delay: f32,
    /// `player_breath_gasp_scale`: the sway's amplitude during the gasp.
    pub breath_gasp_scale: f32,
    /// `player_breath_hold_lerp`, `player_breath_gasp_lerp`: how fast the sway scale follows its target.
    pub breath_hold_lerp: f32,
    pub breath_gasp_lerp: f32,
    /// `perk_extraBreath`: seconds the perk adds to the hold time.
    pub perk_extra_breath: f32,
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
            breath_hold_time: 4.5,
            breath_gasp_time: 1.0,
            breath_fire_delay: 0.0,
            breath_gasp_scale: 4.5,
            breath_hold_lerp: 1.0,
            breath_gasp_lerp: 6.0,
            perk_extra_breath: 5.0,
        }
    }
}
