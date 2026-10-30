// SPDX-License-Identifier: GPL-3.0-only
//! `client-host-console`: the host of a listen server at the console (#366, #370), driven through the control
//! channel as the player types. On the server started with `--listen` (`map`, `sv_cheats 0`): `status` answers in
//! the host's console, `kick <bot>` drops the bot, `map_restart` loads the level again, and each cheat verb
//! (`noclip`, `ufo`, `god`, `give`, `take`, `setviewpos`) is refused with the localized `GAME_CHEATSNOTENABLED`
//! text, never the raw key. Then `devmap mp_crash` typed at the console starts a server with cheats, the team and
//! class menus are clicked through, and the same verbs act: the server's own client state says so (`noclip`,
//! `ufo`, `godmode`, health, owned weapons, origin). Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "client-host-console";
/// The cheat verbs typed without `sv_cheats`, each answered by the one localized refusal.
const REFUSED: [&str; 6] = [
    "noclip",
    "ufo",
    "god",
    "give all",
    "take all",
    "setviewpos 1 2 3",
];
/// The localized `GAME_CHEATSNOTENABLED` (stock `localizedstrings/game.str`).
const REFUSAL: &str = "Cheats are not enabled";

/// The console's lines that contain `needle`.
fn console_count(c: &mut Driven, needle: &str) -> Result<usize, String> {
    let c = c.ask(json!({"cmd": "state", "what": "console"}))?;
    Ok(c["console"]["lines"].as_array().map_or(0, |l| {
        l.iter()
            .filter(|s| s.as_str().is_some_and(|s| s.contains(needle)))
            .count()
    }))
}

