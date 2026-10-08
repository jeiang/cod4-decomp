// SPDX-License-Identifier: GPL-3.0-only
//! Movement tunables (the original's dvars) and the per-weapon values movement reads.
//!
//! Defaults are the multiplayer registration defaults of `iw3mp.exe` 1.7 (fact source: the
//! dvar registration calls in `Jump_RegisterDvars`, `Mantle_RegisterDvars`, `BG_RegisterDvars`,
//! `G_RegisterDvars`).

use super::mantle::MantleAnims;

/// Movement dvars. `Params::default()` is a stock server.
#[derive(Debug, Clone)]
pub struct Params {
    // Core movement.
    pub friction: f32,
    pub stopspeed: f32,
    pub inertia_max: f32,
    pub inertia_angle: f32,
    // Jump.
    pub jump_height: f32,
    pub jump_step_size: f32,
    pub jump_slowdown_enable: bool,
    pub jump_ladder_push_vel: f32,
    pub jump_spread_add: f32,
    // Mantle.
    pub mantle_enable: bool,
    pub mantle_check_range: f32,
    pub mantle_check_radius: f32,
    pub mantle_check_angle: f32,
    pub mantle_view_yawcap: f32,
    // View limits.
    pub bg_ladder_yawcap: f32,
    pub bg_prone_yawcap: f32,
    pub player_view_pitch_up: f32,
    pub player_view_pitch_down: f32,
    // Fall damage.
    pub bg_fall_damage_min_height: f32,
    pub bg_fall_damage_max_height: f32,
    // Foliage sounds.
    pub bg_foliagesnd_minspeed: f32,
    pub bg_foliagesnd_maxspeed: f32,
    pub bg_foliagesnd_slowinterval: i32,
    pub bg_foliagesnd_fastinterval: i32,
    pub bg_foliagesnd_resetinterval: i32,
    // Speed scales.
    pub player_strafe_speed_scale: f32,
    pub player_back_speed_scale: f32,
    pub player_spectate_speed_scale: f32,
    pub player_move_threshhold: f32,
    pub player_footsteps_threshhold: f32,
    // Sprint.
    pub player_sprint_forward_minimum: i32,
    pub player_sprint_speed_scale: f32,
    pub player_sprint_time: f32,
    pub player_sprint_min_time: f32,
    pub player_sprint_recharge_pause: f32,
    pub player_sprint_strafe_speed_scale: f32,
    pub player_sprint_camera_bob: f32,
    pub perk_sprint_multiplier: f32,
    // Damage slowdown and melee charge.
    pub player_dmgtimer_max_time: f32,
    pub player_dmgtimer_min_scale: f32,
    pub player_melee_charge_friction: f32,
    // Aim down sights.
    pub player_ads_exit_delay: i32,
    pub player_scope_exit_on_damage: bool,
    /// Mantle animation tracks; mantling is unavailable until set.
    pub mantle_anims: Option<MantleAnims>,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            friction: 5.5,
            stopspeed: 100.0,
            inertia_max: 50.0,
            inertia_angle: 0.0,
            jump_height: 39.0,
            jump_step_size: 18.0,
            jump_slowdown_enable: true,
            jump_ladder_push_vel: 128.0,
            jump_spread_add: 64.0,
            mantle_enable: true,
            mantle_check_range: 20.0,
            mantle_check_radius: 0.1,
            mantle_check_angle: 60.0,
            mantle_view_yawcap: 60.0,
            bg_ladder_yawcap: 100.0,
            bg_prone_yawcap: 85.0,
            player_view_pitch_up: 85.0,
            player_view_pitch_down: 85.0,
            bg_fall_damage_min_height: 128.0,
            bg_fall_damage_max_height: 300.0,
            bg_foliagesnd_minspeed: 40.0,
            bg_foliagesnd_maxspeed: 180.0,
            bg_foliagesnd_slowinterval: 1500,
            bg_foliagesnd_fastinterval: 500,
            bg_foliagesnd_resetinterval: 500,
            player_strafe_speed_scale: 0.8,
            player_back_speed_scale: 0.7,
            player_spectate_speed_scale: 1.0,
            player_move_threshhold: 10.0,
            player_footsteps_threshhold: 0.0,
            player_sprint_forward_minimum: 105,
            player_sprint_speed_scale: 1.5,
            player_sprint_time: 4.0,
            player_sprint_min_time: 1.0,
            player_sprint_recharge_pause: 0.0,
            player_sprint_strafe_speed_scale: 0.667,
            player_sprint_camera_bob: 0.5,
            perk_sprint_multiplier: 2.0,
            player_dmgtimer_max_time: 750.0,
            player_dmgtimer_min_scale: 0.0,
            player_melee_charge_friction: 1200.0,
            player_ads_exit_delay: 0,
            player_scope_exit_on_damage: false,
            mantle_anims: None,
        }
    }
}

/// The weapon definition values movement reads for the player's current weapon
/// (`WeaponDef` fields of the same names). `Default` is "no weapon".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponMove {
    /// `moveSpeedScale`; 0 means "use the ADS scale".
    pub move_speed_scale: f32,
    pub ads_move_speed_scale: f32,
    /// `sprintDurationScale`.
    pub sprint_duration_scale: f32,
    pub blocks_prone: bool,
    pub freeze_movement_when_firing: bool,
    /// The weapon has a scope overlay (`overlayReticle`): ADS is a sniper scope.
    pub overlay_reticle: bool,
    pub aim_down_sight: bool,
    pub no_ads_when_mag_empty: bool,
    /// Rounds in the current clip (`ammoclip[clipIndex]`).
    pub ammo_in_clip: i32,
    /// Whether the player entity is a real player (`POF_PLAYER`); ADS is only allowed then.
    pub is_player: bool,
    // ADS blend (`fOOPosAnimLength`, reload transition rules).
    pub pos_blend_in_rate: f32,
    pub pos_blend_out_rate: f32,
    pub segmented_reload: bool,
    pub position_reload_trans_time: i32,
    pub rechamber_while_ads: bool,
    pub ads_fire_only: bool,
}

impl Default for WeaponMove {
    fn default() -> Self {
        Self {
            move_speed_scale: 1.0,
            ads_move_speed_scale: 0.0,
            sprint_duration_scale: 1.0,
            blocks_prone: false,
            freeze_movement_when_firing: false,
            overlay_reticle: false,
            aim_down_sight: false,
            no_ads_when_mag_empty: false,
            ammo_in_clip: 0,
            is_player: true,
            pos_blend_in_rate: 0.0,
            pos_blend_out_rate: 0.0,
            segmented_reload: false,
            position_reload_trans_time: 0,
            rechamber_while_ads: false,
            ads_fire_only: false,
        }
    }
}
