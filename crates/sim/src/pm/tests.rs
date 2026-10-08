// SPDX-License-Identifier: GPL-3.0-only
//! Movement behaviour against a hand-built box world.

use super::test_world::{SURF_CONCRETE, SURF_LADDER, SURF_MANTLEON, TestWorld};
use super::*;
use crate::contents;

/// A `Pmove` plus the last command as the client sent it: like the server, the harness feeds
/// `pmove` the previous command as `oldcmd` (`pmove` itself edits the command it is given).
struct Player<'a> {
    pm: Pmove<'a>,
    sent: UserCmd,
}

impl<'a> std::ops::Deref for Player<'a> {
    type Target = Pmove<'a>;
    fn deref(&self) -> &Self::Target {
        &self.pm
    }
}

impl std::ops::DerefMut for Player<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.pm
    }
}

fn spawn<'a>(params: &'a Params, origin: Vec3) -> Player<'a> {
    let ps = PlayerState {
        origin,
        command_time: 10_000,
        ..PlayerState::default()
    };
    Player {
        pm: Pmove::new(ps, params),
        sent: UserCmd::default(),
    }
}

/// Runs one command of `dt` ms.
fn step(p: &mut Player<'_>, world: &TestWorld, buttons: i32, fwd: i8, right: i8, dt: i32) {
    let angles = p.pm.cmd.angles;
    p.pm.cmd = UserCmd {
        buttons,
        forwardmove: fwd,
        rightmove: right,
        angles,
        ..UserCmd::default()
    };
    p.pm.cmd.server_time = p.pm.ps.command_time + dt;
    p.pm.oldcmd = p.sent;
    p.sent = p.pm.cmd;
    pmove(&mut p.pm, world);
}

fn run(p: &mut Player<'_>, world: &TestWorld, buttons: i32, fwd: i8, right: i8, ms: i32) {
    for _ in 0..ms / 20 {
        step(p, world, buttons, fwd, right, 20);
    }
}

fn hspeed(pm: &Player<'_>) -> f32 {
    math::length2(&pm.ps.velocity)
}

fn events(pm: &Player<'_>) -> Vec<(u8, u8)> {
    let n = usize::from(pm.ps.event_sequence).min(4);
    let first = pm.ps.event_sequence.wrapping_sub(n as u8);
    (0..n)
        .map(|i| {
            let slot = usize::from(first.wrapping_add(i as u8) & 3);
            (pm.ps.events[slot], pm.ps.event_parms[slot])
        })
        .collect()
}

#[test]
fn standing_player_stays_put_on_the_ground() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0, 0.0, 0.0]);
    run(&mut pm, &w, 0, 0, 0, 500);
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_WORLD);
    assert!(pm.ps.origin[2].abs() < 0.2, "z = {}", pm.ps.origin[2]);
    assert_eq!(pm.ps.velocity, [0.0; 3]);
    assert_eq!(pm.ps.command_time, 10_500);
}

#[test]
fn running_forward_reaches_the_g_speed() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 127, 0, 1000);
    assert!(
        (pm.ps.velocity[0] - 190.0).abs() < 1.5,
        "vx = {}",
        pm.ps.velocity[0]
    );
    assert!(pm.ps.velocity[1].abs() < 1e-3);
    assert!(pm.ps.origin[0] > 120.0, "ran {}", pm.ps.origin[0]);
    // Footsteps were raised while running on a stepped surface.
    assert!(
        events(&pm)
            .iter()
            .all(|e| e.0 == ev::FOOTSTEP_RUN || e.0 == 0),
        "{:?}",
        events(&pm)
    );
}

#[test]
fn diagonal_input_is_not_faster_than_straight() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 127, 127, 1500);
    // Forward and strafe at full deflection: speed is normalised, strafe scaled by 0.8.
    assert!(
        hspeed(&pm) < 190.0 && hspeed(&pm) > 150.0,
        "speed {}",
        hspeed(&pm)
    );
}

