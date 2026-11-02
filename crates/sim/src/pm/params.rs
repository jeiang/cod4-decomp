// SPDX-License-Identifier: GPL-3.0-only
//! Movement and weapon tunables (the original's dvars) and the per-weapon values movement reads.
//!
//! Defaults are the multiplayer registration defaults of `iw3mp.exe` 1.7 (fact source: the
//! dvar registration calls in `Jump_RegisterDvars`, `Mantle_RegisterDvars`, `BG_RegisterDvars`,
//! `G_RegisterDvars`).

use super::mantle::MantleAnims;
use crate::weapon::WeaponParams;

/// Movement and weapon dvars. `Params::default()` is a stock server.
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
    /// The view and gun bob (`bg_bobMax`, `bg_bobAmplitude*`).
    pub bob: super::bob::Bob,
    /// The dvars the weapon code reads (breath, melee, burst cooldown, penetration, perks).
    pub weapon: WeaponParams,
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
            bob: super::bob::Bob::default(),
            weapon: WeaponParams::default(),
            mantle_anims: None,
        }
    }
}

/// The type and range of a movement dvar (`dvarType_t` with its `DvarLimits`), for the server to register.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Domain {
    Bool,
    Int(i32, i32),
    Float(f32, f32),
    /// Two floats, each held to the range.
    Vec2(f32, f32),
}

