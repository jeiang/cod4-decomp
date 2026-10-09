// SPDX-License-Identifier: GPL-3.0-only
//! `net-match`: a headless server runs a team deathmatch with bots on mp_crash while real UDP
//! clients (no window, no GPU) connect over loopback, spawn, walk and watch. Asserts that every
//! client connects and is assigned a body, that the server runs its usercmds (the client's own
//! player moves), that the clients receive the bots and interpolate them smoothly, and reports
//! the bandwidth each client costs and the server tick time spent per client.
use crate::perf::Percentiles;
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::predict::{Env, PlayerBoxes, Predictor};
use net::ui::{AutoJoin, UiEvent};
use server::server::Server;
use sim::pm::{ANGLE_UNIT, Params, UserCmd};
use sim::weapon::WeaponTable;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const NAME: &str = "net-match";
const CLIENTS: usize = 4;
const BOTS: usize = 12;
/// Client frames recorded after spawning.
const FRAMES: usize = 300;
const FRAME: Duration = Duration::from_millis(33);
/// Horizontal distance under which another player counts as touching: a player is about 30 units wide.
const CROWD: f32 = 40.0;
/// Uncrowded prediction passes a client must make for the corrections check to mean anything.
const MIN_COUNTED: u64 = FRAMES as u64 / 3;
/// Longest a step of an interpolated player may be between two client frames before it counts
/// as a snap: sprinting is about 9 units a frame, so this is generous.
const SMOOTH_STEP: f32 = 40.0;
/// Waiting for the game to give a client a body depends on the match start, not the runner; the
/// limit only stops a hung run.
const SPAWN_LIMIT: Duration = Duration::from_secs(180);

/// What a client needs to predict its own movement: the map's collision and the movement rules.
#[derive(Clone)]
struct Rules {
    clipmap: Arc<assets::zone::clipmap::Clipmap>,
    weapons: WeaponTable,
    params: Params,
}

#[derive(Default)]
struct Result {
    /// Prediction passes made, and the ones that disagreed with the last, counting only passes with no other player
    /// within `CROWD` of the client: the server shoves players apart between a client's commands from where the
    /// others are now, which a snapshot's older positions cannot reproduce, so a crowded pass says nothing about
    /// the prediction code.
    predictions: u64,
    corrections: u64,
    /// Passes skipped because another player was within `CROWD`.
    crowded: u64,
    max_ahead: usize,
    connected: bool,
    refused: Option<String>,
    spawned: bool,
    start: [f32; 3],
    /// The farthest the server's own snapshots placed the client from where it spawned (horizontal). The walk turns a
    /// quarter every second, so it loops back on itself and its end point says nothing about whether it moved.
    reach: f32,
    seen_max: usize,
    steps: u64,
    snaps: u64,
    max_step: f32,
    snapshots: u64,
    /// Times the client's health fell and stayed above zero, and the times its `damage_event` counted up.
    hits: u64,
    damage_events: u64,
    bytes_in: u64,
    bytes_out: u64,
    secs: f64,
    unusable: u64,
    /// Script menus the server opened for the client, in order.
    menus: Vec<String>,
    /// The most hud elements one snapshot listed, and configstrings the table held.
    max_hud: usize,
    materials: usize,
    /// Earthquake events that reached the client, and the strongest camera shake its view felt.
    quakes: u64,
    shake_max: f32,
}

/// What a person at the menus does: takes what the server opens and answers it.
fn service_ui(c: &mut NetClient<UdpTransport>, join: &mut AutoJoin, r: &mut Result) {
    let Some(ui) = c.ui() else { return };
    let events = ui.drain_events();
    r.max_hud = r.max_hud.max(ui.hud().len());
    r.materials = r.materials.max(ui.materials().count());
    let mut answers = Vec::new();
    for ev in &events {
        if let UiEvent::OpenMenu { name, .. } = ev {
            r.menus.push(name.clone());
        }
        answers.extend(join.step(ev));
    }
    for a in answers {
        c.command(&a);
    }
}

