// SPDX-License-Identifier: GPL-3.0-or-later
//! `Pmove` and `PmoveSingle`: the per-command driver.

use super::ads;
use super::duck::{check_duck, frozen_by_weapon};
use super::footsteps::{foliage_sounds, footsteps};
use super::ladder;
use super::mantle;
use super::math::{self, EQUAL_EPSILON};
use super::sprint;
use super::state::{PmType, Stance, button, ef, ev, pmf, weapon_state as ws};
use super::view::{update_prone_pitch, update_view_angles};
use super::walk::{
    air_move, dead_move, fly_move, ground_trace, noclip_move, ufo_move, walk_move,
};
use super::{Pml, Pmove};
use crate::cm::{Collide, ENTITYNUM_NONE, Trace};
use crate::contents::PLAYER;

/// Longest time slice one `PmoveSingle` simulates.
const MAX_STEP_MSEC: i32 = 66;

/// `Pmove`: advances the player to the command's `server_time` in steps of at most 66 ms.
pub(super) fn pmove(pm: &mut Pmove<'_>, world: &dyn Collide) {
    let final_time = pm.cmd.server_time;
    if final_time < pm.ps.command_time {
        return;
    }
    if final_time > pm.ps.command_time + 1000 {
        pm.ps.command_time = final_time - 1000;
    }
    pm.num_touch = 0;
    while pm.ps.command_time != final_time {
        let msec = (final_time - pm.ps.command_time).min(MAX_STEP_MSEC);
        pm.cmd.server_time = msec + pm.ps.command_time;
        pmove_single(pm, world);
        pm.oldcmd = pm.cmd;
    }
}

fn pml_new(msec: i32, ps: &super::PlayerState) -> Pml {
    Pml {
        forward: [0.0; 3],
        right: [0.0; 3],
        up: [0.0; 3],
        frametime: (f64::from(msec) * f64::from(EQUAL_EPSILON)) as f32,
        msec,
        walking: false,
        ground_plane: false,
        almost_ground_plane: false,
        ground_trace: Trace::MISS,
        impact_speed: 0.0,
        previous_origin: ps.origin,
        previous_velocity: ps.velocity,
    }
}

/// `PM_DropTimers`: counts down the knockback/hard-landing/jump timer.
fn drop_timers(pm: &mut Pmove<'_>, pml: &Pml) {
    let ps = &mut pm.ps;
    if ps.pm_time != 0 {
        if pml.msec < ps.pm_time {
            ps.pm_time -= pml.msec;
        } else {
            if ps.pm_flags & pmf::JUMPING != 0 {
                super::jump::clear_state(ps);
            }
            ps.pm_flags &= !(pmf::TIME_HARDLANDING | pmf::TIME_KNOCKBACK | pmf::JUMPING);
            ps.pm_time = 0;
        }
    }
}

/// `PM_MeleeChargeClear`.
fn melee_charge_clear(ps: &mut super::PlayerState) {
    ps.pm_flags &= !pmf::MELEE_CHARGE;
    ps.melee_charge_yaw = 0.0;
    ps.melee_charge_dist = 0;
    ps.melee_charge_time = 0;
}

/// `PM_MeleeChargeUpdate`: a lunge launches the player along a fixed arc.
fn melee_charge_update(pm: &mut Pmove<'_>, pml: &Pml) {
    let ps = &mut pm.ps;
    let valid = ps.pm_flags & pmf::MELEE_CHARGE != 0
        && ps.pm_type == PmType::Normal
        && ps.e_flags & ef::TURRET_ACTIVE == 0
        && ps.pm_flags & (pmf::MANTLE | pmf::LADDER) == 0;
    if !valid {
        melee_charge_clear(ps);
        return;
    }
    let friction = pm.params.player_melee_charge_friction;
    if ps.melee_charge_time == 0 {
        let (dir, _) = math::yaw_vectors_2d(ps.melee_charge_yaw);
        let speed = (friction * (ps.melee_charge_dist as f32 + ps.melee_charge_dist as f32)).sqrt();
        ps.velocity[0] = speed * dir[0];
        ps.velocity[1] = speed * dir[1];
        ps.melee_charge_time = (speed / friction * 1000.0) as i32;
    }
    ps.melee_charge_time -= pml.msec;
    if ps.melee_charge_time <= 0 {
        melee_charge_clear(ps);
    }
}

/// `TurretNVGTrigger`: night vision toggles while manning a turret.
fn turret_nvg_trigger(pm: &mut Pmove<'_>) {
    if pm.oldcmd.buttons & button::NIGHTVISION == 0 && pm.cmd.buttons & button::NIGHTVISION != 0 {
        if pm.ps.weapon_flags & 0x40 != 0 {
            pm.ps.weapon_flags &= !0x40;
            pm.ps.add_event(ev::NIGHTVISION_REMOVE, 0);
        } else {
            pm.ps.weapon_flags |= 0x40;
            pm.ps.add_event(ev::NIGHTVISION_WEAR, 0);
        }
    }
}

