// SPDX-License-Identifier: GPL-3.0-or-later
//! Sprint stamina and start/stop rules (`PM_UpdateSprint` and friends).
//!
//! Sprint time is a budget (`player_sprintTime` seconds, scaled by weapon and perk). Starting
//! needs `player_sprintMinTime` seconds left; the budget refills while not sprinting.

use super::ads::exit_ads;
use super::state::{PERK_SPRINT, PlayerState, PmType, button, pmf, weapon_state as ws};
use super::{PLAYER_MAXS, PLAYER_MINS, Params, Pmove};
use crate::cm::Collide;
use crate::contents::MASK_IGNORE_CHARACTERS;

/// `BG_GetMaxSprintTime`: the sprint budget in ms, capped at 0x3FFF.
pub fn max_sprint_ms(params: &Params, duration_scale: f32, perks: u32) -> i32 {
    let mut t = duration_scale * (params.player_sprint_time * 1000.0);
    if perks & PERK_SPRINT != 0 {
        t *= params.perk_sprint_multiplier;
    }
    (t as i32).min(0x3FFF)
}

fn max_sprint_time(pm: &Pmove<'_>) -> i32 {
    max_sprint_ms(pm.params, pm.weapon.sprint_duration_scale, pm.ps.perks)
}

/// `PM_GetSprintLeft`: the remaining budget in ms at `time`, given the budget `max`.
pub fn sprint_left_ms(ps: &PlayerState, params: &Params, time: i32, max: i32) -> i32 {
    let s = &ps.sprint_state;
    let left = if s.last_sprint_start != 0 {
        if s.last_sprint_start <= s.last_sprint_end {
            let spent = s.last_sprint_end - s.last_sprint_start;
            let base = time + s.sprint_start_max_length - spent - s.last_sprint_end;
            if s.sprint_delay {
                base - (params.player_sprint_recharge_pause * 1000.0) as i32
            } else {
                base
            }
        } else {
            s.sprint_start_max_length - (time - s.last_sprint_start)
        }
    } else {
        max
    };
    left.clamp(0, max)
}

fn sprint_left(pm: &Pmove<'_>, time: i32) -> i32 {
    sprint_left_ms(&pm.ps, pm.params, time, max_sprint_time(pm))
}

/// `PM_IsSprinting`.
pub(super) fn is_sprinting(pm: &Pmove<'_>) -> bool {
    let s = &pm.ps.sprint_state;
    s.last_sprint_start != 0 && s.last_sprint_start > s.last_sprint_end
}

fn melee_or_offhand(state: u8) -> bool {
    matches!(state, ws::MELEE_INIT | ws::MELEE_FIRE | ws::MELEE_END)
        || (ws::OFFHAND_INIT..=ws::OFFHAND_END).contains(&state)
}

/// `PM_SprintStartInterferingButtons`.
fn start_blocked(pm: &Pmove<'_>) -> bool {
    let ps = &pm.ps;
    let buttons = pm.cmd.buttons;
    if ps.pm_flags & pmf::LADDER != 0
        || i32::from(pm.cmd.forwardmove) <= pm.params.player_sprint_forward_minimum
        || buttons
            & (button::ATTACK
                | button::MELEE
                | button::RELOAD
                | button::USE_RELOAD
                | button::JUMP
                | button::FRAG
                | button::SMOKE)
            != 0
        || ps.leanf != 0.0
        || ps.pm_flags & (pmf::MANTLE | pmf::LADDER | pmf::SHELLSHOCKED) != 0
    {
        return true;
    }
    if ps.pm_flags & pmf::JUMPING != 0 && ps.pm_time == 0 {
        return false;
    }
    melee_or_offhand(ps.weapon_state)
}

/// `PM_SprintEndingButtons`.
fn end_requested(pm: &Pmove<'_>) -> bool {
    let ps = &pm.ps;
    let buttons = pm.cmd.buttons;
    ps.pm_flags & (pmf::LADDER | pmf::SIGHT_AIMING | pmf::SHELLSHOCKED) != 0
        || i32::from(pm.cmd.forwardmove) <= pm.params.player_sprint_forward_minimum
        || buttons
            & (button::ATTACK
                | button::MELEE
                | button::RELOAD
                | button::USE_RELOAD
                | button::PRONE
                | button::CROUCH
                | button::JUMP
                | button::FRAG
                | button::SMOKE)
            != 0
        || ps.leanf != 0.0
        || melee_or_offhand(ps.weapon_state)
        || matches!(
            ps.weapon_state,
            ws::NIGHTVISION_WEAR | ws::NIGHTVISION_REMOVE
        )
}

/// `PM_CanStand`: standing up from a crouch or prone would not hit anything.
fn can_stand(pm: &Pmove<'_>, world: &dyn Collide) -> bool {
    if pm.ps.pm_flags & (pmf::PRONE | pmf::DUCKED) == 0 {
        return true;
    }
    let o = pm.ps.origin;
    !pm.trace(
        world,
        o,
        PLAYER_MINS,
        PLAYER_MAXS,
        o,
        pm.tracemask & MASK_IGNORE_CHARACTERS,
    )
    .all_solid
}

/// `PM_EndSprint`.
fn end(pm: &mut Pmove<'_>) {
    if pm.ps.pm_flags & pmf::SPRINTING != 0 {
        pm.ps.sprint_state.sprint_delay = false;
        pm.ps.sprint_state.last_sprint_end = pm.cmd.server_time;
        pm.ps.pm_flags &= !pmf::SPRINTING;
        if pm.cmd.buttons & button::SPRINT != 0 {
            pm.ps.sprint_state.sprint_button_up_required = true;
        }
    }
}

/// `PM_UpdateSprint`.
pub(super) fn update(pm: &mut Pmove<'_>, world: &dyn Collide) {
    let time = pm.cmd.server_time;
    if pm.ps.sprint_state.sprint_button_up_required && pm.cmd.buttons & button::SPRINT == 0 {
        pm.ps.sprint_state.sprint_button_up_required = false;
    }
    if pm.ps.pm_type >= PmType::Noclip || max_sprint_time(pm) <= 0 {
        end(pm);
        return;
    }
    if pm.ps.pm_flags & pmf::SPRINTING != 0 {
        if time - pm.ps.sprint_state.last_sprint_start >= pm.ps.sprint_state.sprint_start_max_length
        {
            end(pm);
            pm.ps.sprint_state.sprint_delay = true;
            return;
        }
        if end_requested(pm) {
            end(pm);
            return;
        }
        if pm.oldcmd.buttons & button::SPRINT == 0 && pm.cmd.buttons & button::SPRINT != 0 {
            end(pm);
            pm.ps.sprint_state.sprint_button_up_required = true;
        }
    } else if (!pm.ps.sprint_state.sprint_delay
        || pm.params.player_sprint_recharge_pause * 1000.0
            <= (time - pm.ps.sprint_state.last_sprint_end) as f32)
        && pm.cmd.buttons & button::SPRINT != 0
        && pm.ps.pm_flags & pmf::NO_SPRINT == 0
        && !pm.ps.sprint_state.sprint_button_up_required
        && !start_blocked(pm)
        && can_stand(pm, world)
    {
        let left = sprint_left(pm, time);
        if pm.params.player_sprint_min_time * 1000.0 < left as f32 {
            pm.ps.sprint_state.sprint_start_max_length = left;
            pm.ps.sprint_state.last_sprint_start = time;
            pm.ps.pm_flags |= pmf::SPRINTING;
            exit_ads(&mut pm.ps);
        }
    }
}
