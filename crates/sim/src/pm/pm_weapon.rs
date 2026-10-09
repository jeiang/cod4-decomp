// SPDX-License-Identifier: GPL-3.0-only
//! `PM_Weapon`: the weapon state machine one user command steps through, and
//! `PM_AdjustAimSpreadScale`.
//!
//! Everything in here is the original's logic over `PlayerState` (`weapon`, `weapon_state`,
//! `weapon_time`, `weapon_delay`, `weapon_flags`, `aim_spread_scale`, ...) and the player's
//! [`PlayerWeapons`] (what is owned, magazines and stock). Weapon values come from the
//! [`WeaponTable`]. It only runs when the [`Pmove`] carries a [`WeaponCtx`](crate::weapon::WeaponCtx).
//! Outputs are the original's predictable events on the player state plus the reliable list of
//! [`WeaponEvent`](crate::weapon::WeaponEvent)s in `Pmove::weapon_out`. Animation selection is not
//! modelled; it does not change which state follows which. Holding the breath on a scoped weapon
//! (`PM_UpdateHoldBreath`, `PM_HoldBreathFire`) is: it steadies the scope's sway through
//! `hold_breath_scale`.
//!
//! Time is in milliseconds: `weapon_time` counts down the current action and `weapon_delay`
//! counts down to the moment within it the action takes effect (the shot leaves the barrel, the
//! grenade is released). A state machine step runs when the action times out (`weapon_time`
//! and `weapon_delay` both zero) or the delay just elapsed.
//!
//! Fact source: `iw3mp.exe` 1.7 `PM_Weapon` and its callees.

use super::ads;
use super::mantle::weapon_inactive;
use super::math;
use super::single::melee_charge_clear;
use super::state::{
    ANGLE_UNIT, PlayerState, PmType, SpreadOverrideState, UserCmd, button, ef, ev, pmf,
    weapon_state as ws, wf,
};
use super::{Pml, Pmove};
use crate::cm::ENTITYNUM_NONE;
use crate::weapon::{
    FireType, OffhandClass, PlayerWeapons, WeaponClass, WeaponEvent, WeaponInfo, WeaponOut,
    WeaponParams, WeaponTable, WeaponType, perk,
};

/// A segmented reload's first phase ignores a fire press until this fraction of it has played.
const RELOAD_START_INTERRUPT_IGNORE_FRAC: f32 = 0.4;
/// Time an off-hand throw with no weapon in hand waits for the arm (ms).
const EMPTY_HAND_OFFHAND_TIME: i32 = 100;
/// Delay added when firing with nothing left (ms).
const DRY_FIRE_PENALTY: i32 = 500;

/// Everything the state machine reads or changes besides the player state.
struct Cx<'a> {
    table: &'a WeaponTable,
    inv: &'a mut PlayerWeapons,
    params: WeaponParams,
    out: WeaponOut,
    cmd: UserCmd,
    old: UserCmd,
    msec: i32,
    mantle_inactive: bool,
}

impl<'a> Cx<'a> {
    fn info(&self, weapon: u32) -> &'a WeaponInfo {
        self.table.info(weapon as u16)
    }

    /// `ammoclip` of a weapon.
    fn clip(&self, weapon: u32) -> i32 {
        self.inv.clip(self.table, weapon as u16)
    }

    fn stock(&self, weapon: u32) -> i32 {
        self.inv.stock(self.table, weapon as u16)
    }

    /// `PM_WeaponAmmoAvailable`.
    fn clip_available(&self, ps: &PlayerState) -> i32 {
        self.clip(ps.weapon)
    }

    fn weapon_ammo(&self, weapon: u32) -> i32 {
        self.inv.weapon_ammo(self.table, weapon as u16)
    }

    fn event(&mut self, e: WeaponEvent) {
        self.out.push(e);
    }
}

fn reload_state(s: u8) -> bool {
    matches!(
        s,
        ws::RELOADING
            | ws::RELOAD_START
            | ws::RELOAD_END
            | ws::RELOAD_START_INTERUPT
            | ws::RELOADING_INTERUPT
    )
}

fn melee_state(s: u8) -> bool {
    matches!(s, ws::MELEE_INIT | ws::MELEE_FIRE | ws::MELEE_END)
}

fn offhand_state(s: u8) -> bool {
    (ws::OFFHAND_INIT..=ws::OFFHAND_END).contains(&s)
}

fn night_vision_state(s: u8) -> bool {
    matches!(s, ws::NIGHTVISION_WEAR | ws::NIGHTVISION_REMOVE)
}

fn sprint_state(s: u8) -> bool {
    (ws::SPRINT_RAISE..=ws::SPRINT_DROP).contains(&s)
}

fn raising_or_dropping(s: u8) -> bool {
    s <= ws::DROPPING_QUICK && s != ws::READY
}

/// `BG_GetViewmodelWeaponIndex`: the weapon the player is showing: the off-hand weapon while
/// throwing one.
pub(super) fn viewmodel_weapon(ps: &PlayerState) -> u16 {
    if ps.weapon_flags & wf::USING_OFFHAND == 0 {
        ps.weapon as u16
    } else {
        ps.offhand_index
    }
}

/// Fills `Pmove::weapon` from the table: what movement reads about the weapon on show, and the
/// rounds in the current weapon's magazine.
pub(super) fn sync_weapon_move(pm: &mut Pmove<'_>) {
    if let Some(ctx) = &pm.weapons {
        let mut m = ctx.table.info(viewmodel_weapon(&pm.ps)).weapon_move();
        m.ammo_in_clip = ctx.inv.clip(ctx.table, pm.ps.weapon as u16);
        pm.weapon = m;
    }
}

/// `PM_Weapon_Idle`.
pub(super) fn idle(ps: &mut PlayerState) {
    ps.weapon_flags &= !wf::USING_OFFHAND;
    ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
    ps.weapon_time = 0;
    ps.weapon_delay = 0;
    ps.weapon_state = ws::READY;
}

/// `PM_ResetWeaponState`.
pub(super) fn reset_weapon_state(ps: &mut PlayerState) {
    idle(ps);
}

/// `PM_SetProneMovementOverride`.
fn set_prone_override(ps: &mut PlayerState) {
    if ps.pm_flags & pmf::PRONE != 0 {
        ps.pm_flags |= pmf::PRONEMOVE_OVERRIDDEN;
    }
}

fn throwing_back(ps: &PlayerState) -> bool {
    ps.throw_back_grenade_owner != ENTITYNUM_NONE
}

