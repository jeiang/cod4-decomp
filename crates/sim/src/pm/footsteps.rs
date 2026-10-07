// SPDX-License-Identifier: GPL-3.0-or-later
//! Footstep cadence and events (`PM_Footsteps`): the bob cycle advances with ground speed and
//! each half-cycle raises a footstep event for the surface underfoot.

use super::duck::view_height_lerp;
use super::math::{self, length2, mad};
use super::sprint::is_sprinting;
use super::state::{PmType, Stance, ef, ev, pmf};
use super::walk::ground_surface_type;
use super::{Pml, Pmove};
use crate::cm::{Collide, ENTITYNUM_NONE};
use crate::contents::{PLAYER, PLAYERCLIP};

const SURF_TYPE_MASK: i32 = 0x01F0_0000;
const SURF_TYPE_SHIFT: i32 = 20;

/// `bobFactorTable[stance][walking]` with the backward rows after the three forward ones.
const BOB_FACTOR: [[f32; 2]; 6] = [
    [0.335, 0.305],
    [0.25, 0.24],
    [0.34, 0.315],
    [0.36, 0.325],
    [0.25, 0.24],
    [0.34, 0.315],
];

/// `PM_ShouldMakeFootsteps`.
pub(super) fn should_make_footsteps(pm: &Pmove<'_>) -> bool {
    let ps = &pm.ps;
    if ps.stance() != Stance::Stand {
        return false;
    }
    let walking = ps.pm_flags & pmf::WALKING != 0;
    !walking && pm.params.player_footsteps_threshhold <= pm.xyspeed
}

/// `PM_FootstepType`.
fn footstep_type(pm: &Pmove<'_>, pml: &Pml) -> u8 {
    let ps = &pm.ps;
    if ground_surface_type(pml) == 0 {
        ev::NONE
    } else if ps.pm_flags & pmf::PRONE != 0 {
        ev::FOOTSTEP_PRONE
    } else if ps.pm_flags & pmf::WALKING != 0 || ps.leanf != 0.0 {
        ev::FOOTSTEP_WALK
    } else if is_sprinting(pm) {
        ev::FOOTSTEP_SPRINT
    } else {
        ev::FOOTSTEP_RUN
    }
}

/// `PM_FootstepEvent`: raises a step when the bob cycle crosses a half period.
fn footstep_event(
    pm: &mut Pmove<'_>,
    world: &dyn Collide,
    pml: &Pml,
    old: u8,
    new: u8,
    step: bool,
) {
    if (new.wrapping_add(64) ^ old.wrapping_add(64)) & 0x80 == 0 {
        return;
    }
    if pm.ps.ground_entity_num == ENTITYNUM_NONE {
        if step && pm.ps.pm_flags & pmf::LADDER != 0 {
            let (mins, maxs) = ladder_probe_bounds(pm);
            let mask = pm.tracemask & !(PLAYER | PLAYERCLIP);
            let end = mad(&pm.ps.origin, -31.0, &pm.ps.ladder_vec);
            let client = pm.ps.client_num;
            let t = world.trace(pm.ps.origin, end, mins, maxs, client, mask);
            let mut surf = ((t.surface_flags & SURF_TYPE_MASK) >> SURF_TYPE_SHIFT) as u32;
            if t.fraction == 1.0 || surf == 0 {
                surf = 21;
            }
            pm.ps.add_event(ev::FOOTSTEP_RUN, surf);
        }
    } else if step {
        let kind = footstep_type(pm, pml);
        pm.ps.add_event(kind, ground_surface_type(pml));
    }
}

/// The reduced hull used to probe a ladder face: inset 6 on X/Y, starting 8 up.
pub(super) fn ladder_probe_bounds(pm: &Pmove<'_>) -> ([f32; 3], [f32; 3]) {
    let mins = [pm.mins[0] + 6.0, pm.mins[1] + 6.0, 8.0];
    let mut maxs = [pm.maxs[0] - 6.0, pm.maxs[1] - 6.0, pm.maxs[2]];
    if 8.0 > maxs[2] {
        maxs[2] = mins[2];
    }
    (mins, maxs)
}

/// Advances the bob cycle by `bobmove` per ms of `pml.msec` and raises any footstep.
fn advance_bob(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml, per_ms: f32, step: bool) {
    let old = pm.ps.bob_cycle;
    pm.ps.bob_cycle = (f64::from(old) + f64::from(pml.msec) * f64::from(per_ms)) as i32 as u8;
    let new = pm.ps.bob_cycle;
    footstep_event(pm, world, pml, old, new, step);
}

/// Bob bump from a stair step (`PM_StepSlideMove`).
pub(super) fn bump_bob_cycle(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml, bob: f32) {
    let old = pm.ps.bob_cycle;
    pm.ps.bob_cycle = (f64::from(old) + f64::from(bob)) as i32 as u8;
    let new = pm.ps.bob_cycle;
    footstep_event(pm, world, pml, old, new, true);
}

