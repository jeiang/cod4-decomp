// SPDX-License-Identifier: GPL-3.0-only
//! Trigger semantics against a hand-built map: the touch pass, queued `trigger` notifies,
//! `trigger_hurt` rate and switching, one-shot triggers and `trigger_damage` volumes.

use gsc::{Builtins, Options, Vm, compile};
use server::client::{Session, Team};
use server::combat::{
    Damage, MOD_EXPLOSIVE, MOD_GRENADE_SPLASH, MOD_MELEE, MOD_PISTOL_BULLET, MOD_RIFLE_BULLET,
};
use server::content::Content;
use server::cvar::Cvars;
use server::game::{Ent, EntKind, Game, TRIGGER_HURT_CONTENTS};
use sim::cm::test_support::{BrushSpec, MapSpec};
use sim::contents::{self, SOLID};
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType};
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
    let mut cvars = Cvars::new();
    for (n, d) in [
        ("g_maxDroppedWeapons", "16"),
        ("g_dropForwardSpeed", "10"),
        ("g_dropUpSpeedBase", "10"),
        ("g_dropUpSpeedRand", "5"),
        ("g_dropHorzSpeedRand", "100"),
        ("player_throwbackInnerRadius", "90"),
        ("player_throwbackOuterRadius", "160"),
        ("bg_maxGrenadeIndicatorSpeed", "20"),
        ("perk_grenadeDeath", "frag_grenade_mp"),
    ] {
        cvars.register(n, d, 0);
    }
    let mut g = Game::new(cvars, Content::default());
    g.reset_level(8);
    g.world = Some(World::new(map.build()));
    g.level.frametime = 50;
    g.level.time = 1000;
    (g, vm())
}

fn add_player(g: &mut Game, vm: &mut Vm, origin: [f32; 3], team: Team) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = team;
    c.ps.origin = origin;
    c.ps.viewangles = [0.0, 0.0, 0.0];
    c.ps.pm_type = PmType::Normal;
    c.ps.view_height_current = 60.0;
    c.ps.client_num = n;
    let e = g.ent_mut(n).unwrap();
    e.origin = origin;
    e.health = 100;
    e.takedamage = true;
    e.mins = PLAYER_MINS;
    e.maxs = PLAYER_MAXS;
    g.set_client_contents(n);
    g.relink(n);
    g.calls.clear();
    n
}

/// A box-shaped trigger of `class` around the origin, spawned the way the map loader does.
fn trigger(
    g: &mut Game,
    class: &str,
    spawnflags: i32,
    wait: Option<f32>,
    accumulate: i32,
    threshold: i32,
) -> u16 {
    let mut e = Ent::new(EntKind::Trigger, class);
    e.spawnflags = spawnflags;
    e.mins = [-50.0; 3];
    e.maxs = [50.0; 3];
    e.contents = match class {
        "trigger_multiple" | "trigger_once" => contents::PLAYERTRIGGER,
        _ => TRIGGER_HURT_CONTENTS,
    };
    if class == "trigger_damage" {
        e.takedamage = true;
        e.health = 32000;
    }
    let n = g.spawn(e).unwrap();
    g.init_trigger_spawn(n, wait, accumulate, threshold);
    g.relink(n);
    n
}

fn pending(g: &Game) -> usize {
    g.level.pending_triggers.len()
}

#[test]
fn a_hurt_volume_hurts_at_its_own_rate() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let fast = trigger(&mut g, "trigger_hurt", 0, None, 0, 0);
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 1);
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 1, "nothing again inside 50 ms");
    g.level.time += 50;
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 2);

    g.free_entity(&mut vm, fast);
    g.level.pending_triggers.clear();
    trigger(&mut g, "trigger_hurt", 0x10, None, 0, 0);
    g.touch_triggers(&mut vm, p);
    g.level.time += 50;
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 1, "slow volumes wait a second");
    g.level.time += 950;
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 2);
}

