// SPDX-License-Identifier: GPL-3.0-or-later
//! Stance changes and eye height (`PM_CheckDuck`, `PM_ViewHeightAdjust`).
//!
//! Stances are 60 (standing), 40 (crouched) and 11 (prone) units of eye height. A change starts
//! a timed lerp whose curve is one of four waypoint tables (below); the collision box height
//! follows the stance as soon as the target changes, not as the eye moves.

use super::ads::exit_ads;
use super::jump::activate_slowdown;
use super::math::{angle_delta, lerp, pitch_for_yaw_on_normal};
use super::prone::prone_allowed;
use super::state::{
    PmType, Stance, VIEW_CROUCH, VIEW_DEAD, VIEW_LASTSTAND, VIEW_PRONE, VIEW_STAND, button, ef, ev,
    pmf,
};
use super::{Pml, Pmove};
use crate::cm::{Collide, ENTITYNUM_NONE};
use crate::contents::MASK_IGNORE_CHARACTERS;

/// `(percent, eye height)` waypoints; linear between entries. Fact source: the
/// `viewLerp_*` tables of `iw3mp.exe`.
type Waypoints = &'static [(i32, f32)];

const STAND_CROUCH: Waypoints = &[
    (0, 60.0),
    (1, 59.5),
    (4, 58.5),
    (30, 56.0),
    (80, 44.0),
    (90, 41.5),
    (95, 40.5),
    (100, 40.0),
];
const CROUCH_STAND: Waypoints = &[
    (0, 40.0),
    (5, 40.5),
    (10, 41.5),
    (20, 44.0),
    (70, 56.0),
    (96, 58.5),
    (99, 59.5),
    (100, 60.0),
];
const CROUCH_PRONE: Waypoints = &[
    (0, 40.0),
    (11, 38.0),
    (22, 33.0),
    (34, 25.0),
    (45, 16.0),
    (50, 15.0),
    (55, 16.0),
    (70, 18.0),
    (90, 17.0),
    (100, 11.0),
];
const PRONE_CROUCH: Waypoints = &[
    (0, 11.0),
    (5, 10.0),
    (30, 21.0),
    (50, 25.0),
    (67, 31.0),
    (83, 34.0),
    (100, 40.0),
];

/// `PM_ViewHeightTableLerp`.
fn table_lerp(frac: i32, table: Waypoints) -> f32 {
    if frac == 0 {
        return table[0].1;
    }
    let mut prev = table[0];
    for &cur in &table[1..] {
        if frac == cur.0 {
            return cur.1;
        }
        if cur.0 > frac {
            let f = (frac - prev.0) as f32 / (cur.0 - prev.0) as f32;
            return (cur.1 - prev.1) * f + prev.1;
        }
        prev = cur;
    }
    table[0].1
}

/// `PM_GetViewHeightLerpTime`: how long a transition to `target` takes, in ms.
pub(super) fn lerp_time(target: i32, down: bool) -> i32 {
    if target == VIEW_PRONE {
        400
    } else if target != VIEW_CROUCH || down {
        200
    } else {
        400
    }
}

/// `PM_GetViewHeightLerp`: progress (0..=1) of the running eye-height lerp between two
/// heights, or 0 when it is not that transition.
pub(super) fn view_height_lerp(pm: &Pmove<'_>, from: i32, to: i32) -> f32 {
    let ps = &pm.ps;
    if ps.view_height_lerp_time == 0 {
        return 0.0;
    }
    if from != -1
        && to != -1
        && (to != ps.view_height_lerp_target
            || to == VIEW_CROUCH
                && (from != VIEW_PRONE || ps.view_height_lerp_down)
                && (from != VIEW_STAND || !ps.view_height_lerp_down))
    {
        return 0.0;
    }
    let f = (pm.cmd.server_time - ps.view_height_lerp_time) as f32
        / lerp_time(ps.view_height_lerp_target, ps.view_height_lerp_down) as f32;
    f.clamp(0.0, 1.0)
}

/// `PM_IsPlayerFrozenByWeapon`.
pub(super) fn frozen_by_weapon(pm: &Pmove<'_>) -> bool {
    pm.ps.weapon_state == super::weapon_state::FIRING
        && pm.ps.weapon != 0
        && pm.weapon.freeze_movement_when_firing
}

