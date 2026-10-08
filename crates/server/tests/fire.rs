// SPDX-License-Identifier: GPL-3.0-or-later
//! Bullets, melee, grenades and rockets: against a hand-built map (always) and against the
//! stock mp_crash and weapon files (skipped without `COD4_PATH`).

use std::path::PathBuf;

use gsc::{Builtins, Options, Value, Vm, compile};
use server::client::{Session, Team};
use server::content::Content;
use server::cvar::Cvars;
use server::game::{Callbacks, Game};
use sim::cm::ENTITYNUM_NONE;
use sim::cm::test_support::{BrushSpec, MapSpec};
use sim::contents::SOLID;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType};
use sim::weapon::damage::PenetrationTable;
use sim::weapon::{
    InventoryType, OffhandClass, PenetrateType, WeaponClass, WeaponEvent, WeaponInfo, WeaponOut,
    WeaponParams, WeaponTable, WeaponType,
};
use sim::world::World;

const SURF_WOOD: i32 = 21 << 20;

fn vm() -> Vm {
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    Vm::new(prog).unwrap()
}

fn multipliers() -> [f32; 19] {
    let mut m = [1.0; 19];
    m[1] = 3.0;
    m[2] = 3.0;
    m[18] = 0.0;
    m
}

fn rifle() -> WeaponInfo {
    WeaponInfo {
        name: "ak47_mp".into(),
        weap_type: WeaponType::Bullet,
        weap_class: WeaponClass::Rifle,
        damage: 40,
        min_damage: 40,
        max_damage_range: 500.0,
        min_damage_range: 1000.0,
        rifle_bullet: true,
        penetrate_type: PenetrateType::Small,
        location_damage_multipliers: multipliers(),
        ..WeaponInfo::default()
    }
}

fn shotgun() -> WeaponInfo {
    WeaponInfo {
        name: "winchester_mp".into(),
        weap_class: WeaponClass::Spread,
        shot_count: 8,
        damage: 20,
        min_damage: 20,
        max_damage_range: 100.0,
        min_damage_range: 600.0,
        rifle_bullet: false,
        penetrate_type: PenetrateType::None,
        ..rifle()
    }
}

fn knife() -> WeaponInfo {
    WeaponInfo {
        name: "knife_mp".into(),
        melee_damage: 135,
        ..rifle()
    }
}

fn frag() -> WeaponInfo {
    WeaponInfo {
        name: "frag_grenade_mp".into(),
        weap_type: WeaponType::Grenade,
        weap_class: WeaponClass::Grenade,
        inventory_type: InventoryType::Offhand,
        offhand_class: OffhandClass::Frag,
        fuse_time: 3500,
        timed_detonation: true,
        projectile_speed: 600,
        explosion_radius: 2000,
        explosion_inner_damage: 130,
        explosion_outer_damage: 25,
        damage_cone_angle: 360.0,
        parallel_bounce: [0.5; 29],
        perpendicular_bounce: [0.4; 29],
        ..WeaponInfo::default()
    }
}

fn c4() -> WeaponInfo {
    WeaponInfo {
        name: "c4_mp".into(),
        timed_detonation: false,
        has_detonator: true,
        stickiness: 1,
        explosion_radius: 500,
        explosion_inner_damage: 200,
        explosion_outer_damage: 50,
        offhand_class: OffhandClass::None,
        ..frag()
    }
}

fn rpg() -> WeaponInfo {
    WeaponInfo {
        name: "rpg_mp".into(),
        weap_type: WeaponType::Projectile,
        weap_class: WeaponClass::RocketLauncher,
        projectile_speed: 1000,
        proj_impact_explode: true,
        proj_lifetime: 10.0,
        damage: 100,
        explosion_radius: 300,
        explosion_inner_damage: 150,
        explosion_outer_damage: 50,
        damage_cone_angle: 360.0,
        ..WeaponInfo::default()
    }
}