/// `PM_AdjustAimSpreadScale`: how the aim spread grows with turning, moving and jumping and
/// decays at rest.
pub(super) fn adjust_aim_spread(pm: &mut Pmove<'_>, pml: &Pml) {
    let Some(ctx) = &pm.weapons else {
        return;
    };
    let w = ctx.table.info(pm.ps.weapon as u16);
    let move_threshold = ctx.params.aim_spread_move_speed_threshold;
    let (cmd, old) = (pm.cmd, pm.oldcmd);
    let ps = &mut pm.ps;

    let mut override_scale = 1.0f32;
    let mut decay = w.hip_spread_decay_rate;
    let (increase, decrease);
    if decay == 0.0 {
        increase = 0.0f32;
        decrease = 1.0f32;
    } else {
        let over = f64::from(ps.spread_override);
        override_scale = ((over - f64::from(w.hip_spread_stand_min))
            / f64::from(w.hip_spread_stand_max - w.hip_spread_stand_min))
            as f32;
        if ps.ground_entity_num != ENTITYNUM_NONE || ps.pm_type == PmType::NormalLinked {
            if ps.e_flags & ef::PRONE != 0 {
                decay *= w.hip_spread_prone_decay;
                override_scale = ((over - f64::from(w.hip_spread_prone_min))
                    / f64::from(w.hip_spread_prone_max - w.hip_spread_prone_min))
                    as f32;
            } else if ps.e_flags & ef::CROUCH != 0 {
                decay *= w.hip_spread_ducked_decay;
                override_scale = ((over - f64::from(w.hip_spread_ducked_min))
                    / f64::from(w.hip_spread_ducked_max - w.hip_spread_ducked_min))
                    as f32;
            }
        } else {
            decay = (f64::from(decay) * 0.5) as f32;
        }
        if ps.spread_override_state == SpreadOverrideState::Resetting {
            decrease = (f64::from(decay * pml.frametime) / f64::from(override_scale)) as f32;
            increase = 0.0;
        } else {
            decrease = decay * pml.frametime;
            if ps.weapon_pos_frac == 1.0 {
                increase = 0.0;
            } else {
                let mut view_change = 0.0f32;
                if w.hip_spread_turn_add != 0.0 {
                    for axis in 0..2 {
                        let a = f64::from(old.angles[axis]) * f64::from(ANGLE_UNIT);
                        let b = f64::from(cmd.angles[axis]) * f64::from(ANGLE_UNIT);
                        let delta = math::angle_delta(b as f32, a as f32);
                        view_change = delta.abs() * 0.01_f32 * w.hip_spread_turn_add
                            / pml.frametime
                            + view_change;
                    }
                }
                if w.hip_spread_move_add != 0.0 && (cmd.forwardmove != 0 || cmd.rightmove != 0) {
                    let speed_sq =
                        ps.velocity[1] * ps.velocity[1] + ps.velocity[0] * ps.velocity[0];
                    if move_threshold * move_threshold < speed_sq {
                        view_change = (f64::from(w.hip_spread_move_add)
                            * f64::from(speed_sq.sqrt())
                            / f64::from(ps.speed)
                            + f64::from(view_change)) as f32;
                    }
                }
                if ps.ground_entity_num == ENTITYNUM_NONE && ps.pm_type != PmType::NormalLinked {
                    for _ in 0..2 {
                        view_change = (f64::from(0.01_f32) * 128.0 + f64::from(view_change)) as f32;
                    }
                }
                increase = view_change * pml.frametime;
            }
        }
    }
    let scale = f64::from(ps.aim_spread_scale);
    ps.aim_spread_scale = if increase <= 0.0 {
        (scale - f64::from(decrease) * 255.0) as f32
    } else {
        (f64::from(increase) * 255.0 + scale) as f32
    };
    if ps.spread_override_state == SpreadOverrideState::Resetting
        && ps.aim_spread_scale * override_scale < 255.0
    {
        ps.spread_override_state = SpreadOverrideState::Disabled;
        ps.aim_spread_scale *= override_scale;
    }
    ps.aim_spread_scale = if ps.aim_spread_scale >= 0.0 {
        ps.aim_spread_scale.min(255.0)
    } else {
        0.0
    };
}

/// Whether the breath can be held on `w`: it has a scope overlay and is not an item.
fn breath_scope(w: &WeaponInfo) -> bool {
    w.overlay_reticle && w.weap_class != WeaponClass::Item
}

/// `PM_UpdateHoldBreath`: the breath is held while the scoped weapon is fully aimed and the button is down, for at
/// most `player_breath_hold_time`; then it is out for the gasp, during which the sway comes back stronger.
/// `hold_breath_timer` counts up while held and down after, and `hold_breath_scale` follows its target.
fn update_hold_breath(pm: &mut Pmove<'_>, pml: &Pml) {
    let Some(ctx) = &pm.weapons else {
        return;
    };
    let p = ctx.params;
    let scoped = breath_scope(ctx.table.info(viewmodel_weapon(&pm.ps)));
    let ps = &mut pm.ps;
    let mut hold_time = math::snap_to_int(p.breath_hold_time * 1000.0);
    let gasp_time = math::snap_to_int(p.breath_gasp_time * 1000.0);
    if ps.perks & perk::EXTRA_BREATH != 0 {
        hold_time += math::snap_to_int(p.perk_extra_breath * 1000.0);
    }
    if hold_time <= 0 {
        ps.weapon_flags &= !wf::HOLD_BREATH;
        ps.hold_breath_scale = 1.0;
        ps.hold_breath_timer = 0;
        return;
    }
    if ps.weapon_pos_frac == 1.0 && scoped && pm.cmd.buttons & button::BREATH != 0 {
        if ps.hold_breath_timer == 0 {
            ps.weapon_flags |= wf::HOLD_BREATH;
        }
    } else {
        ps.weapon_flags &= !wf::HOLD_BREATH;
    }
    let holding = |ps: &PlayerState| ps.weapon_flags & wf::HOLD_BREATH != 0;
    if holding(ps) {
        ps.hold_breath_timer += pml.msec;
    } else {
        ps.hold_breath_timer = (ps.hold_breath_timer - pml.msec).max(0);
    }
    if holding(ps) && ps.hold_breath_timer > hold_time {
        ps.hold_breath_timer = gasp_time + hold_time;
        ps.weapon_flags &= !wf::HOLD_BREATH;
    }
    let (target, lerp) = if holding(ps) {
        (0.0, p.breath_hold_lerp)
    } else {
        let gasp = ps.hold_breath_timer as f32 / (gasp_time + hold_time) as f32;
        ((p.breath_gasp_scale - 1.0) * gasp + 1.0, p.breath_gasp_lerp)
    };
    let target = (target - 1.0) * ps.weapon_pos_frac + 1.0;
    ps.hold_breath_scale = math::diff_track(target, ps.hold_breath_scale, lerp, pml.frametime);
}

/// `PM_HoldBreathFire`: a shot from a fully aimed scope costs breath time and lets the breath out.
fn hold_breath_fire(cx: &Cx<'_>, ps: &mut PlayerState, w: &WeaponInfo) {
    if ps.weapon_pos_frac != 1.0 || !breath_scope(w) {
        return;
    }
    let hold_time = math::snap_to_int(cx.params.breath_hold_time * 1000.0);
    if ps.hold_breath_timer < hold_time {
        ps.hold_breath_timer = (ps.hold_breath_timer
            + math::snap_to_int(cx.params.breath_fire_delay * 1000.0))
        .min(hold_time);
    }
    ps.weapon_flags &= !wf::HOLD_BREATH;
}

/// `PM_Weapon`: one step of the weapon state machine. Called where the original calls it, after
/// movement.
pub(super) fn pm_weapon(pm: &mut Pmove<'_>, pml: &Pml) {
    if pm.weapons.is_none() {
        return;
    }
    sync_weapon_move(pm);
    if pm.ps.pm_type >= PmType::Dead {
        pm.ps.weapon = 0;
        sync_weapon_move(pm);
        return;
    }
    if pm.ps.pm_flags & pmf::RESPAWNED != 0 || pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
        return;
    }
    ads::update_lerp(pm, pml);
    update_hold_breath(pm, pml);
    let mantle_inactive = weapon_inactive(pm);
    let Some(ctx) = pm.weapons.take() else {
        return;
    };
    let mut cx = Cx {
        table: ctx.table,
        inv: ctx.inv,
        params: ctx.params,
        out: std::mem::take(&mut pm.weapon_out),
        cmd: pm.cmd,
        old: pm.oldcmd,
        msec: pml.msec,
        mantle_inactive,
    };
    step(&mut cx, &mut pm.ps);
    pm.weapon_out = cx.out;
    pm.weapons = Some(crate::weapon::WeaponCtx {
        table: cx.table,
        inv: cx.inv,
        params: cx.params,
    });
    sync_weapon_move(pm);
}

