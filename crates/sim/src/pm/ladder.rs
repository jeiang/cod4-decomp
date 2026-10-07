// SPDX-License-Identifier: GPL-3.0-or-later
//! Ladders (`PM_CheckLadderMove`, `PM_LadderMove`): grabbing a ladder face, climbing, and
//! letting go.

use super::footsteps::ladder_probe_bounds;
use super::jump;
use super::math::{self, mad, normalize, normalize2, normalize_to, project_point_on_plane};
use super::slide::step_slide_move;
use super::state::{PlayerState, PmType, Stance, pmf};
use super::walk::{SURF_LADDER, accelerate, cmd_scale};
use super::{Pml, Pmove};
use crate::cm::{Collide, ENTITYNUM_NONE};

/// `PM_ClearLadderFlag`: remembers the fall off a ladder so it is not re-grabbed at once.
pub(super) fn clear_flag(ps: &mut PlayerState) {
    if ps.pm_flags & pmf::LADDER != 0 {
        ps.pm_flags |= pmf::LADDER_FALL;
        ps.pm_flags &= !pmf::LADDER;
    }
}

/// `PM_CheckLadderMove`: grab or release a ladder by probing the surface ahead.
pub(super) fn check(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    if pml.walking {
        pm.ps.pm_flags &= !pmf::LADDER_FALL;
    }
    let ps = &pm.ps;
    if ps.pm_time != 0
        && ps.pm_flags & pmf::LADDER == 0
        && ps.pm_flags & (pmf::TIME_HARDLANDING | pmf::TIME_KNOCKBACK) != 0
    {
        return;
    }
    let trace_dist = if pml.walking { 8.0 } else { 30.0 };
    let fell_off_in_air = ps.pm_flags & pmf::LADDER != 0 && ps.ground_entity_num == ENTITYNUM_NONE;
    let mut check_dir = if fell_off_in_air {
        math::scale(&ps.ladder_vec, -1.0)
    } else {
        let mut d = [pml.forward[0], pml.forward[1], 0.0];
        normalize(&mut d);
        d
    };
    if pm.ps.pm_type >= PmType::Dead {
        pm.ps.ground_entity_num = ENTITYNUM_NONE;
        pml.ground_plane = false;
        pml.almost_ground_plane = false;
        pml.walking = false;
        clear_flag(&mut pm.ps);
        return;
    }
    if pm.ps.pm_flags & pmf::LADDER_FALL != 0
        || pm.ps.stance() == Stance::Prone
        || pm.cmd.server_time - pm.ps.jump_time < 300
    {
        clear_flag(&mut pm.ps);
        return;
    }
    let (mins, maxs) = ladder_probe_bounds(pm);
    let mut spot = mad(&pm.ps.origin, trace_dist, &check_dir);
    let t = pm.player_trace(world, pm.ps.origin, mins, maxs, spot, pm.tracemask);
    let grabbed = 'grab: {
        if t.fraction >= 1.0
            || t.surface_flags & SURF_LADDER == 0
            || pml.walking && pm.cmd.forwardmove <= 0
        {
            break 'grab false;
        }
        if pm.ps.pm_flags & pmf::LADDER == 0 {
            pm.ps.ladder_vec = t.normal;
            check_dir = math::scale(&pm.ps.ladder_vec, -1.0);
            spot = mad(&pm.ps.origin, trace_dist, &check_dir);
            let t2 = pm.player_trace(world, pm.ps.origin, mins, maxs, spot, pm.tracemask);
            if t2.fraction >= 1.0 || t2.surface_flags & SURF_LADDER == 0 {
                break 'grab false;
            }
        }
        true
    };
    if grabbed {
        pm.ps.pm_flags |= pmf::LADDER;
    } else {
        clear_flag(&mut pm.ps);
    }
}

/// `PM_LadderMove`.
pub(super) fn ladder_move(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    if jump::check(pm, pml) {
        super::walk::air_move(pm, world, pml);
        return;
    }
    let upscale = ((pml.forward[2] + 0.25) * 2.5).clamp(-1.0, 1.0);
    pml.forward[2] = 0.0;
    normalize(&mut pml.forward);
    pml.right[2] = 0.0;
    let right_unit = normalize_to(&pml.right).0;
    pml.right = project_point_on_plane(&right_unit, &pm.ps.ladder_vec);
    let scale = cmd_scale(pm);
    let mut wishvel = [0.0f32; 3];
    if pm.cmd.forwardmove != 0 {
        wishvel[2] = 0.5 * upscale * scale * f32::from(pm.cmd.forwardmove);
    }
    if pm.cmd.rightmove != 0 {
        wishvel = mad(&wishvel, scale * 0.2 * f32::from(pm.cmd.rightmove), &pml.right);
    }
    let (wishdir, wishspeed) = normalize_to(&wishvel);
    accelerate(&mut pm.ps, pm.params, pml, &wishdir, wishspeed, 9.0);

    let gravity_step = pm.ps.gravity as f32 * pml.frametime;
    if pm.cmd.forwardmove == 0 {
        // No climb input: bleed vertical speed away.
        if pm.ps.velocity[2] <= 0.0 {
            pm.ps.velocity[2] += gravity_step;
            if pm.ps.velocity[2] > 0.0 {
                pm.ps.velocity[2] = 0.0;
            }
        } else {
            pm.ps.velocity[2] -= gravity_step;
            if pm.ps.velocity[2] < 0.0 {
                pm.ps.velocity[2] = 0.0;
            }
        }
    }
    if pm.cmd.rightmove == 0 {
        let mut side_dir = [pml.right[0], pml.right[1], 0.0];
        normalize2(&mut side_dir);
        let mut side_speed = pm.ps.velocity[1] * side_dir[1] + pm.ps.velocity[0] * side_dir[0];
        if side_speed != 0.0 {
            pm.ps.velocity[0] += -side_speed * side_dir[0];
            pm.ps.velocity[1] += -side_speed * side_dir[1];
            let mut drop = side_speed * pml.frametime * 16.0;
            if drop.abs() < side_speed.abs() {
                if drop.abs() < 1.0 {
                    drop = if drop < 0.0 { -1.0 } else { 1.0 };
                }
                side_speed -= drop;
                pm.ps.velocity[0] += side_speed * side_dir[0];
                pm.ps.velocity[1] += side_speed * side_dir[1];
            }
        }
    }
    if !pml.walking {
        // Strip velocity into the wall; mostly-vertical movement is pressed against it.
        let lv = pm.ps.ladder_vec;
        let into = pm.ps.velocity[1] * lv[1] + pm.ps.velocity[0] * lv[0];
        pm.ps.velocity[0] += -into * lv[0];
        pm.ps.velocity[1] += -into * lv[1];
        let v = pm.ps.velocity;
        if (v[0] != 0.0 || v[1] != 0.0 || v[2] != 0.0)
            && v[2] * v[2] >= v[0] * v[0] + v[1] * v[1]
        {
            pm.ps.velocity[0] += -50.0 * lv[0];
            pm.ps.velocity[1] += -50.0 * lv[1];
        }
    }
    step_slide_move(pm, world, pml, false);
    let facing = math::vec_to_yaw(&pm.ps.ladder_vec) + 180.0;
    let yaw = math::angle_delta(facing, pm.ps.viewangles[1]) as i32;
    pm.ps.movement_dir = yaw.clamp(-75, 75) as i8;
}
