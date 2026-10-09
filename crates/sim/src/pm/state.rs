// SPDX-License-Identifier: GPL-3.0-only
//! Player state, user commands and the flag/event constants player movement shares with the
//! rest of the game. Values are the original engine's (fact source: `iw3mp.exe` 1.7 `pmflags_t`,
//! `entity_event_t`, `usercmd_t` bit assignments).

use crate::Vec3;
use crate::cm::ENTITYNUM_NONE;

/// `playerState_t.pm_type`; the order matters, the original compares with `>=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum PmType {
    #[default]
    Normal = 0,
    NormalLinked,
    Noclip,
    Ufo,
    Spectator,
    Intermission,
    LastStand,
    Dead,
    DeadLinked,
}

impl PmType {
    /// The type a wire byte names; an unknown byte reads as [`PmType::Normal`].
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::NormalLinked,
            2 => Self::Noclip,
            3 => Self::Ufo,
            4 => Self::Spectator,
            5 => Self::Intermission,
            6 => Self::LastStand,
            7 => Self::Dead,
            8 => Self::DeadLinked,
            _ => Self::Normal,
        }
    }
}

/// `pm_flags` bits.
pub mod pmf {
    pub const PRONE: u32 = 1 << 0;
    pub const DUCKED: u32 = 1 << 1;
    pub const MANTLE: u32 = 1 << 2;
    pub const LADDER: u32 = 1 << 3;
    pub const SIGHT_AIMING: u32 = 1 << 4;
    pub const BACKWARDS_RUN: u32 = 1 << 5;
    pub const WALKING: u32 = 1 << 6;
    pub const TIME_HARDLANDING: u32 = 1 << 7;
    pub const TIME_KNOCKBACK: u32 = 1 << 8;
    pub const PRONEMOVE_OVERRIDDEN: u32 = 1 << 9;
    pub const RESPAWNED: u32 = 1 << 10;
    pub const FROZEN: u32 = 1 << 11;
    pub const NO_PRONE: u32 = 1 << 12;
    pub const LADDER_FALL: u32 = 1 << 13;
    pub const JUMPING: u32 = 1 << 14;
    pub const SPRINTING: u32 = 1 << 15;
    pub const SHELLSHOCKED: u32 = 1 << 16;
    pub const MELEE_CHARGE: u32 = 1 << 17;
    pub const NO_SPRINT: u32 = 1 << 18;
    pub const NO_JUMP: u32 = 1 << 19;
    pub const VEHICLE_ATTACHED: u32 = 1 << 20;
}

/// `eFlags` bits player movement reads or writes.
pub mod ef {
    pub const CROUCH: u32 = 0x4;
    pub const PRONE: u32 = 0x8;
    pub const FIRING: u32 = 0x40;
    pub const TURRET_PRONE: u32 = 0x100;
    pub const TURRET_CROUCH: u32 = 0x200;
    pub const TURRET_ACTIVE: u32 = 0x300;
    pub const MANTLE: u32 = 0x8000;
    pub const LOC_SELECTING: u32 = 0x20_0000;
}

/// `usercmd_t.buttons` bits.
pub mod button {
    pub const ATTACK: i32 = 1 << 0;
    pub const SPRINT: i32 = 1 << 1;
    pub const MELEE: i32 = 1 << 2;
    pub const USE: i32 = 1 << 3;
    pub const RELOAD: i32 = 1 << 4;
    pub const USE_RELOAD: i32 = 1 << 5;
    pub const LEAN_LEFT: i32 = 1 << 6;
    pub const LEAN_RIGHT: i32 = 1 << 7;
    pub const PRONE: i32 = 1 << 8;
    pub const CROUCH: i32 = 1 << 9;
    pub const JUMP: i32 = 1 << 10;
    pub const ADS: i32 = 1 << 11;
    pub const TEMP_STANCE: i32 = 1 << 12;
    pub const BREATH: i32 = 1 << 13;
    pub const FRAG: i32 = 1 << 14;
    pub const SMOKE: i32 = 1 << 15;
    pub const LOC_CONFIRM: i32 = 1 << 16;
    pub const LOC_CANCEL: i32 = 1 << 17;
    pub const NIGHTVISION: i32 = 1 << 18;
    pub const THROW: i32 = 1 << 19;
    pub const LOC_SELECTING: i32 = 1 << 20;
}

