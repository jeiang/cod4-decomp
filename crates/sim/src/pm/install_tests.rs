// SPDX-License-Identifier: GPL-3.0-or-later
//! Player movement against the stock map mp_crash. Skipped without `COD4_PATH`.

use super::*;
use crate::contents::{self, MASK_PLAYERSOLID};
use crate::world::World;
use assets::zone::clipmap::Clipmap;
use assets::zone::xanim::XAnimParts;
use assets::zone::{Asset, Consumer, Zone};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

struct Install {
    map: Arc<Clipmap>,
    mantle: MantleAnims,
}

fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

fn decode(root: &Path, zone: &str, mut f: impl FnMut(Asset)) -> Option<()> {
    let dir = find_ci(&find_ci(root, "zone")?, "english")?;
    let file = std::fs::File::open(find_ci(&dir, &format!("{zone}.ff"))?).ok()?;
    Zone::open(std::io::BufReader::new(file))
        .ok()?
        .decode(&Consumer::Server, &mut f)
        .ok()
        .map(|_| ())
}

fn install() -> Option<&'static Install> {
    static INSTALL: LazyLock<Option<Install>> = LazyLock::new(|| {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return None;
        };
        let root = PathBuf::from(root);
        let mut map = None;
        decode(&root, "mp_crash", |a| {
            if let Asset::Clipmap(c) = a {
                map = Some(c);
            }
        })?;
        let mut anims: Vec<Arc<XAnimParts>> = Vec::new();
        for zone in ["common_mp", "mp_crash"] {
            decode(&root, zone, |a| {
                if let Asset::XAnimParts(x) = a
                    && x.name.as_deref().is_some_and(|n| n.contains("mantle"))
                {
                    anims.push(x);
                }
            })?;
        }
        let mantle = MantleAnims::from_xanims(|name| {
            anims
                .iter()
                .find(|a| {
                    a.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                })
                .map(|a| &**a)
        })?;
        Some(Install { map: map?, mantle })
    });
    INSTALL.as_ref()
}

/// `(origin, yaw)` of every `*_spawn` entity in the map's entity string.
fn spawns(map: &Clipmap) -> Vec<(Vec3, f32)> {
    let text = String::from_utf8_lossy(&map.map_ents.as_ref().unwrap().entity_string).into_owned();
    let mut out = Vec::new();
    for block in text.split('}') {
        let mut kv = std::collections::HashMap::new();
        let mut q = block.split('"').skip(1).step_by(2);
        while let (Some(k), Some(v)) = (q.next(), q.next()) {
            kv.insert(k.to_owned(), v.to_owned());
        }
        let is_spawn = kv
            .get("classname")
            .is_some_and(|c| c.ends_with("_spawn") || c == "info_player_start");
        if let (true, Some(o)) = (is_spawn, kv.get("origin")) {
            let n: Vec<f32> = o
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            let yaw = kv
                .get("angles")
                .and_then(|a| a.split_whitespace().nth(1)?.parse().ok())
                .unwrap_or(0.0);
            if n.len() == 3 {
                out.push(([n[0], n[1], n[2]], yaw));
            }
        }
    }
    out
}

struct Player<'a> {
    pm: Pmove<'a>,
    sent: UserCmd,
}

impl<'a> Player<'a> {
    fn new(params: &'a Params, origin: Vec3, yaw: f32) -> Self {
        let mut ps = PlayerState {
            origin,
            command_time: 100_000,
            ..PlayerState::default()
        };
        ps.viewangles[1] = yaw;
        ps.delta_angles[1] = yaw;
        Self {
            pm: Pmove::new(ps, params),
            sent: UserCmd::default(),
        }
    }

    fn step(&mut self, world: &World, buttons: i32, fwd: i8, right: i8, dt: i32) {
        let pm = &mut self.pm;
        pm.cmd = UserCmd {
            buttons,
            forwardmove: fwd,
            rightmove: right,
            ..UserCmd::default()
        };
        pm.cmd.server_time = pm.ps.command_time + dt;
        pm.oldcmd = self.sent;
        self.sent = pm.cmd;
        pmove(pm, world);
    }