fn step(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    if update_grenade_throw(cx, ps) {
        return;
    }
    update_pending_trigger_pull(cx, ps);
    let delayed = weapon_time_adjust(cx, ps);
    if !burst_fire_pending(cx, ps) {
        check_for_night_vision(cx, ps);
        check_for_sprint(cx, ps);
        check_for_offhand(cx, ps);
        check_for_change_weapon(cx, ps);
        check_for_reload(cx, ps);
        check_for_melee(cx, ps, delayed);
        check_for_detonation(cx, ps);
        check_for_grenade_throw_cancel(cx, ps);
    }
    if check_for_rechamber(cx, ps, delayed) {
        return;
    }
    if ps.pm_flags & pmf::PRONE != 0
        && (cx.cmd.forwardmove != 0 || cx.cmd.rightmove != 0)
        && ps.weapon_pos_frac != 1.0
        || melee_state(ps.weapon_state)
    {
        ps.aim_spread_scale = 255.0;
    }
    if !(delayed || ps.weapon_time == 0 && ps.weapon_delay == 0) {
        return;
    }
    match ps.weapon_state {
        ws::RAISING | ws::RAISING_ALTSWITCH => ps.weapon_state = ws::READY,
        ws::DROPPING | ws::DROPPING_QUICK => {
            finish_weapon_change(cx, ps, ps.weapon_state == ws::DROPPING_QUICK);
        }
        ws::RELOADING | ws::RELOADING_INTERUPT => finish_reload(cx, ps, delayed),
        ws::RELOAD_START | ws::RELOAD_START_INTERUPT => finish_reload_start(cx, ps, delayed),
        ws::RELOAD_END => finish_reload_end(cx, ps),
        ws::MELEE_INIT => melee_fire(cx, ps),
        ws::MELEE_FIRE => melee_end(cx, ps),
        ws::MELEE_END | ws::OFFHAND_END | ws::SPRINT_DROP => idle(ps),
        ws::OFFHAND_INIT => offhand_prepare(cx, ps),
        ws::OFFHAND_PREPARE => offhand_hold(cx, ps),
        ws::OFFHAND_HOLD => offhand_use(cx, ps),
        ws::OFFHAND_START => offhand_start(cx, ps),
        ws::OFFHAND => offhand_end(cx, ps),
        ws::DETONATING => detonate(cx, ps, delayed),
        ws::SPRINT_RAISE => sprint_loop(ps),
        ws::SPRINT_LOOP => {}
        ws::NIGHTVISION_WEAR | ws::NIGHTVISION_REMOVE => {
            if ps.weapon_time == 0 {
                ps.weapon_state = ws::READY;
            }
        }
        _ => {
            if ps.weapon != 0
                && should_be_firing(cx, ps, delayed)
                && !check_grenade_hold(cx, ps, delayed)
                && ps.pm_flags & pmf::FROZEN == 0
            {
                fire_weapon(cx, ps, delayed);
            }
        }
    }
}

// ---- timers and fire-rate bookkeeping -----------------------------------------------------------

/// `ShotLimitReached`.
fn shot_limit_reached(ps: &PlayerState, w: &WeaponInfo) -> bool {
    let n = i32::from(w.fire_type.burst_len());
    match w.fire_type {
        FireType::FullAuto => false,
        FireType::SingleShot => ps.weapon_shot_count != 0,
        _ => ps.weapon_shot_count >= n,
    }
}

/// `BurstFirePending`: a burst or single shot has started and has rounds left to fire.
fn burst_fire_pending(cx: &Cx<'_>, ps: &PlayerState) -> bool {
    if ps.weapon == 0 {
        return false;
    }
    let w = cx.info(ps.weapon);
    w.fire_type != FireType::FullAuto && ps.weapon_shot_count != 0 && !shot_limit_reached(ps, w)
}

/// `UpdatePendingTriggerPull`.
fn update_pending_trigger_pull(cx: &Cx<'_>, ps: &mut PlayerState) {
    if cx.info(ps.weapon).fire_type.is_burst()
        && cx.cmd.buttons & button::ATTACK != 0
        && cx.old.buttons & button::ATTACK == 0
    {
        ps.weapon_flags |= wf::PENDING_TRIGGER;
    }
}

/// `PM_Weapon_WeaponTimeAdjust`: counts the timers down and reports whether the delay just
/// elapsed (the delayed part of the action happens now).
fn weapon_time_adjust(cx: &mut Cx<'_>, ps: &mut PlayerState) -> bool {
    let w = cx.info(ps.weapon);
    if ps.weapon_restrict_kick_time > 0 {
        ps.weapon_restrict_kick_time = (ps.weapon_restrict_kick_time - cx.msec).max(0);
    }
    let state = ps.weapon_state;
    let scaled = |mult: f32, ps: &PlayerState| {
        if mult == 0.0 {
            ps.weapon_time.max(ps.weapon_delay)
        } else {
            math::snap_to_int(cx.msec as f32 / mult)
        }
    };
    let msec = if reload_state(state) && ps.perks & crate::weapon::perk::FAST_RELOAD != 0 {
        scaled(cx.params.perk_weap_reload_multiplier, ps)
    } else if matches!(state, ws::FIRING | ws::RECHAMBERING)
        && ps.perks & crate::weapon::perk::RATE_OF_FIRE != 0
    {
        scaled(cx.params.perk_weap_rate_multiplier, ps)
    } else {
        cx.msec
    };

    if ps.weapon_time != 0 {
        ps.weapon_time -= msec;
        if ps.weapon_time <= 0 {
            if state == ws::FIRING && w.fire_type.is_burst() && !burst_fire_pending(cx, ps) {
                ps.weapon_time = if cx.params.burst_fire_cooldown == 0.0 {
                    1
                } else {
                    math::snap_to_int(cx.params.burst_fire_cooldown * 1000.0)
                };
                ps.weapon_state = ws::READY;
                return false;
            }
            let limit_held =
                ps.weapon_flags & wf::PENDING_TRIGGER == 0 && shot_limit_reached(ps, w);
            let hold_throw = w.weap_type == WeaponType::Grenade && w.hold_button_to_throw;
            if !offhand_state(state)
                && (limit_held || hold_throw)
                && cx.cmd.buttons & button::ATTACK != 0
                && ps.weapon == u32::from(cx.cmd.weapon)
                && cx.clip_available(ps) != 0
            {
                // The trigger is still held after a single shot or a spent burst: wait for it
                // to be released.
                ps.weapon_time = 1;
                if reload_state(state) {
                    ps.weapon_time = 0;
                    ps.weapon_shot_count = 0;
                } else if matches!(state, ws::RECHAMBERING | ws::FIRING) || melee_state(state) {
                    ps.weapon_state = ws::READY;
                }
            } else {
                if (cx.cmd.buttons & button::ATTACK == 0
                    || ps.weapon_flags & wf::PENDING_TRIGGER != 0)
                    && !burst_fire_pending(cx, ps)
                {
                    ps.weapon_shot_count = 0;
                }
                ps.weapon_time = 0;
            }
        }
    }
    if ps.weapon_delay == 0 {
        return false;
    }
    ps.weapon_delay -= msec;
    if ps.weapon_delay > 0 {
        return false;
    }
    ps.weapon_delay = 0;
    true
}