/// `ActionSlotType` values.
pub mod action_slot {
    pub const NONE: u8 = 0;
    /// `setActionSlot(n, "weapon", name)`: the key selects that weapon.
    pub const WEAPON: u8 = 1;
    /// `"altmode"`: toggles the alternate fire mode.
    pub const ALT_MODE: u8 = 2;
    pub const NIGHT_VISION: u8 = 3;
}

/// `entity_event_t` values player movement raises (`PM_AddEvent`).
pub mod ev {
    pub const NONE: u8 = 0x00;
    pub const FOLIAGE_SOUND: u8 = 0x01;
    /// Raised on a weapon change away from a reload; the parm is the old weapon state.
    pub const STOP_WEAPON_SOUND: u8 = 0x02;
    pub const STANCE_FORCE_STAND: u8 = 0x06;
    pub const STANCE_FORCE_CROUCH: u8 = 0x07;
    pub const STANCE_FORCE_PRONE: u8 = 0x08;
    pub const NOAMMO: u8 = 0x0B;
    pub const EMPTY_OFFHAND: u8 = 0x0D;
    pub const RESET_ADS: u8 = 0x0E;
    pub const RELOAD: u8 = 0x0F;
    pub const RELOAD_FROM_EMPTY: u8 = 0x10;
    pub const RELOAD_START: u8 = 0x11;
    pub const RELOAD_END: u8 = 0x12;
    pub const RELOAD_START_NOTIFY: u8 = 0x13;
    pub const RELOAD_ADDAMMO: u8 = 0x14;
    pub const RAISE_WEAPON: u8 = 0x15;
    pub const FIRST_RAISE_WEAPON: u8 = 0x16;
    pub const PUTAWAY_WEAPON: u8 = 0x17;
    pub const WEAPON_ALT: u8 = 0x18;
    pub const PULLBACK_WEAPON: u8 = 0x19;
    pub const FIRE_WEAPON: u8 = 0x1A;
    pub const FIRE_WEAPON_LASTSHOT: u8 = 0x1B;
    pub const RECHAMBER_WEAPON: u8 = 0x1C;
    pub const EJECT_BRASS: u8 = 0x1D;
    pub const MELEE_SWIPE: u8 = 0x1E;
    pub const FIRE_MELEE: u8 = 0x1F;
    pub const PREP_OFFHAND: u8 = 0x20;
    pub const USE_OFFHAND: u8 = 0x21;
    pub const SWITCH_OFFHAND: u8 = 0x22;
    pub const GRENADE_SUICIDE: u8 = 0x3E;
    pub const DETONATE: u8 = 0x3F;
    pub const NIGHTVISION_WEAR: u8 = 0x40;
    pub const NIGHTVISION_REMOVE: u8 = 0x41;
    pub const NO_FRAG_GRENADE_HINT: u8 = 0x43;
    pub const NO_SPECIAL_GRENADE_HINT: u8 = 0x44;
    pub const FOOTSTEP_SPRINT: u8 = 0x48;
    pub const FOOTSTEP_RUN: u8 = 0x49;
    pub const FOOTSTEP_WALK: u8 = 0x4A;
    pub const FOOTSTEP_PRONE: u8 = 0x4B;
    pub const JUMP: u8 = 0x4C;
    /// `LANDING_FIRST + surface type` (surface types 1..=28).
    pub const LANDING_FIRST: u8 = 0x4D;
    /// `LANDING_PAIN_FIRST + surface type`: a landing that hurt; the event parm is the damage.
    pub const LANDING_PAIN_FIRST: u8 = 0x6A;
}

