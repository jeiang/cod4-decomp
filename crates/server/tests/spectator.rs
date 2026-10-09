// SPDX-License-Identifier: GPL-3.0-only
//! Spectators on a hand-built map: free flight runs the movement code, a followed player or a killcam holds
//! still, intermission is frozen, and melee drops out of following behind where the watched player's eyes were.

use gsc::{Builtins, Options, Vm, compile};
use server::client::{Session, Team, spec};
use server::content::Content;
use server::cvar::Cvars;
use server::game::Game;
use sim::cm::test_support::{BrushSpec, MapSpec};
use sim::contents::SOLID;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType, UserCmd, button, other};
use sim::world::World;

fn vm() -> Vm {
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    Vm::new(prog).unwrap()
}

fn arena() -> (Game, Vm) {
    let map = MapSpec {
        world: vec![BrushSpec::aabb(
            [-4000.0, -4000.0, -100.0],
            [4000.0, 4000.0, 0.0],
            SOLID,
        )],
        ..Default::default()
    };
    let mut g = Game::new(Cvars::new(), Content::default());
    g.reset_level(8);
    g.world = Some(World::new(map.build()));
    g.level.frametime = 50;
    (g, vm())
}

fn join(g: &mut Game, vm: &mut Vm, origin: [f32; 3], session: Session) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = session;
    c.team = Team::Axis;
    c.ps.origin = origin;
    c.ps.view_height_current = 60.0;
    let e = g.ent_mut(n).unwrap();
    e.origin = origin;
    e.health = 100;
    e.mins = PLAYER_MINS;
    e.maxs = PLAYER_MAXS;
    g.client_end_frame(vm, n);
    n
}

/// `frames` commands of 50 ms holding `buttons` and walking forward (`forwardmove` 127).
fn think(g: &mut Game, vm: &mut Vm, n: u16, buttons: i32, frames: usize) {
    for _ in 0..frames {
        let t = g.client(n).unwrap().ps.command_time + 50;
        g.level.time = t;
        let cmd = UserCmd {
            server_time: t,
            buttons,
            forwardmove: 127,
            ..UserCmd::default()
        };
        g.client_think(vm, n, cmd);
    }
}

#[test]
fn a_free_spectator_flies_where_it_may_and_stays_put_where_it_may_not() {
    let (mut g, mut vm) = arena();
    let flier = join(&mut g, &mut vm, [0.0, 0.0, 200.0], Session::Spectator);
    let barred = join(&mut g, &mut vm, [0.0, 0.0, 200.0], Session::Spectator);
    g.client_mut(flier).unwrap().spec_allow = spec::FREELOOK;
    for n in [flier, barred] {
        think(&mut g, &mut vm, n, 0, 10);
    }
    let moved = g.client(flier).unwrap().ps.origin;
    assert!(moved[0] > 100.0, "flew to {moved:?}");
    assert_eq!(g.client(flier).unwrap().ps.pm_type, PmType::Spectator);
    assert_eq!(g.ent(flier).unwrap().origin, moved, "the entity follows");
    assert_eq!(g.ent(flier).unwrap().contents, 0, "nothing to hit");
    assert_eq!(g.client(barred).unwrap().ps.origin, [0.0, 0.0, 200.0]);
}

#[test]
fn a_flying_spectator_is_stopped_by_the_world() {
    let (mut g, mut vm) = arena();
    let flier = join(&mut g, &mut vm, [0.0, 0.0, 30.0], Session::Spectator);
    g.client_mut(flier).unwrap().spec_allow = spec::FREELOOK;
    // Flying down into the floor stops at it.
    let c = g.client_mut(flier).unwrap();
    c.ps.viewangles = [89.0, 0.0, 0.0];
    think(&mut g, &mut vm, flier, 0, 30);
    let z = g.client(flier).unwrap().ps.origin[2];
    assert!(z > -1.0, "stopped by the floor: z = {z}");
}

#[test]
fn following_a_killcam_and_intermission_hold_the_spectator_still() {
    let (mut g, mut vm) = arena();
    let target = join(&mut g, &mut vm, [500.0, 0.0, 0.0], Session::Playing);
    let n = join(&mut g, &mut vm, [0.0, 0.0, 200.0], Session::Spectator);
    g.client_mut(n).unwrap().spec_allow = spec::FREELOOK | spec::AXIS;
    g.client_mut(n).unwrap().spectator_client = i32::from(target);
    think(&mut g, &mut vm, n, 0, 5);
    assert_eq!(
        g.client(n).unwrap().ps.origin,
        [0.0, 0.0, 200.0],
        "following"
    );
    // A killcam is not the player's to steer, however it was reached.
    let c = g.client_mut(n).unwrap();
    c.archive_time = 2.0;
    think(&mut g, &mut vm, n, button::ATTACK, 2);
    assert_eq!(g.client(n).unwrap().spectator_client, i32::from(target));
    assert_eq!(g.client(n).unwrap().ps.origin, [0.0, 0.0, 200.0], "killcam");
    let c = g.client_mut(n).unwrap();
    c.archive_time = 0.0;
    c.spectator_client = -1;
    c.session = Session::Intermission;
    think(&mut g, &mut vm, n, 0, 5);
    assert_eq!(
        g.client(n).unwrap().ps.origin,
        [0.0, 0.0, 200.0],
        "intermission"
    );
}

#[test]
fn melee_leaves_the_player_watched_and_the_prompts_follow_the_state() {
    let (mut g, mut vm) = arena();
    let target = join(&mut g, &mut vm, [500.0, 0.0, 0.0], Session::Playing);
    g.client_mut(target).unwrap().ps.viewangles = [0.0, 0.0, 0.0];
    let n = join(&mut g, &mut vm, [0.0, 0.0, 200.0], Session::Spectator);
    g.client_mut(n).unwrap().spec_allow = spec::FREELOOK | spec::AXIS;
    g.client_end_frame(&mut vm, n);
    assert_eq!(g.client(n).unwrap().ps.other_flags, other::CAN_CYCLE);
    // Attack picks the player.
    think(&mut g, &mut vm, n, button::ATTACK, 1);
    assert_eq!(g.client(n).unwrap().spectator_client, i32::from(target));
    g.client_end_frame(&mut vm, n);
    assert_eq!(
        g.client(n).unwrap().ps.other_flags,
        other::FOLLOWING | other::CAN_CYCLE | other::CAN_STOP
    );
    // Melee drops out, behind the player's eyes.
    think(&mut g, &mut vm, n, button::MELEE, 1);
    let c = g.client(n).unwrap();
    assert_eq!(c.spectator_client, -1);
    assert!(
        (c.ps.origin[0] - 460.0).abs() < 20.0 && c.ps.origin[2] > 50.0,
        "behind the eyes: {:?}",
        c.ps.origin
    );
    // Without free flight there is no way out of following.
    g.client_mut(n).unwrap().spec_allow = spec::AXIS;
    think(&mut g, &mut vm, n, 0, 1);
    think(&mut g, &mut vm, n, button::ATTACK, 1);
    assert_eq!(g.client(n).unwrap().spectator_client, i32::from(target));
    think(&mut g, &mut vm, n, 0, 1);
    think(&mut g, &mut vm, n, button::MELEE, 1);
    assert_eq!(g.client(n).unwrap().spectator_client, i32::from(target));
    g.client_end_frame(&mut vm, n);
    assert_eq!(g.client(n).unwrap().ps.other_flags & other::CAN_STOP, 0);
}