/// `PM_GetMaxSpeed`: top speed for the current input, used to scale the bob.
fn max_speed(pm: &Pmove<'_>, walking: bool, sprinting: bool) -> f32 {
    let p = pm.params;
    let mut max = pm.ps.speed as f32;
    let (f, r) = (pm.cmd.forwardmove, pm.cmd.rightmove);
    if f != 0 {
        if r != 0 {
            max = ((p.player_strafe_speed_scale - 1.0) * 0.75 + 1.0 + 1.0) * 0.5 * max;
            if f < 0 {
                max = (p.player_back_speed_scale + 1.0) * 0.5 * max;
            }
        } else if f < 0 {
            max *= p.player_back_speed_scale;
        }
    } else if r != 0 {
        max = ((p.player_strafe_speed_scale - 1.0) * 0.75 + 1.0) * max;
    }
    max = if walking {
        max * 0.4
    } else if sprinting {
        max * p.player_sprint_speed_scale
    } else {
        max * 1.0
    };
    if pm.ps.weapon != 0 {
        let w = &pm.weapon;
        if w.move_speed_scale <= 0.0 || pm.ps.pm_flags & pmf::WALKING != 0 {
            if w.ads_move_speed_scale > 0.0 {
                max *= w.ads_move_speed_scale;
            }
        } else {
            max *= w.move_speed_scale;
        }
    }
    stance_scale(pm) * max
}

/// `PM_CmdScaleForStance`.
fn stance_scale(pm: &Pmove<'_>) -> f32 {
    use super::state::{VIEW_CROUCH, VIEW_PRONE};
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

/// `PM_Footsteps`.
pub(super) fn footsteps(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml) {
    if pm.ps.pm_type >= PmType::Dead {
        return;
    }
    pm.xyspeed = length2(&pm.ps.velocity);
    if pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
        return;
    }
    let stance = pm.ps.stance();
    if pm.ps.ground_entity_num == ENTITYNUM_NONE && pm.ps.pm_type != PmType::NormalLinked {
        ladder_footsteps(pm, world, pml);
        return;
    }
    let walking = pm.ps.pm_flags & pmf::WALKING != 0 || pm.ps.leanf != 0.0;
    let sprinting = pm.ps.pm_flags & pmf::SPRINTING != 0;
    if pm.params.player_move_threshhold > pm.xyspeed || pm.ps.pm_type == PmType::NormalLinked {
        if pm.xyspeed < 1.0 {
            pm.ps.bob_cycle = 0;
        }
    } else if pm.cmd.forwardmove != 0 || pm.cmd.rightmove != 0 {
        let row = match stance {
            Stance::Stand => 0,
            Stance::Prone => 1,
            Stance::Crouch => 2,
        } + if pm.ps.pm_flags & pmf::BACKWARDS_RUN != 0 {
            3
        } else {
            0
        };
        let max = max_speed(pm, walking, sprinting);
        let factor = if row != 0 || !sprinting {
            BOB_FACTOR[row][usize::from(walking)]
        } else {
            pm.params.player_sprint_camera_bob
        };
        let bobmove = pm.xyspeed / max * factor;
        let step = should_make_footsteps(pm);
        advance_bob(pm, world, pml, bobmove, step);
    }
}

/// `PM_Footstep_LadderMove`.
fn ladder_footsteps(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml) {
    if pm.ps.pm_flags & pmf::LADDER == 0 || pm.cmd.server_time - pm.ps.jump_time < 300 {
        return;
    }
    let speed = pm.ps.velocity[2];
    let max = 0.5 * 1.5 * 127.0;
    let bobmove = if pm.ps.pm_flags & pmf::WALKING == 0 && pm.ps.leanf == 0.0 {
        speed / (max * 1.0) * 0.45
    } else {
        speed / (max * 0.4) * 0.35
    };
    advance_bob(pm, world, pml, bobmove, true);
}

/// `PM_FoliageSounds`: brushing through foliage raises a rustle at a speed-based interval.
pub(super) fn foliage_sounds(pm: &mut Pmove<'_>, world: &dyn Collide) {
    let p = pm.params;
    if p.bg_foliagesnd_minspeed <= pm.xyspeed {
        let frac = ((pm.xyspeed - p.bg_foliagesnd_minspeed)
            / (p.bg_foliagesnd_maxspeed - p.bg_foliagesnd_minspeed))
            .min(1.0);
        let interval = ((p.bg_foliagesnd_fastinterval - p.bg_foliagesnd_slowinterval) as f32 * frac
            + p.bg_foliagesnd_slowinterval as f32) as i32;
        if interval + pm.ps.foliage_sound_time < pm.cmd.server_time {
            let mins = math::scale(&pm.mins, 0.75);
            let mut maxs = math::scale(&pm.maxs, 0.75);
            maxs[2] = pm.maxs[2] * 0.9;
            let o = pm.ps.origin;
            let t = pm.player_trace(world, o, mins, maxs, o, crate::contents::FOLIAGE);
            if t.start_solid {
                pm.ps.add_event(ev::FOLIAGE_SOUND, 0);
                pm.ps.foliage_sound_time = pm.cmd.server_time;
            }
        }
    } else if p.bg_foliagesnd_resetinterval + pm.ps.foliage_sound_time < pm.cmd.server_time {
        pm.ps.foliage_sound_time = 0;
    }
}