/// `PM_CheckDuck`: processes stance requests, sets the collision bounds and eye height.
pub(super) fn check_duck(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml) {
    pm.prone_change = false;
    if pm.ps.pm_type == PmType::Spectator {
        pm.mins = [-8.0; 3];
        pm.maxs = [8.0, 8.0, 16.0];
        pm.ps.pm_flags &= !(pmf::PRONE | pmf::DUCKED);
        if pm.cmd.buttons & button::PRONE != 0 {
            pm.cmd.buttons &= !button::PRONE;
            pm.ps.add_event(ev::STANCE_FORCE_STAND, 0);
        }
        pm.ps.view_height_target = 0;
        pm.ps.view_height_current = 0.0;
        return;
    }

    let was_prone = pm.ps.pm_flags & pmf::PRONE != 0;
    pm.mins = [-15.0, -15.0, 0.0];
    pm.maxs = [15.0, 15.0, 70.0];
    if pm.ps.pm_type == PmType::Dead {
        pm.ps.view_height_target = VIEW_DEAD;
        view_height_adjust(pm, pml);
    }
    if pm.ps.pm_flags & pmf::VEHICLE_ATTACHED != 0 {
        pm.ps.view_height_target = VIEW_STAND;
        pm.ps.pm_flags &= !(pmf::PRONE | pmf::DUCKED);
        pm.ps.add_event(ev::STANCE_FORCE_STAND, 0);
        view_height_adjust(pm, pml);
        return;
    }
    if pm.ps.pm_flags & pmf::SPRINTING != 0 {
        pm.ps.view_height_target = VIEW_STAND;
        pm.ps.e_flags &= !(ef::CROUCH | ef::PRONE);
        pm.ps.pm_flags &= !(pmf::PRONE | pmf::DUCKED);
        pm.ps.add_event(ev::STANCE_FORCE_STAND, 0);
        view_height_adjust(pm, pml);
        return;
    }

    if pm.ps.e_flags & ef::TURRET_ACTIVE != 0 {
        // A manned turret dictates the stance.
        let prone = pm.ps.e_flags & ef::TURRET_PRONE != 0;
        let crouch = pm.ps.e_flags & ef::TURRET_CROUCH != 0;
        if prone && !crouch {
            pm.ps.pm_flags |= pmf::PRONE;
            pm.ps.pm_flags &= !pmf::DUCKED;
        } else if crouch && !prone {
            pm.ps.pm_flags |= pmf::DUCKED;
            pm.ps.pm_flags &= !pmf::PRONE;
        } else {
            pm.ps.pm_flags &= !(pmf::PRONE | pmf::DUCKED);
        }
    } else if pm.ps.pm_flags & (pmf::RESPAWNED | pmf::FROZEN) == 0 && !frozen_by_weapon(pm) {
        if pm.ps.pm_type == PmType::LastStand {
            pm.ps.pm_flags &= !pmf::PRONE;
            pm.ps.pm_flags |= pmf::DUCKED;
        } else {
            stance_request(pm, world);
        }
    }

    if pm.ps.view_height_lerp_time == 0 {
        if pm.ps.pm_type == PmType::LastStand {
            pm.ps.view_height_target = VIEW_LASTSTAND;
        } else if pm.ps.pm_flags & pmf::PRONE != 0 {
            if pm.ps.view_height_target == VIEW_STAND {
                pm.ps.view_height_target = VIEW_CROUCH;
            } else if pm.ps.view_height_target != VIEW_PRONE {
                pm.ps.view_height_target = VIEW_PRONE;
                pm.prone_change = true;
                activate_slowdown(&mut pm.ps);
            }
        } else if pm.ps.view_height_target == VIEW_PRONE {
            pm.ps.view_height_target = VIEW_CROUCH;
            pm.prone_change = true;
        } else if pm.ps.pm_flags & pmf::DUCKED != 0 {
            pm.ps.view_height_target = VIEW_CROUCH;
        } else {
            pm.ps.view_height_target = VIEW_STAND;
        }
    }
    view_height_adjust(pm, pml);

    match pm.ps.stance() {
        Stance::Prone => {
            pm.maxs[2] = 30.0;
            pm.ps.e_flags |= ef::PRONE;
            pm.ps.e_flags &= !ef::CROUCH;
            pm.ps.pm_flags |= pmf::PRONE;
            pm.ps.pm_flags &= !pmf::DUCKED;
        }
        Stance::Crouch => {
            pm.maxs[2] = 50.0;
            pm.ps.e_flags |= ef::CROUCH;
            pm.ps.e_flags &= !ef::PRONE;
            pm.ps.pm_flags |= pmf::DUCKED;
            pm.ps.pm_flags &= !pmf::PRONE;
        }
        Stance::Stand => {
            pm.maxs[2] = 70.0;
            pm.ps.e_flags &= !(ef::CROUCH | ef::PRONE);
            pm.ps.pm_flags &= !(pmf::PRONE | pmf::DUCKED);
        }
    }

    if pm.ps.pm_flags & pmf::PRONE != 0 && !was_prone {
        enter_prone(pm, world);
    }
}