#[test]
fn a_hurt_volume_starts_off_switches_with_use_and_once_spends_itself() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let t = trigger(&mut g, "trigger_hurt", 1, None, 0, 0);
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 0, "START_OFF");
    g.use_trigger(&mut vm, t, p);
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 1);
    g.use_trigger(&mut vm, t, p);
    g.level.time += 50;
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 1, "switched off again");

    g.free_entity(&mut vm, t);
    g.level.pending_triggers.clear();
    trigger(&mut g, "trigger_hurt", 0x20, None, 0, 0);
    g.touch_triggers(&mut vm, p);
    g.level.time += 50;
    g.touch_triggers(&mut vm, p);
    assert_eq!(pending(&g), 1, "ONCE hurts only the first time");
}

#[test]
fn touches_of_one_trigger_are_delivered_one_per_round() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let t = trigger(&mut g, "trigger_multiple", 0, Some(1.0), 0, 0);
    g.touch_triggers(&mut vm, p);
    g.touch_triggers(&mut vm, p);
    let mut list = std::mem::take(&mut g.level.pending_triggers);
    assert_eq!(list.len(), 2);
    assert!(g.deliver_triggers(&mut vm, &mut list), "one was held back");
    assert_eq!(list.len(), 1);
    assert!(!g.deliver_triggers(&mut vm, &mut list));
    assert!(list.is_empty());
    assert!(
        g.ent(t).unwrap().free_at.is_none(),
        "a waiting trigger stays"
    );
}

#[test]
fn a_touch_of_a_freed_trigger_is_dropped() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let t = trigger(&mut g, "trigger_multiple", 0, None, 0, 0);
    g.touch_triggers(&mut vm, p);
    g.free_entity(&mut vm, t);
    let again = trigger(&mut g, "trigger_multiple", 0, Some(1.0), 0, 0);
    assert_eq!(again, t, "the slot is reused");
    let mut list = std::mem::take(&mut g.level.pending_triggers);
    assert!(!g.deliver_triggers(&mut vm, &mut list));
    assert!(list.is_empty());
}

#[test]
fn one_shot_triggers_are_freed_after_their_first_touch() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let once = trigger(&mut g, "trigger_once", 0, None, 0, 0);
    let no_wait = trigger(&mut g, "trigger_multiple", 0, Some(0.0), 0, 0);
    let repeat = trigger(&mut g, "trigger_multiple", 0, Some(2.0), 0, 0);
    g.touch_triggers(&mut vm, p);
    assert!(g.ent(once).unwrap().free_at.is_some());
    assert!(g.ent(no_wait).unwrap().free_at.is_some());
    assert!(g.ent(repeat).unwrap().free_at.is_none());
}

fn shoot(g: &mut Game, vm: &mut Vm, p: u16, mean: u8) {
    g.check_hit_trigger_damage(vm, p, [-200.0, 0.0, 0.0], [200.0, 0.0, 0.0], 10, mean);
}

#[test]
fn a_shot_through_a_damage_volume_triggers_it_unless_the_kind_is_excluded() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [1000.0, 0.0, 0.0], Team::Allies);
    let t = trigger(&mut g, "trigger_damage", 0, None, 0, 0);
    shoot(&mut g, &mut vm, p, MOD_RIFLE_BULLET);
    assert_eq!(pending(&g), 1);
    // A segment that stops short misses it.
    g.check_hit_trigger_damage(&mut vm, p, [-200.0, 0.0, 0.0], [-100.0, 0.0, 0.0], 10, 2);
    assert_eq!(pending(&g), 1);

    g.ent_mut(t).unwrap().spawnflags = 1;
    shoot(&mut g, &mut vm, p, MOD_PISTOL_BULLET);
    assert_eq!(pending(&g), 1, "NO_PISTOL");
    shoot(&mut g, &mut vm, p, MOD_RIFLE_BULLET);
    assert_eq!(pending(&g), 2);
    g.ent_mut(t).unwrap().spawnflags = 0x20;
    shoot(&mut g, &mut vm, p, MOD_MELEE);
    assert_eq!(pending(&g), 2, "NO_MELEE");
}

