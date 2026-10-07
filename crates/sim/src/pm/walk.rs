// SPDX-License-Identifier: GPL-3.0-or-later
//! Ground and air movement: friction, acceleration, wish velocity, the ground trace, landing
//! and the fly/noclip/dead special cases.

use super::duck::view_height_lerp;
use super::jump;
use super::math::{self, add, cross, dot, length, mad, normalize, normalize_to, scale, sub};
use super::slide::step_slide_move;
use super::state::{
    PlayerState, PmType, Stance, VIEW_CROUCH, VIEW_PRONE, VIEW_STAND, button, ef, ev, pmf,
};
use super::{Pml, Pmove};
use crate::Vec3;
use crate::cm::{Collide, ENTITYNUM_NONE};

/// Surface flags and the surface-type field.
pub(super) const SURF_NODAMAGE: i32 = 0x1;
pub(super) const SURF_SLICK: i32 = 0x2;
pub(super) const SURF_LADDER: i32 = 0x8;
const SURF_NOSTEPS: i32 = 0x2000;
const SURF_TYPE_MASK: i32 = 0x01F0_0000;
const SURF_TYPE_SHIFT: i32 = 20;

/// `PM_GroundSurfaceType`: the footstep surface type of the ground (0 = silent).
pub(super) fn ground_surface_type(pml: &Pml) -> u32 {
    let flags = pml.ground_trace.surface_flags;
    if flags & SURF_NOSTEPS != 0 {
        0
    } else {
        ((flags & SURF_TYPE_MASK) >> SURF_TYPE_SHIFT) as u32
    }
}

/// `PM_ClipVelocity`: slides `v` along a plane, with a 0.1% overclip.
pub(super) fn clip_velocity(v: &Vec3, normal: &Vec3) -> Vec3 {
    let mut parallel = dot(v, normal);
    parallel -= parallel.abs() * math::EQUAL_EPSILON;
    mad(v, -parallel, normal)
}

/// `PM_ProjectVelocity`: rotates a velocity to lie in the plane, keeping its length when that
/// does not slow it.
pub(super) fn project_velocity(v: &Vec3, normal: &Vec3) -> Vec3 {
    let len_sq_2d = v[0] * v[0] + v[1] * v[1];
    if normal[2].abs() < math::EQUAL_EPSILON || len_sq_2d == 0.0 {
        return *v;
    }
    let new_z = -(normal[1] * v[1] + normal[0] * v[0]) / normal[2];
    let adjusted = [v[0], v[1], new_z];
    let original_sq = v[2] * v[2] + len_sq_2d;
    let adjusted_sq = new_z * new_z + len_sq_2d;
    let s = (original_sq / adjusted_sq).sqrt();
    if s < 1.0 || new_z < 0.0 || v[2] > 0.0 {
        scale(&adjusted, s)
    } else {
        *v
    }
}

/// `PM_Friction`.
pub(super) fn friction(pm: &mut Pmove<'_>, pml: &Pml) {
    let params = pm.params;
    let ps = &mut pm.ps;
    let mut vec = ps.velocity;
    if pml.walking {
        vec[2] = 0.0;
    }
    let speed = length(&vec);
    if speed < 1.0 {
        ps.velocity = [0.0; 3];
        return;
    }
    let mut drop = 0.0;
    if ps.pm_flags & pmf::MELEE_CHARGE != 0 {
        drop = params.player_melee_charge_friction * pml.frametime;
    } else if pml.walking
        && pml.ground_trace.surface_flags & SURF_SLICK == 0
        && ps.pm_flags & pmf::TIME_KNOCKBACK == 0
    {
        let value = if params.stopspeed <= speed {
            speed
        } else {
            params.stopspeed
        };
        let mut control = value;
        if ps.pm_flags & pmf::TIME_HARDLANDING != 0 {
            control = value * 0.3;
        } else if ps.pm_flags & pmf::JUMPING != 0 {
            control = jump::reduce_friction(ps, params) * value;
        }
        drop = control * params.friction * pml.frametime + drop;
    }
    if ps.pm_type == PmType::Spectator {
        drop = speed * 5.0 * pml.frametime + drop;
    }
    let new_speed = (speed - drop).max(0.0);
    let s = new_speed / speed;
    ps.velocity = scale(&ps.velocity, s);
}

