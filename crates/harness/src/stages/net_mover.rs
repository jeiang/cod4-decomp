// SPDX-License-Identifier: GPL-3.0-only
//! `net-mover`: a predicted player riding a moving `script_brushmodel` keeps its feet on the platform as the client
//! draws it (KisakCOD `cgame/cg_predict.cpp` `CG_AdjustPositionForMover`: the predicted player is carried to the
//! time the mover is drawn at).
//!
//! A real UDP client (no window) joins a headless war match on mp_cargoship. The stage makes a platform of the
//! map's largest solid `script_brushmodel` model, sinks it into the floor at a spawn point with room above, puts the
//! player on it and runs it up and down at 300 units/s. Every frame the client predicts as the game client does:
//! usercmds stamped [`net::view::INTERP_DELAY_MS`] behind its clock estimate, the other players and the movers from
//! the newest snapshot ([`PlayerBoxes`]), the movers drawn at the clock estimate ([`mover_offset`]).
//!
//! - The player stood on the platform (`groundEntityNum`) while it moved, for at least 30 frames.
//! - The predicted feet stay within 2 units of the drawn platform top in 95% of those frames: carrying the player
//!   only to its commands' time (100 ms behind the drawn platform) leaves the feet about 5 to 10 units off.
use super::net_antilag::{Player, ground, playing};
use crate::stage::{StageCtx, StageReport, Status};
use net::predict::{Env, PlayerBoxes, Predictor, mover_offset};
use server::game::{Ent, EntKind};
use server::server::Server;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, UserCmd};
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-mover";
const LIMIT: Duration = Duration::from_secs(180);
const FRAME: Duration = Duration::from_millis(16);
/// How far the platform rises, and how long a rise takes: 300 units/s.
const RISE: f32 = 192.0;
const RISE_SECS: f32 = 0.64;
/// Up and down this many times.
const TRIPS: usize = 4;
const MIN_FRAMES: usize = 30;
const TOLERANCE: f32 = 2.0;

/// The largest solid inline model a `script_brushmodel` of the map uses (`*N`), its bounds about its origin.
fn platform_model(server: &Server) -> Option<(String, [f32; 3], [f32; 3])> {
    server
        .game
        .ents
        .iter()
        .flatten()
        .filter(|e| {
            &*e.classname == "script_brushmodel"
                && e.model.starts_with('*')
                && e.contents & contents::SOLID != 0
        })
        .map(|e| (e.model.to_string(), e.mins, e.maxs))
        .filter(|(_, mins, maxs)| maxs[0] - mins[0] >= 48.0 && maxs[1] - mins[1] >= 48.0)
        .max_by(|a, b| {
            let area = |m: &(String, [f32; 3], [f32; 3])| (m.2[0] - m.1[0]) * (m.2[1] - m.1[1]);
            area(a).total_cmp(&area(b))
        })
}

/// A spawn point's floor with room for a standing player `RISE` above it.
fn site(server: &Server) -> Option<[f32; 3]> {
    let world = server.game.world.as_ref()?;
    let ents = server.game.content.clipmap()?.map_ents.clone()?;
    server::nav::spawn_points(&ents.entity_string)
        .into_iter()
        .filter_map(|sp| ground(world, sp.origin))
        .find(|p| {
            let up = [p[0], p[1], p[2] + RISE + 8.0];
            let t = world.trace(
                *p,
                up,
                PLAYER_MINS,
                PLAYER_MAXS,
                ENTITYNUM_NONE,
                contents::SOLID | contents::PLAYERCLIP,
            );
            !t.start_solid && t.fraction == 1.0
        })
}