/// `weaponstate_t` values movement looks at.
pub mod weapon_state {
    pub const READY: u8 = 0;
    pub const RAISING: u8 = 1;
    pub const RAISING_ALTSWITCH: u8 = 2;
    pub const DROPPING: u8 = 3;
    pub const DROPPING_QUICK: u8 = 4;
    pub const FIRING: u8 = 5;
    pub const RECHAMBERING: u8 = 6;
    pub const RELOADING: u8 = 7;
    pub const RELOADING_INTERUPT: u8 = 8;
    pub const RELOAD_START: u8 = 9;
    pub const RELOAD_START_INTERUPT: u8 = 10;
    pub const RELOAD_END: u8 = 11;
    pub const MELEE_INIT: u8 = 12;
    pub const MELEE_FIRE: u8 = 13;
    pub const MELEE_END: u8 = 14;
    pub const OFFHAND_INIT: u8 = 15;
    pub const OFFHAND_PREPARE: u8 = 16;
    pub const OFFHAND_HOLD: u8 = 17;
    pub const OFFHAND_START: u8 = 18;
    pub const OFFHAND: u8 = 19;
    pub const OFFHAND_END: u8 = 20;
    pub const DETONATING: u8 = 21;
    pub const SPRINT_RAISE: u8 = 22;
    pub const SPRINT_LOOP: u8 = 23;
    pub const SPRINT_DROP: u8 = 24;
    pub const NIGHTVISION_WEAR: u8 = 25;
    pub const NIGHTVISION_REMOVE: u8 = 26;
}

/// `playerState_t.weapFlags` bits.
pub mod wf {
    /// A reload was requested by the script or the server (`weapFlags & 1`).
    pub const RELOAD_REQUESTED: u32 = 0x1;
    /// The viewmodel is an offhand weapon (`PWF_USING_OFFHAND`).
    pub const USING_OFFHAND: u32 = 0x2;
    pub const HOLD_BREATH: u32 = 0x4;
    /// Aiming down sights is blocked.
    pub const NO_ADS: u32 = 0x20;
    pub const NIGHTVISION: u32 = 0x40;
    /// `disableweapon()`: no weapon may be raised or used.
    pub const DISABLED: u32 = 0x80;
    /// The trigger was pulled during a burst's cooldown; the next burst fires when it ends.
    pub const PENDING_TRIGGER: u32 = 0x100;
}

/// View heights (`viewHeightTarget` values); the original keys the stance off these integers.
pub const VIEW_STAND: i32 = 60;
pub const VIEW_CROUCH: i32 = 40;
pub const VIEW_PRONE: i32 = 11;
pub const VIEW_DEAD: i32 = 8;
/// `LastStand` players sit at this height.
pub const VIEW_LASTSTAND: i32 = 22;

/// Perk bit that stretches sprint time (`perk_sprintMultiplier`).
pub const PERK_SPRINT: u32 = 0x400;

/// `STAT_DEAD_YAW` is initialised to this sentinel.
pub const DEAD_YAW_UNSET: i32 = 999;

/// Per-cmd input (`usercmd_t`, the parts movement reads).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UserCmd {
    pub server_time: i32,
    pub buttons: i32,
    /// View angles as the original packs them: 16-bit fixed point, `360 / 65536` degrees each.
    pub angles: [i32; 3],
    pub weapon: u8,
    pub offhand_index: u8,
    pub forwardmove: i8,
    pub rightmove: i8,
    pub upmove: i8,
    pub pitchmove: i8,
    pub yawmove: i8,
    pub gun_pitch: f32,
    pub gun_yaw: f32,
    pub gun_offset: Vec3,
    pub melee_charge_yaw: f32,
    pub melee_charge_dist: u8,
    pub selected_location: [i8; 2],
}

/// `0.0054931640625`: degrees per usercmd angle unit.
pub const ANGLE_UNIT: f32 = 360.0 / 65536.0;

/// `MantleState`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MantleState {
    pub yaw: f32,
    pub timer: i32,
    pub trans_index: i32,
    /// 1 = over (not just up), 2 = stance forced crouch, 4 = stand up after, 8 = hint, 0x10 = finished.
    pub flags: u32,
}

/// `SprintState`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SprintState {
    pub sprint_button_up_required: bool,
    pub sprint_delay: bool,
    pub last_sprint_start: i32,
    pub last_sprint_end: i32,
    pub sprint_start_max_length: i32,
}