// ---- weapon change ------------------------------------------------------------------------------

/// `PM_Weapon_CheckForChangeWeapon`.
fn check_for_change_weapon(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let s = ps.weapon_state;
    let idle_enough = ps.weapon_time == 0
        || reload_state(s)
        || s == ws::RECHAMBERING
        || (s != ws::FIRING && s != ws::RECHAMBERING && !melee_state(s) && ps.weapon_delay == 0);
    if melee_state(s) || offhand_state(s) || night_vision_state(s) || !idle_enough {
        return;
    }
    let want = u16::from(cx.cmd.weapon);
    if cx.mantle_inactive || ps.pm_flags & pmf::LADDER != 0 {
        if ps.weapon != 0 {
            begin_weapon_change(cx, ps, 0, true);
        }
    } else if ps.weapon_flags & wf::DISABLED != 0 {
        if ps.weapon != 0 {
            begin_weapon_change(cx, ps, 0, false);
        }
    } else if ps.weapon == u32::from(want)
        || ps.pm_flags & (pmf::RESPAWNED | pmf::FROZEN) != 0 && ps.weapon != 0
        || want != 0 && !cx.inv.has(want)
    {
        if ps.weapon == u32::from(want) && matches!(s, ws::DROPPING | ws::DROPPING_QUICK) {
            idle(ps);
        } else if ps.weapon != 0 && !cx.inv.has(ps.weapon as u16) {
            begin_weapon_change(cx, ps, 0, false);
        }
    } else {
        let quick = ps.mantle_state.flags & 0x10 != 0;
        begin_weapon_change(cx, ps, want, quick);
    }
}

/// `PM_BeginWeaponChange`: starts lowering the current weapon.
fn begin_weapon_change(cx: &mut Cx<'_>, ps: &mut PlayerState, new: u16, mut quick: bool) {
    if new != 0 && !cx.inv.has(new) {
        return;
    }
    if matches!(ps.weapon_state, ws::DROPPING | ws::DROPPING_QUICK) {
        return;
    }
    if new != 0 && cx.table.info(new).weap_class == WeaponClass::Pistol {
        quick = true;
    }
    if reload_state(ps.weapon_state) {
        ps.add_event(ev::STOP_WEAPON_SOUND, u32::from(ps.weapon_state));
    }
    ps.weapon_delay = 0;
    let old = ps.weapon as u16;
    cx.event(WeaponEvent::SwitchBegin { from: old, to: new });
    if old != 0 && cx.inv.has(old) && ps.grenade_time_left <= 0 {
        let w = cx.table.info(old);
        let alt = ps.pm_flags & pmf::SPRINTING == 0 && new != 0 && new == w.alt_weapon;
        let no_ammo = cx.clip_available(ps) == 0;
        ps.grenade_time_left = 0;
        if alt {
            ps.add_event(ev::WEAPON_ALT, 0);
        } else {
            ps.add_event(ev::PUTAWAY_WEAPON, 0);
        }
        ps.weapon_state = ws::DROPPING + u8::from(quick);
        set_prone_override(ps);
        ps.weapon_time = if alt {
            w.alt_drop_time
        } else if no_ammo {
            w.empty_drop_time
        } else if quick {
            w.quick_drop_time
        } else {
            w.drop_time
        };
    } else {
        ps.weapon_time = 0;
        ps.weapon_state = ws::DROPPING + u8::from(quick);
        ps.grenade_time_left = 0;
        set_prone_override(ps);
    }
}

/// `PM_Weapon_FinishWeaponChange`: the old weapon is down; the new one starts rising.
fn finish_weapon_change(cx: &mut Cx<'_>, ps: &mut PlayerState, quick: bool) {
    let want = u16::from(cx.cmd.weapon);
    let mut new = if cx.mantle_inactive || ps.pm_flags & pmf::LADDER != 0 {
        0
    } else if cx.inv.has(want)
        && ps.weapon_flags & wf::DISABLED == 0
        && usize::from(want) <= cx.table.len()
    {
        want
    } else {
        0
    };
    if !cx.inv.has(new) {
        new = 0;
    }
    let old = ps.weapon as u16;
    ps.weapon = u32::from(new);
    let w = cx.table.info(new);
    if old == new {
        ps.weapon_state = ws::READY;
        return;
    }
    let first_equip = !cx.inv.was_raised(new);
    cx.inv.mark_raised(new);
    let alt = ps.pm_flags & pmf::SPRINTING == 0
        && old != 0
        && new != 0
        && new == cx.table.info(old).alt_weapon;
    let (time, aim) = if alt {
        (w.alt_raise_time, ps.aim_spread_scale.max(128.0))
    } else {
        let time = if cx.clip_available(ps) == 0 {
            w.empty_raise_time
        } else if first_equip {
            w.first_raise_time
        } else if quick {
            w.quick_raise_time
        } else {
            w.raise_time
        };
        if old != 0 {
            ps.add_event(
                if first_equip {
                    ev::FIRST_RAISE_WEAPON
                } else {
                    ev::RAISE_WEAPON
                },
                0,
            );
        }
        (time, 255.0)
    };
    ps.weapon_state = ws::RAISING + u8::from(alt);
    ps.weapon_time = time;
    ps.aim_spread_scale = aim;
    set_prone_override(ps);
    let had_old = cx.inv.has(old);
    cx.inv.take_clip_only_if_empty(cx.table, ps, old);
    if had_old && !cx.inv.has(old) {
        cx.event(WeaponEvent::Dropped { weapon: old });
    }
    cx.event(WeaponEvent::SwitchComplete { from: old, to: new });
}

// ---- firing -------------------------------------------------------------------------------------

/// `PM_Weapon_ShouldBeFiring`.
fn should_be_firing(cx: &Cx<'_>, ps: &mut PlayerState, delayed: bool) -> bool {
    let w = cx.info(ps.weapon);
    let mut start = cx.cmd.buttons & w.fire_button() != 0;
    if w.freeze_movement_when_firing && ps.ground_entity_num == ENTITYNUM_NONE {
        start = false;
    }
    if start || delayed || burst_fire_pending(cx, ps) {
        return true;
    }
    ps.weapon_state = ws::READY;
    false
}

/// `PM_Weapon_IsHoldingGrenade`.
fn holding_grenade(cx: &Cx<'_>, ps: &PlayerState) -> bool {
    if ps.weapon == 0 {
        return false;
    }
    let w = cx.info(ps.weapon);
    w.weap_type == WeaponType::Grenade
        && !w.hold_button_to_throw
        && cx.cmd.buttons & w.fire_button() != 0
}

/// `PM_Weapon_CheckGrenadeHold`: a grenade primed with a held button waits for the release.
fn check_grenade_hold(cx: &Cx<'_>, ps: &mut PlayerState, delayed: bool) -> bool {
    if !delayed || !holding_grenade(cx, ps) {
        return false;
    }
    ps.weapon_delay = 1;
    true
}

/// `PM_WeaponUseAmmo`.
fn use_ammo(cx: &mut Cx<'_>, weapon: u32, amount: i32) {
    if !cx.params.sustain_ammo {
        let clip = cx.inv.clip_mut(cx.table, weapon as u16);
        *clip -= (*clip).min(amount);
    }
}