/// An empty map: a floor with its top at z = 0, `walls` as extra boxes.
fn arena(
    walls: &[([f32; 3], [f32; 3])],
    surface_flags: i32,
    weapons: Vec<WeaponInfo>,
) -> (Game, Vm) {
    let mut world = vec![BrushSpec::aabb(
        [-4000.0, -4000.0, -100.0],
        [4000.0, 4000.0, 0.0],
        SOLID,
    )];
    for (lo, hi) in walls {
        world.push(BrushSpec::aabb(*lo, *hi, SOLID));
    }
    let map = MapSpec {
        world,
        surface_flags: Some(surface_flags),
        ..Default::default()
    };
    let mut g = Game::new(Cvars::new(), Content::default());
    g.reset_level(8);
    g.world = Some(World::new(map.build()));
    g.weapons = WeaponTable::from_infos(weapons).unwrap();
    g.callbacks = Callbacks {
        player_damage: Some(1),
        ..Callbacks::default()
    };
    g.level.frametime = 5;
    (g, vm())
}

fn add_player(g: &mut Game, vm: &mut Vm, origin: [f32; 3], yaw: f32, team: Team) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = team;
    c.ps.origin = origin;
    c.ps.viewangles = [0.0, yaw, 0.0];
    c.ps.pm_type = PmType::Normal;
    c.ps.view_height_current = 60.0;
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

/// What the damage callback of `victim` was called with: `(damage, flags, mean, hitloc)`.
fn damage_calls(g: &Game, victim: u16) -> Vec<(i32, i32, String, String)> {
    let text = |v: &Value| match v {
        Value::Str(s) => s.to_string(),
        o => panic!("not a string: {o:?}"),
    };
    let int = |v: &Value| match v {
        Value::Int(i) => *i,
        o => panic!("not an int: {o:?}"),
    };
    g.calls
        .iter()
        .filter(|c| c.this == Some(victim))
        .map(|c| {
            (
                int(&c.args[2]),
                int(&c.args[3]),
                text(&c.args[4]),
                text(&c.args[8]),
            )
        })
        .collect()
}

fn fire(g: &mut Game, vm: &mut Vm, shooter: u16, event: WeaponEvent) {
    let mut out = WeaponOut::default();
    out.push(event);
    g.weapon_events(vm, shooter, &out);
}

fn fire_weapon(g: &mut Game, vm: &mut Vm, shooter: u16, name: &str) {
    let weapon = g.weapons.index(name);
    g.client_mut(shooter).unwrap().ps.weapon = u32::from(weapon);
    fire(
        g,
        vm,
        shooter,
        WeaponEvent::Fire {
            weapon,
            shot: 1,
            first: true,
            ads: false,
            burst: false,
            last_round: false,
        },
    );
}

