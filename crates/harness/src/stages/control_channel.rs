// SPDX-License-Identifier: GPL-3.0-only
//! `control-channel`: the client's dev control channel (#378), driven the way an agent drives it. A real client
//! starts a listen server, the stage opens the console with the key the player presses, types `bots 3`, waits for the
//! four players, holds the scoreboard up and reads it, reads the entity list, clicks a menu item at the centre of the
//! rectangle the state document gave for it and checks the same item became the pointer's, and captures the presented
//! frame offscreen. Every request is one line of JSON on the loopback socket the `--control` flag opens
//! (`crate::control::Driven`); the run is the harness's own child, so the capture has no visible window on the host.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::json;
use std::fs;
use std::io;
use std::time::Duration;

const NAME: &str = "control-channel";
const ARGS: &[&str] = &["--listen", "--bots", "0", "--duration", "300"];

/// The acceptance, request by request.
fn drive(c: &mut Driven, report: &mut StageReport) -> Result<(), String> {
    let secs = Duration::from_secs;
    let ping = c.ask(json!({"cmd": "ping"}))?;
    report.metrics.insert(
        "frames_at_ping".to_owned(),
        ping["frames"].as_f64().unwrap_or(0.0),
    );

    // The console, with the key the player presses, then the line typed into it.
    c.key("backquote", "tap")?;
    if c.wait(&json!({"console.open": true}), secs(3))?.is_none() {
        // Some layouts name the key `grave`.
        c.key("grave", "tap")?;
    }
    c.until(
        json!({"console.open": true}),
        secs(10),
        "the console did not open",
    )?;
    c.ask(json!({"cmd": "text", "text": "bots 3"}))?;
    c.key("enter", "tap")?;
    c.key("backquote", "tap")?;

    // Four players on the scoreboard, held up by the bind the player holds.
    c.until(
        json!({"entities.count": {"ge": 4}}),
        secs(120),
        "four players did not arrive",
    )?;
    let ents = c.state("entities")?;
    let listed = ents["list"].as_array().cloned().unwrap_or_default();
    report
        .metrics
        .insert("entities".to_owned(), listed.len() as f64);
    if listed.len() < 4 {
        return Err(format!("{} entities, want 4", listed.len()));
    }
    let players = listed
        .iter()
        .filter(|e| e["kind"].as_str() == Some("player"))
        .count();
    report
        .metrics
        .insert("player_entities".to_owned(), players as f64);

    // A capture of the frame on screen, with no window of the stage's own.
    let png = c.capture("control")?;
    let bytes = fs::metadata(&png).map_err(|e| e.to_string())?.len();
    report.files.push("control/control.png".to_owned());
    report
        .metrics
        .insert("capture_bytes".to_owned(), bytes as f64);

    // A menu item clicked at the centre of its own rectangle is the item under the pointer. The menu is one the
    // stage opens itself: `togglemenu` opens the menu `g_scriptMainMenu` names, which a listen server does not set
    // here, so the stage names the class menu first and then presses the bind the player presses.
    c.ask(json!({"cmd": "exec", "line": "set g_scriptMainMenu class"}))?;
    let open = c.state("menu")?["open"]
        .as_array()
        .is_some_and(|o| !o.is_empty());
    if open {
        // A menu the server opened (the class menu of the spawn flow) is closed first, so what the wait below
        // sees is the stage's own key press.
        c.bind("togglemenu", "tap")?;
    }
    c.until(
        json!({"menu.open": {"len": 0}}),
        secs(5),
        "the open menu did not close",
    )?;
    c.bind("togglemenu", "tap")?;
    c.until(
        json!({"menu.open.0": {"exists": true}}),
        secs(10),
        "the menu did not open",
    )?;
    let menu = c.state("menu")?;
    // The item the menu put the focus on is the one whose rectangle is a click's target.
    let focused = &menu["focused"];
    let name = focused["item"].as_str().unwrap_or_default().to_owned();
    let r = focused["rect"].as_array().cloned().unwrap_or_default();
    if name.is_empty() || r.len() != 4 {
        return Err(format!("no focused menu item to click: {menu}"));
    }
    let (x, y) = (
        r[0].as_f64().unwrap_or(0.0) + r[2].as_f64().unwrap_or(0.0) / 2.0,
        r[1].as_f64().unwrap_or(0.0) + r[3].as_f64().unwrap_or(0.0) / 2.0,
    );
    c.ask(json!({"cmd": "mouse", "x": x, "y": y}))?;
    let hovered = c.state("menu")?;
    let hit = hovered["focused"]["item"].as_str().unwrap_or_default();
    if hit != name {
        return Err(format!(
            "moved to the centre of {name} at {x:.0},{y:.0} and {hit} was under the pointer: {hovered}"
        ));
    }
    report
        .notes
        .push(format!("clicked {name} at {x:.0},{y:.0}"));

    // The menu is closed again before the scoreboard: an open menu takes the keys, so `+scores` would never
    // reach the bind table.
    c.bind("togglemenu", "tap")?;
    c.until(
        json!({"menu.open": {"len": 0}}),
        secs(5),
        "the menu did not close again",
    )?;

    c.bind("+scores", "down")?;
    // The client asks the server for the scoreboard once the bind reaches the input frame; its rows arrive a
    // frame or two later. A timeout here is the stage's failure, with the reason the predicate saw.
    let rows = c.until(
        json!({"scoreboard.rows.0": {"exists": true}}),
        secs(20),
        "the scoreboard did not arrive",
    );
    let board = c.state("scoreboard");
    c.bind("+scores", "up")?;
    rows?;
    let board = board?;
    let rows = board["rows"].as_array().cloned().unwrap_or_default();
    report
        .metrics
        .insert("scoreboard_rows".to_owned(), rows.len() as f64);
    if rows.len() < 4 {
        return Err(format!("{} scoreboard rows, want 4: {board}", rows.len()));
    }
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
    // A run told where the install is when it is not there fails `asset-load`; every stage that needs it says so
    // rather than starting a client that cannot load.
    if !install.is_dir() {
        return Ok(skipped("install not found"));
    }
    let Some(client) = locate_client() else {
        return Ok(skipped("no client binary"));
    };
    if let Some(report) = no_display(&client, NAME)? {
        return Ok(report);
    }
    let dir = ctx.dir.join("control");
    let mut report = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => {
            report.status = Status::Failed;
            report.reason = Some(e);
            return Ok(report);
        }
    };
    report.notes.push(format!("channel {}", c.addr));
    let outcome = drive(&mut c, &mut report);
    let status = c.finish(Duration::from_secs(30));
    if let Err(e) = outcome {
        report.status = Status::Failed;
        report.reason = Some(e);
    } else if !status.is_some_and(|s| s.success()) {
        report.status = Status::Failed;
        report.reason = Some(match status {
            Some(s) => format!("the client exited {s}"),
            None => "the client did not quit and was killed".to_owned(),
        });
    }
    Ok(report)
}