/// `PM_Weapon_CheckFiringAmmo`: whether a round is available; starts a reload when the magazine
/// is empty but stock remains.
fn check_firing_ammo(cx: &mut Cx<'_>, ps: &mut PlayerState) -> bool {
    let w = cx.info(ps.weapon);
    if 1 <= cx.clip_available(ps) {
        return true;
    }
    let stock = cx.stock(ps.weapon);
    let can_reload = 1 <= stock;
    if w.weap_type != WeaponType::Grenade && stock < 1 {
        ps.add_event(ev::NOAMMO, 0);
        cx.event(WeaponEvent::NoAmmo {
            weapon: ps.weapon as u16,
        });
    }
    if can_reload {
        begin_weapon_reload(cx, ps);
    } else {
        cx.inv.set_rechamber(ps.weapon as u16, false);
        if w.weap_type != WeaponType::Grenade {
            ps.weapon_time += DRY_FIRE_PENALTY;
        }
    }
    false
}

/// `PM_Weapon_StartFiring`.
fn start_firing(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) {
    let w = cx.info(ps.weapon);
    if w.weap_type != WeaponType::Grenade {
        ps.weapon_delay = w.fire_delay;
        ps.weapon_time = w.fire_time;
        if w.ads_fire_only {
            ps.weapon_delay = ((1.0 - f64::from(ps.weapon_pos_frac))
                * (1.0 / f64::from(w.oo_pos_anim_length[0]))) as i32;
        }
        if w.bolt_action {
            cx.inv.set_rechamber(ps.weapon as u16, true);
        }
        if ps.weapon_state != ws::FIRING {
            let bullets = if ps.weapon_pos_frac < 1.0 {
                w.hip_gun_kick_reduced_kick_bullets
            } else {
                w.ads_gun_kick_reduced_kick_bullets
            };
            ps.weapon_restrict_kick_time = w.fire_delay + bullets * w.fire_time;
        }
    } else if !delayed {
        if cx.clip_available(ps) != 0 {
            ps.grenade_time_left = w.fuse_time;
            ps.add_event(ev::PULLBACK_WEAPON, ps.weapon);
        }
        ps.weapon_delay = w.hold_fire_time;
        ps.weapon_time = 0;
    }
    ps.weapon_state = ws::FIRING;
    set_prone_override(ps);
    if w.fire_type != FireType::FullAuto {
        if ps.weapon_shot_count == 0 {
            ps.weapon_flags &= !wf::PENDING_TRIGGER;
        }
        ps.weapon_shot_count = (ps.weapon_shot_count + 1).min(4);
    }
}

/// `PM_Weapon_FireWeapon`.
fn fire_weapon(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) {
    let w = cx.info(ps.weapon);
    let was_firing = ps.weapon_state == ws::FIRING;
    if !check_firing_ammo(cx, ps) {
        return;
    }
    start_firing(cx, ps, delayed);
    if ps.weapon_delay != 0 {
        return;
    }
    if ps.e_flags & ef::TURRET_ACTIVE == 0 {
        use_ammo(cx, ps.weapon, 1);
    }
    if w.weap_type == WeaponType::Grenade {
        ps.weapon_time = w.fire_time;
    }
    let last_round = cx.clip_available(ps) == 0;
    ps.add_event(
        if last_round {
            ev::FIRE_WEAPON_LASTSHOT
        } else {
            ev::FIRE_WEAPON
        },
        0,
    );
    cx.event(WeaponEvent::Fire {
        weapon: ps.weapon as u16,
        shot: ps.weapon_shot_count as u8,
        first: !was_firing,
        ads: ps.weapon_pos_frac == 1.0,
        burst: w.fire_type.is_burst(),
        last_round,
    });
    hold_breath_fire(cx, ps, w);
    if ps.weapon_pos_frac != 1.0 {
        ps.aim_spread_scale =
            (f64::from(w.hip_spread_fire_add) * 255.0 + f64::from(ps.aim_spread_scale)) as f32;
        ps.aim_spread_scale = ps.aim_spread_scale.min(255.0);
    }
    if last_round && cx.stock(ps.weapon) == 0 && !w.has_detonator {
        ps.add_event(ev::NOAMMO, 0);
        cx.event(WeaponEvent::NoAmmo {
            weapon: ps.weapon as u16,
        });
    }
}

// ---- rechamber ----------------------------------------------------------------------------------

/// `PM_Weapon_CheckForRechamber`: works the bolt after a bolt-action shot. True when the rest
/// of the step must be skipped.
fn check_for_rechamber(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) -> bool {
    let w = cx.info(ps.weapon);
    if offhand_state(ps.weapon_state) || !w.bolt_action {
        return false;
    }
    let weapon = ps.weapon as u16;
    if !cx.inv.needs_rechamber(weapon) {
        return false;
    }
    if ps.weapon_state == ws::RECHAMBERING && delayed {
        cx.inv.set_rechamber(weapon, false);
        ps.add_event(ev::EJECT_BRASS, 0);
        if ps.weapon_time != 0 {
            return true;
        }
    }
    let s = ps.weapon_state;
    if ps.weapon_time == 0
        || s != ws::FIRING && s != ws::RECHAMBERING && !melee_state(s) && ps.weapon_delay == 0
    {
        if s == ws::RECHAMBERING {
            ps.weapon_state = ws::READY;
        } else if s == ws::READY {
            ps.weapon_state = ws::RECHAMBERING;
            ps.weapon_time = w.rechamber_time;
            ps.weapon_delay =
                if w.rechamber_bolt_time != 0 && w.rechamber_bolt_time < w.rechamber_time {
                    w.rechamber_bolt_time
                } else {
                    1
                };
            ps.add_event(ev::RECHAMBER_WEAPON, 0);
        }
    }
    false
}

// ---- reload -------------------------------------------------------------------------------------

/// `PM_Weapon_AllowReload`.
fn allow_reload(cx: &Cx<'_>, ps: &PlayerState) -> bool {
    let w = cx.info(ps.weapon);
    let clip = cx.clip(ps.weapon);
    if cx.stock(ps.weapon) == 0 || clip >= w.clip_size {
        return false;
    }
    if !w.no_partial_reload {
        return true;
    }
    if w.reload_ammo_add != 0 && w.reload_ammo_add < w.clip_size {
        w.clip_size - clip >= w.reload_ammo_add
    } else {
        clip == 0
    }
}

/// `PM_Weapon_CheckForReload`.
fn check_for_reload(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let s = ps.weapon_state;
    if offhand_state(s) || melee_state(s) {
        return;
    }
    let w = cx.info(ps.weapon);
    let mut requested = cx.cmd.buttons & button::RELOAD != 0;
    if ps.weapon_flags & wf::RELOAD_REQUESTED != 0 {
        ps.weapon_flags &= !wf::RELOAD_REQUESTED;
        requested = true;
    }
    if w.segmented_reload
        && matches!(s, ws::RELOAD_START | ws::RELOADING)
        && cx.cmd.buttons & button::ATTACK != 0
        && cx.old.buttons & button::ATTACK == 0
    {
        if s == ws::RELOAD_START && w.reload_start_time != 0 {
            let frac = (f64::from(w.reload_start_time - ps.weapon_time)
                / f64::from(w.reload_start_time)) as f32;
            if f64::from(RELOAD_START_INTERRUPT_IGNORE_FRAC) < f64::from(frac) {
                ps.weapon_state = ws::RELOAD_START_INTERUPT;
            }
        } else if s == ws::RELOADING {
            ps.weapon_state = ws::RELOADING_INTERUPT;
        }
    }
    let s = ps.weapon_state;
    if raising_or_dropping(s) || reload_state(s) {
        return;
    }
    let mut reload = requested && allow_reload(cx, ps);
    if cx.clip_available(ps) == 0 && cx.stock(ps.weapon) != 0 && s != ws::FIRING && !sprint_state(s)
    {
        reload = true;
    }
    if reload {
        begin_weapon_reload(cx, ps);
    }
}

