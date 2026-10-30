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
use super::control_channel::{ask, launch, settle, until};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, BufReader};
use std::net::TcpStream;
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

type Sock = BufReader<TcpStream>;

/// The console's lines that contain `needle`.
fn console_count(sock: &mut Sock, id: &mut u64, needle: &str) -> Result<usize, String> {
    *id += 1;
    let c = ask(sock, *id, json!({"cmd": "state", "what": "console"}))?;
    Ok(c["console"]["lines"].as_array().map_or(0, |l| {
        l.iter()
            .filter(|s| s.as_str().is_some_and(|s| s.contains(needle)))
            .count()
    }))
}

/// Waits until the console holds at least `want` lines containing `needle`.
fn console_until(
    sock: &mut Sock,
    id: &mut u64,
    needle: &str,
    want: usize,
    what: &str,
) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let n = console_count(sock, id, needle)?;
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
fn server_clients(sock: &mut Sock, id: &mut u64) -> Result<Vec<Value>, String> {
    *id += 1;
    let r = ask(
        sock,
        *id,
        json!({"cmd": "server", "op": "state", "what": "clients"}),
    )?;
    Ok(r["clients"].as_array().cloned().unwrap_or_default())
}

/// The server's record of the person (the one client that is no bot).
fn person(sock: &mut Sock, id: &mut u64) -> Result<Value, String> {
    server_clients(sock, id)?
        .into_iter()
        .find(|c| c["bot"] == json!(false))
        .ok_or_else(|| "the server has no human client".to_owned())
}