/// `PmoveSingle`: one command step of at most 66 ms.
fn pmove_single(pm: &mut Pmove<'_>, world: &dyn Collide) {
    const KEEP_STANCE: i32 = button::PRONE | button::CROUCH | button::TEMP_STANCE;
    if pm.ps.pm_flags & pmf::MELEE_CHARGE != 0 {
        pm.cmd.forwardmove = 127;
    }
    if pm.ps.pm_flags & pmf::FROZEN != 0 {
        pm.cmd.buttons &= KEEP_STANCE;
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
        pm.ps.velocity = [0.0; 3];
    } else if pm.ps.pm_flags & pmf::RESPAWNED != 0 {
        pm.cmd.buttons &= button::ATTACK | KEEP_STANCE;
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
        pm.ps.velocity = [0.0; 3];
    } else if frozen_by_weapon(pm) {
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
        pm.cmd.buttons &= !(button::JUMP | button::LEAN_LEFT | button::LEAN_RIGHT);
        pm.ps.velocity = [0.0; 3];
    }
    if pm.cmd.buttons & button::LOC_SELECTING != 0 {
        pm.cmd.buttons &= button::LOC_SELECTING
            | button::TEMP_STANCE
            | button::ADS
            | button::CROUCH
            | button::SPRINT
            | button::PRONE;
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
    }
    pm.ps.pm_flags &= !pmf::NO_PRONE;
    if pm.ps.pm_type >= PmType::Dead {
        pm.tracemask &= !PLAYER;
    }
    // A prone player's move input cancels weapon states that a prone move interrupts.
    if pm.ps.pm_flags & pmf::PRONE == 0 || ads::using_sniper_scope(pm) {
        pm.ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
    } else {
        let f = |c: i8| f32::from(c).abs();
        let more_forward = pm.cmd.forwardmove != pm.oldcmd.forwardmove && f(pm.oldcmd.forwardmove) < f(pm.cmd.forwardmove);
        let more_right = pm.cmd.rightmove != pm.oldcmd.rightmove && f(pm.oldcmd.rightmove) < f(pm.cmd.rightmove);
        if more_forward || more_right {
            if ads::interrupt_weapon_with_prone_move(&mut pm.ps) {
                pm.ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
                ads::exit_ads(&mut pm.ps);
            }
        } else if pm.ps.pm_flags & pmf::SIGHT_AIMING == 0
            && (pm.ps.weapon_state <= ws::DROPPING_QUICK || pm.ps.weapon_state == ws::RELOADING)
        {
            pm.ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
        }
    }
    let stance = pm.ps.stance();
    if pm.ps.pm_flags & pmf::SIGHT_AIMING != 0 && stance == Stance::Prone && !ads::using_sniper_scope(pm) {
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
    }
    if pm.cmd.buttons & button::LOC_SELECTING != 0 {
        pm.ps.e_flags |= ef::LOC_SELECTING;
    } else {
        pm.ps.e_flags &= !ef::LOC_SELECTING;
    }
    pm.ps.e_flags &= !ef::FIRING;
    if pm.ps.pm_type != PmType::Intermission
        && pm.ps.pm_flags & pmf::RESPAWNED == 0
        && (pm.ps.weapon_state == ws::READY || pm.ps.weapon_state == ws::FIRING)
        && pm.weapon.ammo_in_clip != 0
        && pm.cmd.buttons & button::ATTACK != 0
    {
        pm.ps.e_flags |= ef::FIRING;
    }
    if pm.ps.pm_type < PmType::Dead && pm.cmd.buttons & (button::ATTACK | button::PRONE) == 0 {
        pm.ps.pm_flags &= !pmf::RESPAWNED;
    }

    let msec = (pm.cmd.server_time - pm.ps.command_time).clamp(1, 200);
    let mut pml = pml_new(msec, &pm.ps);
    pm.ps.command_time = pm.cmd.server_time;
    update_view_angles(pm, world, msec as f32);
    let (forward, right, up) = math::angle_vectors(&pm.ps.viewangles);
    pml.forward = forward;
    pml.right = right;
    pml.up = up;

    if pm.cmd.forwardmove < 0 {
        pm.ps.pm_flags |= pmf::BACKWARDS_RUN;
    } else if pm.cmd.forwardmove > 0 || pm.cmd.rightmove != 0 {
        pm.ps.pm_flags &= !pmf::BACKWARDS_RUN;
    }
    if pm.ps.pm_type >= PmType::LastStand {
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
    }
    if stance == Stance::Prone && pm.ps.pm_flags & pmf::PRONEMOVE_OVERRIDDEN != 0 {
        pm.cmd.forwardmove = 0;
        pm.cmd.rightmove = 0;
    }
    mantle::clear_hint(pm);
    melee_charge_update(pm, &pml);

    let pml = &mut pml;
    match pm.ps.pm_type {
        PmType::NormalLinked | PmType::DeadLinked => {
            ladder::clear_flag(&mut pm.ps);
            pm.ps.ground_entity_num = ENTITYNUM_NONE;
            pml.walking = false;
            pml.ground_plane = false;
            pml.almost_ground_plane = false;
            pm.ps.velocity = [0.0; 3];
            ads::update_flag(pm, pml);
            sprint::update(pm, world);
            ads::update_walking_flag(pm);
            drop_timers(pm, pml);
            check_duck(pm, world, pml);
            footsteps(pm, world, pml);
        }
        PmType::Noclip => {
            ladder::clear_flag(&mut pm.ps);
            ads::update_flag(pm, pml);
            sprint::update(pm, world);
            ads::update_walking_flag(pm);
            drop_timers(pm, pml);
            noclip_move(pm, pml);
            ads::update_lerp(pm, pml);
        }
        PmType::Ufo => {
            ladder::clear_flag(&mut pm.ps);
            ads::update_flag(pm, pml);
            sprint::update(pm, world);
            ads::update_walking_flag(pm);
            drop_timers(pm, pml);
            ufo_move(pm, pml);
            ads::update_lerp(pm, pml);
        }
        PmType::Spectator => {
            ladder::clear_flag(&mut pm.ps);
            ads::update_flag(pm, pml);
            sprint::update(pm, world);
            ads::update_walking_flag(pm);
            check_duck(pm, world, pml);
            drop_timers(pm, pml);
            fly_move(pm, world, pml);
            ads::update_lerp(pm, pml);
        }
        PmType::Intermission => {
            ladder::clear_flag(&mut pm.ps);
            ads::update_flag(pm, pml);
            sprint::update(pm, world);
            ads::update_lerp(pm, pml);
        }
        PmType::Normal | PmType::LastStand | PmType::Dead => {
            if pm.ps.pm_type == PmType::LastStand {
                ladder::clear_flag(&mut pm.ps);
                pm.ps.e_flags &= !ef::TURRET_ACTIVE;
            }
            if pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
                turret_move(pm, world, pml);
            } else {
                walking_move(pm, world, pml);
            }
        }
    }
}