/// The slice of `playerState_t` that movement reads and writes, plus what snapshots need.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub command_time: i32,
    pub pm_type: PmType,
    pub bob_cycle: u8,
    pub pm_flags: u32,
    pub pm_time: i32,
    pub origin: Vec3,
    pub velocity: Vec3,
    /// Smoothed horizontal velocity the inertia clamp compares against.
    pub old_velocity: [f32; 2],
    pub foliage_sound_time: i32,
    pub gravity: i32,
    pub leanf: f32,
    /// `g_speed` (190 in stock multiplayer).
    pub speed: i32,
    pub delta_angles: Vec3,
    pub ground_entity_num: u16,
    /// Normal of the ladder surface being climbed.
    pub ladder_vec: Vec3,
    pub jump_time: i32,
    pub jump_origin_z: f32,
    pub damage_timer: i32,
    pub damage_count: i32,
    /// `damageDuration`: `damage_timer` as the last hit left it; the flinch and stumble windows are the first part of it.
    pub damage_duration: i32,
    /// `flinchYawAnim`: which way the last hit pushed the body, 0 forward, 1 back, 2 left, 3 right.
    pub flinch_yaw_anim: u8,
    pub movement_dir: i8,
    pub e_flags: u32,
    pub client_num: u16,
    /// Predictable event ring (`events[eventSequence & 3]`).
    pub event_sequence: u8,
    pub events: [u8; 4],
    pub event_parms: [u8; 4],
    pub weapon: u32,
    pub weapon_state: u8,
    pub weapon_time: i32,
    pub weapon_delay: i32,
    pub weapon_flags: u32,
    /// 0 = hip, 1 = fully aimed down sights.
    pub weapon_pos_frac: f32,
    pub ads_delay_time: i32,
    /// Model index (the model configstrings) of the hands the first-person weapon is drawn with, set by the
    /// script's `setviewmodel`; 0 is the weapon's own hand model.
    pub viewmodel_index: u16,
    pub viewangles: Vec3,
    pub view_height_target: i32,
    pub view_height_current: f32,
    pub view_height_lerp_time: i32,
    pub view_height_lerp_target: i32,
    pub view_height_lerp_down: bool,
    pub view_angle_clamp_base: [f32; 2],
    pub view_angle_clamp_range: [f32; 2],
    /// `stats[STAT_DEAD_YAW]`.
    pub dead_yaw: i32,
    /// `stats[STAT_HEALTH]` and `stats[STAT_MAX_HEALTH]`: the server's, for the HUD.
    pub health: i32,
    pub max_health: i32,
    /// `stats[STAT_SPAWN_COUNT]`: bumped by every spawn, so the client can tell a respawn from a mere state change.
    pub spawn_count: u16,
    pub prone_direction: f32,
    pub prone_direction_pitch: f32,
    pub prone_torso_pitch: f32,
    pub sprint_state: SprintState,
    pub torso_pitch: f32,
    pub waist_pitch: f32,
    pub move_speed_scale_multiplier: f32,
    pub mantle_state: MantleState,
    pub melee_charge_yaw: f32,
    pub melee_charge_dist: i32,
    pub melee_charge_time: i32,
    pub perks: u32,
    pub aim_spread_scale: f32,
    /// Rounds fired in the current trigger pull or burst (`weaponShotCount`, at most 4).
    pub weapon_shot_count: i32,
    /// The weapon whose off-hand slot is used by the grenade buttons (`offHandIndex`).
    pub offhand_index: u16,
    /// `offhandSecondary`: 0 selects smoke, 1 flash for the second grenade button.
    pub offhand_secondary: u8,
    /// Milliseconds of fuse left on a primed grenade (`grenadeTimeLeft`; -1 once it went off).
    pub grenade_time_left: i32,
    pub throw_back_grenade_owner: u16,
    pub throw_back_grenade_time_left: i32,
    /// Remaining time during which kick is reduced after the first shot (`weaponRestrictKickTime`).
    pub weapon_restrict_kick_time: i32,
    /// `spreadOverride` and its state: scripts pin the spread.
    pub spread_override: i32,
    pub spread_override_state: SpreadOverrideState,
    /// Entity the use-hint cursor points at; off-hand weapons cannot start while one shows.
    pub cursor_hint_ent_index: u16,
    /// What the crosshair hint shows (`cursorHint`: 0 none, 1 no icon, 2 activate, 3 health, 4 friendly).
    pub cursor_hint: u8,
    /// The use-trigger string table index of the hint's text, -1 for none (`cursorHintString`).
    pub cursor_hint_string: i8,
    /// What each of the four action slots does (`actionSlotType`: see [`action_slot`]) and its weapon.
    pub action_slot_type: [u8; 4],
    pub action_slot_param: [u16; 4],
    /// `beginLocationSelection`: the selector's material index (0 when not selecting) and its radius as 1/63 of the map.
    pub loc_selection: u16,
    pub loc_radius: u8,
}

