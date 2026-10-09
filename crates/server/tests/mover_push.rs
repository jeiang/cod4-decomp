// SPDX-License-Identifier: GPL-3.0-only
//! What a script mover does to the players around it, on a hand-built map: a lift carries the player standing on it,
//! a door shoves one in its way along, and a door pinned against a wall stalls with the player where it was.

use gsc::{Builtins, EntClass, EntRef, Options, Value, Vm, compile};
use server::client::{Session, Team};
use server::content::Content;
use server::cvar::Cvars;
use server::game::{Ent, EntKind, Game};
use server::mover;
use server::netsv::world_snapshot;
use server::script::Args;
use sim::cm::test_support::{BrushSpec, MapSpec};
use sim::contents::{self, SOLID};
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType};
use sim::world::World;

const FRAME: i32 = 50;

fn vm() -> Vm {
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    Vm::new(prog).unwrap()
}

/// A floor, a wall at x = 300 and two inline models: `*1` a 128 by 128 slab, `*2` a door 20 thick.
fn arena() -> (Game, Vm) {
    let map = MapSpec {
        world: vec![
            BrushSpec::aabb([-4000.0, -4000.0, -100.0], [4000.0, 4000.0, 0.0], SOLID),
            BrushSpec::aabb([300.0, -500.0, 0.0], [400.0, 500.0, 300.0], SOLID),
        ],
        models: vec![
            vec![BrushSpec::aabb(
                [-64.0, -64.0, -16.0],
                [64.0, 64.0, 0.0],
                SOLID,
            )],
            vec![BrushSpec::aabb(
                [-10.0, -100.0, 0.0],
                [10.0, 100.0, 120.0],
                SOLID,
            )],
        ],
        ..Default::default()
    };
    let mut g = Game::new(Cvars::new(), Content::default());
    g.reset_level(8);
    g.world = Some(World::new(map.build()));
    g.level.frametime = FRAME;
    g.level.time = 1000;
    (g, vm())
}

fn player(g: &mut Game, vm: &mut Vm, origin: [f32; 3]) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = Team::Allies;
    c.ps.origin = origin;
    c.ps.pm_type = PmType::Normal;
    c.ps.client_num = n;
    let e = g.ent_mut(n).unwrap();
    e.origin = origin;
    e.health = 100;
    e.mins = PLAYER_MINS;
    e.maxs = PLAYER_MAXS;
    g.set_client_contents(n);
    g.relink(n);
    n
}

fn brush(g: &mut Game, model: &str, origin: [f32; 3]) -> u16 {
    let mut e = Ent::new(EntKind::Brush, "script_brushmodel");
    e.model = model.into();
    e.origin = origin;
    let n = g.spawn(e).unwrap();
    g.init_clip(n, None);
    g.ent_mut(n).unwrap().contents = SOLID;
    g.relink(n);
    n
}

fn moveto(g: &mut Game, n: u16, to: [f32; 3], secs: f32) {
    let me = EntRef {
        num: n,
        class: EntClass::Entity,
    };
    let args = [Value::Vector(to), Value::Float(secs)];
    mover::move_to(g, me, Args::new("moveto", &args)).unwrap();
}

fn step(g: &mut Game, vm: &mut Vm, n: u16, frames: i32) {
    for _ in 0..frames {
        g.level.time += FRAME;
        g.run_mover(vm, n);
    }
}

#[test]
fn a_lift_carries_the_player_standing_on_it() {
    let (mut g, mut vm) = arena();
    let lift = brush(&mut g, "*1", [0.0, 0.0, 100.0]);
    // Standing a little above the slab, the way the movement code rests a player on a surface.
    let p = player(&mut g, &mut vm, [0.0, 0.0, 100.125]);
    g.client_mut(p).unwrap().ps.ground_entity_num = lift;
    moveto(&mut g, lift, [0.0, 0.0, 300.0], 2.0);
    step(&mut g, &mut vm, lift, 20);
    let c = g.client(p).unwrap();
    assert!(
        (c.ps.origin[2] - 200.125).abs() < 0.01,
        "a second in, half way up: {:?}",
        c.ps.origin
    );
    assert_eq!(g.ent(p).unwrap().origin, c.ps.origin);
    assert_eq!(c.ps.ground_entity_num, lift);
    step(&mut g, &mut vm, lift, 20);
    let top = g.client(p).unwrap().ps.origin[2];
    assert!((top - 300.125).abs() < 0.01, "{top}");
}

#[test]
fn a_door_shoves_a_player_in_its_way_along() {
    let (mut g, mut vm) = arena();
    let door = brush(&mut g, "*2", [0.0, 0.0, 0.0]);
    let p = player(&mut g, &mut vm, [30.0, 0.0, 0.0]);
    moveto(&mut g, door, [100.0, 0.0, 0.0], 1.0);
    step(&mut g, &mut vm, door, 20);
    let at = g.client(p).unwrap().ps.origin;
    assert!(
        at[0] >= 100.0 + 10.0 + PLAYER_MAXS[0] - 0.01,
        "the door's face is at x = 110: {at:?}"
    );
    assert!(
        g.world
            .as_ref()
            .unwrap()
            .box_in_solid(at, PLAYER_MINS, PLAYER_MAXS, p, contents::MASK_DEADSOLID)
            .is_none(),
        "not left inside the door"
    );
}

#[test]
fn a_door_against_a_wall_stops_with_the_player_where_it_was() {
    let (mut g, mut vm) = arena();
    let door = brush(&mut g, "*2", [0.0, 0.0, 0.0]);
    // Between the door and the wall at x = 300, touching it.
    let p = player(&mut g, &mut vm, [300.0 - PLAYER_MAXS[0], 0.0, 0.0]);
    let before = g.client(p).unwrap().ps.origin;
    moveto(&mut g, door, [275.0, 0.0, 0.0], 1.0);
    step(&mut g, &mut vm, door, 20);
    let stopped = g.ent(door).unwrap().origin;
    assert!(
        stopped[0] <= 260.0 + 0.01,
        "the door did not get through: {stopped:?}"
    );
    assert_eq!(g.client(p).unwrap().ps.origin, before);
    // The clock waited: it is not done a second later, and the door has not jumped ahead.
    step(&mut g, &mut vm, door, 2);
    assert!((g.ent(door).unwrap().origin[0] - stopped[0]).abs() < 10.0);
}

#[test]
fn a_mover_goes_to_the_clients_as_a_brush_entity_with_its_velocity() {
    let (mut g, mut vm) = arena();
    let door = brush(&mut g, "*2", [0.0, 0.0, 0.0]);
    moveto(&mut g, door, [100.0, 0.0, 0.0], 1.0);
    step(&mut g, &mut vm, door, 2);
    let sent = world_snapshot(&g)
        .into_iter()
        .find(|s| s.state.number == door)
        .expect("sent")
        .state;
    assert_eq!(sent.etype, net::entity::etype::BRUSH);
    assert_eq!(sent.model, 2);
    assert!((sent.velocity[0] - 100.0).abs() < 0.5, "{:?}", sent.velocity);
    assert_ne!(sent.eflags & SOLID as u32, 0);
}
