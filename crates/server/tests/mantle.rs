// SPDX-License-Identifier: GPL-3.0-only
//! Mantling on the stock mp_crash: the live server climbs a ledge and holds the landing spot.
//! Skips without `COD4_PATH`.

use std::path::PathBuf;

use gsc::{Builtins, Options, Vm, compile};
use server::client::{Session, Team};
use server::game::Game;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents::{self, MASK_PLAYERSOLID};
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType, UserCmd, button, pmf};
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

/// Standing spots and yaws with a mantle brush straight ahead (the ledge above may still be unclimbable).
fn find_ledges(world: &World) -> Vec<([f32; 3], f32)> {
    let mut found = Vec::new();
    let Some((lo, hi)) = world.collision().model_bounds(0) else {
        return found;
    };
    for gx in 0..250 {
        for gy in 0..250 {
            let x = lo[0] + (hi[0] - lo[0]) * (gx as f32 + 0.5) / 250.0;
            let y = lo[1] + (hi[1] - lo[1]) * (gy as f32 + 0.5) / 250.0;
            let (top, down) = ([x, y, hi[2] + 8.0], [x, y, lo[2] - 8.0]);
            let t = world.trace(
                top,
                down,
                PLAYER_MINS,
                PLAYER_MAXS,
                ENTITYNUM_NONE,
                MASK_PLAYERSOLID,
            );
            if t.start_solid || t.fraction >= 1.0 || !t.walkable {
                continue;
            }
            let rest = [x, y, top[2] + (down[2] - top[2]) * t.fraction];
            for k in 0..4 {
                let yaw = k as f32 * 90.0;
                let (s, c) = yaw.to_radians().sin_cos();
                let a = [rest[0] - 14.9 * c, rest[1] - 14.9 * s, rest[2]];
                let b = [rest[0] + 19.0 * c, rest[1] + 19.0 * s, rest[2]];
                let m = world.trace(
                    a,
                    b,
                    [-0.1, -0.1, 0.0],
                    [0.1, 0.1, 70.0],
                    ENTITYNUM_NONE,
                    contents::MANTLE,
                );
                if !m.start_solid && m.fraction < 1.0 && m.surface_flags & 0x0600_0000 != 0 {
                    found.push((rest, yaw));
                }
            }
        }
    }
    found
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

fn think(g: &mut Game, vm: &mut Vm, n: u16, buttons: i32, yaw: f32) {
    let t = g.client(n).unwrap().ps.command_time + 25;
    g.level.time = t;
    let cmd = UserCmd {
        server_time: t,
        buttons,
        angles: [0, (yaw / sim::pm::ANGLE_UNIT) as i32, 0],
        ..UserCmd::default()
    };
    g.client_think(vm, n, cmd);
}

#[test]
fn a_jump_at_a_ledge_climbs_it_and_the_landing_spot_stays_blocked() {
    let Some(root) = std::env::var_os("COD4_PATH").map(PathBuf::from) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let args: Vec<String> = ["+set", "net_port", "0", "+map", "mp_crash"]
        .map(String::from)
        .to_vec();
    let mut s = server::server::Server::boot(&root, &args, false).expect("boot");
    let g = &mut s.game;
    assert!(
        g.pm_params.mantle_anims.is_some(),
        "the live movement parameters carry no mantle animations"
    );
    let mut vm = vm();
    g.level.frametime = 25;
    let ledges = find_ledges(g.world.as_ref().expect("world"));
    let n = add_player(g, &mut vm, ledges[0].0, ledges[0].1);
    let mut yaw = 0.0;
    let mut climbing = false;
    for (rest, y) in ledges {
        yaw = y;
        let time = g.level.time.max(1000);
        let c = g.client_mut(n).unwrap();
        c.ps.origin = rest;
        c.ps.velocity = [0.0; 3];
        c.ps.viewangles = [0.0, y, 0.0];
        c.ps.command_time = time;
        g.ent_mut(n).unwrap().origin = rest;
        for _ in 0..12 {
            think(g, &mut vm, n, 0, y);
        }
        for _ in 0..3 {
            think(g, &mut vm, n, button::JUMP, y);
            climbing = g.client(n).unwrap().ps.pm_flags & pmf::MANTLE != 0;
            if climbing {
                break;
            }
        }
        if climbing {
            break;
        }
    }
    assert!(climbing, "no ledge on the map could be mantled");
    let block = g
        .in_use()
        .find(|(_, e)| &*e.classname == "player_mantle_block")
        .map(|(i, e)| (i, e.origin, e.contents))
        .expect("a landing-spot blocker");
    assert_eq!(block.2, contents::PLAYERCLIP);
    let (num, end, _) = block;
    let world = g.world.as_ref().unwrap();
    let probe = |pass| {
        world.trace(
            [end[0], end[1], end[2] + 8.0],
            end,
            PLAYER_MINS,
            PLAYER_MAXS,
            pass,
            MASK_PLAYERSOLID,
        )
    };
    assert!(
        probe(ENTITYNUM_NONE).fraction < 1.0 || probe(ENTITYNUM_NONE).start_solid,
        "another player can stand on the landing spot"
    );
    assert!(
        !probe(n).start_solid,
        "the climber is blocked by its own blocker"
    );
    let mut guard = 0;
    while g.client(n).unwrap().ps.pm_flags & pmf::MANTLE != 0 && guard < 200 {
        think(g, &mut vm, n, 0, yaw);
        guard += 1;
    }
    assert!(guard < 200, "the mantle never finished");
    let o = g.client(n).unwrap().ps.origin;
    let off = ((o[0] - end[0]).powi(2) + (o[1] - end[1]).powi(2)).sqrt();
    assert!(
        off < 2.0 && (o[2] - end[2]).abs() < 2.0,
        "ended at {o:?}, not {end:?}"
    );
    // The blocker outlives the climb by g_mantleBlockTimeBuffer, then goes.
    g.run_entity(&mut vm, num);
    assert!(g.ent(num).is_some(), "the blocker went with the climb");
    g.level.time += 10_000;
    g.run_entity(&mut vm, num);
    assert!(g.ent(num).is_none(), "the blocker was never removed");
}

/// A client and the server build the same mantle set, or prediction would rubber-band.
#[test]
fn client_and_server_content_give_the_same_mantle_set() {
    let Some(root) = std::env::var_os("COD4_PATH").map(PathBuf::from) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let install = server::content::Install::open(&root).expect("install");
    let load = |client: bool| {
        let mut c = server::content::Content::default();
        c.client = client;
        c.load_boot(&install).expect("boot");
        c.load_map(&install, "mp_crash").expect("map");
        c.mantle_anims().expect("mantle animations")
    };
    let (a, b) = (load(false), load(true));
    for slot in 1..sim::pm::MANTLE_ANIM_COUNT {
        assert_eq!(a.anim(slot).length_msec, b.anim(slot).length_msec);
        assert_eq!(a.anim(slot).samples, b.anim(slot).samples);
    }
}