/// `PM_BeginWeaponReload`.
fn begin_weapon_reload(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    let s = ps.weapon_state;
    if !(matches!(s, ws::READY | ws::FIRING | ws::RECHAMBERING) || sprint_state(s))
        || ps.weapon == 0
        || ps.weapon as usize > cx.table.len()
    {
        return;
    }
    ps.weapon_shot_count = 0;
    ps.add_event(ev::RESET_ADS, 0);
    ps.add_event(ev::RELOAD_START_NOTIFY, 0);
    if w.segmented_reload && w.reload_start_time != 0 {
        ps.weapon_time = w.reload_start_time;
        ps.weapon_state = ws::RELOAD_START;
        ps.add_event(ev::RELOAD_START, 0);
        set_reload_add_ammo_delay(cx, ps);
    } else {
        set_reloading_state(cx, ps);
    }
}

/// `PM_SetReloadingState`.
fn set_reloading_state(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    if cx.clip_available(ps) != 0 || w.weap_type != WeaponType::Bullet {
        ps.weapon_time = w.reload_time;
        ps.add_event(ev::RELOAD, 0);
    } else {
        ps.weapon_time = w.reload_empty_time;
        ps.add_event(ev::RELOAD_FROM_EMPTY, 0);
    }
    ps.weapon_state = if ps.weapon_state == ws::RELOAD_START_INTERUPT {
        ws::RELOADING_INTERUPT
    } else {
        ws::RELOADING
    };
    set_reload_add_ammo_delay(cx, ps);
}

/// The time a reload takes before ammunition is added (`iReloadAddTime` caps the whole reload).
fn reload_time_to_add(cx: &Cx<'_>, ps: &PlayerState) -> i32 {
    let w = cx.info(ps.weapon);
    let mut t = if cx.clip_available(ps) != 0 || w.weap_type != WeaponType::Bullet {
        w.reload_time
    } else {
        w.reload_empty_time
    };
    if w.reload_add_time != 0 && w.reload_add_time < t {
        t = w.reload_add_time;
    }
    t
}

/// The same for the first phase of a segmented reload.
fn reload_start_time_to_add(w: &WeaponInfo) -> i32 {
    if w.reload_start_add_time == 0 {
        0
    } else if w.reload_start_add_time >= w.reload_start_time {
        w.reload_start_time
    } else {
        w.reload_start_add_time
    }
}

/// `PM_SetWeaponReloadAddAmmoDelay`.
fn set_reload_add_ammo_delay(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    let mut t = if matches!(
        ps.weapon_state,
        ws::RELOAD_START | ws::RELOAD_START_INTERUPT
    ) {
        reload_start_time_to_add(w)
    } else {
        reload_time_to_add(cx, ps)
    };
    if w.bolt_action && cx.inv.needs_rechamber(ps.weapon as u16) {
        if t == 0 {
            t = ps.weapon_time;
        }
        if w.rechamber_bolt_time < t {
            t = w.rechamber_bolt_time;
        }
        if t == 0 {
            t = 1;
        }
        ps.weapon_delay = t;
    } else if t != 0 {
        ps.weapon_delay = t;
    }
}

/// `PM_ReloadClip`: moves stock rounds into the magazine.
fn reload_clip(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    let start = matches!(
        ps.weapon_state,
        ws::RELOAD_START | ws::RELOAD_START_INTERUPT
    );
    if start && w.reload_start_add == 0 {
        return;
    }
    let weapon = ps.weapon as u16;
    let mut add = (w.clip_size - cx.clip(ps.weapon)).min(cx.stock(ps.weapon));
    if start {
        if w.reload_start_add < w.clip_size && add > w.reload_start_add {
            add = w.reload_start_add;
        }
    } else if w.reload_ammo_add != 0 && w.reload_ammo_add < w.clip_size && add > w.reload_ammo_add {
        add = w.reload_ammo_add;
    }
    if add != 0 {
        *cx.inv.stock_mut(cx.table, weapon) -= add;
        *cx.inv.clip_mut(cx.table, weapon) += add;
        ps.add_event(ev::RELOAD_ADDAMMO, 0);
        cx.event(WeaponEvent::ReloadAmmoAdded {
            weapon,
            amount: add,
        });
    }
}

/// `PM_Weapon_ReloadDelayedAction`: the point in a reload where ammunition is added; a bolt
/// action first ejects the case and works the bolt.
fn reload_delayed_action(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    let weapon = ps.weapon as u16;
    if !w.bolt_action || !cx.inv.needs_rechamber(weapon) {
        reload_clip(cx, ps);
        return;
    }
    cx.inv.set_rechamber(weapon, false);
    ps.add_event(ev::EJECT_BRASS, 0);
    let start = matches!(
        ps.weapon_state,
        ws::RELOAD_START | ws::RELOAD_START_INTERUPT
    );
    if start && w.reload_start_add_time == 0 {
        return;
    }
    if ps.weapon_time != 0 {
        let reload = if start {
            reload_start_time_to_add(w)
        } else {
            reload_time_to_add(cx, ps)
        };
        let rechamber = if w.rechamber_bolt_time >= reload {
            1
        } else {
            w.rechamber_bolt_time
        };
        if reload - rechamber >= 1 {
            ps.weapon_delay = reload - rechamber;
            return;
        }
    }
    reload_clip(cx, ps);
}

/// `PM_Weapon_FinishReload`.
fn finish_reload(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) {
    let w = cx.info(ps.weapon);
    if delayed {
        reload_delayed_action(cx, ps);
    }
    if ps.weapon_time != 0 {
        return;
    }
    if w.segmented_reload && cx.cmd.buttons & button::ATTACK != 0 {
        ps.weapon_state = ws::RELOADING_INTERUPT;
    }
    cx.inv.set_rechamber(ps.weapon as u16, false);
    if w.segmented_reload {
        if ps.weapon_state != ws::RELOADING_INTERUPT && allow_reload(cx, ps) {
            set_reloading_state(cx, ps);
            return;
        }
        if w.reload_end_time != 0 {
            ps.weapon_state = ws::RELOAD_END;
            ps.weapon_time = w.reload_end_time;
            ps.add_event(ev::RELOAD_END, 0);
            return;
        }
    }
    ps.weapon_state = ws::READY;
    cx.event(WeaponEvent::ReloadComplete {
        weapon: ps.weapon as u16,
    });
}

