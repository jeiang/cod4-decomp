// SPDX-License-Identifier: GPL-3.0-only
//! The stock `misc_turret`s of mp_convoy (two `saw_bipod_stand_mp` guns): a player behind one mounts it with use,
//! is held in its arcs, fires it, and gets off again. Skips without `COD4_PATH`.

use std::path::PathBuf;

use gsc::{Builtins, Options, Vm, compile};
use server::client::{Session, Team};
use server::game::Game;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents::MASK_PLAYERSOLID;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType, UserCmd, button, ef};
use sim::weapon::pickup::WEAPON_HINT_OFFSET;

fn vm() -> Vm {
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    Vm::new(prog).unwrap()
}

fn add_player(g: &mut Game, vm: &mut Vm, origin: [f32; 3], yaw: f32) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = Team::Axis;
    c.ps.origin = origin;
    c.ps.viewangles = [0.0, yaw, 0.0];
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
    n
}

/// One 50 ms server frame: a usercmd with `buttons` and `yaw`, then the end of the frame.
fn frame(g: &mut Game, vm: &mut Vm, n: u16, buttons: i32, yaw: f32) {
    let t = g.client(n).unwrap().ps.command_time + 50;
    g.level.time = t;
    let cmd = UserCmd {
        server_time: t,
        buttons,
        angles: [0, (yaw / sim::pm::ANGLE_UNIT) as i32, 0],
        ..UserCmd::default()
    };
    g.client_think(vm, n, cmd);
    g.client_end_frame(vm, n);
}

/// A spot to stand `back` units behind a turret that faces `yaw`, on the floor, with room for a player.
fn standing_behind(g: &Game, origin: [f32; 3], yaw: f32) -> Option<[f32; 3]> {
    let world = g.world.as_ref()?;
    let (s, c) = yaw.to_radians().sin_cos();
    for back in [50.0, 60.0, 70.0, 40.0, 80.0] {
        let (x, y) = (origin[0] - back * c, origin[1] - back * s);
        let t = world.trace(
            [x, y, origin[2] + 40.0],
            [x, y, origin[2] - 120.0],
            PLAYER_MINS,
            PLAYER_MAXS,
            ENTITYNUM_NONE,
            MASK_PLAYERSOLID,
        );
        if !t.start_solid && t.fraction < 1.0 && t.walkable {
            return Some([x, y, origin[2] + 40.0 - 160.0 * t.fraction]);
        }
    }
    None
}

fn yaw_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

