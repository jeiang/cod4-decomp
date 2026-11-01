// SPDX-License-Identifier: GPL-3.0-only
//! `net-antilag`: a shot at a moving player lands where the shooter saw the player, as stock antilag judges it
//! (KisakCOD `game/g_weapon.cpp` `G_AntiLagRewindClientPos`, `game/bullet.cpp` `Bullet_Fire`).
//!
//! Two real UDP clients (no window) join a headless war match on mp_crash on opposite teams. The stage finds an open
//! lane, puts the shooter 160 units in front of it and the target in the middle, and the target strafes from one
//! end of the lane to the other at `g_speed 300`. The shooter stamps its usercmds the way the game client does
//! (the moment it draws the others at, [`net::view::INTERP_DELAY_MS`] behind its clock estimate), aims each command
//! at the chest of the target as interpolated at the command's own time, and taps the trigger every 40 frames while
//! the target moves at speed.
//!
//! - The client's server info carries `g_antilag 1` (`ARCHIVE | SERVERINFO`, KisakCOD `game_mp/g_main_mp.cpp:614`).
//! - With `g_antilag 1`, at least 75% of the shots hit: the server rewinds the target to the command's time, the
//!   hit lands as stock's would. The server's `level.time - lastServerTime` at the shots is reported.
//! - With `g_antilag 0`, the same shots are judged against the live body, which has moved on: at most 35% hit.
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::ui::{UiEvent, cs, menu_response};
use server::client::{Session, Team};
use server::server::Server;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::{ANGLE_UNIT, PLAYER_MAXS, PmType, UserCmd, button};
use sim::world::World;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-antilag";
/// Joining, spawning and two rounds of shots; the frames run in real time.
const LIMIT: Duration = Duration::from_secs(240);
const FRAME: Duration = Duration::from_millis(16);
/// How far in front of the lane the shooter stands.
const DIST: f32 = 160.0;
/// Half the length of the lane the target strafes along.
const HALF: f32 = 140.0;
/// Where on the target the shooter aims, above its feet.
const CHEST: f32 = 40.0;
/// Frames between two taps of the trigger: the recoil of one has settled before the next.
const GAP: usize = 40;
/// Shots per round.
const SHOTS: u32 = 24;
/// The target's speed (units/s) at the command's time below which the shooter holds its fire: turning round at a
/// lane end.
const MIN_SPEED: f32 = 150.0;
const ON_MIN: f64 = 0.75;
const OFF_MAX: f64 = 0.35;
/// What walls a lane or a line of sight.
const WALLS: i32 = contents::SOLID | contents::PLAYERCLIP | contents::GLASS;

/// A client that joins `team` as the stock Spec Ops class (an MP5) and stamps its usercmds as the game client does.
pub(super) struct Player {
    pub(super) c: NetClient<UdpTransport>,
    team: &'static str,
    pub(super) own: Option<u16>,
    cmd_time: i32,
}

impl Player {
    pub(super) fn new(
        addr: SocketAddr,
        name: &str,
        qport: u16,
        team: &'static str,
    ) -> io::Result<Self> {
        let t = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        Ok(Self {
            c: NetClient::new(t, addr, name, "", qport),
            team,
            own: None,
            cmd_time: 0,
        })
    }

    /// Takes the packets and answers the join menus; the time the next usercmd is stamped with: the client's moment
    /// of the others, `INTERP_DELAY_MS` behind its clock estimate (`CL_FinishMove` stamps `cl.serverTime`).
    pub(super) fn pump(&mut self) -> Option<i32> {
        self.c.pump(Duration::from_millis(1));
        let mut answers = Vec::new();
        if let Some(ui) = self.c.ui() {
            for ev in ui.drain_events() {
                if let UiEvent::OpenMenu { name, .. } = ev {
                    if name.starts_with("team_") {
                        answers.push(menu_response(&name, self.team));
                    } else if name.starts_with("changeclass") {
                        answers.push(menu_response("changeclass", "specops_mp"));
                    }
                }
            }
        }
        for a in answers {
            self.c.command(&a);
        }
        let s = self.c.latest()?;
        let n = s.ps.client_num;
        if s.entity(n).is_some_and(|e| e.etype == etype::PLAYER) {
            self.own = Some(n);
        }
        let st = self.c.snaps.server_time(self.c.now_ms())?;
        self.cmd_time = (self.cmd_time + 1).max(st - net::view::INTERP_DELAY_MS);
        Some(self.cmd_time)
    }