/// `PM_Accelerate`.
pub(super) fn accelerate(
    ps: &mut PlayerState,
    params: &super::Params,
    pml: &Pml,
    wishdir: &Vec3,
    wishspeed: f32,
    accel: f32,
) {
    if ps.pm_flags & pmf::LADDER != 0 {
        let wish_velocity = scale(wishdir, wishspeed);
        let mut push = sub(&wish_velocity, &ps.velocity);
        let push_len = normalize(&mut push);
        let mut can_push = accel * pml.frametime * wishspeed;
        if can_push > push_len {
            can_push = push_len;
        }
        ps.velocity = mad(&ps.velocity, can_push, &push);
        return;
    }
    let current = dot(&ps.velocity, wishdir);
    let add_speed = wishspeed - current;
    if add_speed <= 0.0 {
        return;
    }
    let control = if params.stopspeed <= wishspeed {
        wishspeed
    } else {
        params.stopspeed
    };
    let mut accel_speed = accel * pml.frametime * control;
    if accel_speed > add_speed {
        accel_speed = add_speed;
    }
    let inertia = player_inertia(ps, params, accel_speed, wishdir);
    ps.velocity = mad(&ps.velocity, inertia, wishdir);
}

/// `PM_PlayerInertia`: acceleration is clamped when it would reverse the previous direction.
fn player_inertia(
    ps: &PlayerState,
    params: &super::Params,
    accel_speed: f32,
    wishdir: &Vec3,
) -> f32 {
    if ps.pm_type == PmType::Noclip || accel_speed <= params.inertia_max {
        return accel_speed;
    }
    let old_sq = ps.old_velocity[1] * ps.old_velocity[1] + ps.old_velocity[0] * ps.old_velocity[0];
    if old_sq < 0.0001 {
        return accel_speed;
    }
    let vx = accel_speed * wishdir[0] + ps.velocity[0];
    let vy = accel_speed * wishdir[1] + ps.velocity[1];
    let new_sq = vy * vy + vx * vx;
    let len = (new_sq * old_sq).sqrt();
    let dot_angle = vy * ps.old_velocity[1] + vx * ps.old_velocity[0];
    if dot_angle >= params.inertia_angle * len {
        accel_speed
    } else {
        params.inertia_max
    }
}

/// `PM_MoveScale`: the noclip/ufo/spectator speed scale for a command vector.
fn move_scale(ps: &PlayerState, params: &super::Params, fmove: f32, rmove: f32, umove: f32) -> f32 {
    let max = fmove.abs().max(rmove.abs()).max(umove.abs());
    if max == 0.0 {
        return 0.0;
    }
    let total = (umove * umove + rmove * rmove + fmove * fmove).sqrt();
    let mut s = ps.speed as f32 * max / (total * 127.0);
    s = if ps.pm_flags & pmf::WALKING == 0 && ps.leanf == 0.0 {
        s * 1.0
    } else {
        s * 0.4
    };
    match ps.pm_type {
        PmType::Noclip => s *= 3.0,
        PmType::Ufo => s *= 6.0,
        PmType::Spectator => return s * params.player_spectate_speed_scale,
        _ => {}
    }
    s
}

/// `PM_CmdScale`: the speed scale for plain air and ladder movement.
pub(super) fn cmd_scale(pm: &Pmove<'_>) -> f32 {
    let f = i32::from(pm.cmd.forwardmove);
    let r = i32::from(pm.cmd.rightmove);
    let total = ((r * r + f * f) as f32).sqrt();
    let max = f.abs().max(r.abs());
    if max == 0 {
        return 0.0;
    }
    let ps = &pm.ps;
    let mut s = ps.speed as f32 * max as f32 / (total * 127.0);
    s = if ps.pm_flags & pmf::WALKING == 0 && ps.leanf == 0.0 {
        s * 1.0
    } else {
        s * 0.4
    };
    match ps.pm_type {
        PmType::Noclip => s *= 3.0,
        PmType::Ufo => s *= 6.0,
        PmType::Spectator => return s * pm.params.player_spectate_speed_scale,
        _ => {}
    }
    s
}

