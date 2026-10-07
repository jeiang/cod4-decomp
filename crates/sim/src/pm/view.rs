// SPDX-License-Identifier: GPL-3.0-or-later
//! View angles and lean (`PM_UpdateViewAngles*`, `PM_UpdateLean`, `PM_UpdatePronePitch`).

use super::math::{self, angle_delta, angle_normalize_360, angle_wrap_180};
use super::prone::{check_player_prone, check_turned};
use super::state::{ANGLE_UNIT, DEAD_YAW_UNSET, PmType, Stance, button, ef, ev, pmf};
use super::{Pml, Pmove, mantle};
use crate::cm::{Collide, ENTITYNUM_NONE};
use crate::contents::MASK_PLAYERSOLID;

/// `PM_UpdateViewAngles`.
pub(super) fn update_view_angles(pm: &mut Pmove<'_>, world: &dyn Collide, msec: f32) {
    if pm.ps.pm_type == PmType::Intermission {
        return;
    }
    if pm.ps.pm_type >= PmType::Dead {
        if pm.ps.dead_yaw == DEAD_YAW_UNSET {
            let angle = pm.cmd.angles[1] as f32 * ANGLE_UNIT + pm.ps.delta_angles[1];
            pm.ps.dead_yaw = angle_normalize_360(angle) as i32;
        }
        update_lean(pm, world, msec);
        return;
    }
    let old_yaw = pm.ps.viewangles[1];
    clamp(pm);
    if pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
        range_limited(pm, old_yaw);
        return;
    }
    if pm.ps.pm_flags & pmf::MANTLE != 0 {
        mantle::cap_view(pm);
        return;
    }
    if pm.ps.pm_flags & pmf::LADDER != 0
        && pm.ps.ground_entity_num == ENTITYNUM_NONE
        && pm.params.bg_ladder_yawcap != 0.0
    {
        ladder_clamp(pm);
    }
    if pm.ps.pm_flags & pmf::PRONE != 0 {
        update_prone(pm, world, msec, old_yaw);
    }
    if !matches!(pm.ps.pm_type, PmType::Ufo | PmType::Noclip | PmType::Spectator) {
        update_lean(pm, world, msec);
    }
}

/// `PM_UpdateViewAngles_Clamp`: command angles plus the server's delta, pitch limited.
fn clamp(pm: &mut Pmove<'_>) {
    let up = pm.params.player_view_pitch_up;
    let down = pm.params.player_view_pitch_down;
    for i in 0..3 {
        let cmd_angle = pm.cmd.angles[i] as f32 * ANGLE_UNIT;
        let mut temp = angle_wrap_180(pm.ps.delta_angles[i] + cmd_angle);
        if i == 0 {
            if down < temp {
                pm.ps.delta_angles[0] = down - cmd_angle;
                temp = down;
            } else if temp < -up {
                pm.ps.delta_angles[0] = -up - cmd_angle;
                temp = -up;
            }
        }
        pm.ps.viewangles[i] = angle_wrap_180(temp);
    }
}

/// `PM_UpdateViewAngles_RangeLimited`: keeps pitch and yaw within the clamp range around a base
/// (turrets).
fn range_limited(pm: &mut Pmove<'_>, old_yaw: f32) {
    for i in 0..2 {
        let range = pm.ps.view_angle_clamp_range[i];
        if range >= 180.0 {
            continue;
        }
        let base = pm.ps.view_angle_clamp_base[i];
        let delta = if i == 1 {
            angle_delta(old_yaw, pm.ps.viewangles[1]) + angle_delta(base, old_yaw)
        } else {
            angle_delta(base, pm.ps.viewangles[i])
        };
        if range < delta || delta < -range {
            let excess = if range >= delta { delta + range } else { delta - range };
            pm.ps.delta_angles[i] += excess;
            pm.ps.viewangles[i] = if excess <= 0.0 {
                angle_normalize_360(base + range)
            } else {
                angle_normalize_360(base - range)
            };
        }
    }
}

/// `PM_UpdateViewAngles_LadderClamp`: the view stays within `bg_ladder_yawcap` of the ladder.
fn ladder_clamp(pm: &mut Pmove<'_>) {
    let cap = pm.params.bg_ladder_yawcap;
    let facing = math::vec_to_yaw(&pm.ps.ladder_vec) + 180.0;
    let delta = angle_delta(facing, pm.ps.viewangles[1]);
    if cap < delta || delta < -cap {
        let excess = if cap >= delta { delta + cap } else { delta - cap };
        pm.ps.delta_angles[1] += excess;
        pm.ps.viewangles[1] = if excess <= 0.0 {
            angle_normalize_360(facing + cap)
        } else {
            angle_normalize_360(facing - cap)
        };
    }
}