/// Polls the server's record of the person until `check` holds.
fn person_until(
    sock: &mut Sock,
    id: &mut u64,
    what: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<Value, String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let p = person(sock, id)?;
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
fn exec(sock: &mut Sock, id: &mut u64, line: &str) -> Result<(), String> {
    *id += 1;
    ask(sock, *id, json!({"cmd": "exec", "line": line})).map(|_| ())
}

/// Clicks the item of the open menu whose name or text is `want` (case-insensitive), through the pointer.
fn click_item(sock: &mut Sock, id: &mut u64, want: &str) -> Result<(), String> {
    *id += 1;
    let menu = ask(sock, *id, json!({"cmd": "state", "what": "menu"}))?;
    let items = menu["menu"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let item = items
        .iter()
        .find(|i| {
            ["name", "text"]
                .iter()
                .any(|k| i[k].as_str().is_some_and(|s| s.eq_ignore_ascii_case(want)))
        })
        .ok_or_else(|| format!("no menu item {want:?}: {menu}"))?;
    let r = item["rect"].as_array().cloned().unwrap_or_default();
    if r.len() != 4 {
        return Err(format!("item {want:?} has no rectangle: {item}"));
    }
    let f = |i: usize| r[i].as_f64().unwrap_or(0.0);
    let (x, y) = (f(0) + f(2) / 2.0, f(1) + f(3) / 2.0);
    *id += 1;
    ask(sock, *id, json!({"cmd": "mouse", "x": x, "y": y}))?;
    *id += 1;
    ask(
        sock,
        *id,
        json!({"cmd": "button", "name": "mouse1", "action": "tap"}),
    )?;
    Ok(())
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
    fs::create_dir_all(&dir)?;
    let (child, mut sock, addr) = launch(&client, install, &dir, &["--bots", "2"])?;
    let sock = &mut sock;
    let mut id = 0_u64;
    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(format!("channel {addr}"));

    let outcome = (|| -> Result<(), String> {
        until(
            sock,
            &mut id,
            json!({"player.alive": true}),
            "the player did not spawn",
            Duration::from_secs(150),
        )?;

        // #366: the host's console reaches the server's commands.
        exec(sock, &mut id, "status")?;
        console_until(sock, &mut id, "hostname:", 1, "status")?;
        console_until(sock, &mut id, "map: mp_crash", 1, "status")?;
        let clients = server_clients(sock, &mut id)?;
        let bot = clients
            .iter()
            .find(|c| c["bot"] == json!(true))
            .and_then(|c| c["name"].as_str())
            .ok_or("no bot to kick")?
            .to_owned();
        let before = clients.len();
        exec(sock, &mut id, &format!("kick \"{bot}\""))?;
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let n = server_clients(sock, &mut id)?.len();
            if n < before {
                report.metrics.insert("kicked".into(), (before - n) as f64);
                break;
            }
            if Instant::now() > end {
                return Err(format!("kick {bot:?}: the server still has {n} clients"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        id += 1;
        let loads = ask(
            sock,
            id,
            json!({"cmd": "server", "op": "state", "what": "map"}),
        )?["level_loads"]
            .as_u64()
            .unwrap_or(0);
        exec(sock, &mut id, "map_restart")?;
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            id += 1;
            let now = ask(
                sock,
                id,
                json!({"cmd": "server", "op": "state", "what": "map"}),
            )?["level_loads"]
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
        until(
            sock,
            &mut id,
            json!({"player.alive": true}),
            "the player did not spawn after map_restart",
            Duration::from_secs(120),
        )?;

        // #370 without cheats: each verb is refused in the player's language, never as the raw key.
        let seen = console_count(sock, &mut id, REFUSAL)?;
        for verb in REFUSED {
            exec(sock, &mut id, verb)?;
        }
        console_until(sock, &mut id, REFUSAL, seen + REFUSED.len(), "refusals")?;
        if console_count(sock, &mut id, "GAME_CHEATSNOTENABLED")? > 0 {
            return Err(
                "the refusal reached the console as the raw key GAME_CHEATSNOTENABLED".into(),
            );
        }
        let p = person(sock, &mut id)?;
        if p["noclip"] == json!(true) || p["godmode"] == json!(true) {
            return Err(format!("a cheat acted without sv_cheats: {p}"));
        }

        // `devmap` at the console: a server with cheats, joined through the team and class menus.
        exec(sock, &mut id, "devmap mp_crash")?;
        until(
            sock,
            &mut id,
            json!({"menu.open.0": "team_marinesopfor"}),
            "the team menu of the devmap server did not open",
            Duration::from_secs(180),
        )?;
        click_item(sock, &mut id, "auto_assign")?;
        until(
            sock,
            &mut id,
            json!({"menu.open.0": "changeclass"}),
            "the class menu did not open",
            Duration::from_secs(30),
        )?;
        std::thread::sleep(Duration::from_millis(500));
        click_item(sock, &mut id, "Assault")?;
        until(
            sock,
            &mut id,
            json!({"player.alive": true}),
            "the player did not spawn on the devmap server",
            Duration::from_secs(120),
        )?;
        id += 1;
        let cheats = ask(
            sock,
            id,
            json!({"cmd": "server", "op": "cvar", "name": "sv_cheats"}),
        )?;
        if cheats["value"] != json!("1") {
            return Err(format!("devmap left sv_cheats at {cheats}"));
        }

        exec(sock, &mut id, "noclip")?;
        person_until(sock, &mut id, "noclip", |p| p["noclip"] == json!(true))?;
        exec(sock, &mut id, "noclip")?;
        person_until(sock, &mut id, "noclip off", |p| p["noclip"] == json!(false))?;
        exec(sock, &mut id, "ufo")?;
        person_until(sock, &mut id, "ufo", |p| p["ufo"] == json!(true))?;
        exec(sock, &mut id, "ufo")?;
        person_until(sock, &mut id, "ufo off", |p| p["ufo"] == json!(false))?;
        exec(sock, &mut id, "god")?;
        person_until(sock, &mut id, "god", |p| p["godmode"] == json!(true))?;
        let health = person(sock, &mut id)?["health"].as_i64().unwrap_or(0);
        exec(sock, &mut id, "take health 60")?;
        person_until(sock, &mut id, "take health 60", |p| {
            p["health"].as_i64() == Some((health - 60).max(1))
        })?;
        exec(sock, &mut id, "give health")?;
        person_until(sock, &mut id, "give health", |p| {
            p["health"].as_i64() == Some(health)
        })?;
        exec(sock, &mut id, "take weapons")?;
        person_until(sock, &mut id, "take weapons", |p| {
            p["weapons"].as_array().is_some_and(Vec::is_empty)
        })?;
        exec(sock, &mut id, "give rpg_mp")?;
        person_until(sock, &mut id, "give rpg_mp", |p| {
            p["weapons"]
                .as_array()
                .is_some_and(|w| w.contains(&json!("rpg_mp")))
        })?;
        exec(sock, &mut id, "give all")?;
        let p = person_until(sock, &mut id, "give all", |p| {
            p["weapons"].as_array().is_some_and(|w| w.len() > 10)
        })?;
        report.metrics.insert(
            "weapons_after_give_all".into(),
            p["weapons"].as_array().map_or(0.0, |w| w.len() as f64),
        );
        exec(sock, &mut id, "setviewpos 100 200 300 90")?;
        person_until(sock, &mut id, "setviewpos 100 200 300 90", |p| {
            let o = p["origin"].as_array().cloned().unwrap_or_default();
            let at = |i: usize| o.get(i).and_then(Value::as_f64).unwrap_or(f64::NAN);
            (at(0) - 100.0).abs() < 1.0
                && (at(1) - 200.0).abs() < 1.0
                && at(2) < 301.0
                && at(2) > 200.0
        })?;
        id += 1;
        let shot = ask(sock, id, json!({"cmd": "capture"}))?;
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
        id += 1;
        ask(sock, id, json!({"cmd": "quit"}))?;
        Ok(())
    })();
    settle(child, outcome, report)
}