/// `PM_CmdScaleForStance`: eye-height-aware stance speed factor.
fn cmd_scale_for_stance(pm: &Pmove<'_>) -> f32 {
    let to_prone = view_height_lerp(pm, VIEW_CROUCH, VIEW_PRONE);
    if to_prone != 0.0 {
        return to_prone * 0.15 + (1.0 - to_prone) * 0.65;
    }
    let to_crouch = view_height_lerp(pm, VIEW_PRONE, VIEW_CROUCH);
    if to_crouch != 0.0 {
        return to_crouch * 0.65 + (1.0 - to_crouch) * 0.15;
    }
    match pm.ps.stance() {
        Stance::Prone => 0.15,
        Stance::Crouch => 0.65,
        Stance::Stand => 1.0,
    }
}

/// `PM_CmdScale_Walk`: the ground speed scale including stance, sprint, weapon and damage.
fn cmd_scale_walk(pm: &Pmove<'_>) -> f32 {
    let ps = &pm.ps;
    let params = pm.params;
    let aiming_prone = ps.pm_flags & pmf::PRONE != 0 && ps.weapon_pos_frac > 0.0;
    let f = i32::from(pm.cmd.forwardmove);
    let r = i32::from(pm.cmd.rightmove);
    let total = ((r * r + f * f) as f32).sqrt();
    let fmove = if f >= 0 {
        (f as f32).abs()
    } else {
        (params.player_back_speed_scale * f as f32).abs()
    };
    let smove = (params.player_strafe_speed_scale * r as f32).abs();
    let biggest = if fmove - smove < 0.0 { smove } else { fmove };
    if biggest == 0.0 {
        return 0.0;
    }
    let mut s = ps.speed as f32 * biggest / (total * 127.0);
    s = if ps.pm_flags & pmf::WALKING != 0 || ps.leanf != 0.0 || aiming_prone {
        s * 0.4
    } else {
        s * 1.0
    };
    if ps.pm_flags & pmf::SPRINTING != 0 {
        s *= params.player_sprint_speed_scale;
    }
    s = match ps.pm_type {
        PmType::Noclip => s * 3.0,
        PmType::Ufo => s * 6.0,
        _ => cmd_scale_for_stance(pm) * s,
    };
    let w = &pm.weapon;
    if ps.weapon == 0
        || w.move_speed_scale <= 0.0
        || ps.pm_flags & pmf::WALKING != 0
        || aiming_prone
    {
        if ps.weapon != 0 && w.ads_move_speed_scale > 0.0 {
            s *= w.ads_move_speed_scale;
        }
    } else {
        s *= w.move_speed_scale;
    }
    if ps.pm_flags & pmf::SHELLSHOCKED != 0 && pm.shellshock_slows {
        s *= 0.4;
    }
    s * ps.move_speed_scale_multiplier
}

/// `PM_DamageScale_Walk`: walking slows after taking damage.
fn damage_scale_walk(params: &super::Params, damage_timer: i32) -> f32 {
    if damage_timer == 0 {
        return 1.0;
    }
    let max = params.player_dmgtimer_max_time;
    if max == 0.0 {
        return 1.0;
    }
    let gradient = -params.player_dmgtimer_min_scale / max;
    damage_timer as f32 * gradient + 1.0
}

/// `PM_AirMove`.
pub(super) fn air_move(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    friction(pm, pml);
    let fmove = f32::from(pm.cmd.forwardmove);
    let smove = f32::from(pm.cmd.rightmove);
    let s = cmd_scale(pm);
    pml.forward[2] = 0.0;
    pml.right[2] = 0.0;
    normalize(&mut pml.forward);
    normalize(&mut pml.right);
    let wishvel = [
        pml.forward[0] * fmove + pml.right[0] * smove,
        pml.forward[1] * fmove + pml.right[1] * smove,
        0.0,
    ];
    let (wishdir, speed) = normalize_to(&wishvel);
    accelerate(&mut pm.ps, pm.params, pml, &wishdir, speed * s, 1.0);
    if pml.ground_plane {
        pm.ps.velocity = clip_velocity(&pm.ps.velocity, &pml.ground_trace.normal);
    }
    step_slide_move(pm, world, pml, true);
    set_movement_dir(pm, pml);
}

