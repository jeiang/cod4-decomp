// SPDX-License-Identifier: GPL-3.0-only
//! `client-hardpoint`: the real client calls in an airstrike, driven through the control channel as a person plays
//! (#376). The player joins through the server's team and class menus with the pointer. The airstrike is then set up
//! as a killstreak reward (`devhardpoint`, typed at the host's console: a streak of kills is not something a stage can
//! arrange), and the server must hold it. The player presses the key bound to `+actionslot 4` (the d-pad slot selects
//! the weapon), which must put the full-screen map pick (owner-draw 186) up, and confirms the point with the attack
//! button. The d-pad pieces (166, 168, 169) and the map pick must have drawn, and the server must have run the strike.
//! Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "client-hardpoint";
/// `--no-autojoin`: the server's team and class menus are the stage's to answer, with the pointer.
const ARGS: &[&str] = &[
    "--listen",
    "--bots",
    "3",
    "--no-autojoin",
    "--duration",
    "240",
];
/// The killstreak reward the stage sets up.
const REWARD: &str = "airstrike_mp";

/// The listen server's team and class menus answered as a person answers them: `team` in the team menu, then `class`
/// in the class menu, each clicked with the pointer at the centre of the item's rectangle, until the player is alive.
/// The runs start with `--no-autojoin`, so the server waits for the answers.
pub(super) fn join(c: &mut Driven, team: &str, class: &str) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("team menu", |c| {
        c.until(
            json!({"menu.open": {"contains": "team_marinesopfor"}}),
            secs(120),
            "the team menu did not open",
        )?;
        c.click(team)?;
        c.until(
            json!({"menu.open": {"contains": "changeclass"}}),
            secs(30),
            "the class menu did not open after the team was picked",
        )?;
        Ok(())
    })?;
    c.step("class menu", |c| {
        // The class menu settles before the pointer goes to it, as a person's eye finds the item first.
        c.pause(Duration::from_millis(500))?;
        c.click(class)?;
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            secs(120),
            "the player did not spawn",
        )?;
        Ok(())
    })
}

/// A match statistic of the listen server (`plants`, `airstrikes`, `helicopters`, `heli_shots`: `Server::stat`), read
/// through its `expect` command, which changes nothing: asked for more than any match reaches, it fails naming the
/// count it has. A refusal of another form (an unknown statistic) is the error.
pub(super) fn server_stat(c: &mut Driven, name: &str) -> Result<u64, String> {
    const UNREACHED: u64 = 1_000_000_000;
    let line = format!("expect {name} {UNREACHED}");
    match c.ask(json!({"cmd": "server", "op": "exec", "line": line})) {
        Ok(_) => Ok(UNREACHED),
        Err(e) => e
            .strip_prefix(&format!("{line}: only "))
            .and_then(|n| n.trim().parse().ok())
            .ok_or(e),
    }
}

/// Waits up to `limit` for the server's statistic `name` to reach `min`, and returns it.
pub(super) fn until_stat(
    c: &mut Driven,
    name: &str,
    min: u64,
    limit: Duration,
) -> Result<u64, String> {
    let end = Instant::now() + limit;
    loop {
        let n = server_stat(c, name)?;
        if n >= min {
            return Ok(n);
        }
        if Instant::now() > end {
            return Err(format!(
                "the server's {name} stayed at {n} for {} s (want {min})",
                limit.as_secs()
            ));
        }
        c.pause(Duration::from_millis(250))?;
    }
}

/// How often the HUD invoked owner-draw `id` since the match began (`hud.report.drawn`).
pub(super) fn drawn(c: &mut Driven, id: &str) -> Result<u64, String> {
    Ok(c.lookup(&format!("hud.report.drawn.{id}"))?
        .as_u64()
        .unwrap_or(0))
}

/// Polls the run's counter `report.<key>` (the `report` section, which a `wait` does not build) until it exceeds
/// `above` or `limit` runs out, and returns it either way: the caller judges what it fell short of.
pub(super) fn report_above(
    c: &mut Driven,
    key: &str,
    above: u64,
    limit: Duration,
) -> Result<u64, String> {
    let end = Instant::now() + limit;
    loop {
        let n = c.lookup(&format!("report.{key}"))?.as_u64().unwrap_or(0);
        if n > above || Instant::now() > end {
            return Ok(n);
        }
        c.pause(Duration::from_millis(250))?;
    }
}

/// The server's record of the person (the one client that is no bot).
fn person(c: &mut Driven) -> Result<Value, String> {
    let r = c.ask(json!({"cmd": "server", "op": "state", "what": "clients"}))?;
    r["clients"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["bot"] == json!(false)).cloned())
        .ok_or_else(|| "the server has no person".to_owned())
}

/// The match, step by step; `Ok` carries what the finished run fell short of.
fn drive(c: &mut Driven, out: &mut StageReport) -> Result<Vec<String>, String> {
    let secs = Duration::from_secs;
    join(c, "auto_assign", "Assault")?;
    // Setup, not play: a person earns the airstrike with a streak of kills, which a stage cannot arrange. The host
    // types the server's `devhardpoint`, which gives the reward as the scripts would; the server must then hold it.
    c.step("airstrike reward (setup)", |c| {
        c.console(&format!("devhardpoint human {REWARD}"))?;
        let end = Instant::now() + secs(15);
        loop {
            let p = person(c)?;
            if p["weapons"]
                .as_array()
                .is_some_and(|w| w.contains(&json!(REWARD)))
            {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(format!(
                    "the server never gave the player {REWARD}: it holds {}",
                    p["weapons"]
                ));
            }
            c.pause(Duration::from_millis(250))?;
        }
    })?;
    c.pause(secs(2))?;
    c.capture("dpad")?;
    c.step("+actionslot 4 puts the map pick up", |c| {
        let before = drawn(c, "186")?;
        c.bind("+actionslot 4", "tap")?;
        c.until(
            json!({"hud.report.drawn.186": {"gt": before}}),
            secs(10),
            "the map pick (owner-draw 186) never drew after +actionslot 4",
        )?;
        Ok(())
    })?;
    c.pause(secs(3))?;
    c.capture("pick")?;
    let strikes = c.step("the attack button calls the strike in", |c| {
        c.bind("+attack", "down")?;
        c.pause(Duration::from_millis(300))?;
        c.bind("+attack", "up")?;
        until_stat(c, "airstrikes", 1, secs(20))
    })?;
    out.metrics.insert("airstrikes".into(), strikes as f64);
    let mut problems = Vec::new();
    for id in ["166", "168", "169", "186"] {
        let n = drawn(c, id)?;
        out.metrics.insert(format!("drawn.{id}"), n as f64);
        if n == 0 {
            problems.push(format!("owner-draw {id} never drew"));
        }
    }
    Ok(problems)
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let dir = ctx.dir.join("strike");
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => return Ok(out.with_status(Status::Failed).with_reason(e)),
    };
    let result = drive(&mut c, &mut out);
    // The steps and what the HUD and the console held, kept whether or not the run got through.
    let kept = c.report("state.json", &["player", "hud", "console"]);
    let status = c.finish(Duration::from_secs(30));
    out.files
        .extend(["strike/state.json", "strike/dpad.png", "strike/pick.png"].map(String::from));
    let mut problems = match result {
        Ok(problems) => problems,
        Err(e) => vec![e],
    };
    if let Err(e) = kept {
        problems.push(format!("the run's state was not kept: {e}"));
    }
    if !status.is_some_and(|s| s.success()) {
        problems.push(format!("the client did not exit successfully: {status:?}"));
    }
    if !problems.is_empty() {
        return Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")));
    }
    Ok(out)
}
