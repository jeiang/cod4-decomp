// SPDX-License-Identifier: GPL-3.0-or-later
//! Jumping (`Jump_*`): launch speed from the jump height, the slowdown after landing, and the
//! step-up allowance near the top of the arc.

use super::math::{dot, mad, normalize, scale};
use super::state::{PlayerState, Stance, button, ev, pmf};
use super::walk::ground_surface_type;
use super::{Params, PmType, Pml, Pmove};
use crate::cm::ENTITYNUM_NONE;

/// `JUMP_LAND_SLOWDOWN_TIME`: `pm_time` while a jump's landing slowdown runs.
const LAND_SLOWDOWN_TIME: i32 = 1800;

/// `Jump_ClearState`.
pub(super) fn clear_state(ps: &mut PlayerState) {
    ps.pm_flags &= !pmf::JUMPING;
    ps.jump_origin_z = 0.0;
}

/// `Jump_GetStepHeight`: the step height allowed at `origin` this jump, none above the apex.
pub(super) fn step_height(ps: &PlayerState, params: &Params, origin: &crate::Vec3) -> Option<f32> {
    let apex = ps.jump_origin_z + params.jump_height;
    if origin[2] >= apex {
        return None;
    }
    let mut step = params.jump_step_size;
    if apex < origin[2] + step {
        step = apex - origin[2];
    }
    Some(step)
}

/// `Jump_IsPlayerAboveMax`.
pub(super) fn above_max(ps: &PlayerState, params: &Params) -> bool {
    ps.origin[2] >= ps.jump_origin_z + params.jump_height
}

/// `Jump_ActivateSlowdown`: also used when going prone.
pub(super) fn activate_slowdown(ps: &mut PlayerState) {
    if ps.pm_time == 0 {
        ps.pm_flags |= pmf::JUMPING;
        ps.pm_time = LAND_SLOWDOWN_TIME;
    }
}

/// `Jump_ApplySlowdown`: velocity scale while the post-jump timer runs.
pub(super) fn apply_slowdown(ps: &mut PlayerState, params: &Params) {
    let mut s = 1.0;
    if ps.pm_time <= LAND_SLOWDOWN_TIME {
        if ps.pm_time == 0 {
            if ps.origin[2] >= ps.jump_origin_z + 18.0 {
                ps.pm_time = 1200;
                s = 0.5;
            } else {
                ps.pm_time = 1800;
                s = 0.65;
            }
        }
    } else {
        clear_state(ps);
        s = 0.65;
    }
    if params.jump_slowdown_enable {
        ps.velocity = scale(&ps.velocity, s);
    }
}

/// `Jump_ReduceFriction`: ground friction multiplier during the slowdown.
pub(super) fn reduce_friction(ps: &mut PlayerState, params: &Params) -> f32 {
    if ps.pm_time > LAND_SLOWDOWN_TIME {
        clear_state(ps);
        1.0
    } else {
        land_factor(ps, params)
    }
}

/// `Jump_GetSlowdownFriction` / `Jump_GetLandFactor` (the same curve).
fn land_factor(ps: &PlayerState, params: &Params) -> f32 {
    if !params.jump_slowdown_enable {
        return 1.0;
    }
    if ps.pm_time >= 1700 {
        return 2.5;
    }
    ps.pm_time as f32 * 1.5 * 0.000_588_235_3 + 1.0
}

/// `Jump_ClampVelocity`: never rise past the apex of the jump after a step.
pub(super) fn clamp_velocity(ps: &mut PlayerState, params: &Params, origin: &crate::Vec3) {
    if ps.origin[2] - origin[2] > 0.0 {
        let height_diff = ps.jump_origin_z + params.jump_height - ps.origin[2];
        if height_diff >= 0.1 {
            let max_up = (ps.gravity as f32 * (height_diff + height_diff)).sqrt();
            if max_up < ps.velocity[2] {
                ps.velocity[2] = max_up;
            }
        } else {
            ps.velocity[2] = 0.0;
        }
    }
}

/// `Jump_Check`: starts a jump when jump is freshly pressed and allowed.
pub(super) fn check(pm: &mut Pmove<'_>, pml: &mut Pml) -> bool {
    let ps = &pm.ps;
    if ps.pm_flags & (pmf::NO_JUMP | pmf::RESPAWNED | pmf::MANTLE) != 0
        || pm.cmd.server_time - ps.jump_time < 500
        || ps.pm_type >= PmType::Dead
        || ps.stance() != Stance::Stand
        || pm.cmd.buttons & button::JUMP == 0
    {
        return false;
    }
    if pm.oldcmd.buttons & button::JUMP != 0 {
        pm.cmd.buttons &= !button::JUMP;
        return false;
    }
    let height = pm.params.jump_height;
    start(pm, pml, height);
    add_surface_event(&mut pm.ps, pml);
    if pm.ps.pm_flags & pmf::LADDER != 0 {
        push_off_ladder(&mut pm.ps, pml, pm.params);
    }
    true
}

/// `Jump_Start`.
fn start(pm: &mut Pmove<'_>, pml: &mut Pml, height: f32) {
    let params = pm.params;
    let ps = &mut pm.ps;
    let mut velocity_sq = (height + height) * ps.gravity as f32;
    if ps.pm_flags & pmf::JUMPING != 0 && ps.pm_time <= LAND_SLOWDOWN_TIME {
        velocity_sq /= land_factor(ps, params);
    }
    pml.ground_plane = false;
    pml.almost_ground_plane = false;
    pml.walking = false;
    ps.ground_entity_num = ENTITYNUM_NONE;
    ps.jump_time = pm.cmd.server_time;
    ps.jump_origin_z = ps.origin[2];
    ps.velocity[2] = velocity_sq.sqrt();
    ps.pm_flags &= !(pmf::TIME_HARDLANDING | pmf::TIME_KNOCKBACK);
    ps.pm_flags |= pmf::JUMPING;
    ps.pm_time = 0;
    ps.sprint_state.sprint_button_up_required = false;
    ps.aim_spread_scale += params.jump_spread_add;
    if ps.aim_spread_scale > 255.0 {
        ps.aim_spread_scale = 255.0;
    }
}

/// `Jump_PushOffLadder`: leaves the ladder away from its surface.
fn push_off_ladder(ps: &mut PlayerState, pml: &Pml, params: &Params) {
    ps.velocity[2] *= 0.75;
    let mut flat = [pml.forward[0], pml.forward[1], 0.0];
    normalize(&mut flat);
    let push_dir = if dot(&ps.ladder_vec, &pml.forward) >= 0.0 {
        flat
    } else {
        let d = dot(&flat, &ps.ladder_vec);
        let mut dir = mad(&flat, d * -2.0, &ps.ladder_vec);
        normalize(&mut dir);
        dir
    };
    ps.velocity[0] = params.jump_ladder_push_vel * push_dir[0];
    ps.velocity[1] = params.jump_ladder_push_vel * push_dir[1];
    ps.pm_flags &= !pmf::LADDER;
}

/// `Jump_AddSurfaceEvent`.
fn add_surface_event(ps: &mut PlayerState, pml: &Pml) {
    if ps.pm_flags & pmf::LADDER != 0 {
        ps.add_event(ev::JUMP, 0x15);
    } else {
        let surf = ground_surface_type(pml);
        if surf != 0 {
            ps.add_event(ev::JUMP, surf);
        }
    }
}
