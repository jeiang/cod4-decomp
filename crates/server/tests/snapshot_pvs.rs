// SPDX-License-Identifier: GPL-3.0-only
//! What a client is sent: nobody beyond its visibility, and what `hide` and `showtoplayer` leave of the rest.

use gsc::{Builtins, Options, Vm, compile};
use server::client::{Session, Team};
use server::content::Content;
use server::cvar::Cvars;
use server::game::{Ent, EntKind, Game};
use server::netsv::visible_to;
use sim::cm::test_support::MapSpec;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS};
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

/// Two rooms that cannot see each other, split at `x = 0`.
fn rooms() -> (Game, Vm) {
    let map = MapSpec {
        split_x: Some(0.0),
        ..Default::default()
    };
    let mut g = Game::new(Cvars::new(), Content::default());
    g.reset_level(8);
    g.world = Some(World::new(map.build()));
    (g, vm())
}

fn join(g: &mut Game, vm: &mut Vm, origin: [f32; 3]) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = Team::Axis;
    c.ps.origin = origin;
    let e = g.ent_mut(n).unwrap();
    e.origin = origin;
    e.health = 100;
    e.mins = PLAYER_MINS;
    e.maxs = PLAYER_MAXS;
    g.relink(n);
    n
}

fn numbers(v: &[net::entity::EntityState]) -> Vec<u16> {
    v.iter().map(|e| e.number).collect()
}

#[test]
fn a_player_in_the_next_room_is_not_sent_but_the_compass_still_knows_where() {
    let (mut g, mut vm) = rooms();
    let a = join(&mut g, &mut vm, [100.0, 0.0, 10.0]);
    let b = join(&mut g, &mut vm, [-100.0, 0.0, 10.0]);
    let c = join(&mut g, &mut vm, [200.0, 0.0, 10.0]);
    let (seen, actors) = visible_to(&g, a);
    assert_eq!(numbers(&seen), [a, c]);
    assert_eq!(numbers(&actors), [b]);
    assert_eq!(actors[0].origin, [-100.0, 0.0, 10.0]);
    assert_eq!(actors[0].client, b);
    // Walking through the wall: now it is the other way round.
    g.teleport(a, [-50.0, 0.0, 10.0]);
    let (seen, actors) = visible_to(&g, a);
    assert_eq!(numbers(&seen), [a, b]);
    assert_eq!(numbers(&actors), [c]);
}

#[test]
fn a_hidden_model_is_sent_only_to_those_it_was_shown_to() {
    let (mut g, mut vm) = rooms();
    let a = join(&mut g, &mut vm, [100.0, 0.0, 10.0]);
    let b = join(&mut g, &mut vm, [110.0, 0.0, 10.0]);
    let mut e = Ent::new(EntKind::Plain, "script_model");
    e.model = "prop".into();
    e.origin = [105.0, 0.0, 10.0];
    let m = g.spawn(e).unwrap();
    g.note_model("prop");
    g.relink(m);
    let sees = |g: &Game, who| numbers(&visible_to(g, who).0).contains(&m);
    assert!(sees(&g, a) && sees(&g, b));
    {
        let e = g.ent_mut(m).unwrap();
        e.hidden = true;
        e.shown_to = 1 << b;
    }
    assert!(!sees(&g, a));
    assert!(sees(&g, b));
}

#[test]
fn every_event_a_frame_raised_reaches_the_entity_and_a_script_move_flags_a_teleport() {
    let (mut g, mut vm) = rooms();
    let a = join(&mut g, &mut vm, [100.0, 0.0, 10.0]);
    let b = join(&mut g, &mut vm, [150.0, 0.0, 10.0]);
    {
        let ps = &mut g.client_mut(b).unwrap().ps;
        ps.events = [5, 6, 7, 8];
        ps.event_parms = [50, 60, 70, 80];
        // Slot 1 was raised last: the order is 6 (2), 7 (3), 8 (0 wrapped), 5 (1)... by sequence.
        ps.event_sequence = 6;
    }
    let (seen, _) = visible_to(&g, a);
    let body = seen.iter().find(|e| e.number == b).unwrap();
    // Sequences 2, 3, 4, 5 sit in slots 2, 3, 0, 1.
    assert_eq!(body.recent_events(), [(7, 70), (8, 80), (5, 50), (6, 60)]);
    let before = body.eflags & net::entity::TELEPORT_BIT;
    g.teleport(b, [160.0, 0.0, 10.0]);
    let (seen, _) = visible_to(&g, a);
    let after = seen.iter().find(|e| e.number == b).unwrap().eflags & net::entity::TELEPORT_BIT;
    assert_ne!(before, after);
}

#[test]
fn an_enemy_behind_a_wall_reaches_the_compass_only_as_far_as_the_radar_shows_it() {
    let (mut g, mut vm) = rooms();
    let a = join(&mut g, &mut vm, [100.0, 0.0, 10.0]);
    let b = join(&mut g, &mut vm, [-100.0, 0.0, 10.0]);
    g.client_mut(b).unwrap().team = Team::Allies;
    let actors = |g: &Game| numbers(&visible_to(g, a).1);
    assert!(actors(&g).is_empty(), "nothing of an enemy beyond the wall");
    assert!(!numbers(&visible_to(&g, a).0).contains(&b));
    g.client_mut(a).unwrap().ps.radar_enabled = true;
    assert_eq!(actors(&g), [b], "a UAV shows it");
    g.client_mut(a).unwrap().ps.radar_enabled = false;
    g.level.time = 10_000;
    g.client_mut(b).unwrap().last_fire_time = 9_000;
    let (_, shown) = visible_to(&g, a);
    assert_eq!(numbers(&shown), [b], "a shot shows it for a moment");
    assert_ne!(shown[0].eflags & server::netsv::eflags::PING, 0);
    g.level.time = 20_000;
    assert!(actors(&g).is_empty());
}

#[test]
fn a_plane_is_sent_to_everyone_wherever_it_flies() {
    let (mut g, mut vm) = rooms();
    let a = join(&mut g, &mut vm, [100.0, 0.0, 10.0]);
    let mut e = Ent::new(EntKind::Plain, "script_model");
    e.model = "plane".into();
    e.origin = [-500.0, 0.0, 10.0];
    e.x.plane_owner = Some(a);
    let m = g.spawn(e).unwrap();
    g.note_model("plane");
    g.relink(m);
    assert!(numbers(&visible_to(&g, a).0).contains(&m));
}