/// `PM_UpdateViewAngles_Prone`: turning while prone drags the body round; the body direction
/// follows the view up to `bg_prone_yawcap`, blocked by geometry that would not fit.
fn update_prone(pm: &mut Pmove<'_>, world: &dyn Collide, msec: f32, old_view_yaw: f32) {
    let start_view_yaw = pm.ps.viewangles[1];
    let mut blocked = false;
    let delta = angle_delta(pm.ps.prone_direction, start_view_yaw);
    let threshold = pm.params.bg_prone_yawcap - 5.0;
    let over_cap = !(-threshold..=threshold).contains(&delta);
    let moving_off_axis = (pm.cmd.forwardmove != 0 || pm.cmd.rightmove != 0) && delta != 0.0;
    let fits = |pm: &mut Pmove<'_>, yaw: f32| check_player_prone(pm, world, yaw, true, true, 45.0, false);

    if over_cap || moving_off_axis {
        let max_delta_yaw = msec * 55.0 * math::EQUAL_EPSILON;
        let mut new_yaw = if max_delta_yaw <= delta.abs() {
            if delta <= 0.0 {
                pm.ps.prone_direction + max_delta_yaw
            } else {
                pm.ps.prone_direction - max_delta_yaw
            }
        } else {
            pm.ps.viewangles[1]
        };
        let mut retry = true;
        let mut gave_up = false;
        while !check_turned(pm, world, new_yaw) {
            if !retry {
                gave_up = true;
                break;
            }
            let mut step = angle_delta(pm.ps.prone_direction, new_yaw);
            retry = step.abs() > 1.0;
            if !retry {
                blocked = true;
            } else {
                step = if step <= 0.0 { -1.0 } else { 1.0 };
            }
            new_yaw = angle_normalize_360(new_yaw + step);
        }
        if !gave_up {
            let cur = pm.ps.viewangles[1];
            let mut ok = fits(pm, cur);
            if ok {
                ok = fits(pm, new_yaw);
                if ok {
                    pm.ps.prone_direction = new_yaw;
                }
            }
            if !ok {
                blocked = true;
            }
        }
    }

    let mut sync = angle_delta(pm.ps.prone_direction, pm.ps.viewangles[1]);
    if sync != 0.0 {
        let mut test_yaw = pm.ps.prone_direction;
        let mut retry = true;
        loop {
            let ok = fits(pm, test_yaw);
            if ok && check_turned(pm, world, test_yaw) {
                pm.ps.prone_direction = test_yaw;
                break;
            }
            if !retry {
                break;
            }
            retry = sync.abs() > 1.0;
            if retry {
                sync = if sync <= 0.0 { -1.0 } else { 1.0 };
            }
            blocked = true;
            pm.ps.delta_angles[1] += sync;
            pm.ps.viewangles[1] = angle_normalize_360(pm.ps.viewangles[1] + sync);
            sync = angle_delta(pm.ps.prone_direction, pm.ps.viewangles[1]);
            if !ok {
                test_yaw = angle_normalize_360(test_yaw + sync);
            }
        }
    }
    yaw_clamp(pm, sync, blocked, old_view_yaw, start_view_yaw);
    pitch_clamp(pm);
}

/// `PM_UpdateViewAngles_ProneYawClamp`.
fn yaw_clamp(pm: &mut Pmove<'_>, delta: f32, blocked: bool, old_view_yaw: f32, new_view_yaw: f32) {
    let cap = pm.params.bg_prone_yawcap;
    if cap < delta || delta < -cap {
        let excess = if cap >= delta { delta + cap } else { delta - cap };
        pm.ps.delta_angles[1] += excess;
        pm.ps.viewangles[1] = if excess <= 0.0 {
            angle_normalize_360(pm.ps.prone_direction + cap)
        } else {
            angle_normalize_360(pm.ps.prone_direction - cap)
        };
    }
    if blocked {
        pm.ps.pm_flags |= pmf::NO_PRONE;
        let d1 = angle_delta(old_view_yaw, pm.ps.viewangles[1]);
        if d1.abs() <= 1.0 {
            let d2 = angle_delta(new_view_yaw, pm.ps.viewangles[1]);
            if d1 * d2 > 0.0 {
                let back = d1 * 0.98;
                pm.ps.viewangles[1] = angle_normalize_360(pm.ps.viewangles[1] + back);
                pm.ps.delta_angles[1] += back;
            }
        }
    }
}

