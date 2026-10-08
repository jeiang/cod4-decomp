// SPDX-License-Identifier: GPL-3.0-only
//! `net-ui`: what the stock scripts tell a person's screen. A headless server runs Domination
//! with bots; one real UDP client (no window) answers the menus the scripts open the way the
//! stock menus would (team, then class) and records everything the UI store delivers. Asserts
//! that the scripts opened the team and class menus by themselves, that the gametype's hud
//! elements and its objectives arrive and resolve through the configstring table, that client
//! dvars and print lines come through in order, and reports the HUD cost in bytes per second.
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::ui::{AutoJoin, PrintKind, UiEvent, he};
use server::server::Server;
use std::collections::BTreeSet;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-ui";
const BOTS: usize = 6;
/// Client frames recorded once the player has a body.
const FRAMES: usize = 400;
const FRAME: Duration = Duration::from_millis(33);
/// The match start depends on the game, not the runner; the limit only stops a hung run.
const LIMIT: Duration = Duration::from_secs(180);

#[derive(Default)]
struct Seen {
    connected: bool,
    spawned: bool,
    events: Vec<UiEvent>,
    /// Hud element kinds ever listed, and how many at most in one snapshot.
    kinds: BTreeSet<u8>,
    max_hud: usize,
    /// Text hud elements whose string index resolved to text.
    resolved_text: usize,
    names: BTreeSet<String>,
    unresolved_text: usize,
    resolved_material: usize,
    unresolved_material: usize,
    /// Visible objectives ever seen, with their icon names.
    objectives: BTreeSet<String>,
    unresolved_icons: usize,
    clients: usize,
    obituaries: usize,
    bad_obituaries: usize,
    scoreboard_rows: usize,
    named_rows: usize,
    best_score_first: bool,
    stat: i32,
    /// Highest score any client info carried: live without a scoreboard request.
    info_score_max: i32,
    bytes_in: u64,
    secs: f64,
}