/// The button-driven part of `PM_CheckDuck`: crouch, prone and stand requests.
fn stance_request(pm: &mut Pmove<'_>, world: &dyn Collide) {
    let mask = pm.tracemask & MASK_IGNORE_CHARACTERS;
    if pm.ps.pm_flags & pmf::LADDER != 0 && pm.cmd.buttons & (button::PRONE | button::CROUCH) != 0 {
        pm.cmd.buttons &= !(button::PRONE | button::CROUCH);
        pm.ps.add_event(ev::STANCE_FORCE_STAND, 0);
    }
    let temp = pm.cmd.buttons & button::TEMP_STANCE != 0;
    let wants_prone = pm.cmd.buttons & button::PRONE != 0 && pm.ps.pm_flags & pmf::RESPAWNED == 0;
    if !wants_prone {
        let origin = pm.ps.origin;
        let stuck = |pm: &Pmove<'_>| {
            pm.trace(world, origin, pm.mins, pm.maxs, origin, mask)
                .all_solid
        };
        if pm.cmd.buttons & button::CROUCH != 0 {
            if pm.ps.pm_flags & pmf::PRONE != 0 {
                pm.maxs[2] = 50.0;
                if stuck(pm) {
                    if !temp {
                        pm.ps.add_event(ev::STANCE_FORCE_PRONE, 2);
                    }
                } else {
                    pm.ps.pm_flags &= !pmf::PRONE;
                    pm.ps.pm_flags |= pmf::DUCKED;
                }
            } else {
                pm.ps.pm_flags |= pmf::DUCKED;
            }
        } else if pm.ps.pm_flags & pmf::PRONE != 0 {
            if stuck(pm) {
                pm.maxs[2] = 50.0;
                if stuck(pm) {
                    if !temp {
                        pm.ps.add_event(ev::STANCE_FORCE_PRONE, 1);
                    }
                } else {
                    pm.ps.pm_flags &= !pmf::PRONE;
                    pm.ps.pm_flags |= pmf::DUCKED;
                }
            } else {
                pm.ps.pm_flags &= !(pmf::PRONE | pmf::DUCKED);
            }
        } else if pm.ps.pm_flags & pmf::DUCKED != 0 {
            if stuck(pm) {
                if !temp {
                    pm.ps.add_event(ev::STANCE_FORCE_CROUCH, 1);
                }
            } else {
                pm.ps.pm_flags &= !pmf::DUCKED;
            }
        }
    } else if prone_allowed(pm, world) {
        pm.ps.pm_flags |= pmf::PRONE;
        pm.ps.pm_flags &= !pmf::DUCKED;
    } else if pm.ps.ground_entity_num != ENTITYNUM_NONE {
        pm.ps.pm_flags |= pmf::NO_PRONE;
        if !temp {
            if pm.ps.pm_flags & (pmf::PRONE | pmf::DUCKED) != 0 {
                pm.ps.add_event(ev::STANCE_FORCE_CROUCH, 0);
            } else {
                pm.ps.add_event(ev::STANCE_FORCE_STAND, 0);
            }
        }
    }
}

/// The first step of a prone stance: nudge the body clear of the ground and fix the facing.
fn enter_prone(pm: &mut Pmove<'_>, world: &dyn Collide) {
    let mask = pm.tracemask & MASK_IGNORE_CHARACTERS;
    if pm.cmd.forwardmove != 0 || pm.cmd.rightmove != 0 {
        pm.ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
        exit_ads(&mut pm.ps);
    }
    let origin = pm.ps.origin;
    let up = [origin[0], origin[1], origin[2] + 10.0];
    let t = pm.trace(world, origin, pm.mins, pm.maxs, up, mask);
    let raised = lerp(&origin, &up, t.fraction);
    let t = pm.trace(world, raised, pm.mins, pm.maxs, origin, mask);
    pm.ps.origin = lerp(&raised, &origin, t.fraction);
    pm.ps.prone_direction = pm.ps.viewangles[1];
    let o = pm.ps.origin;
    let down = [o[0], o[1], o[2] - 0.25];
    let t = pm.trace(world, o, pm.mins, pm.maxs, down, mask);
    pm.ps.prone_direction_pitch = if t.start_solid || t.fraction >= 1.0 {
        0.0
    } else {
        pitch_for_yaw_on_normal(pm.ps.prone_direction, &t.normal)
    };
    let delta = angle_delta(pm.ps.prone_direction_pitch, pm.ps.viewangles[0]);
    pm.ps.prone_torso_pitch = if delta < -45.0 {
        pm.ps.viewangles[0] - 45.0
    } else if delta > 45.0 {
        pm.ps.viewangles[0] + 45.0
    } else {
        pm.ps.prone_direction_pitch
    };
}