/// `PM_Weapon_FinishReloadStart`: the first phase of a segmented reload is over.
fn finish_reload_start(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) {
    let w = cx.info(ps.weapon);
    if delayed {
        reload_delayed_action(cx, ps);
    }
    if ps.weapon_time != 0 {
        return;
    }
    if w.segmented_reload && cx.cmd.buttons & button::ATTACK != 0 {
        ps.weapon_state = ws::RELOAD_START_INTERUPT;
    }
    if ps.weapon_state == ws::RELOAD_START_INTERUPT && cx.clip_available(ps) != 0
        || !allow_reload(cx, ps)
    {
        cx.inv.set_rechamber(ps.weapon as u16, false);
        if w.reload_end_time != 0 {
            ps.weapon_state = ws::RELOAD_END;
            ps.weapon_time = w.reload_end_time;
            ps.add_event(ev::RELOAD_END, 0);
        } else {
            ps.weapon_state = ws::READY;
            cx.event(WeaponEvent::ReloadComplete {
                weapon: ps.weapon as u16,
            });
        }
    } else {
        set_reloading_state(cx, ps);
    }
}

/// `PM_Weapon_FinishReloadEnd`.
fn finish_reload_end(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    ps.weapon_state = ws::READY;
    cx.event(WeaponEvent::ReloadComplete {
        weapon: ps.weapon as u16,
    });
}

// ---- melee --------------------------------------------------------------------------------------

/// `PM_Weapon_CheckForMelee`.
fn check_for_melee(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) {
    let w = cx.info(ps.weapon);
    let s = ps.weapon_state;
    if melee_state(s)
        || offhand_state(s)
        || night_vision_state(s)
        || w.melee_damage == 0
        || delayed
        || !(ps.weapon_delay == 0 || reload_state(s))
        || cx.cmd.buttons & button::MELEE == 0
        || cx.old.buttons & button::MELEE != 0
        || !(ps.weapon_pos_frac <= 0.0 || !w.overlay_reticle)
    {
        return;
    }
    if s == ws::READY || s > ws::DROPPING_QUICK {
        melee_charge_start(cx, ps);
        melee_init(cx, ps);
    }
}

/// `PM_MeleeChargeStart`.
fn melee_charge_start(cx: &Cx<'_>, ps: &mut PlayerState) {
    if cx.cmd.melee_charge_dist != 0 {
        ps.pm_flags |= pmf::MELEE_CHARGE;
        ps.melee_charge_yaw = cx.cmd.melee_charge_yaw;
        ps.melee_charge_dist = i32::from(cx.cmd.melee_charge_dist);
        ps.melee_charge_time = 0;
    } else {
        melee_charge_clear(ps);
    }
}

/// `PM_Weapon_MeleeInit`.
fn melee_init(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    let charge =
        ps.pm_flags & pmf::MELEE_CHARGE != 0 && w.has_melee_charge_anim && w.melee_charge_time > 0;
    if charge {
        ps.weapon_time = w.melee_charge_time;
        ps.weapon_delay = w.melee_charge_delay;
    } else {
        ps.weapon_time = w.melee_time;
        ps.weapon_delay = w.melee_delay;
    }
    ps.weapon_state = ws::MELEE_INIT;
    ps.add_event(ev::MELEE_SWIPE, 0);
    set_prone_override(ps);
}

/// `PM_Weapon_MeleeFire`: the swing connects.
fn melee_fire(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    ps.weapon_state = ws::MELEE_FIRE;
    ps.add_event(ev::FIRE_MELEE, 0);
    cx.event(WeaponEvent::Melee {
        weapon: ps.weapon as u16,
    });
    set_prone_override(ps);
}

/// `PM_Weapon_MeleeEnd`.
fn melee_end(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(ps.weapon);
    if w.has_knife_model {
        ps.weapon_state = ws::MELEE_END;
        ps.weapon_time = w.quick_raise_time;
        ps.weapon_delay = 0;
        set_prone_override(ps);
    } else {
        idle(ps);
    }
}

// ---- off-hand grenades --------------------------------------------------------------------------

/// `PM_Weapon_CheckForOffHand`: a grenade button starts throwing the equipped grenade.
fn check_for_offhand(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let s = ps.weapon_state;
    if ps.e_flags & ef::TURRET_ACTIVE != 0
        || ps.weapon_flags & wf::DISABLED != 0
        || ps.pm_flags & pmf::SPRINTING != 0
        || holding_grenade(cx, ps)
        || (ws::OFFHAND_INIT..ws::OFFHAND_END).contains(&s)
        || night_vision_state(s)
    {
        return;
    }
    let requested = u16::from(cx.cmd.offhand_index);
    if cx.inv.has(requested) {
        ps.offhand_index = requested;
    }
    let (class, found) = if cx.cmd.buttons & button::FRAG != 0 {
        (
            OffhandClass::Frag,
            cx.inv
                .first_available_offhand(cx.table, ps, OffhandClass::Frag),
        )
    } else if cx.cmd.buttons & button::SMOKE != 0 {
        let class = if ps.offhand_secondary != 0 {
            OffhandClass::Flash
        } else {
            OffhandClass::Smoke
        };
        (class, cx.inv.first_available_offhand(cx.table, ps, class))
    } else {
        return;
    };
    if found == 0 {
        send_empty_offhand_event(cx, ps, class);
        return;
    }
    ps.add_event(ev::SWITCH_OFFHAND, u32::from(found));
    ps.offhand_index = found;
    if cx.table.info(found).weap_type != WeaponType::Grenade {
        return;
    }
    if ps.cursor_hint_ent_index == ENTITYNUM_NONE && (ps.weapon == 0 || s == ws::OFFHAND_END) {
        offhand_prepare(cx, ps);
    } else {
        offhand_init(cx, ps);
    }
}

/// `PM_SendEmtpyOffhandEvent`.
fn send_empty_offhand_event(cx: &mut Cx<'_>, ps: &mut PlayerState, class: OffhandClass) {
    ps.add_event(ev::EMPTY_OFFHAND, 0);
    cx.event(WeaponEvent::EmptyOffhand);
    if cx.inv.first_equipped_offhand(cx.table, class) != 0 {
        ps.add_event(
            if class == OffhandClass::Frag {
                ev::NO_FRAG_GRENADE_HINT
            } else {
                ev::NO_SPECIAL_GRENADE_HINT
            },
            0,
        );
    }
}

/// `PM_Weapon_OffHandInit`: lowers the weapon in hand to make room for the grenade.
fn offhand_init(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    ps.weapon_state = ws::OFFHAND_INIT;
    ps.weapon_delay = 0;
    ps.weapon_flags &= !wf::USING_OFFHAND;
    ps.throw_back_grenade_owner = ENTITYNUM_NONE;
    ads::exit_ads(ps);
    ps.weapon_time = if ps.weapon != 0 {
        cx.info(ps.weapon).quick_drop_time
    } else {
        EMPTY_HAND_OFFHAND_TIME
    };
}

/// `PM_Weapon_OffHandPrepare`.
fn offhand_prepare(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(u32::from(ps.offhand_index));
    ps.weapon_state = ws::OFFHAND_PREPARE;
    ps.weapon_time = w.hold_fire_time;
    ps.weapon_delay = 0;
    ps.weapon_flags |= wf::USING_OFFHAND;
    if !throwing_back(ps) {
        ps.add_event(ev::PREP_OFFHAND, u32::from(ps.offhand_index));
    }
    set_prone_override(ps);
}

/// `PM_Weapon_OffHandHold`: the grenade is ready; the fuse starts.
fn offhand_hold(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    ps.weapon_state = ws::OFFHAND_START;
    ps.weapon_time = 0;
    ps.weapon_delay = 0;
    ps.weapon_flags |= wf::USING_OFFHAND;
    if !throwing_back(ps) {
        ps.grenade_time_left = cx.info(u32::from(ps.offhand_index)).fuse_time;
    }
}