#[test]
fn backpedalling_is_slower() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, -127, 0, 1000);
    assert!(
        (pm.ps.velocity[0] + 190.0 * 0.7).abs() < 1.5,
        "vx = {}",
        pm.ps.velocity[0]
    );
}

#[test]
fn sprint_is_faster_and_runs_out() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, button::SPRINT, 127, 0, 1500);
    assert!(pm.ps.pm_flags & pmf::SPRINTING != 0);
    assert!(
        (pm.ps.velocity[0] - 190.0 * 1.5).abs() < 2.0,
        "vx = {}",
        pm.ps.velocity[0]
    );
    // The 4 s budget is gone; the held button no longer sprints until released.
    run(&mut pm, &w, button::SPRINT, 127, 0, 3000);
    assert!(pm.ps.pm_flags & pmf::SPRINTING == 0);
    assert!(pm.ps.sprint_state.sprint_button_up_required);
    assert!(
        (pm.ps.velocity[0] - 190.0).abs() < 2.0,
        "vx = {}",
        pm.ps.velocity[0]
    );
    // Released and pressed again with a refilled budget: sprint resumes.
    run(&mut pm, &w, 0, 127, 0, 6000);
    run(&mut pm, &w, button::SPRINT, 127, 0, 500);
    assert!(pm.ps.pm_flags & pmf::SPRINTING != 0);
}

#[test]
fn sprint_needs_forward_input() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, button::SPRINT, 60, 0, 600);
    assert!(pm.ps.pm_flags & pmf::SPRINTING == 0);
}

#[test]
fn crouch_lowers_eye_and_speed_then_stand_restores() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 0, 0, 100);
    assert_eq!(pm.ps.view_height_current, 60.0);
    // The eye eases down over 200 ms along the waypoint table.
    run(&mut pm, &w, button::CROUCH, 127, 0, 100);
    assert!(pm.ps.view_height_current < 60.0 && pm.ps.view_height_current > 40.0);
    run(&mut pm, &w, button::CROUCH, 127, 0, 1500);
    assert_eq!(pm.ps.view_height_current, 40.0);
    assert_eq!(pm.maxs[2], 50.0);
    assert_eq!(pm.ps.stance(), Stance::Crouch);
    assert!(
        (pm.ps.velocity[0] - 190.0 * 0.65).abs() < 2.0,
        "vx = {}",
        pm.ps.velocity[0]
    );
    run(&mut pm, &w, 0, 127, 0, 800);
    assert_eq!(pm.ps.view_height_current, 60.0);
    assert_eq!(pm.maxs[2], 70.0);
}

#[test]
fn prone_is_slow_and_low() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, button::PRONE, 0, 0, 1000);
    assert_eq!(pm.ps.stance(), Stance::Prone);
    assert_eq!(pm.ps.view_height_current, 11.0);
    assert_eq!(pm.maxs[2], 30.0);
    assert!(pm.ps.pm_flags & pmf::PRONE != 0);
    run(&mut pm, &w, button::PRONE, 127, 0, 1500);
    assert!(
        (pm.ps.velocity[0] - 190.0 * 0.15).abs() < 2.0,
        "vx = {}",
        pm.ps.velocity[0]
    );
    // Standing back up passes through crouch eye height on the way.
    run(&mut pm, &w, 0, 0, 0, 1500);
    assert_eq!(pm.ps.stance(), Stance::Stand);
    assert_eq!(pm.ps.view_height_current, 60.0);
}

#[test]
fn a_low_ceiling_keeps_the_player_crouched() {
    let mut w = TestWorld::floor();
    w.add(
        [-100.0, -100.0, 55.0],
        [100.0, 100.0, 80.0],
        contents::SOLID,
        0,
    );
    let params = Params::default();
    let mut pm = spawn(&params, [0.0; 3]);
    // Crouch first: a crouched hull (height 50) fits under the ceiling at 55.
    run(&mut pm, &w, button::CROUCH, 0, 0, 600);
    assert_eq!(pm.ps.stance(), Stance::Crouch);
    let before = pm.ps.event_sequence;
    run(&mut pm, &w, 0, 0, 0, 600);
    assert_eq!(pm.ps.stance(), Stance::Crouch, "stood up into a ceiling");
    assert!(pm.ps.event_sequence != before);
    assert!(events(&pm).contains(&(ev::STANCE_FORCE_CROUCH, 1)));
}

