// SPDX-License-Identifier: GPL-3.0-only
//! `net-mantle`: one real UDP client (no window) joins a match on mp_crash and jumps at a ledge.
//! The stage moves the player to a spot where the movement code mantles (found by running the
//! shared movement code over the map's mantle brushes), then drives the jump with real
//! usercmds. The server must start the mantle and finish on the ledge, hold the landing spot
//! with its blocker meanwhile, and the client's own prediction must start the same mantle, end
//! at the same place and never disagree with the server's answer.
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::predict::{Env, PlayerBoxes, Predictor};
use net::ui::AutoJoin;
use server::client::{Session, Team};
use server::server::Server;
use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents::{self, MASK_PLAYERSOLID};
use sim::pm::{
    ANGLE_UNIT, PLAYER_MAXS, PLAYER_MINS, Params, PlayerState, UserCmd, button, pmf, run_usercmd,
};
use sim::weapon::{PlayerWeapons, WeaponTable};
use sim::world::World;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-mantle";
/// Waiting for the match to give the client a body depends on the scripts, not the runner.
const LIMIT: Duration = Duration::from_secs(120);
/// Spots tried live before the stage gives up.
const ATTEMPTS: usize = 6;
/// How far the end of the climb may be from the one the shared movement code reaches alone.
const END_TOLERANCE: f32 = 4.0;

/// A client that sends the commands the stage sets and predicts its own player.
struct Walker {
    c: NetClient<UdpTransport>,
    join: AutoJoin,
    own: Option<u16>,
    cmd_time: i32,
    boxes: PlayerBoxes,
    pred: Predictor,
    weapons: WeaponTable,
    params: Params,
    speed: i32,
    buttons: i32,
    yaw: f32,
    /// The player state of the latest prediction pass.
    predicted: Option<PlayerState>,
}

impl Walker {
    fn step(&mut self) {
        self.c.pump(Duration::from_millis(1));
        if let Some(ui) = self.c.ui() {
            let events = ui.drain_events();
            for ev in &events {
                if let Some(a) = self.join.step(ev) {
                    self.c.command(&a);
                }
            }
        }
        let Some(s) = self.c.latest() else { return };
        let n = s.ps.client_num;
        if s.entity(n).is_some_and(|e| e.etype == etype::PLAYER) {
            self.own = Some(n);
        }
        let delta = s.ps.delta_angles[1];
        let now = self.c.now_ms();
        let Some(st) = self.c.snaps.server_time(now) else {
            return;
        };
        self.cmd_time = (self.cmd_time + 1).max(st);
        let cmd = UserCmd {
            server_time: self.cmd_time,
            buttons: self.buttons,
            angles: [0, (((self.yaw - delta) / ANGLE_UNIT) as i32) & 0xffff, 0],
            ..UserCmd::default()
        };
        self.pred.push(cmd);
        self.c.send_cmd(cmd);
        if let Some(s) = self.c.latest() {
            self.boxes.sync(s);
            let env = Env {
                world: self.boxes.world(),
                weapons: &self.weapons,
                params: &self.params,
                speed: self.speed,
            };
            self.predicted = Some(self.pred.predict(s, &env).ps);
        }
    }
}