/// `PM_Weapon_OffHandStart`: holds the primed grenade while the button stays down.
fn offhand_start(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let w = cx.info(u32::from(ps.offhand_index));
    let both = button::FRAG | button::SMOKE;
    if !w.hold_button_to_throw && cx.old.buttons & both != 0 && cx.cmd.buttons & both != 0 {
        ps.weapon_delay = 1;
    } else {
        ps.weapon_state = ws::OFFHAND_HOLD;
        ps.weapon_time = w.fire_time;
        ps.weapon_delay = w.fire_delay;
        ps.weapon_flags |= wf::USING_OFFHAND;
    }
}

/// `PM_Weapon_OffHand`: the grenade leaves the hand.
fn offhand_use(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let weapon = u32::from(ps.offhand_index);
    ps.add_event(ev::USE_OFFHAND, weapon);
    let fuse_left = ps.grenade_time_left;
    cx.event(WeaponEvent::OffhandThrow {
        weapon: weapon as u16,
        fuse_left,
        cooked: (cx.info(weapon).fuse_time - fuse_left).max(0),
    });
    if !throwing_back(ps) {
        if cx.weapon_ammo(weapon) != 0 {
            use_ammo(cx, weapon, 1);
        } else {
            ps.add_event(ev::EMPTY_OFFHAND, 0);
            cx.event(WeaponEvent::EmptyOffhand);
        }
    }
    ps.weapon_state = ws::OFFHAND;
    ps.weapon_flags |= wf::USING_OFFHAND;
}

/// `PM_Weapon_OffHandEnd`: the weapon in hand comes back up.
fn offhand_end(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    if ps.weapon != 0 {
        ps.weapon_time = cx.info(ps.weapon).quick_raise_time;
        ps.weapon_delay = 0;
    } else {
        ps.weapon_time = 0;
        ps.weapon_delay = 1;
    }
    ps.throw_back_grenade_time_left = 0;
    ps.throw_back_grenade_owner = ENTITYNUM_NONE;
    ps.weapon_state = ws::OFFHAND_END;
    ps.weapon_flags &= !wf::USING_OFFHAND;
    ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
}

/// `PM_UpdateGrenadeThrow`: cooks the fuse of a grenade in hand; when it runs out the grenade
/// goes off in the player's hand and the step ends.
fn update_grenade_throw(cx: &mut Cx<'_>, ps: &mut PlayerState) -> bool {
    let weapon = if ps.weapon_flags & wf::USING_OFFHAND != 0 {
        u32::from(ps.offhand_index)
    } else if ps.weapon != 0 {
        ps.weapon
    } else {
        return false;
    };
    let w = cx.info(weapon);
    if w.weap_type != WeaponType::Grenade || ps.grenade_time_left <= 0 {
        return false;
    }
    if w.cook_off_hold {
        ps.grenade_time_left -= cx.msec;
    }
    if ps.grenade_time_left > 0 {
        return false;
    }
    ps.grenade_time_left = -1;
    ps.add_event(ev::GRENADE_SUICIDE, u32::from(ps.offhand_index));
    cx.event(WeaponEvent::GrenadeSuicide {
        weapon: weapon as u16,
    });
    if !throwing_back(ps) {
        use_ammo(cx, weapon, 1);
    }
    true
}

/// `PM_Weapon_CheckForGrenadeThrowCancel`: releasing the button before the throw cancels a
/// hold-to-throw grenade.
fn check_for_grenade_throw_cancel(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    if ps.weapon_state == ws::OFFHAND_PREPARE {
        let w = cx.info(u32::from(ps.offhand_index));
        if w.hold_button_to_throw && cx.cmd.buttons & (button::FRAG | button::SMOKE) == 0 {
            offhand_end(cx, ps);
        }
    } else if ps.weapon != 0 {
        let w = cx.info(ps.weapon);
        if w.weap_type == WeaponType::Grenade
            && ps.weapon_state == ws::FIRING
            && w.hold_button_to_throw
            && cx.cmd.buttons & button::ATTACK == 0
        {
            idle(ps);
        }
    }
}

// ---- detonation, night vision, sprint ------------------------------------------------------------

/// `PM_Weapon_CheckForDetonation`.
fn check_for_detonation(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    if ps.weapon == 0 {
        return;
    }
    let w = cx.info(ps.weapon);
    let s = ps.weapon_state;
    if w.weap_type == WeaponType::Grenade
        && w.has_detonator
        && s != ws::DETONATING
        && !reload_state(s)
        && !matches!(s, ws::FIRING | ws::RECHAMBERING)
        && !melee_state(s)
        && !matches!(
            s,
            ws::RAISING | ws::RAISING_ALTSWITCH | ws::DROPPING | ws::DROPPING_QUICK
        )
        && !offhand_state(s)
        && !night_vision_state(s)
        && cx.cmd.buttons & button::ATTACK != 0
    {
        ps.weapon_state = ws::DETONATING;
        ps.weapon_time = w.detonate_time;
        ps.weapon_delay = w.detonate_delay;
    }
}

/// `PM_Detonate`.
fn detonate(cx: &mut Cx<'_>, ps: &mut PlayerState, delayed: bool) {
    if delayed && ps.weapon != 0 {
        ps.add_event(ev::DETONATE, 0);
        cx.event(WeaponEvent::Detonate {
            weapon: ps.weapon as u16,
        });
    } else {
        idle(ps);
    }
}

/// `PM_Weapon_CheckForNightVision`: toggles on the button press (multiplayer has no
/// animation state for it).
fn check_for_night_vision(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    if cx.old.buttons & button::NIGHTVISION == 0 && cx.cmd.buttons & button::NIGHTVISION != 0 {
        let on = ps.weapon_flags & wf::NIGHTVISION == 0;
        ps.weapon_flags ^= wf::NIGHTVISION;
        ps.add_event(
            if on {
                ev::NIGHTVISION_WEAR
            } else {
                ev::NIGHTVISION_REMOVE
            },
            0,
        );
        cx.event(WeaponEvent::NightVision { on });
    }
}

/// `PM_Weapon_CheckForSprint`: lowers the weapon while sprinting and raises it again after.
fn check_for_sprint(cx: &mut Cx<'_>, ps: &mut PlayerState) {
    let s = ps.weapon_state;
    if cx.cmd.weapon == 0
        || matches!(s, ws::FIRING | ws::RECHAMBERING)
        || melee_state(s)
        || raising_or_dropping(s)
        || offhand_state(s)
        || night_vision_state(s)
    {
        return;
    }
    let sprinting = ps.pm_flags & pmf::SPRINTING != 0;
    let w = cx.info(ps.weapon);
    if sprinting && !sprint_state(s) {
        ps.weapon_state = ws::SPRINT_RAISE;
        ps.weapon_time = w.sprint_in_time;
        ps.weapon_delay = 0;
    } else if !sprinting && matches!(s, ws::SPRINT_RAISE | ws::SPRINT_LOOP) {
        ps.weapon_state = ws::SPRINT_DROP;
        ps.weapon_time = w.sprint_out_time;
        ps.weapon_delay = 0;
    }
}

/// `Sprint_State_Loop`.
fn sprint_loop(ps: &mut PlayerState) {
    ps.weapon_state = ws::SPRINT_LOOP;
    ps.weapon_time = 0;
    ps.weapon_delay = 0;
}