#[test]
fn jump_peaks_at_jump_height_and_lands() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 0, 0, 100);
    let mut apex = 0.0f32;
    step(&mut pm, &w, button::JUMP, 0, 0, 20);
    assert!(pm.ps.pm_flags & pmf::JUMPING != 0);
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_NONE);
    for _ in 0..80 {
        step(&mut pm, &w, 0, 0, 0, 20);
        apex = apex.max(pm.ps.origin[2]);
    }
    assert!((apex - 39.0).abs() < 2.5, "apex {apex}");
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_WORLD);
    // Landing from 39 units is a hard landing, not damage.
    assert!(events(&pm).iter().any(|e| e.0 == ev::LANDING_FIRST + 5));
    assert!(events(&pm).iter().all(|e| e.0 < ev::LANDING_PAIN_FIRST));
}

#[test]
fn holding_jump_does_not_bunny_hop() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 0, 0, 100);
    let mut jumps = 0;
    let mut was_airborne = false;
    for _ in 0..160 {
        step(&mut pm, &w, button::JUMP, 0, 0, 20);
        let airborne = pm.ps.ground_entity_num == crate::cm::ENTITYNUM_NONE;
        if airborne && !was_airborne {
            jumps += 1;
        }
        was_airborne = airborne;
    }
    assert_eq!(jumps, 1, "held jump re-triggered");
}

#[test]
fn falling_far_hurts_and_slows() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0, 0.0, 200.0]);
    for _ in 0..200 {
        step(&mut pm, &w, 0, 0, 0, 20);
        if pm.ps.ground_entity_num != crate::cm::ENTITYNUM_NONE {
            break;
        }
    }
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_WORLD);
    // 200 units fallen, min 128 / max 300: (200 - 128) / 172 of 100 = 41.
    let (event, damage) = events(&pm)
        .into_iter()
        .find(|e| e.0 >= ev::LANDING_PAIN_FIRST)
        .expect("pain landing");
    assert_eq!(event, ev::LANDING_PAIN_FIRST + 5);
    assert!((38..=44).contains(&damage), "damage {damage}");
    assert!(pm.ps.pm_flags & pmf::TIME_HARDLANDING != 0);
    assert_eq!(pm.ps.pm_time, 35 * i32::from(damage) + 500);
}

#[test]
fn falling_from_height_below_the_minimum_does_no_damage() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0, 0.0, 100.0]);
    for _ in 0..80 {
        step(&mut pm, &w, 0, 0, 0, 20);
    }
    assert!(events(&pm).iter().all(|e| e.0 < ev::LANDING_PAIN_FIRST));
    assert!(events(&pm).iter().any(|e| e.0 == ev::LANDING_FIRST + 5));
    assert_eq!(pm.ps.pm_flags & pmf::TIME_HARDLANDING, 0);
}

#[test]
fn steps_up_low_ledges_but_not_walls() {
    let mut w = TestWorld::floor();
    w.add(
        [60.0, -100.0, 0.0],
        [120.0, 100.0, 10.0],
        contents::SOLID,
        SURF_CONCRETE,
    );
    w.add(
        [200.0, -100.0, 0.0],
        [260.0, 100.0, 80.0],
        contents::SOLID,
        SURF_CONCRETE,
    );
    let params = Params::default();
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 127, 0, 600);
    assert!(pm.ps.origin[0] > 90.0, "stuck at {}", pm.ps.origin[0]);
    assert!(
        (pm.ps.origin[2] - 10.0).abs() < 0.5,
        "z {}",
        pm.ps.origin[2]
    );
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_WORLD);
    run(&mut pm, &w, 0, 127, 0, 2000);
    // Blocked by the 80-unit wall at x = 200 (hull half-width 15).
    assert!(pm.ps.origin[0] < 186.0, "x = {}", pm.ps.origin[0]);
    assert!(pm.ps.origin[0] > 180.0);
    assert!(
        pm.ps.origin[2] < 0.5,
        "walked off the ledge and onto the floor, z = {}",
        pm.ps.origin[2]
    );
}