    /// A usercmd looking at `pitch`/`yaw` (degrees).
    fn cmd(&self, server_time: i32, pitch: f32, yaw: f32) -> UserCmd {
        let delta = self.c.latest().map_or([0.0; 3], |s| s.ps.delta_angles);
        let pack = |a: f32, d: f32| (((a - d) / ANGLE_UNIT) as i32) & 0xffff;
        UserCmd {
            server_time,
            angles: [pack(pitch, delta[0]), pack(yaw, delta[1]), 0],
            ..UserCmd::default()
        }
    }

    fn server_info(&self, key: &str) -> Option<String> {
        let ui = self.c.ui_ref()?;
        Some(server::netsv::info_value(ui.config(cs::SERVERINFO), key).to_owned())
    }
}

pub(super) fn playing(server: &Server, p: &Player) -> bool {
    p.own.and_then(|n| server.game.client(n)).is_some_and(|c| {
        c.session == Session::Playing
            && matches!(c.team, Team::Axis | Team::Allies)
            && c.ps.pm_type == PmType::Normal
    })
}

/// Where a player stands on the floor below `p` (searched from 40 above to 100 below).
pub(super) fn ground(world: &World, p: [f32; 3]) -> Option<[f32; 3]> {
    let (top, bottom) = (p[2] + 40.0, p[2] - 100.0);
    let t = world.trace(
        [p[0], p[1], top],
        [p[0], p[1], bottom],
        sim::pm::PLAYER_MINS,
        PLAYER_MAXS,
        ENTITYNUM_NONE,
        WALLS,
    );
    (!t.start_solid && t.fraction < 1.0 && t.walkable)
        .then(|| [p[0], p[1], top + (bottom - top) * t.fraction + 1.0])
}

/// An open stretch of floor the target strafes along, and the spot the shooter sees all of it from.
struct Lane {
    shooter: [f32; 3],
    center: [f32; 3],
    /// The direction from the lane to the shooter (degrees): the target faces it and strafes.
    facing: f32,
}

/// The first spawn point with a flat, open lane through it and a clear view of the lane from `DIST` in front.
fn lane(server: &Server) -> Option<Lane> {
    let world = server.game.world.as_ref()?;
    let ents = server.game.content.clipmap()?.map_ents.clone()?;
    for sp in server::nav::spawn_points(&ents.entity_string) {
        let Some(c) = ground(world, sp.origin) else {
            continue;
        };
        for k in 0..8 {
            let facing = k as f32 * 45.0;
            let (s, co) = facing.to_radians().sin_cos();
            let Some(shooter) = ground(world, [c[0] + co * DIST, c[1] + s * DIST, c[2]]) else {
                continue;
            };
            let side = [-s, co];
            let at = |f: f32| [c[0] + side[0] * HALF * f, c[1] + side[1] * HALF * f, c[2]];
            let ends = [ground(world, at(-1.0)), ground(world, at(1.0))];
            let flat = |p: Option<[f32; 3]>| p.is_some_and(|p| (p[2] - c[2]).abs() < 12.0);
            if (shooter[2] - c[2]).abs() > 24.0 || !ends.into_iter().all(flat) {
                continue;
            }
            // The whole lane is clear above step height, plus a player's width at each end.
            let (a, b) = (at(-1.2), at(1.2));
            let sweep = world.trace(
                a,
                b,
                [-15.0, -15.0, 18.0],
                PLAYER_MAXS,
                ENTITYNUM_NONE,
                WALLS,
            );
            if sweep.start_solid || sweep.fraction < 1.0 {
                continue;
            }
            let eye = [shooter[0], shooter[1], shooter[2] + 60.0];
            let seen = [-1.2f32, -0.6, 0.0, 0.6, 1.2].into_iter().all(|f| {
                let p = at(f);
                [CHEST - 20.0, CHEST, CHEST + 20.0].into_iter().all(|z| {
                    world
                        .trace(
                            eye,
                            [p[0], p[1], p[2] + z],
                            [0.0; 3],
                            [0.0; 3],
                            ENTITYNUM_NONE,
                            WALLS,
                        )
                        .fraction
                        == 1.0
                })
            });
            if seen {
                return Some(Lane {
                    shooter,
                    center: c,
                    facing,
                });
            }
        }
    }
    None
}