/// `PM_ViewHeightAdjust`: moves the eye toward the target height.
pub(super) fn view_height_adjust(pm: &mut Pmove<'_>, pml: &Pml) {
    let time = pm.cmd.server_time;
    let ps = &mut pm.ps;
    let target = ps.view_height_target;
    if target != 0 && ps.view_height_current != 0.0 {
        if ps.view_height_current == target as f32 && ps.view_height_lerp_time == 0 {
            return;
        }
        let mut frac = 0;
        if matches!(target, VIEW_PRONE | VIEW_CROUCH | VIEW_STAND) {
            if ps.view_height_lerp_time != 0 {
                let total = lerp_time(ps.view_height_lerp_target, ps.view_height_lerp_down);
                frac = (100 * (time - ps.view_height_lerp_time) / total).clamp(0, 100);
                if frac == 100 {
                    ps.view_height_current = ps.view_height_lerp_target as f32;
                    ps.view_height_lerp_time = 0;
                } else {
                    ps.view_height_current =
                        curve(ps.view_height_lerp_target, ps.view_height_lerp_down, frac);
                }
            }
            if ps.view_height_lerp_time != 0 {
                let reversed = target != ps.view_height_lerp_target
                    && (target < ps.view_height_lerp_target && !ps.view_height_lerp_down
                        || target > ps.view_height_lerp_target && ps.view_height_lerp_down);
                if reversed {
                    frac = 100 - frac;
                    ps.view_height_lerp_down = !ps.view_height_lerp_down;
                    if ps.view_height_lerp_down {
                        if ps.view_height_lerp_target == VIEW_STAND {
                            ps.view_height_lerp_target = VIEW_CROUCH;
                        } else if ps.view_height_lerp_target == VIEW_CROUCH {
                            ps.view_height_lerp_target = VIEW_PRONE;
                        }
                    } else if ps.view_height_lerp_target == VIEW_PRONE {
                        ps.view_height_lerp_target = VIEW_CROUCH;
                    } else if ps.view_height_lerp_target == VIEW_CROUCH {
                        ps.view_height_lerp_target = VIEW_STAND;
                    }
                    if frac == 100 {
                        ps.view_height_current = ps.view_height_lerp_target as f32;
                        ps.view_height_lerp_time = 0;
                    } else {
                        let total = lerp_time(ps.view_height_lerp_target, ps.view_height_lerp_down);
                        ps.view_height_lerp_time =
                            time - (frac as f32 * 0.01 * total as f32) as i32;
                    }
                }
            } else if ps.view_height_current != target as f32 {
                ps.view_height_lerp_time = time;
                match target {
                    VIEW_PRONE => {
                        ps.view_height_lerp_down = true;
                        ps.view_height_lerp_target = if ps.view_height_current <= 40.0 {
                            VIEW_PRONE
                        } else {
                            VIEW_CROUCH
                        };
                    }
                    VIEW_CROUCH => {
                        ps.view_height_lerp_down = ps.view_height_current > target as f32;
                        ps.view_height_lerp_target = VIEW_CROUCH;
                    }
                    _ => {
                        ps.view_height_lerp_down = false;
                        ps.view_height_lerp_target = if ps.view_height_current >= 40.0 {
                            VIEW_STAND
                        } else {
                            VIEW_CROUCH
                        };
                    }
                }
            }
        } else {
            // Dead, last stand and other off-table heights glide at 180 units per second.
            ps.view_height_lerp_time = 0;
            let step = pml.frametime * 180.0;
            if ps.view_height_current >= target as f32 {
                ps.view_height_current -= step;
                if ps.view_height_current <= target as f32 {
                    ps.view_height_current = target as f32;
                }
            } else {
                ps.view_height_current += step;
                if ps.view_height_current >= target as f32 {
                    ps.view_height_current = target as f32;
                }
            }
        }
    } else if ps.pm_type == PmType::Spectator {
        ps.view_height_current = 0.0;
    } else {
        ps.view_height_current = target as f32;
    }
}

/// The eye height along the running lerp for its target and direction.
fn curve(lerp_target: i32, down: bool, frac: i32) -> f32 {
    match lerp_target {
        VIEW_PRONE => table_lerp(frac, CROUCH_PRONE),
        VIEW_CROUCH if down => table_lerp(frac, STAND_CROUCH),
        VIEW_CROUCH => table_lerp(frac, PRONE_CROUCH),
        _ => table_lerp(frac, CROUCH_STAND),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lerp_hits_waypoints_and_interpolates() {
        assert_eq!(table_lerp(0, STAND_CROUCH), 60.0);
        assert_eq!(table_lerp(30, STAND_CROUCH), 56.0);
        // Halfway between (30, 56) and (80, 44): 55 -> 56 - 12 * 25/50.
        assert!((table_lerp(55, STAND_CROUCH) - 50.0).abs() < 1e-5);
        // Between (83, 34) and (100, 40).
        assert!((table_lerp(99, PRONE_CROUCH) - (34.0 + 6.0 * 16.0 / 17.0)).abs() < 1e-4);
    }
}