    fn run(&mut self, world: &World, buttons: i32, fwd: i8, right: i8, ms: i32) {
        for _ in 0..ms / 25 {
            self.step(world, buttons, fwd, right, 25);
        }
    }

    fn grounded(&self) -> bool {
        self.pm.ps.ground_entity_num != crate::cm::ENTITYNUM_NONE
    }
}

/// Lowers a spawn onto the floor beneath it, or `None` when it starts inside geometry.
fn settle(world: &World, p: Vec3) -> Option<Vec3> {
    let (mins, maxs) = (PLAYER_MINS, PLAYER_MAXS);
    let start = [p[0], p[1], p[2] + 8.0];
    let end = [p[0], p[1], p[2] - 64.0];
    let t = crate::cm::Collide::trace(world, start, end, mins, maxs, 1023, MASK_PLAYERSOLID);
    if t.start_solid || t.all_solid || t.fraction >= 1.0 {
        return None;
    }
    Some([p[0], p[1], start[2] + (end[2] - start[2]) * t.fraction])
}

/// The facing, among eight, with the most open floor ahead.
fn open_yaw(world: &World, p: Vec3) -> f32 {
    let mut best = (0.0, -1.0f32);
    for i in 0..8 {
        let yaw = i as f32 * 45.0;
        let (f, _, _) = math::angle_vectors(&[0.0, yaw, 0.0]);
        let s = [p[0], p[1], p[2] + 8.0];
        let e = [s[0] + f[0] * 400.0, s[1] + f[1] * 400.0, s[2]];
        let t = crate::cm::Collide::trace(
            world,
            s,
            e,
            PLAYER_MINS,
            PLAYER_MAXS,
            1023,
            MASK_PLAYERSOLID,
        );
        if t.fraction > best.1 {
            best = (yaw, t.fraction);
        }
    }
    best.0
}

fn fingerprint(ps: &PlayerState, h: &mut u64) {
    let mut eat = |v: u32| {
        *h ^= u64::from(v);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for v in ps.origin.iter().chain(&ps.velocity).chain(&ps.viewangles) {
        eat(v.to_bits());
    }
    eat(ps.pm_flags);
    eat(ps.command_time as u32);
    eat(u32::from(ps.ground_entity_num));
    eat(ps.view_height_current.to_bits());
}

/// Walk, sprint, crouch, prone, jump from a stock spawn; returns a fingerprint of every state.
fn locomotion(world: &World, params: &Params, spawn: Vec3, yaw: f32) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut p = Player::new(params, spawn, yaw);
    let bounds = world.collision().model_bounds(0).unwrap();
    let checkpoint = |p: &Player<'_>, h: &mut u64| {
        let o = p.pm.ps.origin;
        assert!(o.iter().all(|c| c.is_finite()), "NaN origin {o:?}");
        for i in 0..3 {
            assert!(
                o[i] > bounds.0[i] - 1.0 && o[i] < bounds.1[i] + 1.0,
                "left the world: {o:?}"
            );
        }
        fingerprint(&p.pm.ps, h);
    };
    macro_rules! phase {
        ($($arg:expr),*) => {{
            for _ in 0..40 {
                p.run(world, $($arg),*, 25);
                checkpoint(&p, &mut h);
            }
        }};
    }
    // Settle onto the floor.
    p.run(world, 0, 0, 0, 400);
    assert!(p.grounded(), "spawn did not land: {:?}", p.pm.ps.origin);
    let rest = p.pm.ps.origin;
    assert!((rest[2] - spawn[2]).abs() < 2.0);

    let start = p.pm.ps.origin;
    phase!(0, 127, 0);
    let walked = math::length(&math::sub(&p.pm.ps.origin, &start));
    assert!(walked > 60.0, "walked only {walked}");
    assert!(p.grounded());
    phase!(button::SPRINT, 127, 0);
    phase!(button::CROUCH, 127, 0);
    assert_eq!(p.pm.ps.stance(), Stance::Crouch);
    assert!(p.grounded());
    phase!(button::PRONE, 0, 0);
    assert!(p.pm.ps.view_height_current <= 40.0);
    phase!(0, 0, 0);
    assert_eq!(p.pm.ps.stance(), Stance::Stand);
    assert!(p.grounded());
    // A jump rises, comes down, and stands again on solid ground.
    let floor = p.pm.ps.origin[2];
    p.step(world, button::JUMP, 0, 0, 25);
    let mut apex = floor;
    for _ in 0..60 {
        p.step(world, 0, 0, 0, 25);
        apex = apex.max(p.pm.ps.origin[2]);
        checkpoint(&p, &mut h);
    }
    assert!(
        apex - floor > 25.0 && apex - floor < 45.0,
        "jump rose {}",
        apex - floor
    );
    assert!(p.grounded());
    h
}