/// The turret branch of `PmoveSingle`: the player is pinned to the mount.
fn turret_move(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    ladder::clear_flag(&mut pm.ps);
    pm.ps.ground_entity_num = ENTITYNUM_NONE;
    pml.walking = false;
    pml.ground_plane = false;
    pml.almost_ground_plane = false;
    pm.ps.velocity = [0.0; 3];
    ads::update_flag(pm, pml);
    sprint::update(pm, world);
    ads::update_walking_flag(pm);
    turret_nvg_trigger(pm);
    drop_timers(pm, pml);
    check_duck(pm, world, pml);
    ads::update_lerp(pm, pml);
    footsteps(pm, world, pml);
}

/// The ordinary on-foot branch of `PmoveSingle`.
fn walking_move(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    if pm.ps.pm_flags & pmf::MANTLE == 0 {
        ads::update_flag(pm, pml);
        sprint::update(pm, world);
        ads::update_walking_flag(pm);
        check_duck(pm, world, pml);
        ground_trace(pm, world, pml);
    }
    mantle::check(pm, world, pml);
    if pm.ps.pm_flags & pmf::MANTLE != 0 {
        ladder::clear_flag(&mut pm.ps);
        pm.ps.ground_entity_num = ENTITYNUM_NONE;
        pml.ground_plane = false;
        pml.walking = false;
        ads::update_flag(pm, pml);
        sprint::update(pm, world);
        ads::update_walking_flag(pm);
        check_duck(pm, world, pml);
        mantle::advance(pm, pml);
        return;
    }
    update_prone_pitch(pm, world, pml);
    drop_timers(pm, pml);
    if pm.ps.pm_type >= PmType::LastStand {
        dead_move(&mut pm.ps, pml);
    }
    ladder::check(pm, world, pml);
    if pm.ps.pm_flags & pmf::LADDER != 0 {
        ladder::ladder_move(pm, world, pml);
    } else if pml.walking {
        walk_move(pm, world, pml);
    } else {
        air_move(pm, world, pml);
    }
    ground_trace(pm, world, pml);
    footsteps(pm, world, pml);
    foliage_sounds(pm, world);

    // If the move got much less far than the velocity says, trust the position.
    let moved = math::sub(&pm.ps.origin, &pml.previous_origin);
    let real_sq = math::length_sq(&moved) / (pml.frametime * pml.frametime);
    let supposed_sq = math::length_sq(&pm.ps.velocity);
    if real_sq < supposed_sq * 0.25 {
        pm.ps.velocity = math::scale(&moved, 1.0 / pml.frametime);
    }
    let change = [
        pm.ps.velocity[0] - pm.ps.old_velocity[0],
        pm.ps.velocity[1] - pm.ps.old_velocity[1],
    ];
    let blend = if 1.0 - pml.frametime < 0.0 { 1.0 } else { pml.frametime };
    pm.ps.old_velocity[0] += blend * change[0];
    pm.ps.old_velocity[1] += blend * change[1];
    math::snap_vector(&mut pm.ps.velocity);
}