#[test]
fn slides_along_a_wall() {
    let mut w = TestWorld::floor();
    w.add(
        [100.0, -500.0, 0.0],
        [140.0, 500.0, 200.0],
        contents::SOLID,
        SURF_CONCRETE,
    );
    let params = Params::default();
    let mut pm = spawn(&params, [0.0; 3]);
    // Forward plus strafe left (+Y when facing +X): +x is blocked, y keeps changing.
    run(&mut pm, &w, 0, 127, -127, 2500);
    assert!(pm.ps.origin[0] < 86.0, "x = {}", pm.ps.origin[0]);
    assert!(pm.ps.origin[1] > 80.0, "y = {}", pm.ps.origin[1]);
    assert!(pm.ps.velocity[1] > 50.0);
}

#[test]
fn entities_hit_are_reported_once() {
    let mut w = TestWorld::floor();
    w.add(
        [60.0, -100.0, 0.0],
        [90.0, 100.0, 100.0],
        contents::SOLID,
        0,
    );
    w.blocks.last_mut().unwrap().entity = 7;
    let params = Params::default();
    let mut pm = spawn(&params, [0.0; 3]);
    for _ in 0..40 {
        step(&mut pm, &w, 0, 127, 0, 20);
        if pm.num_touch > 0 {
            break;
        }
    }
    assert_eq!(pm.touched(), &[7]);
    step(&mut pm, &w, 0, 127, 0, 20);
    step(&mut pm, &w, 0, 127, 0, 20);
    assert_eq!(pm.touched().iter().filter(|&&e| e == 7).count(), 1);
}

#[test]
fn lean_ramps_and_returns() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, button::LEAN_RIGHT, 0, 0, 100);
    assert!(
        pm.ps.leanf > 0.1 && pm.ps.leanf < 0.2,
        "lean {}",
        pm.ps.leanf
    );
    run(&mut pm, &w, button::LEAN_RIGHT, 0, 0, 600);
    assert_eq!(pm.ps.leanf, 0.5);
    run(&mut pm, &w, 0, 0, 0, 400);
    assert_eq!(pm.ps.leanf, 0.0);
    run(&mut pm, &w, button::LEAN_LEFT, 0, 0, 600);
    assert_eq!(pm.ps.leanf, -0.5);
}

#[test]
fn lean_is_cut_short_by_a_wall() {
    let mut w = TestWorld::floor();
    // A pillar just right of the player's head blocks the lean sweep.
    w.add([-20.0, 12.0, 0.0], [20.0, 40.0, 100.0], contents::SOLID, 0);
    let params = Params::default();
    let mut pm = spawn(&params, [0.0, 0.0, 0.0]);
    pm.ps.viewangles = [0.0, 0.0, 0.0];
    run(&mut pm, &w, button::LEAN_RIGHT, 0, 0, 800);
    // Facing +X, "right" is -Y here and open; lean left into the pillar instead.
    run(&mut pm, &w, button::LEAN_LEFT, 0, 0, 1200);
    assert!(pm.ps.leanf > -0.5, "lean {}", pm.ps.leanf);
}

#[test]
fn a_long_gap_is_split_into_66_ms_steps() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut one = spawn(&params, [0.0; 3]);
    let mut split = spawn(&params, [0.0; 3]);
    run(&mut one, &w, 0, 0, 0, 100);
    run(&mut split, &w, 0, 0, 0, 100);
    step(&mut one, &w, 0, 127, 0, 200);
    for dt in [66, 66, 66, 2] {
        step(&mut split, &w, 0, 127, 0, dt);
    }
    assert_eq!(one.ps, split.ps);
    assert_eq!(one.ps.command_time, 10_300);
}

