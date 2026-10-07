// SPDX-License-Identifier: GPL-3.0-or-later
//! Collision response: sliding along up to eight clip planes and stepping over low ledges
//! (`PM_SlideMove`, `PM_StepSlideMove`).

use super::jump;
use super::math::{self, cross, dot, mad, normalize, normalize_to, scale};
use super::prone::verify_position;
use super::state::{PmType, pmf};
use super::walk::{clip_velocity, project_velocity};
use super::{Pml, Pmove};
use crate::Vec3;
use crate::cm::{Collide, ENTITYNUM_NONE};

const MAX_CLIP_PLANES: usize = 8;
/// Hits below this entity number are other players: stepping onto them is not allowed.
const MAX_CLIENTS: u16 = 64;

/// `PM_PermuteRestrictiveClipPlanes`: orders planes by how much the velocity runs into them
/// (most restrictive first) and returns the smallest dot product.
fn permute_planes(velocity: &Vec3, planes: &[Vec3], order: &mut [usize; MAX_CLIP_PLANES]) -> f32 {
    let mut parallel = [0.0f32; MAX_CLIP_PLANES];
    for (i, plane) in planes.iter().enumerate() {
        parallel[i] = dot(velocity, plane);
        let mut at = i;
        while at != 0 && parallel[i] <= parallel[order[at - 1]] {
            order[at] = order[at - 1];
            at -= 1;
        }
        order[at] = i;
    }
    parallel[order[0]]
}

/// `PM_SlideMove`: returns whether the velocity was clipped.
pub(super) fn slide_move(
    pm: &mut Pmove<'_>,
    world: &dyn Collide,
    pml: &mut Pml,
    gravity: bool,
) -> bool {
    const NUM_BUMPS: usize = 4;
    let mut primal_velocity = pm.ps.velocity;
    let mut end_velocity = pm.ps.velocity;
    if gravity {
        end_velocity[2] -= pm.ps.gravity as f32 * pml.frametime;
        pm.ps.velocity[2] = (pm.ps.velocity[2] + end_velocity[2]) * 0.5;
        primal_velocity[2] = end_velocity[2];
        if pml.ground_plane {
            pm.ps.velocity = clip_velocity(&pm.ps.velocity, &pml.ground_trace.normal);
        }
    }
    let mut time_left = pml.frametime;
    let mut planes = [[0.0f32; 3]; MAX_CLIP_PLANES];
    let mut num_planes = 0;
    if pml.ground_plane {
        planes[0] = pml.ground_trace.normal;
        num_planes = 1;
    }
    planes[num_planes] = normalize_to(&pm.ps.velocity).0;
    num_planes += 1;

    let mut bump = 0;
    while bump < NUM_BUMPS {
        let end = mad(&pm.ps.origin, time_left, &pm.ps.velocity);
        let trace = pm.player_trace_body(world, pm.ps.origin, end);
        if trace.all_solid {
            // Trapped in another solid: keep sliding sideways but build no fall damage.
            pm.ps.velocity[2] = 0.0;
            return true;
        }
        if trace.fraction > 0.0 {
            pm.ps.origin = math::lerp(&pm.ps.origin, &end, trace.fraction);
        }
        if trace.fraction == 1.0 {
            break;
        }
        pm.add_touch_ent(trace.hit_id);
        time_left -= time_left * trace.fraction;
        if num_planes >= MAX_CLIP_PLANES {
            pm.ps.velocity = [0.0; 3];
            return true;
        }
        // Hugging the same plane again: nudge off it instead of re-clipping.
        let mut i = 0;
        while i < num_planes {
            if dot(&trace.normal, &planes[i]) > 0.999 {
                pm.ps.velocity = clip_velocity(&pm.ps.velocity, &trace.normal);
                pm.ps.velocity = math::add(&trace.normal, &pm.ps.velocity);
                break;
            }
            i += 1;
        }
        if i >= num_planes {
            planes[num_planes] = trace.normal;
            num_planes += 1;
            let mut order = [0usize; MAX_CLIP_PLANES];
            let into = permute_planes(&pm.ps.velocity, &planes[..num_planes], &mut order);
            if into < 0.1 {
                if pml.impact_speed < -into {
                    pml.impact_speed = -into;
                }
                let mut clip_v = clip_velocity(&pm.ps.velocity, &planes[order[0]]);
                let mut end_clip_v = clip_velocity(&end_velocity, &planes[order[0]]);
                for j in 1..num_planes {
                    if dot(&clip_v, &planes[order[j]]) >= 0.1 {
                        continue;
                    }
                    clip_v = clip_velocity(&clip_v, &planes[order[j]]);
                    end_clip_v = clip_velocity(&end_clip_v, &planes[order[j]]);
                    if dot(&clip_v, &planes[order[0]]) >= 0.0 {
                        continue;
                    }
                    // A crease: slide along the line where the two planes meet.
                    let mut dir = cross(&planes[order[0]], &planes[order[j]]);
                    normalize(&mut dir);
                    clip_v = scale(&dir, dot(&dir, &pm.ps.velocity));
                    end_clip_v = scale(&dir, dot(&dir, &end_velocity));
                    for k in 1..num_planes {
                        if k != j && dot(&clip_v, &planes[order[k]]) < 0.1 {
                            // Wedged into a corner: stop dead.
                            pm.ps.velocity = [0.0; 3];
                            return true;
                        }
                    }
                }
                pm.ps.velocity = clip_v;
                end_velocity = end_clip_v;
            }
        }
        bump += 1;
    }
    if gravity {
        pm.ps.velocity = end_velocity;
    }
    if pm.ps.pm_time != 0 {
        pm.ps.velocity = primal_velocity;
    }
    bump != 0
}