#[test]
fn a_player_behind_a_stock_turret_mounts_aims_fires_and_dismounts() {
    let Some(root) = std::env::var_os("COD4_PATH").map(PathBuf::from) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let args: Vec<String> = ["+set", "net_port", "0", "+map", "mp_convoy"]
        .map(String::from)
        .to_vec();
    let mut s = server::server::Server::boot(&root, &args, false).expect("boot");
    let g = &mut s.game;
    let stand = g.weapons.index("saw_bipod_stand_mp");
    let turrets: Vec<(u16, [f32; 3], f32)> = g
        .turrets()
        .map(|(n, e, t)| {
            assert_eq!(t.weapon, stand);
            // The map's arcs are the weapon's: 45 each way, 15 up and 15 down.
            assert_eq!(t.arc_min, [-15.0, -45.0]);
            assert_eq!(t.arc_max, [15.0, 45.0]);
            (n, e.origin, e.angles[1])
        })
        .collect();
    assert_eq!(turrets.len(), 2, "mp_convoy places two turrets");

    let mut vm = vm();
    g.level.frametime = 50;
    g.level.time = 1000;
    let (t, origin, yaw, spot) = turrets
        .iter()
        .find_map(|&(t, o, y)| standing_behind(g, o, y).map(|p| (t, o, y, p)))
        .expect("a place to stand behind a turret");
    let n = add_player(g, &mut vm, spot, yaw);
    g.client_mut(n).unwrap().ps.command_time = 1000;
    // Landed and settled.
    for _ in 0..4 {
        frame(g, &mut vm, n, 0, yaw);
    }
    let ground = g.client(n).unwrap().ps.ground_entity_num;
    assert_ne!(ground, ENTITYNUM_NONE, "the player never reached the floor");

    // Usable from behind, not from in front of the gun.
    assert!(
        g.turret_usable(t, n),
        "the turret is not usable from behind"
    );
    let (s_, c_) = yaw.to_radians().sin_cos();
    let front = [origin[0] + 50.0 * c_, origin[1] + 50.0 * s_, spot[2]];
    let other = add_player(g, &mut vm, front, yaw + 180.0);
    g.client_mut(other).unwrap().ps.ground_entity_num = 1;
    assert!(
        !g.turret_usable(t, other),
        "a turret can be used from in front of it"
    );
    g.client_mut(other).unwrap().ps.ground_entity_num = ENTITYNUM_NONE;

    // The crosshair offers it.
    let c = g.client(n).unwrap();
    assert_eq!(c.ps.cursor_hint_ent_index, t, "the turret is not the hint");
    assert_eq!(c.ps.cursor_hint, WEAPON_HINT_OFFSET + stand as u8);

    // Holding use mounts it.
    for _ in 0..30 {
        frame(g, &mut vm, n, button::USE, yaw);
        if g.client(n).unwrap().turret.is_some() {
            break;
        }
    }
    let c = g.client(n).unwrap();
    assert_eq!(c.turret, Some(t), "use did not mount the turret");
    assert_eq!(c.ps.e_flags & ef::TURRET_ACTIVE, ef::TURRET_ACTIVE);
    assert_eq!(g.ent(t).unwrap().turret.as_ref().unwrap().gunner, Some(n));
    let mounted_at = g.client(n).unwrap().ps.origin;

    // Turned hard away, the view stays inside the 45 degrees each way.
    let mut turn = yaw;
    for _ in 0..20 {
        turn += 20.0;
        frame(g, &mut vm, n, 0, turn);
    }
    let c = g.client(n).unwrap();
    let off = yaw_delta(c.ps.viewangles[1], yaw);
    assert!(
        off.abs() <= 45.5,
        "the view is {off} degrees off the gun's line"
    );
    assert!(
        (off.abs() - 45.0).abs() < 1.0,
        "the view should be held at the arc's edge, is {off}"
    );
    // The gunner stays at the mount while looking about.
    let held = c.ps.origin;
    assert!(
        (0..3).all(|i| (held[i] - mounted_at[i]).abs() < 1.0),
        "the gunner walked off the turret: {mounted_at:?} to {held:?}"
    );

    // Back on the line, the attack button fires bullets.
    for _ in 0..6 {
        frame(g, &mut vm, n, 0, yaw);
    }
    let shots = g.stats.shots;
    for _ in 0..6 {
        frame(g, &mut vm, n, button::ATTACK, yaw);
    }
    assert!(g.stats.shots >= shots + 3, "the turret did not fire");

    // Use again lets go: the stance flag is cleared and the player is back where they mounted.
    frame(g, &mut vm, n, button::USE, yaw);
    let c = g.client(n).unwrap();
    assert_eq!(c.turret, None, "use did not dismount");
    assert_eq!(c.ps.e_flags & ef::TURRET_ACTIVE, 0);
    assert!(g.ent(t).unwrap().turret.as_ref().unwrap().gunner.is_none());
    let back = c.ps.origin;
    assert!(
        (0..2).all(|i| (back[i] - spot[i]).abs() < 2.0),
        "dismounted at {back:?}, mounted from {spot:?}"
    );
}

#[test]
fn a_gunner_who_dies_or_respawns_lets_go_of_the_gun() {
    let Some(root) = std::env::var_os("COD4_PATH").map(PathBuf::from) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let args: Vec<String> = ["+set", "net_port", "0", "+map", "mp_convoy"]
        .map(String::from)
        .to_vec();
    let mut s = server::server::Server::boot(&root, &args, false).expect("boot");
    let g = &mut s.game;
    let mut vm = vm();
    g.level.frametime = 50;
    g.level.time = 1000;
    let turrets: Vec<_> = g
        .turrets()
        .map(|(n, e, _)| (n, e.origin, e.angles[1]))
        .collect();
    let (t, spot, yaw) = turrets
        .iter()
        .find_map(|&(t, o, y)| standing_behind(g, o, y).map(|p| (t, p, y)))
        .expect("a place to stand behind a turret");
    for dies in [true, false] {
        let n = add_player(g, &mut vm, spot, yaw);
        g.client_mut(n).unwrap().ps.command_time = g.level.time;
        for _ in 0..3 {
            frame(g, &mut vm, n, 0, yaw);
        }
        g.turret_use(t, n);
        frame(g, &mut vm, n, 0, yaw);
        assert_eq!(g.client(n).unwrap().turret, Some(t));
        if dies {
            g.ent_mut(n).unwrap().health = 0;
            frame(g, &mut vm, n, 0, yaw);
        } else {
            g.client_spawn(&mut vm, n, spot, [0.0, yaw, 0.0]);
        }
        assert_eq!(g.client(n).unwrap().turret, None);
        assert!(
            g.ent(t).unwrap().turret.as_ref().unwrap().gunner.is_none(),
            "the gun still holds its gunner"
        );
        assert_eq!(g.client(n).unwrap().ps.e_flags & ef::TURRET_ACTIVE, 0);
        g.disconnect_client(&mut vm, n);
        g.finish_disconnects(&mut vm);
    }
}