fn client(addr: SocketAddr, stop: &std::sync::atomic::AtomicBool) -> Seen {
    let mut s = Seen::default();
    let Ok(t) = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))) else {
        return s;
    };
    let mut c = NetClient::new(t, addr, "uiclient", "", 5100);
    let mut join = AutoJoin::default();
    let deadline = Instant::now() + LIMIT;
    let mut frames = 0;
    let mut began = None;
    let mut base = 0;
    let mut asked = false;
    // The match decides when the first kill happens, not the runner.
    while (frames < FRAMES || s.obituaries == 0)
        && Instant::now() < deadline
        && !stop.load(std::sync::atomic::Ordering::Relaxed)
    {
        c.pump(FRAME);
        if c.refused().is_some() {
            return s;
        }
        if c.latest().is_some() {
            s.connected = true;
            c.send();
        }
        let own = c.latest().map(|n| n.ps.client_num);
        if let (Some(own), Some(snap)) = (own, c.latest())
            && snap.entity(own).is_some_and(|e| e.etype == etype::PLAYER)
            && !s.spawned
        {
            s.spawned = true;
            began = Some(Instant::now());
            base = c.stats().map_or(0, |st| st.bytes_in);
        }
        let bytes = c.stats().map_or(0, |st| st.bytes_in);
        let Some(ui) = c.ui() else { continue };
        let events = ui.drain_events();
        s.max_hud = s.max_hud.max(ui.hud().len());
        // A snapshot can name a string whose configstring is still on its way (the reliable
        // stream is paced); only the state the client settles in must resolve.
        (s.unresolved_text, s.unresolved_material) = (0, 0);
        for e in ui.hud() {
            s.kinds.insert(e.kind);
            match e.kind {
                he::TEXT if e.text != 0 => {
                    if ui.localized(e.text).is_empty() {
                        s.unresolved_text += 1;
                    } else {
                        s.resolved_text += 1;
                        s.names.insert(ui.localized(e.text).to_owned());
                    }
                }
                he::MATERIAL if e.material != 0 => {
                    if ui.material(e.material).is_empty() {
                        s.unresolved_material += 1;
                    } else {
                        s.resolved_material += 1;
                    }
                }
                _ => {}
            }
        }
        for o in ui.objectives().iter().filter(|o| o.visible()) {
            let icon = ui.material(o.icon);
            if icon.is_empty() {
                s.unresolved_icons += 1;
            } else {
                s.objectives.insert(icon.to_owned());
            }
        }
        s.clients = s
            .clients
            .max((0..64).filter(|n| ui.client(*n).is_some()).count());
        if s.spawned {
            frames += 1;
            s.bytes_in = bytes - base;
        }
        if s.spawned && frames % 30 == 0 {
            asked = true;
        }
        let sb = ui.scoreboard();
        s.scoreboard_rows = s.scoreboard_rows.max(sb.rows.len());
        s.best_score_first = sb.rows.windows(2).all(|w| w[0].score >= w[1].score);
        s.named_rows = sb
            .rows
            .iter()
            .filter(|r| ui.client(r.client).is_some_and(|c| !c.name.is_empty()))
            .count();
        s.stat = ui.stat(205);
        s.info_score_max = (0..64)
            .filter_map(|n| ui.client(n))
            .map(|c| c.score)
            .fold(s.info_score_max, i32::max);
        for e in &events {
            if let UiEvent::Obituary(o) = e {
                s.obituaries += 1;
                if ui.client(o.victim).is_none()
                    || o.weapon.is_empty()
                    || !o.mean.starts_with("MOD_")
                {
                    s.bad_obituaries += 1;
                }
            }
        }
        let mut answers = Vec::new();
        for ev in &events {
            answers.extend(join.step(ev));
        }
        s.events.extend(events);
        for a in answers {
            c.command(&a);
        }
        if std::mem::take(&mut asked) {
            c.request_scores();
        }
    }
    s.secs = began.map_or(0.0, |b| b.elapsed().as_secs_f64());
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
        "set g_gametype dom",
        "set scr_dom_timelimit 0",
        "set scr_dom_scorelimit 0",
        "set sv_mapRotation \"gametype dom map mp_crash\"",
        "map mp_crash",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    if let Err(e) = server.exec_line(&format!("bots {BOTS}")) {
        return Ok(StageReport::new(NAME, Status::Failed).with_reason(e));
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = {
        let stop = stop.clone();
        std::thread::spawn(move || client(addr, &stop))
    };
    while !handle.is_finished() {
        server.run_for(Duration::from_millis(100));
    }
    let seen = handle.join().unwrap_or_default();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut failures = Vec::new();
    let opened: Vec<&str> = seen
        .events
        .iter()
        .filter_map(|e| match e {
            UiEvent::OpenMenu { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if !seen.connected {
        failures.push("the client never received a snapshot".to_owned());
    } else if !seen.spawned {
        failures.push(format!(
            "the client was never given a body; the scripts opened {opened:?}"
        ));
    }
    let first_team = opened.iter().position(|m| *m == "team_marinesopfor");
    let first_class = opened.iter().position(|m| m.starts_with("changeclass"));
    match (first_team, first_class) {
        (Some(t), Some(c)) if t < c => {}
        _ => failures.push(format!(
            "the team menu must open before the class menu, got {opened:?}"
        )),
    }
    // Client dvars come before the menu that reads them is not guaranteed, but they must arrive.
    let dvar = |n: &str| {
        seen.events.iter().any(
            |e| matches!(e, UiEvent::SetDvar { name, value } if name.eq_ignore_ascii_case(n) && value == "1"),
        )
    };
    if !dvar("ui_3dwaypointtext") {
        failures.push("setclientdvar ui_3dwaypointtext 1 never arrived".to_owned());
    }
    let printed = seen
        .events
        .iter()
        .filter(|e| matches!(e, UiEvent::Print { .. }))
        .count();
    if printed == 0 {
        failures.push("no iprintln line arrived (the stock connect message)".to_owned());
    }
    if seen.spawned {
        if seen.max_hud == 0 {
            failures.push("the gametype's hud elements never arrived".to_owned());
        }
        if seen.resolved_text + seen.resolved_material == 0 {
            failures.push(
                "no hud element text or shader name resolved through the configstrings".to_owned(),
            );
        }
        if seen.unresolved_text + seen.unresolved_material > 0 {
            failures.push(format!(
                "{} hud elements named a string or material the client has no configstring for",
                seen.unresolved_text + seen.unresolved_material
            ));
        }
        if seen.objectives.is_empty() {
            failures.push("Domination's flag objectives never became visible".to_owned());
        }
        if seen.unresolved_icons > 0 {
            failures.push(format!(
                "{} objective icons had no material name",
                seen.unresolved_icons
            ));
        }
        if seen.obituaries == 0 || seen.bad_obituaries > 0 {
            failures.push(format!(
                "{} obituaries arrived, {} without a known victim, weapon or means of death",
                seen.obituaries, seen.bad_obituaries
            ));
        }
        if seen.scoreboard_rows <= BOTS {
            failures.push(format!(
                "the scoreboard listed {} rows for {} players",
                seen.scoreboard_rows,
                BOTS + 1
            ));
        } else if seen.named_rows < seen.scoreboard_rows - 1 || !seen.best_score_first {
            failures.push("scoreboard rows unnamed or not sorted best first".to_owned());
        }
        // The server's stand-in profile stats stay its own: a client that took them for news would write them into
        // its saved profile (perk 1 in every custom class, a script error at every spawn).
        if seen.stat != 0 {
            failures.push(format!(
                "stat 205 is {} on the client: the join announced the server's stand-in",
                seen.stat
            ));
        }
        if seen.info_score_max <= 0 {
            failures
                .push("no kill ever raised a score in the client info configstrings".to_owned());
        }
        if seen.clients < BOTS {
            failures.push(format!(
                "the client info configstrings named {} players, expected at least {BOTS}",
                seen.clients
            ));
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
        .insert("ui.max_hud_elems".into(), seen.max_hud as f64);
    report
        .metrics
        .insert("ui.objectives".into(), seen.objectives.len() as f64);
    report
        .metrics
        .insert("ui.events".into(), seen.events.len() as f64);
    report.metrics.insert(
        "ui.prints".into(),
        seen.events
            .iter()
            .filter(|e| matches!(e, UiEvent::Print { kind, .. } if *kind != PrintKind::Console))
            .count() as f64,
    );
    if seen.secs > 0.0 {
        report.metrics.insert(
            "client.down_bytes_per_sec".into(),
            seen.bytes_in as f64 / seen.secs,
        );
    }
    report.notes.push(format!(
        "hud kinds {:?}, texts {:?}, objective icons {:?}, menus {:?}",
        seen.kinds, seen.names, seen.objectives, opened
    ));
    Ok(report)
}