/// Finds a ledge the player can mantle and plays the climb out; returns the final state.
fn mantle_run(world: &World, params: &Params) -> Option<(Vec3, Vec3, PlayerState)> {
    let (lo, hi) = world.collision().model_bounds(0)?;
    // Drop hulls from above on a grid and look sideways for mantle brushes.
    for gx in 0..250 {
        for gy in 0..250 {
            let x = lo[0] + (hi[0] - lo[0]) * (gx as f32 + 0.5) / 250.0;
            let y = lo[1] + (hi[1] - lo[1]) * (gy as f32 + 0.5) / 250.0;
            let top = [x, y, hi[2] + 8.0];
            let down = [x, y, lo[2] - 8.0];
            let t = crate::cm::Collide::trace(
                world,
                top,
                down,
                PLAYER_MINS,
                PLAYER_MAXS,
                1023,
                MASK_PLAYERSOLID,
            );
            if t.start_solid || t.fraction >= 1.0 || !t.walkable {
                continue;
            }
            let rest = [x, y, top[2] + (down[2] - top[2]) * t.fraction];
            for k in 0..4 {
                let yaw = k as f32 * 90.0;
                let (f, _, _) = math::angle_vectors(&[0.0, yaw, 0.0]);
                let a = [rest[0] - 14.9 * f[0], rest[1] - 14.9 * f[1], rest[2]];
                let b = [rest[0] + 19.0 * f[0], rest[1] + 19.0 * f[1], rest[2]];
                let m = crate::cm::Collide::trace(
                    world,
                    a,
                    b,
                    [-0.1, -0.1, 0.0],
                    [0.1, 0.1, 70.0],
                    1023,
                    contents::MANTLE,
                );
                if m.start_solid || m.fraction >= 1.0 || m.surface_flags & 0x0600_0000 == 0 {
                    continue;
                }
                let mut p = Player::new(params, rest, yaw);
                p.run(world, 0, 0, 0, 300);
                if !p.grounded() || math::length(&math::sub(&p.pm.ps.origin, &rest)) > 1.0 {
                    continue;
                }
                let before = p.pm.ps.origin;
                p.step(world, button::JUMP, 0, 0, 25);
                if p.pm.ps.pm_flags & pmf::MANTLE == 0 {
                    continue;
                }
                let mut guard = 0;
                while p.pm.ps.pm_flags & pmf::MANTLE != 0 && guard < 200 {
                    p.step(world, 0, 0, 0, 25);
                    guard += 1;
                }
                p.run(world, 0, 0, 0, 1000);
                return Some((before, rest, p.pm.ps));
            }
        }
    }
    None
}