/// `PM_UpdateViewAngles_PronePitchClamp`: the view pitch stays within 45 degrees of the torso.
fn pitch_clamp(pm: &mut Pmove<'_>) {
    let delta = angle_delta(pm.ps.prone_torso_pitch, pm.ps.viewangles[0]);
    if delta > 45.0 || delta < -45.0 {
        let excess = if delta <= 45.0 { delta + 45.0 } else { delta - 45.0 };
        pm.ps.delta_angles[0] += excess;
        let target = if excess <= 0.0 {
            pm.ps.prone_torso_pitch + 45.0
        } else {
            pm.ps.prone_torso_pitch - 45.0
        };
        pm.ps.viewangles[0] = angle_wrap_180(target);
    }
}

/// `PM_UpdateLean`.
pub(super) fn update_lean(pm: &mut Pmove<'_>, world: &dyn Collide, msec: f32) {
    let mut leaning = 0i32;
    if pm.cmd.buttons & (button::LEAN_LEFT | button::LEAN_RIGHT) != 0
        && pm.ps.pm_flags & pmf::FROZEN == 0
        && pm.ps.pm_type < PmType::Dead
        && (pm.ps.ground_entity_num != ENTITYNUM_NONE || pm.ps.pm_type == PmType::NormalLinked)
    {
        if pm.cmd.buttons & button::LEAN_LEFT != 0 {
            leaning = -1;
        }
        if pm.cmd.buttons & button::LEAN_RIGHT != 0 {
            leaning += 1;
        }
    }
    if pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
        leaning = 0;
    }
    let lean_max = if pm.ps.stance() == Stance::Prone { 0.25 } else { 0.5 };
    let mut lean = pm.ps.leanf;
    if leaning != 0 {
        if leaning <= 0 {
            if lean > -lean_max {
                lean -= msec / 350.0 * lean_max;
            }
            if lean < -lean_max {
                lean = -lean_max;
            }
        } else {
            if lean_max > lean {
                lean += msec / 350.0 * lean_max;
            }
            if lean_max < lean {
                lean = lean_max;
            }
        }
    } else if lean <= 0.0 {
        if lean < 0.0 {
            lean += msec / 280.0 * lean_max;
            if lean > 0.0 {
                lean = 0.0;
            }
        }
    } else {
        lean -= msec / 280.0 * lean_max;
        if lean < 0.0 {
            lean = 0.0;
        }
    }
    pm.ps.leanf = lean;

    if pm.ps.leanf != 0.0 {
        let frac = if pm.ps.leanf < 0.0 { -1.0 } else { 1.0 };
        let start = [
            pm.ps.origin[0],
            pm.ps.origin[1],
            pm.ps.origin[2] + pm.ps.view_height_current,
        ];
        let mut end = start;
        math::add_lean_to_position(&mut end, pm.ps.viewangles[1], frac, 16.0, 20.0);
        let t = pm.trace(world, start, [-8.0; 3], [8.0; 3], end, MASK_PLAYERSOLID);
        let allowed = math::un_get_lean_fraction(t.fraction);
        if allowed < pm.ps.leanf.abs() {
            pm.ps.leanf = allowed * if pm.ps.leanf < 0.0 { -1.0 } else { 1.0 };
        }
    }
}

/// `PM_UpdatePronePitch`: prone body pitch follows the ground slope.
pub(super) fn update_prone_pitch(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml) {
    if pm.ps.pm_flags & pmf::PRONE == 0 {
        return;
    }
    if pm.ps.ground_entity_num == ENTITYNUM_NONE {
        let walkable = if pml.ground_plane { pml.ground_trace.walkable } else { true };
        let yaw = pm.ps.prone_direction;
        if !check_player_prone(pm, world, yaw, true, walkable, 50.0, true) {
            pm.ps.add_event(ev::STANCE_FORCE_CROUCH, 0);
            pm.ps.pm_flags |= pmf::NO_PRONE;
        }
    } else if pml.ground_plane && !pml.ground_trace.walkable {
        pm.ps.add_event(ev::STANCE_FORCE_CROUCH, 0);
    }

    let step = pml.frametime * 70.0;
    let approach = |current: f32, target: f32| -> f32 {
        let delta = angle_delta(target, current);
        if delta == 0.0 {
            return current;
        }
        let moved = if delta.abs() <= step {
            current + delta
        } else {
            step * if delta < 0.0 { -1.0 } else { 1.0 } + current
        };
        angle_wrap_180(moved)
    };
    let dir_target = if pml.ground_plane {
        math::pitch_for_yaw_on_normal(pm.ps.prone_direction, &pml.ground_trace.normal)
    } else {
        0.0
    };
    pm.ps.prone_direction_pitch = approach(pm.ps.prone_direction_pitch, dir_target);
    let torso_target = if pml.ground_plane {
        math::pitch_for_yaw_on_normal(pm.ps.viewangles[1], &pml.ground_trace.normal)
    } else {
        0.0
    };
    pm.ps.prone_torso_pitch = approach(pm.ps.prone_torso_pitch, torso_target);
}