/// One round of shots: how many fired, how many hit, and the server's `level.time - lastServerTime` at each.
#[derive(Default)]
struct Round {
    shots: u32,
    hits: u32,
    lag_ms: Vec<i32>,
}

impl Round {
    fn rate(&self) -> f64 {
        f64::from(self.hits) / f64::from(self.shots.max(1))
    }
}

struct Match {
    server: Server,
    shooter: Player,
    target: Player,
    lane: Option<Lane>,
    /// Which way the target strafes: 1 to its right, -1 to its left.
    dir: f32,
    deadline: Instant,
}

impl Match {
    /// One frame. With `fire`, the shooter taps the trigger if the target moves at speed at its command's time;
    /// whether it did.
    fn frame(&mut self, fire: bool) -> bool {
        self.server.run_for(FRAME);
        // The target soaks up every hit.
        if let Some(e) = self.target.own.and_then(|n| self.server.game.ent_mut(n)) {
            e.health = 100;
        }
        if let Some(t) = self.target.pump() {
            let cmd = match &self.lane {
                Some(l) => {
                    let o = self.target.c.latest().map_or(l.center, |s| s.ps.origin);
                    // How far along its right the target is from the middle of the lane.
                    let (s, c) = l.facing.to_radians().sin_cos();
                    let along = (o[0] - l.center[0]) * s - (o[1] - l.center[1]) * c;
                    if along > HALF && self.dir > 0.0 || along < -HALF && self.dir < 0.0 {
                        self.dir = -self.dir;
                    }
                    let mut cmd = self.target.cmd(t, 0.0, l.facing);
                    cmd.rightmove = (127.0 * self.dir) as i8;
                    cmd
                }
                None => UserCmd {
                    server_time: t,
                    ..UserCmd::default()
                },
            };
            self.target.c.send_cmd(cmd);
        }
        let Some(t) = self.shooter.pump() else {
            return false;
        };
        let (Some(me), Some(them)) = (self.shooter.own, self.target.own) else {
            self.shooter.c.send_cmd(UserCmd {
                server_time: t,
                ..UserCmd::default()
            });
            return false;
        };
        let eye = self.shooter.c.latest().map(|s| {
            let o = s.ps.origin;
            [o[0], o[1], o[2] + s.ps.view_height_current]
        });
        let at = |time: i32| {
            self.shooter
                .c
                .snaps
                .interpolate(time, Some(me))
                .into_iter()
                .find(|e| e.number == them && e.etype == etype::PLAYER)
                .map(|e| e.origin)
        };
        let (Some(eye), Some(now), Some(before)) = (eye, at(t), at(t - 32)) else {
            self.shooter.c.send_cmd(UserCmd {
                server_time: t,
                ..UserCmd::default()
            });
            return false;
        };
        let speed = ((now[0] - before[0]).powi(2) + (now[1] - before[1]).powi(2)).sqrt() / 0.032;
        let d = [now[0] - eye[0], now[1] - eye[1], now[2] + CHEST - eye[2]];
        let yaw = d[1].atan2(d[0]).to_degrees();
        let pitch = -d[2].atan2(d[0].hypot(d[1])).to_degrees();
        let mut cmd = self.shooter.cmd(t, pitch, yaw);
        let pressed = fire && self.lane.is_some() && speed >= MIN_SPEED;
        if pressed {
            cmd.buttons = button::ATTACK;
        }
        self.shooter.c.send_cmd(cmd);
        pressed
    }

