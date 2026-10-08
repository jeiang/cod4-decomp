// SPDX-License-Identifier: GPL-3.0-only
//! `net-killcam`: the stock killcam, replayed from the server's state ring. A headless server
//! runs team deathmatch with bots; one real UDP client (no window) joins through the menus, and
//! once it has a body a bot's rifle kills it. The stock `_killcam.gsc` then puts the client in
//! the spectator state watching its killer a few seconds in the past. Asserts that the client's
//! snapshots turn into a follow view of the killer ([`net::snapshot::Follow`]) that is a replay
//! (the archived player state is seconds older than the server time, the client is still told
//! who it is), that the killcam's own hud text arrives, that the view ends, and that the client
//! respawns afterwards.
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::ui::{AutoJoin, he};
use server::client::Session;
use server::server::Server;
use sim::pm::PmType;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

const NAME: &str = "net-killcam";
const BOTS: usize = 6;
const FRAME: Duration = Duration::from_millis(33);
/// Frames after spawning before the kill, so the ring holds a second of history.
const LIVE_FRAMES: usize = 90;
/// Waiting for the game (match start, killcam length) depends on it, not the runner.
const LIMIT: Duration = Duration::from_secs(240);

#[derive(Default)]
struct Seen {
    spawned: bool,
    own: u16,
    killcam_snaps: u32,
    max_archive_ms: u32,
    /// Smallest age of the archived player state against the server time during the replay.
    min_ps_age: i32,
    followed: Option<u16>,
    wrong_identity: u32,
    skip_text: bool,
    ended: bool,
    respawned: bool,
}

fn client(
    addr: SocketAddr,
    own_slot: &AtomicI32,
    kill_now: &AtomicBool,
    stop: &AtomicBool,
) -> Seen {
    let mut s = Seen {
        min_ps_age: i32::MAX,
        ..Seen::default()
    };
    let Ok(t) = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))) else {
        return s;
    };
    let mut c = NetClient::new(t, addr, "kcclient", "", 5200);
    let mut join = AutoJoin::default();
    let deadline = Instant::now() + LIMIT;
    let mut live = 0;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) && !s.respawned {
        c.pump(FRAME);
        if c.refused().is_some() {
            return s;
        }
        if c.latest().is_some() {
            c.send();
        }
        if let Some(ui) = c.ui() {
            let answers: Vec<String> = ui
                .drain_events()
                .iter()
                .filter_map(|e| join.step(e))
                .collect();
            for a in answers {
                c.command(&a);
            }
        }
        let Some(snap) = c.latest().cloned() else {
            continue;
        };
        let alive = snap
            .entity(snap.own())
            .is_some_and(|e| e.etype == etype::PLAYER);
        if let Some(f) = snap.follow {
            if f.archive_ms > 0 {
                s.killcam_snaps += 1;
                s.max_archive_ms = s.max_archive_ms.max(f.archive_ms);
                s.min_ps_age = s.min_ps_age.min(snap.server_time - snap.ps.command_time);
                s.followed = Some(f.followed);
                if f.own != s.own || f.followed == f.own || snap.ps.client_num != f.followed {
                    s.wrong_identity += 1;
                }
                if let Some(ui) = c.ui() {
                    s.skip_text |= ui
                        .hud()
                        .iter()
                        .any(|h| h.kind == he::TEXT && ui.localized(h.text).contains("PRESS_TO"));
                }
            }
        } else if s.killcam_snaps > 0 {
            s.ended = true;
            if alive && snap.ps.pm_type == PmType::Normal {
                s.respawned = true;
            }
        } else if alive {
            if !s.spawned {
                s.spawned = true;
                s.own = snap.ps.client_num;
                own_slot.store(i32::from(s.own), Ordering::SeqCst);
            }
            live += 1;
            if live == LIVE_FRAMES {
                kill_now.store(true, Ordering::SeqCst);
            }
        }
    }
    c.disconnect();
    s
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
    let (own, kill_now, stop) = (
        Arc::new(AtomicI32::new(-1)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let handle = {
        let (own, kill_now, stop) = (own.clone(), kill_now.clone(), stop.clone());
        std::thread::spawn(move || client(addr, &own, &kill_now, &stop))
    };
    let mut killed_by = None;
    while !handle.is_finished() {
        server.run_for(Duration::from_millis(100));
        let victim = own.load(Ordering::SeqCst);
        if killed_by.is_none() && kill_now.load(Ordering::SeqCst) && victim >= 0 {
            let bot = server
                .game
                .clients
                .iter()
                .position(|c| c.bot && c.session == Session::Playing);
            if let Some(b) = bot {
                let _ = server.exec_line(&format!("devkill {victim} {b}"));
                killed_by = Some(b as u16);
            }
        }
    }
    let seen = handle.join().unwrap_or_default();
    stop.store(true, Ordering::Relaxed);

    let mut failures = Vec::new();
    if !seen.spawned {
        failures.push("the client was never given a body".to_owned());
    } else if killed_by.is_none() {
        failures.push("no bot was playing to kill the client".to_owned());
    } else if seen.killcam_snaps == 0 {
        failures.push("the client was killed but never received a killcam snapshot".to_owned());
    } else {
        if seen.followed != killed_by {
            failures.push(format!(
                "the killcam followed {:?}, the killer was {killed_by:?}",
                seen.followed
            ));
        }
        if seen.wrong_identity > 0 {
            failures.push(format!(
                "{} killcam snapshots mixed up who the client is",
                seen.wrong_identity
            ));
        }
        if seen.max_archive_ms < 2000 {
            failures.push(format!(
                "the killcam looked back only {} ms; the stock one starts at least 2.5 s back",
                seen.max_archive_ms
            ));
        }
        if seen.min_ps_age < 1500 {
            failures.push(format!(
                "the killer's archived player state was only {} ms old: not a replay",
                seen.min_ps_age
            ));
        }
        if !seen.skip_text {
            failures.push("the killcam's own hud text (skip prompt) never arrived".to_owned());
        }
        if !seen.ended {
            failures.push("the killcam never ended".to_owned());
        } else if !seen.respawned {
            failures.push("the client did not respawn after the killcam".to_owned());
        }
    }
    let mut report = StageReport::new(
        NAME,
        if failures.is_empty() {
            Status::Passed
        } else {
            Status::Failed
        },
    );
    if !failures.is_empty() {
        report.reason = Some(failures.join("; "));
    }
    report
        .metrics
        .insert("killcam.snapshots".into(), f64::from(seen.killcam_snaps));
    report.metrics.insert(
        "killcam.max_archive_ms".into(),
        f64::from(seen.max_archive_ms),
    );
    Ok(report)
}