#[test]
fn a_damage_volume_honours_its_threshold_and_dies_once_when_it_waits_for_nobody() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [1000.0, 0.0, 0.0], Team::Allies);
    let t = trigger(&mut g, "trigger_damage", 0, Some(0.0), 0, 50);
    shoot(&mut g, &mut vm, p, MOD_RIFLE_BULLET);
    assert_eq!(pending(&g), 0, "10 is under the threshold of 50");
    g.check_hit_trigger_damage(&mut vm, p, [-200.0, 0.0, 0.0], [200.0, 0.0, 0.0], 60, 2);
    assert_eq!(pending(&g), 1);
    assert!(
        g.ent(t).unwrap().free_at.is_some(),
        "wait <= 0 is a one-shot"
    );
}

#[test]
fn explosions_reach_a_damage_volume_through_damage() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [1000.0, 0.0, 0.0], Team::Allies);
    let t = trigger(&mut g, "trigger_damage", 8, None, 0, 0);
    let blast = |g: &mut Game, vm: &mut Vm, mean| {
        let mut d = Damage::new(100, mean);
        d.attacker = Some(p);
        g.g_damage(vm, t, d);
    };
    blast(&mut g, &mut vm, MOD_GRENADE_SPLASH);
    assert_eq!(pending(&g), 0, "flag 8 ignores explosions");
    blast(&mut g, &mut vm, MOD_EXPLOSIVE);
    assert_eq!(pending(&g), 0);
    g.ent_mut(t).unwrap().spawnflags = 0;
    blast(&mut g, &mut vm, MOD_GRENADE_SPLASH);
    assert_eq!(pending(&g), 1);
    assert_eq!(g.ent(t).unwrap().health, 32000, "healed after every hit");
}

#[test]
fn a_volume_that_accumulates_waits_for_enough_damage() {
    let (mut g, mut vm) = arena();
    let p = add_player(&mut g, &mut vm, [1000.0, 0.0, 0.0], Team::Allies);
    let t = trigger(&mut g, "trigger_damage", 0, None, 150, 0);
    let hit = |g: &mut Game, vm: &mut Vm| {
        let mut d = Damage::new(100, MOD_GRENADE_SPLASH);
        d.attacker = Some(p);
        g.g_damage(vm, t, d);
    };
    hit(&mut g, &mut vm);
    assert_eq!(pending(&g), 0);
    hit(&mut g, &mut vm);
    assert_eq!(pending(&g), 1);
}

/// Whether `touch` reaches a script waiting on the entity of `class` (spawned with `flags`) when a
/// player stands in it.
fn hears_touch(class: &str, flags: i32) -> bool {
    use gsc::EntClass;
    use server::script::{Dispatch, ScriptHost};

    let script = r#"
watch() { self waittill("touch", other); level.touched = 1; }
"#;
    let prog = compile(
        &[("t.gsc", script)],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    let watch = prog.find("t", "watch").unwrap();
    let dispatch = Dispatch::new(&prog);
    let mut vm = Vm::new(prog).unwrap();
    let (mut g, _) = arena();
    let p = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let t = trigger(&mut g, class, flags, None, 0, 0);
    let mut host = ScriptHost {
        game: &mut g,
        dispatch: &dispatch,
    };
    let obj = vm.entity(t, EntClass::Entity);
    vm.call(&mut host, watch, Some(obj), &[]).unwrap();
    host.game.touch_triggers(&mut vm, p);
    assert!(vm.run_current_threads(&mut host).is_empty());
    vm.level().get("touched").is_some()
}

#[test]
fn volumes_without_a_touch_handler_still_hear_touch() {
    assert!(hears_touch("trigger_damage", 0));
    assert!(
        hears_touch("trigger_hurt", 1),
        "a switched-off trigger_hurt"
    );
    assert!(hears_touch("trigger_multiple", 0));
}
