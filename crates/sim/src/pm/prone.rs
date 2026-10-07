// SPDX-License-Identifier: GPL-3.0-or-later
//! Prone placement checks (`BG_CheckProne` / `BG_CheckProneValid`): whether a prone body fits
//! at a position and yaw, and the torso and waist pitch it settles into on uneven ground.

use super::math::{self, mad, normalize, normalize_to, sub};
use super::Pmove;
use crate::Vec3;
use crate::cm::{Collide, ENTITYNUM_NONE};
use crate::contents::MASK_DEADSOLID;

/// Tunable inputs of one check, the original's argument list.
pub(super) struct ProneCheck {
    pub size: f32,
    pub height: f32,
    pub yaw: f32,
    pub already_prone: bool,
    pub on_ground: bool,
    pub ground_walkable: bool,
    pub feet_dist: f32,
}

/// Returns whether a prone body fits. `pitches` receives (torso, waist) pitch when asked for.
pub(super) fn check_prone(
    world: &dyn Collide,
    client_num: u16,
    pos: &Vec3,
    c: &ProneCheck,
    mut pitches: Option<&mut [f32; 2]>,
) -> bool {
    let trace = |start: Vec3, mins: Vec3, maxs: Vec3, end: Vec3| {
        world.trace(start, end, mins, maxs, client_num, MASK_DEADSOLID)
    };
    if !c.already_prone {
        let mins = [-c.size, -c.size, 0.0];
        let maxs = [c.size, c.size, c.height];
        let end = [pos[0], pos[1], pos[2] + 10.0];
        if trace(*pos, mins, maxs, end).all_solid {
            return false;
        }
    }
    if c.on_ground && !c.ground_walkable {
        return false;
    }

    let mins = [-6.0; 3];
    let maxs = [6.0; 3];
    let (mut forward, _, _) = math::angle_vectors(&[0.0, c.yaw - 180.0, 0.0]);
    let trace_height = c.height - 6.0;
    let reach = c.feet_dist - 6.0;
    let mut start = [pos[0], pos[1], pos[2] + trace_height];
    let mut end = mad(&start, reach, &forward);
    let mut t = trace(start, mins, maxs, end);
    let mut first_hit = false;
    let mut first_dist = c.feet_dist;
    if t.fraction < 1.0 {
        if !c.on_ground {
            return false;
        }
        first_hit = true;
        first_dist = reach * t.fraction + 6.0;
        if first_dist < c.size + 2.0 {
            return false;
        }
        if first_dist < trace_height * 0.7 + 18.0 {
            // Low obstacle: try again aimed 22 units higher, which re-aims `forward` too.
            first_hit = false;
            end[2] += 22.0;
            let (dir, len) = normalize_to(&sub(&end, &start));
            forward = dir;
            t = trace(start, mins, maxs, end);
            if t.fraction < 1.0 {
                first_hit = true;
                first_dist = t.fraction * len + 6.0;
                if first_dist < trace_height * 0.7 + 18.0 {
                    return false;
                }
            } else {
                first_dist = c.feet_dist;
            }
        }
    }
    let mut feet_pos = math::lerp(&start, &end, t.fraction);

    'fail: {
        start = mad(pos, 18.0, &forward);
        start[2] += trace_height;
        let probe = c.size * 2.5 + trace_height - 6.0;
        end = [start[0], start[1], start[2] - probe];
        t = trace(start, mins, maxs, end);
        if t.fraction == 1.0 {
            break 'fail;
        }
        if !t.walkable {
            return false;
        }
        let waist_dist = probe * t.fraction + 6.0;
        let mut waist_pos = math::lerp(&start, &end, t.fraction);
        waist_pos[2] -= 6.0;
        if first_hit {
            if waist_dist * -0.75 > first_dist - waist_dist {
                break 'fail;
            }
            let mut delta = mad(&sub(&feet_pos, &waist_pos), 6.0, &forward);
            delta[2] += 6.0;
            normalize(&mut delta);
            end = mad(&start, reach - 18.0, &delta);
            end[0] = (reach * forward[0] + pos[0] + end[0]) * 0.5;
            end[1] = (reach * forward[1] + pos[1] + end[1]) * 0.5;
            t = trace(start, mins, maxs, end);
            if t.fraction < 1.0 {
                start = math::lerp(&start, &end, t.fraction);
                start[2] += 18.0;
                end[2] += 18.0;
                t = trace(start, mins, maxs, end);
                if t.fraction < 1.0 {
                    break 'fail;
                }
            }
            feet_pos = math::lerp(&start, &end, t.fraction);
        }
        let drop = (feet_pos[2] - waist_pos[2]) + (feet_pos[2] - waist_pos[2]) + c.size;
        start = feet_pos;
        end = [feet_pos[0], feet_pos[1], feet_pos[2] - drop];
        t = trace(start, mins, maxs, end);
        if t.fraction == 1.0 {
            break 'fail;
        }
        if !t.walkable {
            return false;
        }
        feet_pos = math::lerp(&start, &end, t.fraction);
        feet_pos[2] -= 6.0;
        let torso_pitch = wrap_pitch(math::vec_to_pitch(&sub(pos, &waist_pos)));
        let waist_pitch = wrap_pitch(math::vec_to_pitch(&sub(&waist_pos, &feet_pos)));
        let diff = math::angle_delta(torso_pitch, waist_pitch);
        let mut success = (-50.0..=70.0).contains(&diff);
        let zero = [0.0; 3];
        let s = [pos[0], pos[1], pos[2] + 5.0];
        let w = [waist_pos[0], waist_pos[1], waist_pos[2] + 5.0];
        let f = [feet_pos[0], feet_pos[1], feet_pos[2] + 5.0];
        success &= trace(s, zero, zero, w).fraction >= 1.0;
        success &= trace(w, zero, zero, f).fraction >= 1.0;
        if let Some(p) = pitches.as_deref_mut() {
            *p = [torso_pitch, waist_pitch];
        }
        if success {
            return true;
        }
    }
    if c.on_ground {
        return false;
    }
    // Airborne: nothing to conform to, so the pose is accepted flat.
    if let Some(p) = pitches {
        *p = [0.0; 2];
    }
    true
}