    /// Fires `SHOTS` taps at the strafing target and counts what the server made of them.
    fn round(&mut self) -> Result<Round, String> {
        let me = self.shooter.own.ok_or("the shooter has no body")?;
        let count = |m: &Match| {
            m.server
                .game
                .client(me)
                .map_or((0, 0), |c| (c.shots, c.hits))
        };
        let (shots0, hits0) = count(self);
        let mut r = Round::default();
        let mut since = GAP;
        loop {
            let (shots, hits) = count(self);
            (r.shots, r.hits) = (shots - shots0, hits - hits0);
            if r.shots >= SHOTS {
                break;
            }
            if Instant::now() > self.deadline {
                return Err(format!(
                    "{} of {SHOTS} shots fired before the time ran out",
                    r.shots
                ));
            }
            let pressed = self.frame(since >= GAP);
            since = if pressed { 0 } else { since + 1 };
            if since == 1 {
                // The tap reached the server this frame or the next: what its lastServerTime was behind by.
                if let Some(c) = self.server.game.client(me) {
                    r.lag_ms
                        .push(self.server.game.level.time - c.last_server_time);
                }
            }
        }
        // The last shot's hit lands before the count is read.
        for _ in 0..10 {
            self.frame(false);
        }
        let (shots, hits) = count(self);
        (r.shots, r.hits) = (shots - shots0, hits - hits0);
        Ok(r)
    }
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
        "set g_speed 300",
        "set sv_mapRotation \"gametype war map mp_crash\"",
        "map mp_crash",
    ] {
        if let Err(e) = server.exec_line(line) {
            return fail(format!("{line}: {e}"));
        }
    }
    let shooter = Player::new(addr, "lagshooter", 7301, "axis")?;
    let target = Player::new(addr, "lagtarget", 7302, "allies")?;
    let mut m = Match {
        server,
        shooter,
        target,
        lane: None,
        dir: 1.0,
        deadline: Instant::now() + LIMIT,
    };
    while !(playing(&m.server, &m.shooter) && playing(&m.server, &m.target)) {
        if Instant::now() > m.deadline {
            return fail(format!(
                "the clients never got bodies (shooter {}, target {})",
                playing(&m.server, &m.shooter),
                playing(&m.server, &m.target)
            ));
        }
        m.frame(false);
    }
    let (Some(sn), Some(tn)) = (m.shooter.own, m.target.own) else {
        return fail("a client has no body".into());
    };
    let Some(l) = lane(&m.server) else {
        return fail("mp_crash has no open lane by a spawn point".into());
    };
    m.server.game.teleport(sn, l.shooter);
    m.server.game.teleport(tn, l.center);
    m.lane = Some(l);
    for _ in 0..60 {
        m.frame(false);
    }
    let mut why: Vec<String> = Vec::new();
    let mut report = StageReport::new(NAME, Status::Passed);
    let info = m.shooter.server_info("g_antilag");
    if info.as_deref() != Some("1") {
        why.push(format!("the server info has g_antilag {info:?}, not 1"));
    }
    let record = |report: &mut StageReport, key: &str, r: &Round| {
        report.metrics.insert(format!("{key}.hit_rate"), r.rate());
        report
            .metrics
            .insert(format!("{key}.shots"), f64::from(r.shots));
        let mean =
            r.lag_ms.iter().map(|&x| f64::from(x)).sum::<f64>() / r.lag_ms.len().max(1) as f64;
        report.metrics.insert(format!("{key}.rewind_ms"), mean);
    };
    let on = match m.round() {
        Ok(r) => r,
        Err(e) => return fail(format!("g_antilag 1: {e}")),
    };
    record(&mut report, "antilag_on", &on);
    if on.rate() < ON_MIN {
        why.push(format!(
            "g_antilag 1: {} of {} shots hit ({:.0}%, under {:.0}%); the server's lastServerTime lagged {:?} ms",
            on.hits,
            on.shots,
            on.rate() * 100.0,
            ON_MIN * 100.0,
            on.lag_ms
        ));
    }
    if let Err(e) = m.server.exec_line("set g_antilag 0") {
        return fail(format!("set g_antilag 0: {e}"));
    }
    for _ in 0..20 {
        m.frame(false);
    }
    let info = m.shooter.server_info("g_antilag");
    if info.as_deref() != Some("0") {
        why.push(format!(
            "after set g_antilag 0 the server info has {info:?}"
        ));
    }
    let off = match m.round() {
        Ok(r) => r,
        Err(e) => return fail(format!("g_antilag 0: {e}")),
    };
    record(&mut report, "antilag_off", &off);
    if off.rate() > OFF_MAX {
        why.push(format!(
            "g_antilag 0: {} of {} shots hit ({:.0}%, over {:.0}%): the live body was where the shooter saw it",
            off.hits,
            off.shots,
            off.rate() * 100.0,
            OFF_MAX * 100.0
        ));
    }
    m.shooter.c.disconnect();
    m.target.c.disconnect();
    for _ in 0..5 {
        m.server.run_for(FRAME);
    }
    if !why.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(why.join("; "));
    }
    Ok(report)
}