#[test]
fn walks_sprints_stances_and_jumps_from_a_stock_spawn() {
    let Some(inst) = install() else { return };
    let world = World::new(inst.map.clone());
    let params = Params {
        mantle_anims: Some(inst.mantle.clone()),
        ..Params::default()
    };
    let mut ran = 0;
    let mut prints = Vec::new();
    for (origin, _) in spawns(&inst.map).into_iter().take(40) {
        let Some(rest) = settle(&world, origin) else {
            continue;
        };
        if world_blocked(&world, rest) {
            continue;
        }
        let yaw = open_yaw(&world, rest);
        let h = locomotion(&world, &params, rest, yaw);
        // Determinism: the same script gives the same states, bit for bit.
        assert_eq!(h, locomotion(&world, &params, rest, yaw));
        prints.push(h);
        ran += 1;
        if ran == 6 {
            break;
        }
    }
    assert!(ran >= 3, "only {ran} usable spawns");
    // Recorded from this implementation (Mac arm64 and Linux x86-64 agree): any change to the
    // movement arithmetic or the trace results moves these.
    const GOLDEN: [u64; 6] = [
        0x2a10_f411_b2c3_cae8,
        0x5ff1_6fce_1d11_abc3,
        0x5e86_9275_03fd_80c9,
        0x018e_bdd4_83b2_0a60,
        0x19c3_4e38_bd6f_b917,
        0xa373_767b_56ef_7aa5,
    ];
    assert_eq!(prints, GOLDEN[..prints.len()]);
}

fn world_blocked(world: &World, p: Vec3) -> bool {
    // The hull must fit standing and have room to run in some direction.
    crate::cm::Collide::trace(
        world,
        p,
        p,
        PLAYER_MINS,
        PLAYER_MAXS,
        1023,
        MASK_PLAYERSOLID,
    )
    .start_solid
}

#[test]
fn mantles_over_a_real_ledge() {
    let Some(inst) = install() else { return };
    let world = World::new(inst.map.clone());
    let params = Params {
        mantle_anims: Some(inst.mantle.clone()),
        ..Params::default()
    };
    let (before, rest, end) =
        mantle_run(&world, &params).expect("no mantle ledge found on mp_crash");
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fingerprint(&end, &mut h);
    assert_eq!(h, 2469497806190789872, "{rest:?} -> {:?}", end.origin);
    assert!(
        end.origin[2] > before[2] + 15.0,
        "did not climb: {before:?} -> {:?}",
        end.origin
    );
    assert_eq!(end.pm_flags & pmf::MANTLE, 0);
    assert!(
        end.ground_entity_num != crate::cm::ENTITYNUM_NONE,
        "left floating"
    );
    assert_eq!(end.velocity[2], 0.0);
}

/// `cargo test -p sim --release -- --ignored --nocapture pmove_cost`
#[test]
#[ignore = "benchmark"]
fn pmove_cost() {
    let Some(inst) = install() else { return };
    let world = World::new(inst.map.clone());
    let params = Params {
        mantle_anims: Some(inst.mantle.clone()),
        ..Params::default()
    };
    let (origin, _) = spawns(&inst.map)
        .into_iter()
        .find_map(|(o, y)| settle(&world, o).map(|r| (r, y)))
        .unwrap();
    let yaw = open_yaw(&world, origin);
    let mut p = Player::new(&params, origin, yaw);
    let cmds = 200_000;
    let t = std::time::Instant::now();
    let mut keep = 0.0f32;
    for i in 0..cmds {
        let buttons = match i % 400 {
            0..=20 => button::JUMP,
            100..=180 => button::SPRINT,
            250..=300 => button::CROUCH,
            _ => 0,
        };
        p.step(&world, buttons, 127, ((i / 50 % 3) as i8 - 1) * 60, 25);
        keep += p.pm.ps.origin[0];
        if i % 1000 == 999 {
            p.pm.ps.origin = origin;
            p.pm.ps.velocity = [0.0; 3];
        }
    }
    let ns = t.elapsed().as_nanos() as f64 / f64::from(cmds);
    eprintln!("pmove: {ns:.0} ns per 25 ms usercmd on mp_crash ({keep})");
}