fn wrap_pitch(p: f32) -> f32 {
    let t = p * 0.002_777_777_8;
    (t - (t + 0.5).floor()) * 360.0
}

/// The player-state flavour: `BG_CheckProne(..., 15, 30, ...)` used by stance and turning.
pub(super) fn check_player_prone(
    pm: &mut Pmove<'_>,
    world: &dyn Collide,
    yaw: f32,
    already_prone: bool,
    ground_walkable: bool,
    feet_dist: f32,
    want_pitches: bool,
) -> bool {
    let ps = &pm.ps;
    let c = ProneCheck {
        size: 15.0,
        height: 30.0,
        yaw,
        already_prone,
        on_ground: ps.ground_entity_num != ENTITYNUM_NONE,
        ground_walkable,
        feet_dist,
    };
    let mut p = [ps.torso_pitch, ps.waist_pitch];
    let ok = check_prone(world, ps.client_num, &ps.origin, &c, want_pitches.then_some(&mut p));
    if want_pitches {
        pm.ps.torso_pitch = p[0];
        pm.ps.waist_pitch = p[1];
    }
    ok
}

/// `BG_CheckProneTurned`: can the prone body rotate to `new_yaw`.
pub(super) fn check_turned(pm: &mut Pmove<'_>, world: &dyn Collide, new_yaw: f32) -> bool {
    let delta = math::angle_delta(new_yaw, pm.ps.viewangles[1]);
    let fraction = delta.abs() / 240.0;
    let test_yaw = math::angle_normalize_360(new_yaw - (1.0 - fraction) * delta);
    let feet = fraction * 45.0 + (1.0 - fraction) * 50.0;
    check_player_prone(pm, world, test_yaw, true, true, feet, true)
}

/// `PM_VerifyPronePosition`: a prone player must still fit after a slide move; otherwise the
/// caller's fallback state is restored.
pub(super) fn verify_position(
    pm: &mut Pmove<'_>,
    world: &dyn Collide,
    fallback_origin: &Vec3,
    fallback_velocity: &Vec3,
) -> bool {
    if pm.ps.pm_flags & super::pmf::PRONE == 0 {
        return true;
    }
    let yaw = pm.ps.prone_direction;
    let ok = check_player_prone(pm, world, yaw, true, true, 50.0, true);
    if !ok {
        pm.ps.origin = *fallback_origin;
        pm.ps.velocity = *fallback_velocity;
    }
    ok
}

/// `PlayerProneAllowed`: whether a standing or crouched player on the ground may go prone here.
pub(super) fn prone_allowed(pm: &mut Pmove<'_>, world: &dyn Collide) -> bool {
    if pm.weapon.blocks_prone {
        return false;
    }
    if pm.ps.pm_flags & super::pmf::PRONE != 0 {
        return true;
    }
    if pm.ps.ground_entity_num == ENTITYNUM_NONE {
        return false;
    }
    let ps = &pm.ps;
    let c = ProneCheck {
        size: pm.maxs[0],
        height: 30.0,
        yaw: ps.viewangles[1],
        already_prone: false,
        on_ground: true,
        ground_walkable: true,
        feet_dist: 50.0,
    };
    let mut p = [ps.torso_pitch, ps.waist_pitch];
    let ok = check_prone(world, ps.client_num, &ps.origin, &c, Some(&mut p));
    pm.ps.torso_pitch = p[0];
    pm.ps.waist_pitch = p[1];
    ok
}