/// `PM_StepSlideMove`: a slide move that also tries stepping up over low obstacles and snaps
/// back down onto the ground.
pub(super) fn step_slide_move(
    pm: &mut Pmove<'_>,
    world: &dyn Collide,
    pml: &mut Pml,
    gravity: bool,
) {
    let params = pm.params;
    let mut step_amount = 0.0f32;
    let mut jumping = false;
    let had_ground;
    if ps_flag(pm, pmf::LADDER) {
        had_ground = false;
        jump::clear_state(&mut pm.ps);
    } else if pml.ground_plane {
        had_ground = true;
    } else {
        had_ground = false;
        if ps_flag(pm, pmf::JUMPING) && pm.ps.pm_time != 0 {
            jump::clear_state(&mut pm.ps);
        }
    }
    let start_o = pm.ps.origin;
    let start_v = pm.ps.velocity;
    let bumped = slide_move(pm, world, pml, gravity);
    let mut step_size = if ps_flag(pm, pmf::PRONE) { 10.0 } else { 18.0 };

    if pm.ps.ground_entity_num == ENTITYNUM_NONE {
        if ps_flag(pm, pmf::JUMPING) && pm.ps.pm_time != 0 {
            jump::clear_state(&mut pm.ps);
        }
        if bumped
            && ps_flag(pm, pmf::JUMPING)
            && let Some(h) = jump::step_height(&pm.ps, params, &start_o)
        {
            if h < 1.0 {
                return;
            }
            step_size = h;
            jumping = true;
        }
        if !jumping && !(ps_flag(pm, pmf::LADDER) && pm.ps.velocity[2] > 0.0) {
            return;
        }
    }

    let down_o = pm.ps.origin;
    let down_v = pm.ps.velocity;
    let flat_delta = [down_o[0] - start_o[0], down_o[1] - start_o[1]];
    if bumped || pml.ground_plane && pml.ground_trace.normal[2] < 0.9 {
        let up = [start_o[0], start_o[1], step_size + 1.0 + start_o[2]];
        let trace = pm.player_trace_body(world, start_o, up);
        step_amount = (step_size + 1.0) * trace.fraction - 1.0;
        if step_amount >= 1.0 {
            pm.ps.origin = [up[0], up[1], step_amount + start_o[2]];
            pm.ps.velocity = start_v;
            slide_move(pm, world, pml, gravity);
        } else {
            step_amount = 0.0;
        }
    }
    if had_ground || step_amount != 0.0 {
        let o = pm.ps.origin;
        let mut down = [o[0], o[1], o[2] - step_amount];
        if had_ground {
            down[2] -= 9.0;
        }
        let trace = pm.player_trace_body(world, o, down);
        if trace.hit_id < MAX_CLIENTS {
            pm.ps.origin = down_o;
            pm.ps.velocity = down_v;
            return;
        }
        if trace.fraction >= 1.0 {
            if step_amount != 0.0 {
                pm.ps.origin[2] -= step_amount;
            }
        } else {
            if !trace.walkable && trace.normal[2] < 0.3 {
                pm.ps.origin = down_o;
                pm.ps.velocity = down_v;
                return;
            }
            pm.ps.origin = math::lerp(&o, &down, trace.fraction);
            pm.ps.velocity = project_velocity(&pm.ps.velocity, &trace.normal);
        }
    }
    let step_delta = [pm.ps.origin[0] - start_o[0], pm.ps.origin[1] - start_o[1]];
    let along_step = step_delta[0] * start_v[0] + step_delta[1] * start_v[1];
    let along_flat = start_v[1] * flat_delta[1] + start_v[0] * flat_delta[0];
    if along_step <= along_flat + math::EQUAL_EPSILON || jumping && jump::above_max(&pm.ps, params)
    {
        // The step did not get further than sliding: take the slide.
        pm.ps.origin = down_o;
        pm.ps.velocity = down_v;
        if had_ground {
            let o = pm.ps.origin;
            let down = [o[0], o[1], o[2] - 9.0];
            let trace = pm.player_trace_body(world, o, down);
            if trace.fraction < 1.0 {
                let end = math::lerp(&o, &down, trace.fraction);
                pm.ps.origin = end;
                pm.ps.velocity = clip_velocity(&pm.ps.velocity, &trace.normal);
            }
        }
    }
    if jumping {
        jump::clamp_velocity(&mut pm.ps, params, &down_o);
    }
    if had_ground && pm.ps.pm_type < PmType::Dead && verify_position(pm, world, &start_o, &start_v)
    {
        let dz = pm.ps.origin[2] - down_o[2];
        if dz.abs() > 0.5 {
            let delta = math::snap_to_int(dz);
            if delta != 0 {
                if pm.view_change_time < pm.ps.command_time {
                    pm.view_change += dz;
                    pm.view_change_time = pm.ps.command_time;
                }
                let moved = (pm.ps.origin[2] - start_o[2]).abs();
                let speed_scale = 1.0 - 0.8 + (1.0 - moved / step_size) * 0.8;
                pm.ps.velocity = scale(&pm.ps.velocity, speed_scale);
                pm.xyspeed = math::length2(&pm.ps.velocity);
                if delta.abs() > 3
                    && pm.ps.ground_entity_num != ENTITYNUM_NONE
                    && super::footsteps::should_make_footsteps(pm)
                {
                    let steps = (delta.abs() / 2).min(4);
                    let bob = steps as f32 * 1.25 + 7.0;
                    super::footsteps::bump_bob_cycle(pm, world, pml, bob);
                }
            }
        }
    }
}

fn ps_flag(pm: &Pmove<'_>, flag: u32) -> bool {
    pm.ps.pm_flags & flag != 0
}
