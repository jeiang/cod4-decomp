// SPDX-License-Identifier: GPL-3.0-only
//! `net-triggers`: a headless server with bots on mp_crash. A bot is held inside trigger volumes
//! the stage spawns: a `trigger_hurt` must wear its health down, a `trigger_once` must be freed
//! after the first touch, and a `trigger_damage` volume must be set off by a bullet through it
//! (a one-shot one is freed by that).
use crate::stage::{StageCtx, StageReport, Status};
use server::client::Session;
use server::game::{Ent, EntKind, TRIGGER_HURT_CONTENTS};
use server::server::Server;
use sim::contents;
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "net-triggers";
/// Waiting for a bot to get a body depends on the scripts, not the runner.
const LIMIT: Duration = Duration::from_secs(120);

/// Spawns a box trigger of `class` centred on `at`, the way the map loader sets one up.
fn spawn(server: &mut Server, class: &str, at: [f32; 3], wait: Option<f32>) -> u16 {
    let g = &mut server.game;
    let mut e = Ent::new(EntKind::Trigger, class);
    e.origin = at;
    e.mins = [-100.0; 3];
    e.maxs = [100.0; 3];
    e.contents = match class {
        "trigger_once" => contents::PLAYERTRIGGER,
        _ => TRIGGER_HURT_CONTENTS,
    };
    if class == "trigger_hurt" {
        e.dmg = 10;
    }
    if class == "trigger_damage" {
        e.takedamage = true;
        e.health = 32000;
    }
    let n = g.spawn(e).expect("free entity");
    g.init_trigger_spawn(n, wait, 0, 0);
    g.relink(n);
    n
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
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_crash",
        "bots 2",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let deadline = Instant::now() + LIMIT;
    let bot = loop {
        if Instant::now() > deadline {
            return fail("no bot got a body".into());
        }
        server.run_for(Duration::from_millis(50));
        if let Some(n) = server
            .game
            .clients
            .iter()
            .position(|c| c.bot && c.session == Session::Playing)
        {
            break n as u16;
        }
    };
    // Far from any real trigger and above the floor, where the bot stands while held.
    let hold = |server: &mut Server, at: [f32; 3], frames: u32| {
        for _ in 0..frames {
            server.game.teleport(bot, at);
            server.run_for(Duration::from_millis(50));
        }
    };
    let at = server.game.client(bot).map_or([0.0; 3], |c| c.ps.origin);

    let once = spawn(&mut server, "trigger_once", at, None);
    hold(&mut server, at, 4);
    if server.game.ent(once).is_some() {
        return fail("a trigger_once was not freed after a bot stood in it".into());
    }

    let health = |server: &Server| server.game.ent(bot).map_or(0, |e| e.health);
    let before = health(&server);
    let hurt = spawn(&mut server, "trigger_hurt", at, None);
    let mut lowest = before;
    for _ in 0..20 {
        hold(&mut server, at, 1);
        lowest = lowest.min(health(&server));
    }
    if let Some(e) = server.game.ent_mut(hurt) {
        e.free_at = Some(0);
    }
    if lowest >= before {
        return fail(format!(
            "standing in a trigger_hurt never cost health ({before} -> {lowest})"
        ));
    }

    let volume = spawn(&mut server, "trigger_damage", at, Some(0.0));
    server.run_for(Duration::from_millis(100));
    if server.game.ent(volume).is_none() {
        return fail("the trigger_damage volume vanished before it was shot".into());
    }
    if let Err(e) = server.exec_line("set sv_cheats 1") {
        return fail(format!("sv_cheats: {e}"));
    }
    let shot = format!(
        "devshoot {bot} {} {} {} {} {} {}",
        at[0] - 400.0,
        at[1],
        at[2],
        at[0] + 400.0,
        at[1],
        at[2]
    );
    if let Err(e) = server.exec_line(&shot) {
        return fail(format!("devshoot: {e}"));
    }
    server.run_for(Duration::from_millis(200));
    if server.game.ent(volume).is_some() {
        return fail("a bullet through a one-shot trigger_damage did not set it off".into());
    }

    let mut report = StageReport::new(NAME, Status::Passed);
    report
        .notes
        .push(format!("trigger_hurt cost {} health", before - lowest));
    if !server.script_errors.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(format!("{:?}", server.script_errors));
    }
    Ok(report)
}