/// `PM_WalkMove`.
pub(super) fn walk_move(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    if pm.ps.pm_flags & pmf::JUMPING != 0 {
        jump::apply_slowdown(&mut pm.ps, pm.params);
    }
    if pm.ps.pm_flags & pmf::SPRINTING != 0 {
        let strafe =
            f64::from(pm.cmd.rightmove) * f64::from(pm.params.player_sprint_strafe_speed_scale);
        pm.cmd.rightmove = strafe as i8;
    }
    if jump::check(pm, pml) {
        air_move(pm, world, pml);
        return;
    }
    friction(pm, pml);
    let fmove = f32::from(pm.cmd.forwardmove);
    let smove = f32::from(pm.cmd.rightmove);
    let mut s = cmd_scale_walk(pm);
    s *= damage_scale_walk(pm.params, pm.ps.damage_timer);
    pm.ps.damage_timer -= (pml.frametime * 1000.0) as i32;
    if pm.ps.damage_timer <= 0 {
        pm.ps.damage_timer = 0;
    }
    pml.forward[2] = 0.0;
    pml.right[2] = 0.0;
    math::normalize2(&mut pml.forward);
    math::normalize2(&mut pml.right);
    let wishvel = [
        fmove * pml.forward[0] + smove * pml.right[0],
        fmove * pml.forward[1] + smove * pml.right[1],
        0.0,
    ];
    let (mut wishdir, mut wishspeed) = normalize_to(&wishvel);
    wishspeed *= s;
    wishdir = project_velocity(&wishdir, &pml.ground_trace.normal);
    let slick = pml.ground_trace.surface_flags & SURF_SLICK != 0
        || pm.ps.pm_flags & pmf::TIME_KNOCKBACK != 0;
    let mut acceleration = if slick {
        1.0
    } else {
        match pm.ps.stance() {
            Stance::Prone => 19.0,
            Stance::Crouch => 12.0,
            Stance::Stand => 9.0,
        }
    };
    if pm.ps.pm_flags & pmf::TIME_HARDLANDING != 0 {
        acceleration *= 0.25;
    }
    accelerate(
        &mut pm.ps,
        pm.params,
        pml,
        &wishdir,
        wishspeed,
        acceleration,
    );
    if slick {
        pm.ps.velocity[2] -= pm.ps.gravity as f32 * pml.frametime;
    }
    pm.ps.velocity = project_velocity(&pm.ps.velocity, &pml.ground_trace.normal);
    if pm.ps.velocity[0] != 0.0 || pm.ps.velocity[1] != 0.0 {
        step_slide_move(pm, world, pml, false);
    }
    set_movement_dir(pm, pml);
}

/// `PM_SetMovementDir`: the direction of travel relative to the view, for animation.
pub(super) fn set_movement_dir(pm: &mut Pmove<'_>, pml: &Pml) {
    let ps = &mut pm.ps;
    let clamp = |yaw: i32, limit: i32| yaw.clamp(-limit, limit);
    if ps.pm_flags & pmf::PRONE != 0 && ps.e_flags & ef::TURRET_ACTIVE == 0 {
        let yaw = math::angle_delta(ps.prone_direction, ps.viewangles[1]) as i32;
        ps.movement_dir = clamp(yaw, 90) as i8;
    } else if ps.pm_flags & pmf::LADDER != 0 {
        let facing = math::vec_to_yaw(&ps.ladder_vec) + 180.0;
        let yaw = math::angle_delta(facing, ps.viewangles[1]) as i32;
        ps.movement_dir = clamp(yaw, 90) as i8;
    } else {
        let moved = sub(&ps.origin, &pml.previous_origin);
        let len = length(&moved);
        if pm.cmd.forwardmove == 0 && pm.cmd.rightmove == 0
            || ps.ground_entity_num == ENTITYNUM_NONE
            || len == 0.0
            || len <= pml.frametime * 5.0
        {
            ps.movement_dir = 0;
        } else {
            let (dir, _) = normalize_to(&moved);
            let dir_yaw = math::vec_to_yaw(&dir);
            let mut yaw = math::angle_delta(dir_yaw, ps.viewangles[1]) as i32;
            if pm.cmd.forwardmove < 0 {
                yaw = math::angle_wrap_180(yaw as f32 + 180.0) as i32;
            }
            ps.movement_dir = clamp(yaw, 90) as i8;
        }
    }
}