/// Standing spots and yaws with a mantle brush straight ahead; the ledge above may still be
/// unclimbable.
fn candidates(world: &World) -> Vec<(Vec3, f32)> {
    let mut found = Vec::new();
    let Some((lo, hi)) = world.collision().model_bounds(0) else {
        return found;
    };
    const GRID: usize = 250;
    for gx in 0..GRID {
        for gy in 0..GRID {
            let x = lo[0] + (hi[0] - lo[0]) * (gx as f32 + 0.5) / GRID as f32;
            let y = lo[1] + (hi[1] - lo[1]) * (gy as f32 + 0.5) / GRID as f32;
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

/// Where the shared movement code, run alone from `rest`, ends a jump at the ledge ahead, or
/// `None` when it does not mantle.
fn simulate(
    world: &World,
    params: &Params,
    table: &WeaponTable,
    rest: Vec3,
    yaw: f32,
    speed: i32,
) -> Option<Vec3> {
    let mut ps = PlayerState {
        origin: rest,
        command_time: 100_000,
        ..PlayerState::default()
    };
    ps.viewangles[1] = yaw;
    ps.delta_angles[1] = yaw;
    let mut inv = PlayerWeapons::default();
    let mut old = UserCmd::default();
    for i in 0..15 {
        let cmd = UserCmd {
            server_time: ps.command_time + 25,
            buttons: if i < 12 { 0 } else { button::JUMP },
            ..UserCmd::default()
        };
        let out = run_usercmd(&mut ps, &mut inv, cmd, old, speed, table, params, world);
        old = cmd;
        if let Some((end, _)) = out.mantle {
            return Some(end);
        }
    }
    None
}

/// One live attempt.
#[derive(Default)]
struct Attempt {
    server_started: bool,
    predicted_started: bool,
    blocked: bool,
    corrections: u64,
    server_end: Vec3,
    predicted_end: Vec3,
}

fn blocker(server: &Server) -> bool {
    server
        .game
        .in_use()
        .any(|(_, e)| &*e.classname == "player_mantle_block")
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
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_crash",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let Some(clipmap) = server.game.content.clipmap().cloned() else {
        return fail("the map has no collision data".into());
    };
    if server.game.pm_params.mantle_anims.is_none() {
        return fail("the live movement parameters carry no mantle animations".into());
    }
    let t = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let mut w = Walker {
        c: NetClient::new(t, addr, "mantler", "", 7000),
        join: AutoJoin::default(),
        own: None,
        cmd_time: 0,
        boxes: PlayerBoxes::new(clipmap),
        pred: Predictor::default(),
        weapons: server.game.weapons.clone(),
        params: server.game.pm_params.clone(),
        speed: server.game.cvars.int("g_speed"),
        buttons: 0,
        yaw: 0.0,
        predicted: None,
    };
    let deadline = Instant::now() + LIMIT;
    let frame = |server: &mut Server, w: &mut Walker| {
        server.run_for(Duration::from_millis(16));
        w.step();
    };
    let playing = |server: &Server, w: &Walker| {
        w.own.and_then(|n| server.game.client(n)).is_some_and(|c| {
            c.session == Session::Playing
                && matches!(c.team, Team::Axis | Team::Allies)
                && c.ps.pm_type == sim::pm::PmType::Normal
        })
    };
    while !playing(&server, &w) {
        if Instant::now() > deadline {
            return fail("the client never got a body".into());
        }
        frame(&mut server, &mut w);
    }
    let Some(n) = w.own else {
        return fail("the client has no body".into());
    };

    // Spots where the shared movement code mantles, spread over the map.
    let spots: Vec<(Vec3, f32, Vec3)> = {
        let Some(world) = server.game.world.as_ref() else {
            return fail("the server has no collision world".into());
        };
        let all = candidates(world);
        let stride = (all.len() / (ATTEMPTS * 4)).max(1);
        all.into_iter()
            .step_by(stride)
            .filter_map(|(rest, yaw)| {
                simulate(world, &w.params, &w.weapons, rest, yaw, w.speed)
                    .map(|end| (rest, yaw, end))
            })
            .take(ATTEMPTS)
            .collect()
    };
    if spots.is_empty() {
        return fail("no spot on the map lets the movement code mantle".into());
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    let mut why = Vec::new();
    for (rest, yaw, end) in &spots {
        server.game.teleport(n, *rest);
        w.yaw = *yaw;
        w.buttons = 0;
        for _ in 0..40 {
            frame(&mut server, &mut w);
        }
        let mut a = Attempt::default();
        let base = w.pred.corrections;
        let seen = |server: &Server, w: &Walker, a: &mut Attempt| {
            let flagged = |ps: &PlayerState| ps.pm_flags & pmf::MANTLE != 0;
            a.server_started |= server.game.client(n).is_some_and(|c| flagged(&c.ps));
            a.predicted_started |= w.predicted.as_ref().is_some_and(flagged);
            a.blocked |= blocker(server);
        };
        w.buttons = button::JUMP;
        for _ in 0..40 {
            frame(&mut server, &mut w);
            seen(&server, &w, &mut a);
            if a.server_started {
                break;
            }
        }
        w.buttons = 0;
        for _ in 0..200 {
            frame(&mut server, &mut w);
            seen(&server, &w, &mut a);
            if server
                .game
                .client(n)
                .is_some_and(|c| c.ps.pm_flags & pmf::MANTLE == 0)
                && a.server_started
            {
                break;
            }
        }
        for _ in 0..40 {
            frame(&mut server, &mut w);
        }
        a.corrections = w.pred.corrections - base;
        a.server_end = server.game.client(n).map_or([0.0; 3], |c| c.ps.origin);
        a.predicted_end = w.predicted.as_ref().map_or([0.0; 3], |p| p.origin);
        if !a.server_started {
            why.push(format!(
                "the jump at {rest:?} yaw {yaw} did not mantle on the server"
            ));
            continue;
        }
        let off = |p: Vec3| {
            (
                ((p[0] - end[0]).powi(2) + (p[1] - end[1]).powi(2)).sqrt(),
                (p[2] - end[2]).abs(),
            )
        };
        let (sxy, sz) = off(a.server_end);
        let mut failures = Vec::new();
        if !a.predicted_started {
            failures.push("the client's prediction never started the mantle".to_owned());
        }
        if !a.blocked {
            failures.push("the landing spot was never blocked".into());
        }
        if sxy > END_TOLERANCE || sz > END_TOLERANCE {
            failures.push(format!(
                "the server ended at {:?}, not on the ledge at {end:?}",
                a.server_end
            ));
        }
        let (pxy, pz) = off(a.predicted_end);
        if pxy > END_TOLERANCE || pz > END_TOLERANCE {
            failures.push(format!(
                "the client predicted {:?}, the ledge is at {end:?}",
                a.predicted_end
            ));
        }
        if a.corrections != 0 {
            failures.push(format!(
                "the server corrected the client's prediction {} times",
                a.corrections
            ));
        }
        report
            .metrics
            .insert("mantle.prediction_corrections".into(), a.corrections as f64);
        report
            .metrics
            .insert("mantle.end_error_server".into(), f64::from(sxy.max(sz)));
        report
            .metrics
            .insert("mantle.end_error_client".into(), f64::from(pxy.max(pz)));
        if !failures.is_empty() {
            report.status = Status::Failed;
            report.reason = Some(failures.join("; "));
        } else {
            report.notes.push(format!(
                "mantled at {rest:?} yaw {yaw} to {:?}",
                a.server_end
            ));
        }
        if !server.all_script_errors.is_empty() {
            report.status = Status::Failed;
            report.reason = Some(format!(
                "{} script runtime errors",
                server.all_script_errors.len()
            ));
        }
        w.c.disconnect();
        return Ok(report);
    }
    fail(format!("no jump mantled live: {}", why.join("; ")))
}
