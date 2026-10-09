// SPDX-License-Identifier: GPL-3.0-only
//! What bots are sent to walk to, against a hand-built map: a bomb pickup lying free comes
//! first, a carried bomb moves the team's zone up front, and a level without a pickup (domination,
//! deathmatch) is not a bomb level.

use gsc::{Builtins, Options, Vm, compile};
use server::activate::BombState;
use server::client::{Session, Team};
use server::content::Content;
use server::cvar::Cvars;
use server::game::{Ent, EntKind, Game};
use sim::cm::test_support::{BrushSpec, MapSpec};
use sim::contents::{self, SOLID};
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

/// A flat arena with one bot of `team`.
fn arena(team: Team) -> (Game, u16) {
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
    g.level.time = 1000;
    let mut vm = vm();
    let n = g.connect_client(&mut vm, true, "bot").expect("slot");
    g.client_begin(&mut vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = team;
    (g, n)
}

fn spawn_trigger(g: &mut Game, class: &str, name: Option<&str>, team: Team, at: [f32; 3]) -> u16 {
    let mut e = Ent::new(EntKind::Trigger, class);
    e.targetname = name.map(Into::into);
    e.x.trigger_team = team;
    e.origin = at;
    e.mins = [-30.0; 3];
    e.maxs = [30.0; 3];
    e.contents = if class.starts_with("trigger_use") {
        contents::USE
    } else {
        contents::PLAYERTRIGGER
    };
    let t = g.spawn(e).unwrap();
    g.relink(t);
    t
}

#[test]
fn a_free_bomb_pickup_comes_before_the_teams_zone() {
    let (mut g, n) = arena(Team::Allies);
    let zone = spawn_trigger(
        &mut g,
        "trigger_use_touch",
        None,
        Team::Allies,
        [500.0, 0.0, 30.0],
    );
    let pickup = spawn_trigger(
        &mut g,
        "trigger_multiple",
        Some("bomb_pickup_trig"),
        Team::Free,
        [-500.0, 0.0, 30.0],
    );
    let o = g.bot_objectives(n);
    assert_eq!(o.bomb, BombState::Free);
    assert_eq!(o.list[0].0, pickup);
    assert_eq!(
        o.n_first, 1,
        "only the pickup is favoured while it lies free"
    );
    assert_eq!(o.list[o.team.clone()][0].0, zone);
}

#[test]
fn a_carried_bomb_puts_the_teams_zone_first() {
    let (mut g, n) = arena(Team::Allies);
    let zone = spawn_trigger(
        &mut g,
        "trigger_use_touch",
        None,
        Team::Allies,
        [500.0, 0.0, 30.0],
    );
    // A carried object parks its trigger 10000 units up.
    spawn_trigger(
        &mut g,
        "trigger_multiple",
        Some("bomb_pickup_trig"),
        Team::Free,
        [-500.0, 0.0, 10030.0],
    );
    let o = g.bot_objectives(n);
    assert_eq!(o.bomb, BombState::Carried);
    assert_eq!(o.list[0].0, zone);
    assert_eq!(o.n_first, 1);
}

#[test]
fn a_level_without_a_pickup_is_not_a_bomb_level() {
    let (mut g, n) = arena(Team::Allies);
    spawn_trigger(
        &mut g,
        "trigger_use_touch",
        None,
        Team::Free,
        [500.0, 0.0, 30.0],
    );
    spawn_trigger(
        &mut g,
        "trigger_use_touch",
        None,
        Team::Allies,
        [0.0, 500.0, 30.0],
    );
    assert_eq!(g.bot_objectives(n).bomb, BombState::None);
}