fn client(
    addr: SocketAddr,
    id: usize,
    rules: &Rules,
    stop: &AtomicBool,
    ready: &AtomicUsize,
) -> Result {
    let mut r = Result::default();
    let mut boxes = PlayerBoxes::new(rules.clipmap.clone());
    let mut pred = Predictor::default();
    let Ok(t) = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))) else {
        return r;
    };
    let mut c = NetClient::new(t, addr, &format!("net{id}"), "", 5000 + id as u16);
    let deadline = Instant::now() + SPAWN_LIMIT;
    let mut join = AutoJoin::default();
    // Connect, then wait for a body: the snapshot lists the client's own player.
    let mut own: Option<u16> = None;
    while own.is_none() && Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        c.pump(Duration::from_millis(10));
        if let Some(reason) = c.refused() {
            r.refused = Some(reason.to_owned());
            return r;
        }
        if let Some(s) = c.latest() {
            r.connected = true;
            let n = s.ps.client_num;
            // Before the game gives the client a body its player state is blank (`client_num` 0 or stale, a bot's
            // entity). The bots took the first slots, so a real client's number is at least `BOTS`.
            if usize::from(n) >= BOTS && s.entity(n).is_some_and(|e| e.etype == etype::PLAYER) {
                own = Some(n);
            }
        }
        service_ui(&mut c, &mut join, &mut r);
        if r.connected {
            c.send();
        }
    }
    let Some(own) = own else { return r };
    r.spawned = true;
    ready.fetch_add(1, Ordering::SeqCst);
    r.start = c.latest().map_or([0.0; 3], |s| s.ps.origin);
    let began = Instant::now();
    let mut last: HashMap<u16, [f32; 3]> = HashMap::new();
    let mut cmd_time = 0;
    let mut shakes = sim::shake::CameraShakes::default();
    let mut quake_seq = None;
    let mut vitals: Option<(i32, u8)> = None;
    let mut next = Instant::now();
    for frame in 0..FRAMES {
        next += FRAME;
        while Instant::now() < next {
            c.pump(
                next.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5)),
            );
        }
        service_ui(&mut c, &mut join, &mut r);
        let now = c.now_ms();
        let Some(st) = c.snaps.server_time(now) else {
            continue;
        };
        cmd_time = (cmd_time + 1).max(st);
        // Walk forward, turning a quarter every second so walls do not stop the run.
        let yaw = (frame / 30) as f32 * 90.0
            + 13.0 * c.latest().map_or(0, |s| s.ps.client_num as i32) as f32;
        let cmd = UserCmd {
            server_time: cmd_time,
            forwardmove: 127,
            angles: [0, (yaw / ANGLE_UNIT) as i32 & 0xffff, 0],
            ..UserCmd::default()
        };
        pred.push(cmd);
        c.send_cmd(cmd);
        if let Some(s) = c.latest() {
            let ps = &s.ps;
            if let Some((health, event)) = vitals {
                r.hits += u64::from(ps.health > 0 && ps.health < health);
                if ps.damage_event != event {
                    r.damage_events += 1;
                }
            }
            vitals = Some((ps.health, ps.damage_event));
            boxes.sync(s);
            let env = Env {
                world: boxes.world(),
                weapons: &rules.weapons,
                params: &rules.params,
                time: cmd.server_time,
            };
            let before = pred.corrections;
            let p = pred.predict(s, &env);
            let crowd = s
                .entities
                .iter()
                .filter(|e| e.etype == etype::PLAYER && e.number != own)
                .any(|e| {
                    (e.origin[0] - p.ps.origin[0]).hypot(e.origin[1] - p.ps.origin[1]) < CROWD
                });
            if crowd {
                r.crowded += 1;
            } else {
                r.predictions += 1;
                r.corrections += pred.corrections - before;
            }
            r.max_ahead = r.max_ahead.max(p.replayed);
            r.reach = r
                .reach
                .max((s.ps.origin[0] - r.start[0]).hypot(s.ps.origin[1] - r.start[1]));
        }
        if let Some(s) = c.latest() {
            for e in s.entities.iter().filter(|e| e.etype == etype::EVENT) {
                if let Some(q) = server::tempev::Earthquake::decode(e)
                    && quake_seq != Some((e.number, e.event_seq))
                {
                    quake_seq = Some((e.number, e.event_seq));
                    r.quakes += 1;
                    shakes.start(st, s.ps.origin, q.scale, q.duration_ms, e.origin, q.radius);
                }
            }
            r.shake_max = r.shake_max.max(shakes.strength(st, s.ps.origin));
        }
        let ents = c
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own));
        r.seen_max = r
            .seen_max
            .max(ents.iter().filter(|e| e.etype == etype::PLAYER).count());
        for e in ents.iter().filter(|e| e.etype == etype::PLAYER) {
            if let Some(p) = last.insert(e.number, e.origin) {
                let d = ((e.origin[0] - p[0]).powi(2)
                    + (e.origin[1] - p[1]).powi(2)
                    + (e.origin[2] - p[2]).powi(2))
                .sqrt();
                r.steps += 1;
                r.max_step = r.max_step.max(d);
                if d > SMOOTH_STEP {
                    r.snaps += 1;
                }
            }
        }
    }
    r.secs = began.elapsed().as_secs_f64();
    if let Some(s) = c.stats() {
        r.snapshots = s.packets_in;
        r.bytes_in = s.bytes_in;
        r.bytes_out = s.bytes_out;
    }
    r.unusable = c.unusable();
    c.disconnect();
    r
}