/// `spreadOverrideState_t`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpreadOverrideState {
    #[default]
    Disabled,
    Enabled,
    Resetting,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            command_time: 0,
            pm_type: PmType::Normal,
            bob_cycle: 0,
            pm_flags: 0,
            pm_time: 0,
            origin: [0.0; 3],
            velocity: [0.0; 3],
            old_velocity: [0.0; 2],
            foliage_sound_time: 0,
            gravity: 800,
            leanf: 0.0,
            speed: 190,
            delta_angles: [0.0; 3],
            ground_entity_num: ENTITYNUM_NONE,
            ladder_vec: [0.0; 3],
            jump_time: 0,
            jump_origin_z: 0.0,
            damage_timer: 0,
            damage_count: 0,
            damage_duration: 0,
            flinch_yaw_anim: 0,
            movement_dir: 0,
            e_flags: 0,
            client_num: 0,
            event_sequence: 0,
            events: [0; 4],
            event_parms: [0; 4],
            weapon: 0,
            weapon_state: weapon_state::READY,
            weapon_time: 0,
            weapon_delay: 0,
            weapon_flags: 0,
            weapon_pos_frac: 0.0,
            ads_delay_time: 0,
            viewmodel_index: 0,
            viewangles: [0.0; 3],
            view_height_target: VIEW_STAND,
            view_height_current: VIEW_STAND as f32,
            view_height_lerp_time: 0,
            view_height_lerp_target: 0,
            view_height_lerp_down: false,
            view_angle_clamp_base: [0.0; 2],
            view_angle_clamp_range: [180.0; 2],
            dead_yaw: DEAD_YAW_UNSET,
            health: 0,
            max_health: 0,
            spawn_count: 0,
            prone_direction: 0.0,
            prone_direction_pitch: 0.0,
            prone_torso_pitch: 0.0,
            sprint_state: SprintState::default(),
            torso_pitch: 0.0,
            waist_pitch: 0.0,
            move_speed_scale_multiplier: 1.0,
            mantle_state: MantleState::default(),
            melee_charge_yaw: 0.0,
            melee_charge_dist: 0,
            melee_charge_time: 0,
            perks: 0,
            aim_spread_scale: 0.0,
            weapon_shot_count: 0,
            offhand_index: 0,
            offhand_secondary: 0,
            grenade_time_left: 0,
            throw_back_grenade_owner: ENTITYNUM_NONE,
            throw_back_grenade_time_left: 0,
            weapon_restrict_kick_time: 0,
            spread_override: 0,
            spread_override_state: SpreadOverrideState::Disabled,
            cursor_hint_ent_index: ENTITYNUM_NONE,
            cursor_hint: 0,
            cursor_hint_string: -1,
            action_slot_type: [0; 4],
            action_slot_param: [0; 4],
            loc_selection: 0,
            loc_radius: 0,
        }
    }
}

impl PlayerState {
    /// `BG_AddPredictableEventToPlayerstate`.
    pub fn add_event(&mut self, event: u8, parm: u32) {
        if event != ev::NONE {
            let slot = (self.event_sequence & 3) as usize;
            self.events[slot] = event;
            self.event_parms[slot] = parm as u8;
            self.event_sequence = self.event_sequence.wrapping_add(1);
        }
    }

    /// The effective stance from the view height target (`PM_GetEffectiveStance`).
    pub fn stance(&self) -> Stance {
        match self.view_height_target {
            VIEW_CROUCH | VIEW_LASTSTAND => Stance::Crouch,
            VIEW_PRONE => Stance::Prone,
            _ => Stance::Stand,
        }
    }
}

/// `PM_STANCE_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stance {
    Stand,
    Prone,
    Crouch,
}