fn moveto(server: &mut Server, n: u16, to: [f32; 3]) {
    let g = &mut server.game;
    let now = g.level.time;
    let Some(e) = g.ent_mut(n) else { return };
    let mut cur = e.origin;
    server::mover::setup_move(&mut e.mv.pos, to, (RISE_SECS, 0.0, 0.0), &mut cur, now);
    e.origin = cur;
    g.relink(n);
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let Some(addr) = server.net_addr() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("cannot bind a UDP socket"));
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "set sv_mapRotation \"gametype war map mp_cargoship\"",
        "map mp_cargoship",
    ] {
        if let Err(e) = server.exec_line(line) {
            return fail(format!("{line}: {e}"));
        }
    }
    let deadline = Instant::now() + LIMIT;
    let mut p = Player::new(addr, "rider", 7311, "allies")?;
    while !playing(&server, &p) {
        if Instant::now() > deadline {
            return fail("the client never got a body".into());
        }
        server.run_for(FRAME);
        if let Some(t) = p.pump() {
            p.c.send_cmd(UserCmd {
                server_time: t,
                ..UserCmd::default()
            });
        }
    }
    let Some(own) = p.own else {
        return fail("the client has no body".into());
    };
    let Some((model, mins, maxs)) = platform_model(&server) else {
        return fail("mp_cargoship has no solid script_brushmodel 48 units across".into());
    };
    let Some(floor) = site(&server) else {
        return fail("no spawn point on mp_cargoship has room above it".into());
    };
    // The platform's top in the middle of it at the floor.
    let low = [
        floor[0] - (mins[0] + maxs[0]) * 0.5,
        floor[1] - (mins[1] + maxs[1]) * 0.5,
        floor[2] - 1.0 - maxs[2],
    ];
    let high = [low[0], low[1], low[2] + RISE];
    let mut e = Ent::new(EntKind::Brush, "script_brushmodel");
    e.model = model.as_str().into();
    e.origin = low;
    let plat = match server.game.spawn(e) {
        Ok(n) => n,
        Err(e) => return fail(format!("spawning the platform: {e}")),
    };
    server.game.init_clip(plat, None);
    if let Some(e) = server.game.ent_mut(plat) {
        e.contents = contents::SOLID;
    }
    server.game.relink(plat);
    server
        .game
        .teleport(own, [floor[0], floor[1], floor[2] + 1.0]);

    let Some(clipmap) = server.game.content.clipmap().cloned() else {
        return fail("the map has no collision data".into());
    };
    let (weapons, params) = (server.game.weapons.clone(), server.game.pm_params.clone());
    let mut boxes = PlayerBoxes::new(clipmap);
    let mut pred = Predictor::default();
    let (mut riding, mut errors) = (0usize, Vec::<f32>::new());
    let mut trip = 0;
    let mut up = false;
    let mut idle = 0;
    while trip < TRIPS * 2 {
        if Instant::now() > deadline {
            return fail(format!("ran out of time after {trip} moves"));
        }
        server.run_for(FRAME);
        let moving = server
            .game
            .ent(plat)
            .is_some_and(|e| !e.mv.pos.finished(server.game.level.time));
        if !moving {
            idle += 1;
            // Settled a moment at each end, then the next move.
            if idle > 20 {
                idle = 0;
                up = !up;
                moveto(&mut server, plat, if up { high } else { low });
                trip += 1;
            }
        }
        let Some(ct) = p.pump() else { continue };
        let cmd = UserCmd {
            server_time: ct,
            ..UserCmd::default()
        };
        pred.push(cmd);
        p.c.send_cmd(cmd);
        let (Some(s), Some(st)) = (p.c.latest(), p.c.snaps.server_time(p.c.now_ms())) else {
            continue;
        };
        let Some(m) = s.entity(plat) else { continue };
        if s.ps.ground_entity_num != plat || m.velocity == [0.0; 3] {
            continue;
        }
        boxes.sync(s);
        let env = Env {
            world: boxes.world(),
            weapons: &weapons,
            params: &params,
            time: ct,
            draw_time: st,
        };
        let feet = pred.predict(s, &env).ps.origin[2];
        let top = m.origin[2] + maxs[2] + mover_offset(m, st - s.server_time)[2];
        riding += 1;
        errors.push((feet - top).abs());
    }
    p.c.disconnect();
    for _ in 0..5 {
        server.run_for(FRAME);
    }
    errors.sort_by(f32::total_cmp);
    let at = |q: f32| {
        errors
            .get(((errors.len() as f32 - 1.0) * q).round() as usize)
            .copied()
            .unwrap_or(f32::NAN)
    };
    let (median, p95, max) = (at(0.5), at(0.95), at(1.0));
    let mut report = StageReport::new(NAME, Status::Passed);
    report
        .metrics
        .insert("mover.riding_frames".into(), riding as f64);
    report
        .metrics
        .insert("mover.feet_error.median".into(), f64::from(median));
    report
        .metrics
        .insert("mover.feet_error.p95".into(), f64::from(p95));
    report
        .metrics
        .insert("mover.feet_error.max".into(), f64::from(max));
    report.notes.push(format!(
        "platform {model} at {floor:?}, {riding} frames riding it while it moved"
    ));
    let mut why = Vec::new();
    if riding < MIN_FRAMES {
        why.push(format!(
            "the player stood on the moving platform in only {riding} frames"
        ));
    } else if p95 > TOLERANCE {
        why.push(format!(
            "the predicted feet were {p95:.1} units off the drawn platform in 5% of the frames (median {median:.1}, max {max:.1})"
        ));
    }
    if !why.is_empty() {
        report.status = Status::Failed;
        report = report.with_reason(why.join("; "));
    }
    Ok(report)
}