/// One tunable's storage with the range its stock registration holds it to, so [`Params`] has a single table of
/// dvar names.
enum Tunable<'a> {
    Float(&'a mut f32, f32, f32),
    Int(&'a mut i32, i32, i32),
    Bool(&'a mut bool),
    /// Two floats, `"x y"`, each held to the range (`min`, `max`).
    Vec2(&'a mut [f32; 2], f32, f32),
}

impl Tunable<'_> {
    fn text(&self) -> String {
        match self {
            Tunable::Float(v, ..) => v.to_string(),
            Tunable::Int(v, ..) => v.to_string(),
            Tunable::Bool(v) => u8::from(**v).to_string(),
            Tunable::Vec2(v, ..) => format!("{} {}", v[0], v[1]),
        }
    }

    fn domain(&self) -> Domain {
        match *self {
            Tunable::Float(_, lo, hi) => Domain::Float(lo, hi),
            Tunable::Int(_, lo, hi) => Domain::Int(lo, hi),
            Tunable::Bool(_) => Domain::Bool,
            Tunable::Vec2(_, lo, hi) => Domain::Vec2(lo, hi),
        }
    }

    /// Takes the value `text` gives; text that is no number leaves it as it was (the original's `atof`/`atoi`
    /// would give 0, but a dvar only holds what its registration accepts). A two-float value takes its
    /// components in order and a missing second one is 0, as `Dvar_StringToVec2` reads them.
    fn set(&mut self, text: &str) {
        let text = text.trim();
        let mut words = text.split_whitespace().map(str::parse::<f32>);
        let first = match self {
            Tunable::Vec2(..) => words.next(),
            _ => Some(text.parse::<f32>()),
        };
        let Some(Ok(n)) = first else {
            tracing::warn!(
                value = text,
                "tunable value is not a number, kept as it was"
            );
            return;
        };
        match self {
            Tunable::Float(v, lo, hi) => **v = n.clamp(*lo, *hi),
            Tunable::Int(v, lo, hi) => **v = (n as i32).clamp(*lo, *hi),
            Tunable::Bool(v) => **v = n != 0.0,
            Tunable::Vec2(v, lo, hi) => {
                let y = words.next().and_then(Result::ok).unwrap_or(0.0);
                **v = [n.clamp(*lo, *hi), y.clamp(*lo, *hi)];
            }
        }
    }
}

impl Params {
    /// Every tunable with the dvar it is, as `Jump_RegisterDvars`, `Mantle_RegisterDvars`, `BG_RegisterDvars`,
    /// `BG_RegisterPerks` and `G_RegisterDvars` name them.
    fn tunables(&mut self) -> Vec<(&'static str, Tunable<'_>)> {
        use Tunable::{Bool, Float, Int, Vec2};
        vec![
            ("friction", Float(&mut self.friction, 0.0, 100.0)),
            ("stopspeed", Float(&mut self.stopspeed, 0.0, 1000.0)),
            ("inertiaMax", Float(&mut self.inertia_max, 0.0, 1000.0)),
            ("inertiaAngle", Float(&mut self.inertia_angle, -1.0, 1.0)),
            ("jump_height", Float(&mut self.jump_height, 0.0, 1000.0)),
            ("jump_stepSize", Float(&mut self.jump_step_size, 0.0, 64.0)),
            ("jump_slowdownEnable", Bool(&mut self.jump_slowdown_enable)),
            (
                "jump_ladderPushVel",
                Float(&mut self.jump_ladder_push_vel, 0.0, 1024.0),
            ),
            (
                "jump_spreadAdd",
                Float(&mut self.jump_spread_add, 0.0, 512.0),
            ),
            ("mantle_enable", Bool(&mut self.mantle_enable)),
            (
                "mantle_check_range",
                Float(&mut self.mantle_check_range, 0.0, 128.0),
            ),
            (
                "mantle_check_radius",
                Float(&mut self.mantle_check_radius, 0.0, 15.0),
            ),
            (
                "mantle_check_angle",
                Float(&mut self.mantle_check_angle, 0.0, 180.0),
            ),
            (
                "mantle_view_yawcap",
                Float(&mut self.mantle_view_yawcap, 0.0, 180.0),
            ),
            (
                "bg_ladder_yawcap",
                Float(&mut self.bg_ladder_yawcap, 0.0, 360.0),
            ),
            (
                "bg_prone_yawcap",
                Float(&mut self.bg_prone_yawcap, 0.0, 360.0),
            ),
            (
                "player_view_pitch_up",
                Float(&mut self.player_view_pitch_up, 0.0, 90.0),
            ),
            (
                "player_view_pitch_down",
                Float(&mut self.player_view_pitch_down, 0.0, 90.0),
            ),
            (
                "bg_fallDamageMinHeight",
                Float(&mut self.bg_fall_damage_min_height, 1.0, f32::MAX),
            ),
            (
                "bg_fallDamageMaxHeight",
                Float(&mut self.bg_fall_damage_max_height, 1.0, f32::MAX),
            ),
            (
                "bg_foliagesnd_minspeed",
                Float(&mut self.bg_foliagesnd_minspeed, 0.0, f32::MAX),
            ),
            (
                "bg_foliagesnd_maxspeed",
                Float(&mut self.bg_foliagesnd_maxspeed, 0.0, f32::MAX),
            ),
            (
                "bg_foliagesnd_slowinterval",
                Int(&mut self.bg_foliagesnd_slowinterval, 0, i32::MAX),
            ),
            (
                "bg_foliagesnd_fastinterval",
                Int(&mut self.bg_foliagesnd_fastinterval, 0, i32::MAX),
            ),
            (
                "bg_foliagesnd_resetinterval",
                Int(&mut self.bg_foliagesnd_resetinterval, 0, i32::MAX),
            ),
            (
                "player_strafeSpeedScale",
                Float(&mut self.player_strafe_speed_scale, 0.0, 20.0),
            ),
            (
                "player_backSpeedScale",
                Float(&mut self.player_back_speed_scale, 0.0, 20.0),
            ),
            (
                "player_spectateSpeedScale",
                Float(&mut self.player_spectate_speed_scale, 0.0, 20.0),
            ),
            (
                "player_moveThreshhold",
                Float(&mut self.player_move_threshhold, 0.000_000_01, 20.0),
            ),
            (
                "player_footstepsThreshhold",
                Float(&mut self.player_footsteps_threshhold, 0.0, 50000.0),
            ),
            (
                "player_sprintForwardMinimum",
                Int(&mut self.player_sprint_forward_minimum, 0, 255),
            ),
            (
                "player_sprintSpeedScale",
                Float(&mut self.player_sprint_speed_scale, 0.0, 5.0),
            ),
            (
                "player_sprintTime",
                Float(&mut self.player_sprint_time, 0.0, 12.8),
            ),
            (
                "player_sprintMinTime",
                Float(&mut self.player_sprint_min_time, 0.0, 12.8),
            ),
            (
                "player_sprintRechargePause",
                Float(&mut self.player_sprint_recharge_pause, 0.0, 9000.0),
            ),
            (
                "player_sprintStrafeSpeedScale",
                Float(&mut self.player_sprint_strafe_speed_scale, 0.0, 5000.0),
            ),
            (
                "player_sprintCameraBob",
                Float(&mut self.player_sprint_camera_bob, 0.0, 2.0),
            ),
            (
                "perk_sprintMultiplier",
                Float(&mut self.perk_sprint_multiplier, 0.0, f32::MAX),
            ),
            (
                "player_dmgtimer_maxTime",
                Float(&mut self.player_dmgtimer_max_time, 0.0, f32::MAX),
            ),
            (
                "player_dmgtimer_minScale",
                Float(&mut self.player_dmgtimer_min_scale, 0.0, 1.0),
            ),
            (
                "player_meleeChargeFriction",
                Float(&mut self.player_melee_charge_friction, 1.0, 5000.0),
            ),
            (
                "player_adsExitDelay",
                Int(&mut self.player_ads_exit_delay, 0, 1000),
            ),
            (
                "player_scopeExitOnDamage",
                Bool(&mut self.player_scope_exit_on_damage),
            ),
            // `BG_RegisterDvars`: the bob of the eye and the gun, cheat protected. The amplitudes are
            // (horizontal, vertical) per unit of speed.
            ("bg_bobMax", Float(&mut self.bob.max, 0.0, 36.0)),
            (
                "bg_bobAmplitudeStanding",
                Vec2(&mut self.bob.standing, 0.0, 1.0),
            ),
            (
                "bg_bobAmplitudeDucked",
                Vec2(&mut self.bob.ducked, 0.0, 1.0),
            ),
            ("bg_bobAmplitudeProne", Vec2(&mut self.bob.prone, 0.0, 1.0)),
            (
                "bg_bobAmplitudeSprinting",
                Vec2(&mut self.bob.sprinting, 0.0, 1.0),
            ),
            // The weapon dvars (`BG_RegisterDvars`, `BG_RegisterPerks`, `G_RegisterDvars`), all cheat protected.
            (
                "bg_aimSpreadMoveSpeedThreshold",
                Float(&mut self.weapon.aim_spread_move_speed_threshold, 0.0, 300.0),
            ),
            (
                "player_burstFireCooldown",
                Float(&mut self.weapon.burst_fire_cooldown, 0.0, 60.0),
            ),
            ("player_sustainAmmo", Bool(&mut self.weapon.sustain_ammo)),
            (
                "perk_weapSpreadMultiplier",
                Float(&mut self.weapon.perk_weap_spread_multiplier, 0.0, 1.0),
            ),
            (
                "perk_weapReloadMultiplier",
                Float(&mut self.weapon.perk_weap_reload_multiplier, 0.0, 1.0),
            ),
            (
                "perk_weapRateMultiplier",
                Float(&mut self.weapon.perk_weap_rate_multiplier, 0.0, 1.0),
            ),
            (
                "perk_bulletPenetrationMultiplier",
                Float(
                    &mut self.weapon.perk_bullet_penetration_multiplier,
                    0.0,
                    30.0,
                ),
            ),
            (
                "player_meleeRange",
                Float(&mut self.weapon.melee_range, 0.0, 1000.0),
            ),
            (
                "player_meleeWidth",
                Float(&mut self.weapon.melee_width, 0.0, 1000.0),
            ),
            (
                "player_meleeHeight",
                Float(&mut self.weapon.melee_height, 0.0, 1000.0),
            ),
            (
                "bullet_penetrationEnabled",
                Bool(&mut self.weapon.bullet_penetration_enabled),
            ),
            (
                "player_breath_hold_time",
                Float(&mut self.weapon.breath_hold_time, 0.0, 30.0),
            ),
            (
                "player_breath_gasp_time",
                Float(&mut self.weapon.breath_gasp_time, 0.0, 30.0),
            ),
            (
                "player_breath_fire_delay",
                Float(&mut self.weapon.breath_fire_delay, 0.0, 30.0),
            ),
            (
                "player_breath_gasp_scale",
                Float(&mut self.weapon.breath_gasp_scale, 0.0, 50.0),
            ),
            (
                "player_breath_hold_lerp",
                Float(&mut self.weapon.breath_hold_lerp, 0.0, 50.0),
            ),
            (
                "player_breath_gasp_lerp",
                Float(&mut self.weapon.breath_gasp_lerp, 0.0, 50.0),
            ),
            (
                "perk_extraBreath",
                Float(&mut self.weapon.perk_extra_breath, 0.0, f32::MAX),
            ),
        ]
    }

    /// The dvar names, their stock values and their stock domains, for the server to register.
    pub fn dvars() -> Vec<(&'static str, String, Domain)> {
        Params::default()
            .tunables()
            .into_iter()
            .map(|(name, t)| (name, t.text(), t.domain()))
            .collect()
    }

    /// Takes every tunable `get` has a value for (by dvar name, case-insensitive); the rest keep their value.
    pub fn set_from<'a>(&mut self, get: impl Fn(&str) -> Option<&'a str>) {
        for (name, mut t) in self.tunables() {
            if let Some(v) = get(name) {
                t.set(v);
            }
        }
    }

    /// The tunables that are not stock, as an info string (`\name\value...`): what the server sends its clients so
    /// their prediction moves as the server's does. Empty for a stock server.
    pub fn info_diff(&self) -> String {
        let (mut mine, mut stock) = (self.clone(), Params::default());
        let mut out = String::new();
        for ((name, a), (_, b)) in mine.tunables().into_iter().zip(stock.tunables()) {
            let (a, b) = (a.text(), b.text());
            if a != b {
                out.push_str(&format!("\\{name}\\{a}"));
            }
        }
        out
    }

    /// The stock tunables with the ones `info` (an [`Self::info_diff`] string) changes.
    pub fn from_info(info: &str) -> Params {
        let mut p = Params::default();
        let mut it = info.split('\\').skip(1);
        let mut pairs = Vec::new();
        while let (Some(k), Some(v)) = (it.next(), it.next()) {
            pairs.push((k, v));
        }
        p.set_from(|n| {
            pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, v)| *v)
        });
        p
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

impl WeaponMove {
    /// These values with the ones that govern how the player moves taken from `held`, the weapon in the player's
    /// hand: walking pace, sprint time, prone and freezing while firing follow `ps->weapon` even while an off-hand
    /// grenade is on show (`PM_CmdScale_Walk`, `BG_WeaponBlocksProne`, `PM_UpdateSprint`), the sights follow the weapon
    /// shown.
    pub fn moving_as(mut self, held: &WeaponMove) -> Self {
        self.move_speed_scale = held.move_speed_scale;
        self.ads_move_speed_scale = held.ads_move_speed_scale;
        self.sprint_duration_scale = held.sprint_duration_scale;
        self.blocks_prone = held.blocks_prone;
        self.freeze_movement_when_firing = held.freeze_movement_when_firing;
        self
    }
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