/// `PM_DeadMove`: a corpse on the ground skids to a stop.
pub(super) fn dead_move(ps: &mut PlayerState, pml: &Pml) {
    if pml.walking {
        let forward = length(&ps.velocity) - 20.0;
        if forward > 0.0 {
            normalize(&mut ps.velocity);
            ps.velocity = scale(&ps.velocity, forward);
        } else {
            ps.velocity = [0.0; 3];
        }
    }
}

/// Shared by noclip and ufo: friction at 1.5x and vertical input from the lean buttons.
fn free_fly_friction(
    pm: &mut Pmove<'_>,
    pml: &Pml,
    only_if_moving: bool,
    vertical: f32,
    moving: bool,
) {
    let params = pm.params;
    let ps = &mut pm.ps;
    let speed = if only_if_moving && !moving && vertical == 0.0 {
        0.0
    } else {
        length(&ps.velocity)
    };
    if speed >= 1.0 {
        let cur_friction = params.friction * 1.5;
        let value = if params.stopspeed <= speed {
            speed
        } else {
            params.stopspeed
        };
        let drop = value * cur_friction * pml.frametime;
        let new_speed = (speed - drop).max(0.0);
        ps.velocity = scale(&ps.velocity, new_speed / speed);
    } else {
        ps.velocity = [0.0; 3];
    }
}

fn lean_vertical(buttons: i32) -> f32 {
    let mut u = 0.0;
    if buttons & button::LEAN_RIGHT != 0 {
        u += 127.0;
    }
    if buttons & button::LEAN_LEFT != 0 {
        u -= 127.0;
    }
    u
}

/// `PM_NoclipMove`.
pub(super) fn noclip_move(pm: &mut Pmove<'_>, pml: &Pml) {
    pm.ps.view_height_target = VIEW_STAND;
    free_fly_friction(pm, pml, false, 0.0, true);
    let fmove = f32::from(pm.cmd.forwardmove);
    let smove = f32::from(pm.cmd.rightmove);
    let umove = lean_vertical(pm.cmd.buttons);
    let s = move_scale(&pm.ps, pm.params, fmove, smove, umove);
    let mut wishdir = [0.0; 3];
    for i in 0..3 {
        wishdir[i] = pml.forward[i] * fmove + pml.right[i] * smove + pml.up[i] * umove;
    }
    let wishspeed = normalize(&mut wishdir) * s;
    accelerate(&mut pm.ps, pm.params, pml, &wishdir, wishspeed, 9.0);
    pm.ps.origin = mad(&pm.ps.origin, pml.frametime, &pm.ps.velocity);
}

/// `PM_UFOMove`: like noclip but "forward" stays level.
pub(super) fn ufo_move(pm: &mut Pmove<'_>, pml: &Pml) {
    pm.ps.view_height_target = VIEW_STAND;
    let fmove = f32::from(pm.cmd.forwardmove);
    let smove = f32::from(pm.cmd.rightmove);
    let umove = lean_vertical(pm.cmd.buttons);
    free_fly_friction(pm, pml, true, umove, fmove != 0.0 || smove != 0.0);
    let s = move_scale(&pm.ps, pm.params, fmove, smove, umove);
    let up = [0.0, 0.0, 1.0];
    let forward = cross(&up, &pml.right);
    let mut wishdir = [0.0; 3];
    for i in 0..3 {
        wishdir[i] = forward[i] * fmove + pml.right[i] * smove + up[i] * umove;
    }
    let wishspeed = normalize(&mut wishdir) * s;
    accelerate(&mut pm.ps, pm.params, pml, &wishdir, wishspeed, 9.0);
    pm.ps.origin = mad(&pm.ps.origin, pml.frametime, &pm.ps.velocity);
}

/// `PM_FlyMove`: spectators.
pub(super) fn fly_move(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    friction(pm, pml);
    let s = cmd_scale(pm);
    let mut wishvel = [0.0; 3];
    if s != 0.0 {
        let up = [0.0, 0.0, 1.0];
        let forward = cross(&up, &pml.right);
        for i in 0..3 {
            wishvel[i] = (f32::from(pm.cmd.forwardmove) * forward[i]
                + f32::from(pm.cmd.rightmove) * pml.right[i])
                * s;
        }
    }
    if pm.ps.speed != 0 {
        let u = move_scale(&pm.ps, pm.params, 0.0, 0.0, 127.0);
        if pm.cmd.buttons & button::LEAN_LEFT != 0 {
            wishvel[2] -= u * 127.0;
        }
        if pm.cmd.buttons & button::LEAN_RIGHT != 0 {
            wishvel[2] += u * 127.0;
        }
    }
    let mut wishdir = wishvel;
    let wishspeed = normalize(&mut wishdir);
    accelerate(&mut pm.ps, pm.params, pml, &wishdir, wishspeed, 8.0);
    step_slide_move(pm, world, pml, false);
}