#[test]
fn a_stale_command_is_ignored_and_a_huge_gap_is_clamped() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 0, 0, 200);
    let before = pm.ps.clone();
    step(&mut pm, &w, 0, 127, 0, -50);
    assert_eq!(pm.ps, before);
    assert_eq!(pm.pm.num_touch, 0);
    step(&mut pm, &w, 0, 127, 0, 100_000);
    // Only the last second of the gap is simulated.
    assert_eq!(pm.ps.command_time, 10_200 + 100_000);
    assert!(pm.ps.origin[0] < 200.0, "ran {}", pm.ps.origin[0]);
}

#[test]
fn noclip_moves_through_walls_and_spectators_fly() {
    let mut w = TestWorld::floor();
    w.add(
        [60.0, -100.0, 0.0],
        [90.0, 100.0, 100.0],
        contents::SOLID,
        0,
    );
    let params = Params::default();
    let mut pm = spawn(&params, [0.0, 0.0, 10.0]);
    pm.ps.pm_type = PmType::Noclip;
    run(&mut pm, &w, 0, 127, 0, 1000);
    assert!(pm.ps.origin[0] > 150.0, "x = {}", pm.ps.origin[0]);

    let mut spec = spawn(&params, [0.0, 0.0, 300.0]);
    spec.ps.pm_type = PmType::Spectator;
    run(&mut spec, &w, 0, 0, 0, 200);
    run(&mut spec, &w, button::LEAN_RIGHT, 0, 0, 500);
    assert!(spec.ps.origin[2] > 310.0, "z = {}", spec.ps.origin[2]);
    assert_eq!(spec.ps.view_height_current, 0.0);
}

#[test]
fn a_dead_body_skids_to_a_stop() {
    let (w, params) = (TestWorld::floor(), Params::default());
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 127, 0, 600);
    assert!(hspeed(&pm) > 100.0);
    pm.ps.pm_type = PmType::Dead;
    run(&mut pm, &w, 0, 127, 0, 1000);
    assert!(hspeed(&pm) < 1.0, "speed {}", hspeed(&pm));
    assert_eq!(pm.ps.view_height_current, 8.0);
}

#[test]
fn climbs_a_ladder_and_lets_go_when_jumping() {
    let mut w = TestWorld::floor();
    w.add(
        [16.5, -50.0, 0.0],
        [20.0, 50.0, 300.0],
        contents::SOLID,
        SURF_LADDER,
    );
    let params = Params::default();
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 0, 0, 100);
    run(&mut pm, &w, 0, 127, 0, 1500);
    assert!(pm.ps.pm_flags & pmf::LADDER != 0);
    assert!(pm.ps.origin[2] > 40.0, "z = {}", pm.ps.origin[2]);
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_NONE);
    let z = pm.ps.origin[2];
    // Jumping off pushes away from the ladder.
    step(&mut pm, &w, button::JUMP, 127, 0, 20);
    assert!(pm.ps.pm_flags & pmf::LADDER == 0);
    assert!(pm.ps.velocity[0] < 0.0 || pm.ps.velocity[2] > 0.0);
    assert!(pm.ps.origin[2] >= z - 0.5);
}

fn mantle_anims() -> MantleAnims {
    let anim = |len: i32, delta: Vec3| MantleAnim {
        length_msec: len,
        samples: (0..=10)
            .map(|i| math_scale(&delta, i as f32 / 10.0))
            .collect(),
    };
    let mut set: [MantleAnim; MANTLE_ANIM_COUNT] = Default::default();
    for t in TRANSITIONS {
        set[t.up_anim] = anim(700, [16.0, 0.0, t.height]);
        set[t.over_anim] = anim(500, [31.0, 0.0, -18.0]);
    }
    MantleAnims::new(set)
}

fn math_scale(v: &Vec3, s: f32) -> Vec3 {
    [v[0] * s, v[1] * s, v[2] * s]
}