#[test]
fn a_rifle_shot_at_the_head_hits_the_head_for_triple_damage() {
    let (mut g, mut vm) = arena(&[], 0, vec![rifle()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let victim = add_player(&mut g, &mut vm, [40.0, 0.0, 0.0], 180.0, Team::Axis);
    // The eye is at the top of the box, where the stance box calls a head.
    g.client_mut(shooter).unwrap().ps.view_height_current = 66.0;
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    let calls = damage_calls(&g, victim);
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (damage, flags, mean, hitloc) = &calls[0];
    assert_eq!((*damage, *flags), (120, 0));
    assert_eq!(
        (mean.as_str(), hitloc.as_str()),
        ("MOD_RIFLE_BULLET", "head")
    );
    assert_eq!((g.stats.shots, g.stats.hits), (1, 1));
}

#[test]
fn firing_tells_clients_who_fired_what_from_where() {
    use server::tempev::ev;
    let (mut g, mut vm) = arena(&[], 0, vec![rifle()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    g.client_mut(shooter).unwrap().ps.view_height_current = 66.0;
    g.client_mut(shooter).unwrap().ps.viewangles = [0.0, 90.0, 0.0];
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    let now = g.level.time;
    let fired: Vec<_> = g
        .tempev
        .live(now)
        .filter(|e| e.event == ev::WEAPON_FIRE)
        .collect();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].client, shooter);
    assert_eq!(fired[0].weapon, g.weapons.index("ak47_mp"));
    assert_eq!(fired[0].origin[2], 66.0);
    assert!((fired[0].angles[1] - 90.0).abs() < 0.5);
}

#[test]
fn a_shot_into_the_legs_is_not_a_headshot_and_a_miss_hurts_nobody() {
    let (mut g, mut vm) = arena(&[], 0, vec![rifle()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let victim = add_player(&mut g, &mut vm, [40.0, 0.0, 0.0], 180.0, Team::Axis);
    g.client_mut(shooter).unwrap().ps.view_height_current = 10.0;
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    let calls = damage_calls(&g, victim);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, 40);
    assert!(
        calls[0].3.contains("leg") || calls[0].3.contains("foot"),
        "{calls:?}"
    );
    g.calls.clear();
    g.client_mut(shooter).unwrap().ps.viewangles = [0.0, 90.0, 0.0];
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    assert!(g.calls.is_empty());
}

/// A rifle whose hip spread is `hip` degrees in every stance and whose aimed spread is `ads`.
fn spread_rifle(hip: f32, ads: f32) -> WeaponInfo {
    WeaponInfo {
        hip_spread_stand_min: hip,
        hip_spread_stand_max: hip,
        hip_spread_ducked_min: hip,
        hip_spread_ducked_max: hip,
        hip_spread_prone_min: hip,
        hip_spread_prone_max: hip,
        ads_spread: ads,
        ..rifle()
    }
}

/// Fires `shots` rounds from a fixed eye straight at a standing target 300 units away and returns how many hit.
/// The aim never moves and the target never moves, so the only thing between a shot and a hit is the weapon's spread.
fn rounds_that_hit(hip: f32, ads: f32, aimed: bool, shots: i32) -> (u64, u64) {
    let (mut g, mut vm) = arena(&[], 0, vec![spread_rifle(hip, ads)]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    add_player(&mut g, &mut vm, [300.0, 0.0, 0.0], 180.0, Team::Axis);
    let c = g.client_mut(shooter).unwrap();
    // Eye level with the middle of the target's body.
    c.ps.view_height_current = 35.0;
    c.ps.aim_spread_scale = 0.0;
    c.ps.weapon_pos_frac = if aimed { 1.0 } else { 0.0 };
    for i in 0..shots {
        // The game seeds each shot from the server time, which moves between shots.
        g.level.time = 1000 + i * 50;
        fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    }
    (g.stats.shots, g.stats.hits)
}

#[test]
fn aimed_fire_at_a_standing_target_hits_every_time() {
    // 0.5 degrees is 2.6 units at 300: well inside the body.
    assert_eq!(rounds_that_hit(8.0, 0.5, true, 200), (200, 200));
    // A tight hip spread (2 degrees, 10 units) also stays on the torso.
    assert_eq!(rounds_that_hit(2.0, 0.5, false, 200), (200, 200));
}

#[test]
fn a_wide_spread_misses_some_shots_but_not_all() {
    // 10 degrees is 53 units at 300: a 30-unit wide body takes a fair share, not everything.
    let (shots, hits) = rounds_that_hit(10.0, 0.5, false, 400);
    assert_eq!(shots, 400);
    assert!(
        (40..=280).contains(&hits),
        "{hits} of {shots} hit with a spread of 10 degrees"
    );
    // Aiming down the sights tightens the same weapon to a sure hit.
    assert_eq!(rounds_that_hit(10.0, 0.5, true, 100), (100, 100));
}

#[test]
fn a_shot_is_judged_against_the_bodies_as_the_shooter_saw_them() {
    use server::lagcomp::Sample;
    let (mut g, mut vm) = arena(&[], 0, vec![rifle()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let victim = add_player(&mut g, &mut vm, [40.0, 300.0, 0.0], 180.0, Team::Axis);
    g.client_mut(shooter).unwrap().ps.view_height_current = 66.0;
    // The victim stood in the line of fire 100 ms ago and has since stepped out of it.
    for (time, y) in [(900, 0.0), (1000, 0.0), (1100, 300.0)] {
        g.lag.record(
            victim,
            Sample {
                time,
                origin: [40.0, y, 0.0],
                mins: PLAYER_MINS,
                maxs: PLAYER_MAXS,
                pose: Default::default(),
            },
        );
    }
    g.level.time = 1100;
    // Judged live, the shot misses.
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    assert!(damage_calls(&g, victim).is_empty());
    // Judged where the shooter saw the victim, it hits.
    g.lag_time = Some(1000);
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    g.lag_time = None;
    assert_eq!(damage_calls(&g, victim).len(), 1);
    // A shooter cannot reach further back than the cap: 1100 - 250 is long after he stood there.
    g.calls.clear();
    g.level.time = 1500;
    g.lag_time = Some(900);
    g.lag.record(
        victim,
        Sample {
            time: 1500,
            origin: [40.0, 300.0, 0.0],
            mins: PLAYER_MINS,
            maxs: PLAYER_MAXS,
            pose: Default::default(),
        },
    );
    fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
    g.lag_time = None;
    assert!(damage_calls(&g, victim).is_empty());
}

#[test]
fn bullets_pass_thin_walls_at_a_cost_and_stop_at_thick_ones() {
    let table = PenetrationTable::parse("BULLET_PEN_TABLE\\small_wood\\8\\medium_wood\\16");
    assert_eq!(table.depth(PenetrateType::Small, 21), 8.0);
    let thin = ([100.0, -500.0, 0.0], [104.0, 500.0, 500.0]);
    let thick = ([100.0, -500.0, 0.0], [120.0, 500.0, 500.0]);
    let run = |wall, weapon: WeaponInfo| {
        let (mut g, mut vm) = arena(&[wall], SURF_WOOD, vec![weapon]);
        let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
        let victim = add_player(&mut g, &mut vm, [140.0, 0.0, 0.0], 180.0, Team::Axis);
        g.penetration = Some(std::sync::Arc::new(table.clone()));
        // Torso height: neither head nor legs.
        g.client_mut(shooter).unwrap().ps.view_height_current = 45.0;
        fire_weapon(&mut g, &mut vm, shooter, "ak47_mp");
        damage_calls(&g, victim)
    };
    // 4 units of a wood that takes 8 (the trace stops an eighth of a unit short of each
    // face): about half the damage, flagged as penetration.
    let calls = run(thin, rifle());
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!((17..=19).contains(&calls[0].0), "{calls:?}");
    assert_eq!(calls[0].1, 8);
    // 20 units are more than the bullet can cross.
    assert!(run(thick, rifle()).is_empty());
    // A weapon that cannot penetrate stops at any wall.
    let mut solid = rifle();
    solid.penetrate_type = PenetrateType::None;
    assert!(run(thin, solid).is_empty());
}

#[test]
fn every_shotgun_pellet_is_a_trace() {
    let (mut g, mut vm) = arena(&[], 0, vec![shotgun()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let victim = add_player(&mut g, &mut vm, [30.0, 0.0, 0.0], 180.0, Team::Axis);
    g.client_mut(shooter).unwrap().ps.view_height_current = 40.0;
    fire_weapon(&mut g, &mut vm, shooter, "winchester_mp");
    // Zero spread at point-blank: the whole charge lands.
    let calls = damage_calls(&g, victim);
    assert_eq!(calls.len(), 8, "{calls:?}");
    assert!(
        calls
            .iter()
            .all(|c| c.2 == "MOD_PISTOL_BULLET" && c.0 == 20)
    );
}

#[test]
fn the_knife_reaches_a_short_way() {
    let (mut g, mut vm) = arena(&[], 0, vec![knife()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let near = add_player(&mut g, &mut vm, [45.0, 0.0, 0.0], 180.0, Team::Axis);
    let far = add_player(&mut g, &mut vm, [0.0, 200.0, 0.0], 180.0, Team::Axis);
    g.client_mut(shooter).unwrap().ps.view_height_current = 40.0;
    let weapon = g.weapons.index("knife_mp");
    fire(&mut g, &mut vm, shooter, WeaponEvent::Melee { weapon });
    let calls = damage_calls(&g, near);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].2, "MOD_MELEE");
    assert!((135..=139).contains(&calls[0].0), "{calls:?}");
    assert!(damage_calls(&g, far).is_empty());
    // Turned the other way there is nothing within reach.
    g.calls.clear();
    g.client_mut(shooter).unwrap().ps.viewangles = [0.0, 270.0, 0.0];
    fire(&mut g, &mut vm, shooter, WeaponEvent::Melee { weapon });
    assert!(g.calls.is_empty());
}

/// Runs `g` in 5 ms frames until entity `n` is gone; returns the level time then and the
/// heights it flew at.
fn fly(g: &mut Game, vm: &mut Vm, n: u16, from: i32, to: i32) -> (Option<i32>, Vec<f32>) {
    let mut heights = Vec::new();
    for t in (from..to).step_by(5) {
        g.level.time = t;
        g.run_entity(vm, n);
        match g.ent(n) {
            Some(e) => heights.push(e.origin[2]),
            None => return (Some(t), heights),
        }
    }
    (None, heights)
}

#[test]
fn a_thrown_grenade_bounces_and_goes_off_when_the_fuse_ends() {
    // A wall behind the thrower shields the player on its far side.
    let wall = ([-52.0, -2000.0, 0.0], [-48.0, 2000.0, 1000.0]);
    let (mut g, mut vm) = arena(&[wall], 0, vec![frag()]);
    let thrower = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let near = add_player(&mut g, &mut vm, [250.0, 100.0, 0.0], 0.0, Team::Axis);
    let hidden = add_player(&mut g, &mut vm, [-100.0, 0.0, 0.0], 0.0, Team::Axis);
    g.level.time = 1000;
    let weapon = g.weapons.index("frag_grenade_mp");
    fire(
        &mut g,
        &mut vm,
        thrower,
        WeaponEvent::OffhandThrow {
            weapon,
            fuse_left: 3500,
            cooked: 0,
        },
    );
    let grenade = server_first_missile(&g);
    let (gone, heights) = fly(&mut g, &mut vm, grenade, 1005, 6000);
    let gone = gone.expect("the grenade exploded");
    assert!((4500..=4505).contains(&gone), "exploded at {gone}");
    // It fell to the floor, came back up, and finally lay still.
    let first_floor = heights
        .iter()
        .position(|z| *z < 4.0)
        .expect("it reached the floor");
    assert!(
        heights[first_floor..].windows(2).any(|w| w[1] > w[0] + 0.3),
        "no bounce: {:?}",
        &heights[first_floor..first_floor + 20.min(heights.len() - first_floor)]
    );
    assert!(heights.last().is_some_and(|z| *z < 4.0));
    // The player in the open was hurt, the one behind the wall was not.
    assert!(!damage_calls(&g, near).is_empty());
    assert!(!damage_calls(&g, thrower).is_empty());
    assert!(
        damage_calls(&g, hidden).is_empty(),
        "{:?}",
        damage_calls(&g, hidden)
    );
    let (damage, flags, mean, _) = &damage_calls(&g, near)[0];
    assert_eq!((*flags, mean.as_str()), (1 | 4, "MOD_GRENADE_SPLASH"));
    assert!((25..=130).contains(damage), "{damage}");
}

#[test]
fn c4_sticks_where_it_lands_and_waits_to_be_detonated() {
    let (mut g, mut vm) = arena(&[], 0, vec![c4()]);
    let planter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let victim = add_player(&mut g, &mut vm, [200.0, 0.0, 0.0], 0.0, Team::Axis);
    g.level.time = 1000;
    let weapon = g.weapons.index("c4_mp");
    fire(
        &mut g,
        &mut vm,
        planter,
        WeaponEvent::OffhandThrow {
            weapon,
            fuse_left: 0,
            cooked: 0,
        },
    );
    let c4 = server_first_missile(&g);
    // No fuse: after twenty seconds it is still there, at rest on the floor.
    let (gone, _) = fly(&mut g, &mut vm, c4, 1005, 21_000);
    assert_eq!(gone, None);
    let e = g.ent(c4).unwrap();
    let m = e.missile.as_ref().unwrap();
    assert_eq!(m.pos.kind, sim::traj::TrType::Stationary);
    assert!(
        m.surface_normal[2] > 0.99 && e.origin[2] < 3.0,
        "{:?}",
        e.origin
    );
    assert!(g.calls.is_empty());
    // `detonate` sets it off; the player in the radius is hurt.
    g.detonate_missile(&mut vm, c4);
    assert!(g.ent(c4).is_none());
    assert!(!damage_calls(&g, victim).is_empty());
}

fn server_first_missile(g: &Game) -> u16 {
    g.in_use()
        .find(|(_, e)| e.missile.is_some())
        .map(|(n, _)| n)
        .expect("a missile in flight")
}

#[test]
fn a_rocket_hurts_what_it_hits_once_and_the_neighbours_by_the_blast() {
    let (mut g, mut vm) = arena(&[], 0, vec![rpg()]);
    let shooter = add_player(&mut g, &mut vm, [0.0; 3], 0.0, Team::Allies);
    let hit = add_player(&mut g, &mut vm, [400.0, 0.0, 0.0], 180.0, Team::Axis);
    let beside = add_player(&mut g, &mut vm, [400.0, 100.0, 0.0], 180.0, Team::Axis);
    g.client_mut(shooter).unwrap().ps.view_height_current = 40.0;
    g.level.time = 1000;
    fire_weapon(&mut g, &mut vm, shooter, "rpg_mp");
    let rocket = server_first_missile(&g);
    let (gone, _) = fly(&mut g, &mut vm, rocket, 1005, 3000);
    let gone = gone.expect("the rocket exploded");
    // 400 units at 1000 u/s: about 0.4 s.
    assert!((1380..=1480).contains(&gone), "{gone}");
    let direct = damage_calls(&g, hit);
    assert_eq!(direct.len(), 1, "{direct:?}");
    assert_eq!(direct[0].2, "MOD_PROJECTILE");
    let splash = damage_calls(&g, beside);
    assert_eq!(splash.len(), 1, "{splash:?}");
    assert_eq!(splash[0].2, "MOD_PROJECTILE_SPLASH");
    // The shooter was a full 400 units away: outside the 300 unit blast.
    assert!(damage_calls(&g, shooter).is_empty());
    assert_eq!(WeaponParams::default().melee_range, 64.0);
}

// ---- the stock install --------------------------------------------------------------------

fn crash() -> Option<server::server::Server> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    let args: Vec<String> = ["+set", "net_port", "0", "+map", "mp_crash"]
        .map(String::from)
        .to_vec();
    Some(server::server::Server::boot(&root, &args, false).expect("boot"))
}

/// A standing player on the floor of mp_crash at a TDM spawn, nudged by `dx`.
fn stock_player(g: &mut Game, vm: &mut Vm, dx: f32, dy: f32, yaw: f32) -> u16 {
    let clip = g.content.clipmap().expect("clipmap").clone();
    let text = &clip.map_ents.as_ref().expect("entities").entity_string;
    let spawns = server::game::parse_spawn_vars(text).unwrap();
    let spawn = spawns
        .iter()
        .find(|v| server::game::spawn_var(v, "classname") == Some("mp_tdm_spawn"))
        .expect("a tdm spawn");
    let o = server::game::parse_vec3(server::game::spawn_var(spawn, "origin").unwrap());
    let n = add_player(g, vm, [o[0] + dx, o[1] + dy, o[2]], yaw, Team::Axis);
    g.ent_mut(n).unwrap().model = "body_mp_usmc_assault".into();
    n
}

#[test]
fn stock_aimed_rifle_fire_at_a_standing_player_hits_the_body_every_time() {
    let Some(mut s) = crash() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let mut vm = vm();
    let g = &mut s.game;
    g.callbacks = Callbacks {
        player_damage: Some(1),
        ..Callbacks::default()
    };
    g.level.frametime = 33;
    // Level, open floor: a spawn and a direction with 200 clear units behind it for the shooter to stand on.
    let clip = g.content.clipmap().expect("clipmap").clone();
    let text = &clip.map_ents.as_ref().expect("entities").entity_string;
    let world = g.world.as_ref().expect("world");
    let lift = |p: [f32; 3], z: f32| [p[0], p[1], p[2] + z];
    let floor = |p: [f32; 3]| {
        let t = world.bullet_trace(lift(p, 20.0), lift(p, -20.0), ENTITYNUM_NONE, SOLID);
        t.fraction < 1.0 && (t.fraction * 40.0 - 20.0).abs() < 12.0
    };
    let (o, stand, yaw) = server::game::parse_spawn_vars(text)
        .unwrap()
        .iter()
        .filter(|v| server::game::spawn_var(v, "classname") == Some("mp_tdm_spawn"))
        .map(|v| server::game::parse_vec3(server::game::spawn_var(v, "origin").unwrap()))
        .find_map(|o| {
            (0..24).map(|k| k as f32 * 15.0).find_map(|yaw| {
                let (s, c) = yaw.to_radians().sin_cos();
                let from = [o[0] - c * 200.0, o[1] - s * 200.0, o[2]];
                let clear = world
                    .bullet_trace(lift(from, 45.0), lift(o, 45.0), ENTITYNUM_NONE, SOLID)
                    .fraction
                    >= 1.0;
                (clear && floor(from)).then_some((o, from, yaw))
            })
        })
        .expect("a spawn with open floor 200 units off");
    let victim = add_player(g, &mut vm, o, yaw + 180.0, Team::Axis);
    g.ent_mut(victim).unwrap().model = "body_mp_usmc_assault".into();
    let shooter = add_player(g, &mut vm, stand, yaw, Team::Allies);
    for t in 0..30 {
        g.level.time = 1000 + t * 33;
        g.client_end_frame(&mut vm, victim);
    }
    g.ensure_player_anims();
    // The eye level with the chest of the standing body, aimed down the sights.
    let c = g.client_mut(shooter).unwrap();
    c.ps.view_height_current = 45.0;
    c.ps.aim_spread_scale = 0.0;
    c.ps.weapon_pos_frac = 1.0;
    let shots = 60;
    for i in 0..shots {
        g.level.time = 2000 + i * 50;
        fire_weapon(g, &mut vm, shooter, "ak47_mp");
    }
    let calls = damage_calls(g, victim);
    assert_eq!(
        (g.stats.shots, g.stats.hits, calls.len() as u64),
        (shots as u64, shots as u64, shots as u64),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|c| c.3.contains("torso") || c.3.contains("neck")),
        "an aimed shot at the chest hit {:?}",
        calls.iter().map(|c| c.3.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn stock_sniper_headshot_kills_with_default_damage() {
    let Some(mut s) = crash() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let mut vm = vm();
    let g = &mut s.game;
    g.callbacks = Callbacks {
        player_damage: Some(1),
        ..Callbacks::default()
    };
    g.level.frametime = 33;
    let victim = stock_player(g, &mut vm, 0.0, 0.0, 180.0);
    let o = g.ent(victim).unwrap().origin;
    let shooter = add_player(g, &mut vm, [o[0] - 10.0, o[1], o[2]], 0.0, Team::Allies);
    // Let the victim's body settle into its standing animation.
    for t in 0..30 {
        g.level.time = 1000 + t * 33;
        g.client_end_frame(&mut vm, victim);
    }
    g.ensure_player_anims();
    let anims = g.player_anims.clone().expect("player skeleton");
    let mut pose = sim::skel::Pose::default();
    g.client(victim).unwrap().pose.pose(&anims, &mut pose);
    let head = pose.bones[anims.rig().bone_index("j_head").unwrap()].trans;
    // The head, 10 units in front: the shooter's eye is level with the head bone and the
    // victim faces the shooter.
    let eye = sim::skel::Pose::to_world(&head, &o, 180.0);
    g.client_mut(shooter).unwrap().ps.view_height_current = eye[2] - o[2];
    g.client_mut(shooter).unwrap().ps.origin[1] = eye[1];
    g.ent_mut(shooter).unwrap().origin[1] = eye[1];
    g.client_mut(shooter).unwrap().ps.origin[0] = eye[0] - 10.0;
    g.ent_mut(shooter).unwrap().origin[0] = eye[0] - 10.0;
    g.relink(shooter);
    g.level.time = 2000;
    // The sniper rifle: 70 damage, 1.5 to the head.
    fire_weapon(g, &mut vm, shooter, "m40a3_mp");
    let calls = damage_calls(g, victim);
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (damage, _, mean, loc) = &calls[0];
    assert_eq!(mean, "MOD_RIFLE_BULLET");
    assert!(loc == "head" || loc == "helmet", "hit the {loc}");
    assert!(*damage >= 100, "{damage} would not kill");
    // The script accepts the damage; the player dies.
    let call = g.calls.iter().find(|c| c.this == Some(victim)).unwrap();
    let Value::Int(amount) = call.args[2] else {
        panic!()
    };
    let mut d = server::combat::Damage::new(amount, server::combat::MOD_RIFLE_BULLET);
    d.attacker = Some(shooter);
    g.finish_player_damage(&mut vm, victim, d).unwrap();
    assert_eq!(g.client(victim).unwrap().ps.pm_type, PmType::Dead);
    assert_eq!(g.stats.kills, 1);
}

#[test]
fn stock_frag_grenade_bounces_and_explodes_at_its_fuse() {
    let Some(mut s) = crash() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let mut vm = vm();
    let g = &mut s.game;
    g.callbacks = Callbacks {
        player_damage: Some(1),
        ..Callbacks::default()
    };
    g.level.frametime = 5;
    let thrower = stock_player(g, &mut vm, 0.0, 0.0, 0.0);
    let o = g.ent(thrower).unwrap().origin;
    let victim = add_player(
        g,
        &mut vm,
        [o[0] + 60.0, o[1] + 30.0, o[2]],
        0.0,
        Team::Allies,
    );
    g.client_mut(thrower).unwrap().team = Team::Axis;
    let weapon = g.weapons.index("frag_grenade_mp");
    let fuse = g.weapons.info(weapon).fuse_time;
    g.client_mut(thrower).unwrap().ps.viewangles = [60.0, 0.0, 0.0];
    g.level.time = 1000;
    fire(
        g,
        &mut vm,
        thrower,
        WeaponEvent::OffhandThrow {
            weapon,
            fuse_left: fuse,
            cooked: 0,
        },
    );
    let grenade = server_first_missile(g);
    let (gone, heights) = fly(g, &mut vm, grenade, 1005, 1000 + fuse + 500);
    let gone = gone.expect("exploded");
    assert!((1000 + fuse..=1000 + fuse + 5).contains(&gone), "{gone}");
    assert!(heights.iter().any(|z| *z < o[2] + 20.0));
    assert!(
        !damage_calls(g, victim).is_empty(),
        "the blast reached the neighbour"
    );
}

#[test]
fn stock_shotgun_fires_its_pellets() {
    let Some(mut s) = crash() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let mut vm = vm();
    let g = &mut s.game;
    g.callbacks = Callbacks {
        player_damage: Some(1),
        ..Callbacks::default()
    };
    let victim = stock_player(g, &mut vm, 0.0, 0.0, 180.0);
    let o = g.ent(victim).unwrap().origin;
    let shooter = add_player(g, &mut vm, [o[0] - 20.0, o[1], o[2]], 0.0, Team::Allies);
    g.client_mut(shooter).unwrap().ps.view_height_current = 40.0;
    let name = "winchester1200_mp";
    let weapon = g.weapons.index(name);
    assert_ne!(weapon, 0, "stock shotgun missing");
    let pellets = g.weapons.info(weapon).shot_count as usize;
    assert!(pellets > 1);
    fire_weapon(g, &mut vm, shooter, name);
    // At 20 units a skeleton hit box is wide enough for most of the spread, never for more
    // than the charge.
    let hits = damage_calls(g, victim).len();
    assert!(
        hits >= pellets / 2 && hits <= pellets,
        "{hits} of {pellets}"
    );
}