/// `PM_GroundTrace`: finds the ground under the player and updates the ground state.
pub(super) fn ground_trace(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    let o = pm.ps.origin;
    let (mut start, point) = if pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
        ([o[0], o[1], o[2]], [o[0], o[1], o[2] - 1.0])
    } else {
        ([o[0], o[1], o[2] + 0.25], [o[0], o[1], o[2] - 0.25])
    };
    let mut trace = pm.player_trace_body(world, start, point);
    pml.ground_trace = trace;
    if trace.all_solid && !correct_all_solid(pm, world, pml, &mut trace) {
        return;
    }
    if trace.start_solid {
        start[2] = pm.ps.origin[2] - math::EQUAL_EPSILON;
        trace = pm.player_trace_body(world, start, point);
        if trace.start_solid {
            pm.ps.ground_entity_num = ENTITYNUM_NONE;
            pml.ground_plane = false;
            pml.almost_ground_plane = false;
            pml.walking = false;
            return;
        }
        pml.ground_trace = trace;
    }
    if trace.fraction == 1.0 {
        ground_trace_missed(pm, world, pml);
        return;
    }
    let ps = &mut pm.ps;
    if ps.pm_flags & pmf::LADDER != 0
        || ps.velocity[2] <= 0.0
        || dot(&ps.velocity, &trace.normal) <= 10.0
    {
        if trace.walkable {
            pml.ground_plane = true;
            pml.almost_ground_plane = true;
            pml.walking = true;
            if ps.ground_entity_num == ENTITYNUM_NONE {
                crash_land(ps, pm.params, pml);
            }
            ps.ground_entity_num = trace.hit_id;
            let g = ps.ground_entity_num;
            pm.add_touch_ent(g);
        } else {
            ps.ground_entity_num = ENTITYNUM_NONE;
            pml.ground_plane = true;
            pml.almost_ground_plane = true;
            pml.walking = false;
            jump::clear_state(ps);
        }
    } else {
        // Moving up fast enough to leave the surface.
        pml.almost_ground_plane = false;
        ps.ground_entity_num = ENTITYNUM_NONE;
        pml.ground_plane = false;
        pml.walking = false;
    }
}

/// `PM_CorrectAllSolid`: when stuck, nudge by up to one unit in 26 directions until free.
fn correct_all_solid(
    pm: &mut Pmove<'_>,
    world: &dyn Collide,
    pml: &mut Pml,
    trace: &mut crate::cm::Trace,
) -> bool {
    for d in &CORRECT_SOLID_DELTAS {
        let point = add(&pm.ps.origin, d);
        *trace = pm.player_trace_body(world, point, point);
        if !trace.start_solid {
            pm.ps.origin = point;
            let down = [point[0], point[1], point[2] - 1.0 - 0.25];
            *trace = pm.player_trace_body(world, pm.ps.origin, down);
            pml.ground_trace = *trace;
            pm.ps.origin = math::lerp(&pm.ps.origin, &down, trace.fraction);
            return true;
        }
    }
    pm.ps.ground_entity_num = ENTITYNUM_NONE;
    pml.ground_plane = false;
    pml.almost_ground_plane = false;
    pml.walking = false;
    jump::clear_state(&mut pm.ps);
    false
}