/// Waits until the console holds at least `want` lines containing `needle`.
fn console_until(c: &mut Driven, needle: &str, want: usize, what: &str) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let n = console_count(c, needle)?;
        if n >= want {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!(
                "{what}: {n} console lines with {needle:?}, want {want}"
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The listen server's client list, as the server holds it.
fn server_clients(c: &mut Driven) -> Result<Vec<Value>, String> {
    let r = c.ask(json!({"cmd": "server", "op": "state", "what": "clients"}))?;
    Ok(r["clients"].as_array().cloned().unwrap_or_default())
}

/// The server's record of the person (the one client that is no bot).
fn person(c: &mut Driven) -> Result<Value, String> {
    server_clients(c)?
        .into_iter()
        .find(|c| c["bot"] == json!(false))
        .ok_or_else(|| "the server has no human client".to_owned())
}

/// Polls the server's record of the person until `check` holds.
fn person_until(
    c: &mut Driven,
    what: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<Value, String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let p = person(c)?;
        if check(&p) {
            return Ok(p);
        }
        if Instant::now() > end {
            return Err(format!("{what}: the server's client is {p}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Types one line at the console as Enter sends it.
fn exec(c: &mut Driven, line: &str) -> Result<(), String> {
    c.ask(json!({"cmd": "exec", "line": line})).map(|_| ())
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let skipped = |why: &str| {
        let mut r = StageReport::new(NAME, Status::Skipped);
        r.reason = Some(why.to_owned());
        r
    };
    let Some(install) = &ctx.install else {
        return Ok(skipped("no install"));
    };
    if !install.is_dir() {
        return Ok(skipped("install not found"));
    }
    let Some(client) = locate_client() else {
        return Ok(skipped("no client binary"));
    };
    if let Some(report) = no_display(&client, NAME)? {
        return Ok(report);
    }
    let dir = ctx.dir.join("host-console");
    let mut driven = Driven::start(
        &client,
        install,
        &dir,
        &["--listen", "--bots", "2", "--duration", "600"],
    )
    .map_err(io::Error::other)?;
    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(format!("channel {}", driven.addr));
    let c = &mut driven;

    let outcome = (|| -> Result<(), String> {
        c.until(
            json!({"player.alive": true}),
            Duration::from_secs(150),
            "the player did not spawn",
        )?;

        // #366: the host's console reaches the server's commands.
        exec(c, "status")?;
        console_until(c, "hostname:", 1, "status")?;
        console_until(c, "map: mp_crash", 1, "status")?;
        let clients = server_clients(c)?;
        let bot = clients
            .iter()
            .find(|c| c["bot"] == json!(true))
            .and_then(|c| c["name"].as_str())
            .ok_or("no bot to kick")?
            .to_owned();
        let before = clients.len();
        exec(c, &format!("kick \"{bot}\""))?;
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let n = server_clients(c)?.len();
            if n < before {
                report.metrics.insert("kicked".into(), (before - n) as f64);
                break;
            }
            if Instant::now() > end {
                return Err(format!("kick {bot:?}: the server still has {n} clients"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let loads = c.ask(json!({"cmd": "server", "op": "state", "what": "map"}))?["level_loads"]
            .as_u64()
            .unwrap_or(0);
        exec(c, "map_restart")?;
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let now = c.ask(json!({"cmd": "server", "op": "state", "what": "map"}))?["level_loads"]
                .as_u64()
                .unwrap_or(0);
            if now > loads {
                break;
            }
            if Instant::now() > end {
                return Err(format!("map_restart: level_loads stayed {now}"));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        c.until(
            json!({"player.alive": true}),
            Duration::from_secs(120),
            "the player did not spawn after map_restart",
        )?;

        // #370 without cheats: each verb is refused in the player's language, never as the raw key.
        let seen = console_count(c, REFUSAL)?;
        for verb in REFUSED {
            exec(c, verb)?;
        }
        console_until(c, REFUSAL, seen + REFUSED.len(), "refusals")?;
        if console_count(c, "GAME_CHEATSNOTENABLED")? > 0 {
            return Err(
                "the refusal reached the console as the raw key GAME_CHEATSNOTENABLED".into(),
            );
        }
        let p = person(c)?;
        if p["noclip"] == json!(true) || p["godmode"] == json!(true) {
            return Err(format!("a cheat acted without sv_cheats: {p}"));
        }

        // `devmap` at the console: a server with cheats, joined through the team and class menus.
        exec(c, "devmap mp_crash")?;
        c.until(
            json!({"menu.open.0": "team_marinesopfor"}),
            Duration::from_secs(180),
            "the team menu of the devmap server did not open",
        )?;
        c.click("auto_assign")?;
        c.until(
            json!({"menu.open.0": "changeclass"}),
            Duration::from_secs(30),
            "the class menu did not open",
        )?;
        std::thread::sleep(Duration::from_millis(500));
        c.click("Assault")?;
        c.until(
            json!({"player.alive": true}),
            Duration::from_secs(120),
            "the player did not spawn on the devmap server",
        )?;

        let cheats = c.ask(json!({"cmd": "server", "op": "cvar", "name": "sv_cheats"}))?;
        if cheats["value"] != json!("1") {
            return Err(format!("devmap left sv_cheats at {cheats}"));
        }

        exec(c, "noclip")?;
        person_until(c, "noclip", |p| p["noclip"] == json!(true))?;
        exec(c, "noclip")?;
        person_until(c, "noclip off", |p| p["noclip"] == json!(false))?;
        exec(c, "ufo")?;
        person_until(c, "ufo", |p| p["ufo"] == json!(true))?;
        exec(c, "ufo")?;
        person_until(c, "ufo off", |p| p["ufo"] == json!(false))?;
        exec(c, "god")?;
        person_until(c, "god", |p| p["godmode"] == json!(true))?;
        let health = person(c)?["health"].as_i64().unwrap_or(0);
        exec(c, "take health 60")?;
        person_until(c, "take health 60", |p| {
            p["health"].as_i64() == Some((health - 60).max(1))
        })?;
        exec(c, "give health")?;
        person_until(c, "give health", |p| p["health"].as_i64() == Some(health))?;
        exec(c, "take weapons")?;
        person_until(c, "take weapons", |p| {
            p["weapons"].as_array().is_some_and(Vec::is_empty)
        })?;
        exec(c, "give rpg_mp")?;
        person_until(c, "give rpg_mp", |p| {
            p["weapons"]
                .as_array()
                .is_some_and(|w| w.contains(&json!("rpg_mp")))
        })?;
        exec(c, "give all")?;
        let p = person_until(c, "give all", |p| {
            p["weapons"].as_array().is_some_and(|w| w.len() > 10)
        })?;
        report.metrics.insert(
            "weapons_after_give_all".into(),
            p["weapons"].as_array().map_or(0.0, |w| w.len() as f64),
        );
        exec(c, "setviewpos 100 200 300 90")?;
        person_until(c, "setviewpos 100 200 300 90", |p| {
            let o = p["origin"].as_array().cloned().unwrap_or_default();
            let at = |i: usize| o.get(i).and_then(Value::as_f64).unwrap_or(f64::NAN);
            (at(0) - 100.0).abs() < 1.0
                && (at(1) - 200.0).abs() < 1.0
                && at(2) < 301.0
                && at(2) > 200.0
        })?;

        let shot = c.ask(json!({"cmd": "capture"}))?;
        if let Some(path) = shot["path"].as_str() {
            report.files.push(format!(
                "host-console/{}",
                std::path::Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
        }
        report.notes.push(
            "status, kick and map_restart reached the listen server from the host's console; the cheat verbs were refused in the player's language without sv_cheats and acted under devmap"
                .to_owned(),
        );
        c.ask(json!({"cmd": "quit"}))?;
        Ok(())
    })();
    let status = driven.finish(Duration::from_secs(30));
    if let Err(e) = outcome {
        report.status = Status::Failed;
        report.reason = Some(e);
    } else if !status.is_some_and(|s| s.success()) {
        report.status = Status::Failed;
        report.reason = Some(format!("the client did not exit successfully: {status:?}"));
    }
    Ok(report)
}