#[test]
fn mantles_onto_a_ledge() {
    let mut w = TestWorld::floor();
    w.add(
        [18.0, -100.0, 0.0],
        [120.0, 100.0, 50.0],
        contents::SOLID | contents::MANTLE,
        SURF_CONCRETE | SURF_MANTLEON,
    );
    let params = Params {
        mantle_anims: Some(mantle_anims()),
        ..Params::default()
    };
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, 0, 0, 0, 100);
    // Facing the ledge near enough, the hint shows before jump is pressed.
    step(&mut pm, &w, 0, 0, 0, 20);
    assert!(pm.ps.mantle_state.flags & 8 != 0, "no mantle hint");
    step(&mut pm, &w, button::JUMP, 0, 0, 20);
    assert!(pm.ps.pm_flags & pmf::MANTLE != 0);
    assert_eq!(
        pm.ps.mantle_state.trans_index, 1,
        "50 units is closest to the 51 animation"
    );
    let mut last_z = pm.ps.origin[2];
    for _ in 0..60 {
        step(&mut pm, &w, 0, 0, 0, 20);
        assert!(pm.ps.origin[2] >= last_z - 0.01, "went down mid-mantle");
        last_z = pm.ps.origin[2];
        if pm.ps.pm_flags & pmf::MANTLE == 0 {
            break;
        }
    }
    assert!(pm.ps.pm_flags & pmf::MANTLE == 0, "mantle never finished");
    // The climb's last velocity carries the player a little above the ledge before gravity wins.
    run(&mut pm, &w, 0, 0, 0, 600);
    assert!(
        (pm.ps.origin[2] - 50.0).abs() < 0.5,
        "z = {}",
        pm.ps.origin[2]
    );
    assert!(
        pm.ps.origin[0] > 18.0 && pm.ps.origin[0] < 120.0,
        "x = {}",
        pm.ps.origin[0]
    );
    assert_eq!(pm.ps.ground_entity_num, crate::cm::ENTITYNUM_WORLD);
}

#[test]
fn does_not_mantle_a_ledge_that_is_too_high() {
    let mut w = TestWorld::floor();
    w.add(
        [18.0, -100.0, 0.0],
        [120.0, 100.0, 130.0],
        contents::SOLID | contents::MANTLE,
        SURF_CONCRETE | SURF_MANTLEON,
    );
    let params = Params {
        mantle_anims: Some(mantle_anims()),
        ..Params::default()
    };
    let mut pm = spawn(&params, [0.0; 3]);
    run(&mut pm, &w, button::JUMP, 127, 0, 600);
    assert!(pm.ps.pm_flags & pmf::MANTLE == 0);
    assert!(pm.ps.origin[0] < 5.0);
}

#[test]
fn identical_commands_give_identical_states() {
    fn script() -> PlayerState {
        let mut w = TestWorld::floor();
        w.add(
            [100.0, -100.0, 0.0],
            [130.0, 100.0, 12.0],
            contents::SOLID,
            SURF_CONCRETE,
        );
        let params = Params::default();
        let mut pm = spawn(&params, [0.0; 3]);
        for i in 0..300 {
            let buttons = match i % 90 {
                10..=12 => button::JUMP,
                30..=60 => button::SPRINT,
                70..=75 => button::CROUCH,
                _ => 0,
            };
            pm.pm.cmd.angles = [0, (i * 37) % 65536, 0];
            step(
                &mut pm,
                &w,
                buttons,
                127,
                ((i % 7) as i8 - 3) * 20,
                8 + (i % 5) * 4,
            );
        }
        pm.pm.ps
    }
    assert_eq!(script(), script());
}

#[test]
fn event_ring_keeps_the_last_four() {
    let mut ps = PlayerState::default();
    for i in 1..=6u8 {
        ps.add_event(ev::FOOTSTEP_RUN, u32::from(i));
    }
    ps.add_event(ev::NONE, 9);
    assert_eq!(ps.event_sequence, 6);
    let mut parms = ps.event_parms;
    parms.sort_unstable();
    assert_eq!(parms, [3, 4, 5, 6]);
}