/// What a person who never picks a team sees: a free-flying spectator, which the server moves with the movement code.
#[derive(Default)]
struct Flight {
    connected: bool,
    spectator: bool,
    start: [f32; 3],
    end: [f32; 3],
}

/// Joins once the players have bodies, answers no menu, and flies straight ahead.
fn spectator(addr: SocketAddr, stop: &AtomicBool, ready: &AtomicUsize) -> Flight {
    let mut f = Flight::default();
    let deadline = Instant::now() + SPAWN_LIMIT;
    while ready.load(Ordering::SeqCst) < CLIENTS && Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return f;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let Ok(t) = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))) else {
        return f;
    };
    let mut c = NetClient::new(t, addr, "netspec", "", 5900);
    let mut cmd_time = 0;
    let mut flying = 0;
    while flying < 90 && Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        c.pump(Duration::from_millis(33));
        if let Some(ui) = c.ui() {
            ui.drain_events();
        }
        let Some(s) = c.latest() else { continue };
        f.connected = true;
        f.spectator = s.ps.pm_type == sim::pm::PmType::Spectator;
        if !f.spectator {
            continue;
        }
        if flying == 0 {
            f.start = s.ps.origin;
        }
        f.end = s.ps.origin;
        let Some(st) = c.snaps.server_time(c.now_ms()) else {
            continue;
        };
        cmd_time = (cmd_time + 1).max(st);
        c.send_cmd(UserCmd {
            server_time: cmd_time,
            forwardmove: 127,
            ..UserCmd::default()
        });
        flying += 1;
    }
    c.disconnect();
    f
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
    let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(
        server::server::TickSample,
        usize,
    )>::new()));
    let peers = std::rc::Rc::new(std::cell::Cell::new(0usize));
    {
        let samples = samples.clone();
        let peers = peers.clone();
        server.on_tick = Some(Box::new(move |t| {
            samples.borrow_mut().push((*t, peers.get()))
        }));
    }
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        // Movement rules other than the stock ones: a client that assumed g_speed 190 or the stock jump and
        // friction would be corrected on every snapshot, which the corrections check below catches.
        "set g_speed 250",
        "set jump_height 45",
        "set friction 6",
        "set sv_mapRotation \"gametype war map mp_crash\"",
        "map mp_crash",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    if let Err(e) = server.exec_line(&format!("bots {BOTS}")) {
        return Ok(StageReport::new(NAME, Status::Failed).with_reason(e));
    }

    let Some(clipmap) = server.game.content.clipmap().cloned() else {
        return Ok(
            StageReport::new(NAME, Status::Failed).with_reason("the map has no collision data")
        );
    };
    let rules = Rules {
        clipmap,
        weapons: server.game.weapons.clone(),
        params: server.game.pm_params.clone(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..CLIENTS)
        .map(|i| {
            let (stop, ready, rules) = (stop.clone(), ready.clone(), rules.clone());
            std::thread::spawn(move || client(addr, i, &rules, &stop, &ready))
        })
        .collect();
    let flier = {
        let (stop, ready) = (stop.clone(), ready.clone());
        std::thread::spawn(move || spectator(addr, &stop, &ready))
    };
    // The server runs in this thread until every client thread has finished.
    let mut quaked = false;
    while !handles.iter().all(|h| h.is_finished()) || !flier.is_finished() {
        server.run_for(Duration::from_millis(100));
        // Once everyone has a body, an earthquake from the first client's position: every client is inside its radius.
        if !quaked && ready.load(Ordering::SeqCst) == CLIENTS {
            let at = server
                .game
                .connected_clients()
                .next()
                .map(|(_, c)| c.ps.origin);
            if let Some(at) = at {
                quaked = true;
                server.game.earthquake(at, 0.6, 15_000, 16_000.0);
            }
        }
        peers.set(server.net_clients());
    }
    let results: Vec<Result> = handles
        .into_iter()
        .map(|h| h.join().unwrap_or_default())
        .collect();
    let flight = flier.join().unwrap_or_default();
    stop.store(true, Ordering::Relaxed);
    let stats = server.net_stats().unwrap_or_default();

    let mut report = StageReport::new(NAME, Status::Passed);
    let mut failures = Vec::new();
    // Team deathmatch scores a kill for the killer's team through `setteamscore`, so a team that out-killed the
    // other must not hold the lower score (the allies/axis indices once swapped).
    let kills = |team: server::client::Team| -> i32 {
        server
            .game
            .connected_clients()
            .filter(|(_, c)| c.team == team)
            .map(|(_, c)| c.kills)
            .sum()
    };
    let (axis_kills, allies_kills) = (
        kills(server::client::Team::Axis),
        kills(server::client::Team::Allies),
    );
    let (axis, allies) = (server.game.team_score[1], server.game.team_score[2]);
    if allies + axis == 0 && axis_kills + allies_kills > 0 {
        failures.push("kills were made but no team score was set".to_owned());
    }
    if (axis_kills > allies_kills && axis < allies) || (allies_kills > axis_kills && allies < axis)
    {
        failures.push(format!(
            "team scores follow the wrong team: kills axis {axis_kills} allies {allies_kills}, scores axis {axis} allies {allies}"
        ));
    }
    // A spectator who joined and chose no team flies: the server runs its movement, so its origin leaves the spot.
    let flown = ((flight.end[0] - flight.start[0]).powi(2)
        + (flight.end[1] - flight.start[1]).powi(2)
        + (flight.end[2] - flight.start[2]).powi(2))
    .sqrt();
    if !flight.connected || !flight.spectator {
        failures.push("the team-less client never became a spectator".to_owned());
    } else if flown < 100.0 {
        failures.push(format!(
            "the free spectator flew {flown:.0} units in 90 frames"
        ));
    }
    report
        .metrics
        .insert("spectator.flown_units".into(), flown as f64);
    for (i, r) in results.iter().enumerate() {
        if let Some(why) = &r.refused {
            failures.push(format!("client {i} refused: {why}"));
        } else if !r.connected {
            failures.push(format!("client {i} never received a snapshot"));
        } else if !r.spawned {
            failures.push(format!("client {i} was never given a body"));
        } else {
            if !r.menus.iter().any(|m| m == "team_marinesopfor")
                || !r.menus.iter().any(|m| m.starts_with("changeclass"))
            {
                failures.push(format!(
                    "client {i}: the scripts opened {:?} instead of the team and class menus",
                    r.menus
                ));
            }
            if r.materials == 0 {
                failures.push(format!("client {i} was told no materials"));
            }
            let moved = r.reach;
            if moved < 50.0 {
                failures.push(format!(
                    "client {i}: the server did not move it ({moved:.0} units in {FRAMES} frames)"
                ));
            }
            if r.seen_max < BOTS / 2 {
                failures.push(format!(
                    "client {i} saw at most {} other players of {BOTS} bots",
                    r.seen_max
                ));
            }
            // Prediction runs ahead of the snapshot and the server seldom disagrees with it.
            if r.max_ahead == 0 {
                failures.push(format!(
                    "client {i}: prediction never ran ahead of the server"
                ));
            }
            // Walking clear of other players the server and the replay agree; passes beside another player are
            // not counted (see `Result::predictions`).
            if r.predictions < MIN_COUNTED {
                failures.push(format!(
                    "client {i}: only {} of {} prediction passes were clear of other players ({} crowded)",
                    r.predictions,
                    r.predictions + r.crowded,
                    r.crowded
                ));
            }
            if r.corrections * 7 > r.predictions {
                failures.push(format!(
                    "client {i}: the server corrected {} of {} predictions",
                    r.corrections, r.predictions
                ));
            }
            if r.quakes == 0 {
                failures.push(format!("client {i} was never told of the earthquake"));
            } else if r.shake_max <= 0.0 {
                failures.push(format!(
                    "client {i}'s camera did not shake in the earthquake"
                ));
            }
            // A bot's bullet that wounded the player reached its screen as a hit. (Whether a bot hits within the run
            // is up to the match; with no hit the stage says so rather than passing silently.)
            if r.hits > 0 && r.damage_events == 0 {
                failures.push(format!(
                    "client {i}: wounded {} times but damage_event never counted up",
                    r.hits
                ));
            }
            if r.steps == 0 || r.snaps * 100 > r.steps {
                failures.push(format!("client {i}: {} of {} interpolated steps jumped over {SMOOTH_STEP} units (max {:.0})", r.snaps, r.steps, r.max_step));
            }
        }
    }
    let live: Vec<&Result> = results
        .iter()
        .filter(|r| r.spawned && r.secs > 0.0)
        .collect();
    if !live.is_empty() {
        let n = live.len() as f64;
        let secs: f64 = live.iter().map(|r| r.secs).sum::<f64>() / n;
        let mean = |f: fn(&Result) -> f64| live.iter().map(|r| f(r)).sum::<f64>() / n;
        report.metrics.insert(
            "client.down_bytes_per_sec".into(),
            mean(|r| r.bytes_in as f64) / secs,
        );
        report.metrics.insert(
            "client.up_bytes_per_sec".into(),
            mean(|r| r.bytes_out as f64) / secs,
        );
        report.metrics.insert(
            "client.snapshots_per_sec".into(),
            mean(|r| r.snapshots as f64) / secs,
        );
        report.metrics.insert(
            "client.crowded_passes".into(),
            live.iter().map(|r| r.crowded as f64).sum(),
        );
        let wounds: u64 = live.iter().map(|r| r.hits).sum();
        report.metrics.insert("client.wounds".into(), wounds as f64);
        if wounds == 0 {
            report
                .notes
                .push("damage feedback untested: no bot wounded a client".into());
        }
        report.metrics.insert(
            "client.damage_events".into(),
            live.iter().map(|r| r.damage_events as f64).sum(),
        );
        report.metrics.insert(
            "client.max_hud_elems".into(),
            live.iter().map(|r| r.max_hud as f64).fold(0.0, f64::max),
        );
        report.metrics.insert(
            "client.camera_shake_max".into(),
            live.iter().map(|r| r.shake_max as f64).fold(0.0, f64::max),
        );
        report.metrics.insert(
            "client.max_players_seen".into(),
            live.iter().map(|r| r.seen_max).max().unwrap_or(0) as f64,
        );
        report.metrics.insert(
            "client.max_interp_step".into(),
            live.iter().map(|r| r.max_step as f64).fold(0.0, f64::max),
        );
        report.metrics.insert(
            "client.interp_snaps".into(),
            live.iter().map(|r| r.snaps as f64).sum(),
        );
        report.metrics.insert(
            "client.unusable_snapshots".into(),
            live.iter().map(|r| r.unusable as f64).sum(),
        );
        for (k, v) in [
            (
                "client.prediction_passes",
                live.iter().map(|r| r.predictions as f64).sum(),
            ),
            (
                "client.prediction_corrections",
                live.iter().map(|r| r.corrections as f64).sum(),
            ),
            (
                "client.prediction_max_ahead",
                live.iter().map(|r| r.max_ahead as f64).fold(0.0, f64::max),
            ),
        ] {
            report.metrics.insert(k.into(), v);
        }
    }
    // Server cost per client: ticks while every client was connected.
    let all = CLIENTS;
    let samples = samples.borrow();
    let loaded: Vec<f64> = samples
        .iter()
        .filter(|(_, p)| *p >= all)
        .map(|(t, _)| t.net_ms)
        .collect();
    let totals: Vec<f64> = samples
        .iter()
        .filter(|(_, p)| *p >= all)
        .map(|(t, _)| t.total_ms)
        .collect();
    if let (Some(n), Some(t)) = (
        Percentiles::from_samples(&loaded),
        Percentiles::from_samples(&totals),
    ) {
        report.metrics.insert("server.net_ms_p50".into(), n.p50);
        report.metrics.insert("server.net_ms_p99".into(), n.p99);
        report
            .metrics
            .insert("server.net_ms_per_client_p50".into(), n.p50 / all as f64);
        report.metrics.insert("server.tick_ms_p50".into(), t.p50);
        report.metrics.insert("server.tick_ms_p99".into(), t.p99);
    } else {
        failures.push(format!(
            "no server ticks ran with all {all} clients connected"
        ));
    }
    report
        .metrics
        .insert("server.snapshots_out".into(), stats.snapshots_out as f64);
    report
        .metrics
        .insert("server.bytes_out".into(), stats.bytes_out as f64);
    report
        .metrics
        .insert("server.bytes_in".into(), stats.bytes_in as f64);
    if !server.all_script_errors.is_empty() {
        failures.push(format!(
            "{} script runtime errors",
            server.all_script_errors.len()
        ));
    }
    report.notes.push(format!(
        "{CLIENTS} clients and {BOTS} bots on mp_crash over loopback UDP"
    ));
    if !failures.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(failures.join("; "));
    }
    Ok(report)
}