/// Fact source: `CorrectSolidDeltas` in `iw3mp.exe`.
const CORRECT_SOLID_DELTAS: [Vec3; 26] = [
    [0.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [0.0, -1.0, 1.0],
    [1.0, 0.0, 1.0],
    [0.0, 1.0, 1.0],
    [-1.0, 0.0, 0.0],
    [0.0, -1.0, 0.0],
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 0.0, -1.0],
    [0.0, 1.0, -1.0],
    [-1.0, -1.0, 1.0],
    [1.0, -1.0, 1.0],
    [1.0, 1.0, 1.0],
    [-1.0, 1.0, 1.0],
    [-1.0, -1.0, 0.0],
    [1.0, -1.0, 0.0],
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [-1.0, -1.0, -1.0],
    [1.0, -1.0, -1.0],
    [1.0, 1.0, -1.0],
    [-1.0, 1.0, -1.0],
];

/// `PM_GroundTraceMissed`: nothing underneath; decide whether we are nearly grounded.
fn ground_trace_missed(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &mut Pml) {
    let o = pm.ps.origin;
    if pm.ps.ground_entity_num == ENTITYNUM_NONE {
        let t = pm.trace_body(world, o, [o[0], o[1], o[2] - 1.0]);
        pml.almost_ground_plane = t.fraction != 1.0;
    } else {
        let t = pm.trace_body(world, o, [o[0], o[1], o[2] - 64.0]);
        pml.almost_ground_plane = t.fraction != 1.0 && t.fraction < 0.015_625;
    }
    pm.ps.ground_entity_num = ENTITYNUM_NONE;
    pml.ground_plane = false;
    pml.walking = false;
}

/// `PM_CrashLand`: landing events, fall damage and the hard-landing slowdown.
///
/// The fall height comes from solving the free-fall arc between the previous and current
/// origin for the impact speed. Damage ramps from 0 at `bg_fallDamageMinHeight` to 100 at
/// `bg_fallDamageMaxHeight`; the damage rides in the event parameter.
fn crash_land(ps: &mut PlayerState, params: &super::Params, pml: &Pml) {
    let dist = pml.previous_origin[2] - ps.origin[2];
    let vel = pml.previous_velocity[2];
    let acc = -(ps.gravity as f32);
    let a = acc * 0.5;
    let den = vel * vel - a * 4.0 * dist;
    if den < 0.0 {
        return;
    }
    let t = (-vel - den.sqrt()) / (a * 2.0);
    let land_vel = (t * acc + vel) * -1.0;
    let fall_height = land_vel * land_vel / (ps.gravity as f32 * 2.0);
    let (min_h, max_h) = (
        params.bg_fall_damage_min_height,
        params.bg_fall_damage_max_height,
    );
    let damage = if min_h < max_h {
        if min_h >= fall_height
            || pml.ground_trace.surface_flags & SURF_NODAMAGE != 0
            || ps.pm_type >= PmType::Dead
        {
            0
        } else if max_h > fall_height {
            (((fall_height - min_h) / (max_h - min_h) * 100.0) as i32).clamp(0, 100)
        } else {
            100
        }
    } else {
        0
    };
    let view_dip = if fall_height > 12.0 {
        (((fall_height - 12.0) / 26.0 * 4.0 + 4.0) as i32).min(24)
    } else {
        0
    };
    let surf = ground_surface_type(pml);
    let event = |first: u8| {
        if surf != 0 {
            first + surf as u8
        } else {
            ev::NONE
        }
    };
    if damage != 0 {
        if damage >= 100 || pml.ground_trace.surface_flags & SURF_SLICK != 0 {
            ps.velocity = scale(&ps.velocity, 0.67);
        } else {
            let stun = (35 * damage + 500).min(2000);
            let speed_mult = if stun > 500 {
                if stun < 1500 {
                    0.5 - (stun as f32 - 500.0) / 1000.0 * 0.3
                } else {
                    0.2
                }
            } else {
                0.5
            };
            ps.pm_time = stun;
            ps.pm_flags |= pmf::TIME_HARDLANDING;
            ps.velocity = scale(&ps.velocity, speed_mult);
        }
        ps.add_event(event(ev::LANDING_PAIN_FIRST), damage as u32);
    } else if fall_height > 4.0 {
        if fall_height >= 8.0 {
            if fall_height >= 12.0 {
                ps.velocity = scale(&ps.velocity, 0.67);
                ps.add_event(event(ev::LANDING_FIRST), view_dip as u32);
            } else if surf != 0 {
                ps.add_event(ev::FOOTSTEP_RUN, surf);
            }
        } else if surf != 0 {
            ps.add_event(ev::FOOTSTEP_WALK, surf);
        }
    }
}
