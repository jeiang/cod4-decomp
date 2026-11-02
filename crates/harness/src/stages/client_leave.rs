// SPDX-License-Identifier: GPL-3.0-only
//! `client-leave`: a player leaves a match through the stock menus. Start a server from the main menu, spawn,
//! open the in-game menu, confirm Leave Game: the client must be back at the front end (no connection, no world,
//! the main menu open and drawn, not a black screen). Then Quit from the main menu must end the process with
//! status 0. Needs a display and the install; skips without.
use super::client_custom_class::session_report;
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::steps_problem;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::Duration;

const NAME: &str = "client-leave";

/// The least share of the frame the front end lights up (a black screen is 0).
const MIN_LIT: f64 = 0.02;

/// The steps whose outcome this stage is about: a report that stopped before them proves nothing.
const REQUIRED_STEPS: &[&str] = &[
    "click=Leave Game",
    "menu=popup_leavegame:5",
    "shot=home",
    "click=Quit",
];

/// Why the run failed, or `None`.
fn verdict(report: &Value) -> Option<String> {
    if let Some(w) = steps_problem(report) {
        return Some(w);
    }
    let steps = report["steps"].as_array().map_or(&[][..], Vec::as_slice);
    let missing: Vec<&str> = REQUIRED_STEPS
        .iter()
        .copied()
        .filter(|want| !steps.iter().any(|s| s["step"] == *want))
        .collect();
    if !missing.is_empty() {
        return Some(format!("the run never reached {}", missing.join(", ")));
    }
    let lit = steps
        .iter()
        .find(|s| s["step"] == "shot=home")
        .and_then(|s| s["lit_fraction"].as_f64());
    match lit {
        Some(l) if l >= MIN_LIT => {}
        Some(l) => return Some(format!("the screen after leaving is black (lit {l:.3})")),
        None => return Some("no screenshot after leaving".into()),
    }
    if report["quit"] != Value::Bool(true) {
        return Some("Quit never reached the exit".into());
    }
    None
}

fn spawn(c: &mut Driven) -> Result<(), String> {
    c.until(
        json!({"net.spawned": true, "player.alive": true}),
        Duration::from_secs(120),
        "the player did not spawn",
    )?;
    Ok(())
}

fn lit_fraction(path: &std::path::Path) -> Result<f64, String> {
    let (w, h, rgb) = decode(path)?;
    let lit = rgb
        .chunks_exact(3)
        .filter(|p| p.iter().map(|v| u32::from(*v)).sum::<u32>() > 24)
        .count();
    Ok(lit as f64 / (w * h) as f64)
}

/// Plays through Leave Game and the return to the front end. Quit is separate: its real click closes the channel.
fn leave(c: &mut Driven) -> Result<(), String> {
    c.step("stock menu spawn", |c| {
        c.console("set ui_netGametypeName war")?;
        c.click("Start New Server")?;
        c.until(
            json!({"menu.open": {"contains": "createserver"}}),
            Duration::from_secs(20),
            "Start New Server did not open the create-server menu",
        )?;
        c.click("Start")?;
        c.until(
            json!({"menu.open": {"contains": "team_marinesopfor"}}),
            Duration::from_secs(90),
            "the listen server's team menu did not open",
        )?;
        c.click("auto_assign")?;
        c.until(
            json!({"menu.open": {"contains": "changeclass"}}),
            Duration::from_secs(30),
            "the class menu did not open after the team was picked",
        )?;
        c.pause(Duration::from_secs(1))?;
        c.click("Assault")?;
        spawn(c)
    })?;
    c.pause(Duration::from_secs(2))?;
    c.step("ingame", |c| {
        c.bind("togglemenu", "tap")?;
        c.until(
            json!({"menu.open.0": {"exists": true}}),
            Duration::from_secs(5),
            "the in-game menu did not open on the menu key",
        )?;
        c.pause(Duration::from_secs(1))?;
        c.capture("ingame")?;
        Ok(())
    })?;
    c.step("click=Leave Game", |c| c.click("Leave Game").map(|_| ()))?;
    c.step("menu=popup_leavegame:5", |c| {
        c.until(
            json!({"menu.open": {"contains": "popup_leavegame"}}),
            Duration::from_secs(5),
            "Leave Game did not open its confirmation popup",
        )?;
        c.click("Yes")?;
        Ok(())
    })?;
    c.step("home=20", |c| {
        c.until(
            json!({"net": null, "client.in_world": false, "menu.open": {"contains": "main"}}),
            Duration::from_secs(20),
            "Leave Game did not return to the main menu with no connection and no world",
        )?;
        c.pause(Duration::from_secs(2))?;
        Ok(())
    })?;
    let lit = c.step("shot=home", |c| {
        let path = c.capture("home")?;
        lit_fraction(&path)
    })?;
    if let Some(step) = c.steps.last_mut() {
        step["lit_fraction"] = json!(lit);
    }
    Ok(())
}

/// Whether a request failed because the client's channel went away: what the confirmed Quit is meant to do.
fn channel_gone(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("the channel closed")
        || e.contains("connection reset")
        || e.contains("broken pipe")
        || e.contains("connection aborted")
}

/// Clicks Quit and confirms it. The confirmation ends the client, so its channel closing during that click is the
/// expected outcome; the caller still requires the process to exit with status 0.
fn quit(c: &mut Driven) -> Result<(), String> {
    c.step("click=Quit", |c| {
        c.click("Quit")?;
        c.until(
            json!({"menu.open": {"contains": "quit_popmenu"}}),
            Duration::from_secs(5),
            "Quit did not open its confirmation popup",
        )?;
        match c.click("Yes") {
            Ok(_) => Ok(()),
            Err(e) if channel_gone(&e) => Ok(()),
            Err(e) => Err(e),
        }
    })
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
    let mut out = StageReport::new(NAME, Status::Passed);
    let dir = ctx.dir.join("control");
    let mut c = match Driven::start(&client, install, &dir, &["--bots", "9"]) {
        Ok(c) => c,
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
            return Ok(out);
        }
    };
    let left = leave(&mut c);
    let report = session_report(&mut c, "control-run.json");
    let quit = if left.is_ok() && report.is_ok() {
        Some(quit(&mut c))
    } else {
        None
    };
    let steps = c.steps.clone();
    let status = c.finish(Duration::from_secs(30));
    let exited = quit.as_ref().is_some_and(|r| r.is_ok()) && status.is_some_and(|s| s.success());
    out.files.push("control/control-run.json".into());
    for f in ["ingame", "home"] {
        if dir.join(format!("{f}.png")).is_file() {
            out.files.push(format!("control/{f}.png"));
        }
    }
    let mut problem = left.err();
    match quit {
        Some(Err(e)) => problem = Some(e),
        _ => {}
    }
    match report {
        Err(e) if problem.is_none() => {
            problem = Some(format!("the run's report could not be read: {e}"));
        }
        Err(_) => {}
        Ok(mut report) => {
            report["steps"] = json!(steps);
            report["quit"] = json!(exited);
            if let Some(step) = report["steps"]
                .as_array()
                .and_then(|steps| steps.iter().find(|step| step["step"] == "shot=home"))
            {
                out.metrics.insert(
                    "home.lit_fraction".into(),
                    step["lit_fraction"].as_f64().unwrap_or(0.0),
                );
            }
            if let Err(e) = std::fs::write(
                dir.join("control-run.json"),
                serde_json::to_vec_pretty(&report).unwrap_or_default(),
            ) {
                if problem.is_none() {
                    problem = Some(format!("could not finish the run report: {e}"));
                }
            } else if let Some(w) = verdict(&report) {
                if problem.is_none() {
                    problem = Some(w);
                }
            }
        }
    }
    if let Some(w) = problem {
        out.status = Status::Failed;
        out.reason = Some(w);
    } else {
        out.notes
            .push("Leave Game returned to the drawn main menu; Quit exited with status 0".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(lit: f64, quit: bool) -> Value {
        json!({"quit": quit, "steps": [
            {"step": "ingame=120", "ok": true},
            {"step": "click=Leave Game", "ok": true},
            {"step": "menu=popup_leavegame:5", "ok": true},
            {"step": "shot=home", "ok": true, "lit_fraction": lit},
            {"step": "click=Quit", "ok": true},
        ]})
    }

    #[test]
    fn a_black_screen_after_leaving_fails() {
        assert!(verdict(&report(0.0, true)).unwrap().contains("black"));
    }

    #[test]
    fn quit_must_reach_the_exit() {
        assert!(verdict(&report(0.3, false)).unwrap().contains("Quit"));
        assert!(verdict(&report(0.3, true)).is_none());
    }

    #[test]
    fn a_run_that_never_reached_leaving_fails() {
        let short = json!({"quit": true, "steps": [{"step": "ingame=120", "ok": true}]});
        let why = verdict(&short).unwrap();
        assert!(why.contains("Leave Game") && why.contains("Quit"), "{why}");
        assert!(
            verdict(&json!({"quit": true, "steps": []}))
                .unwrap()
                .contains("no steps")
        );
    }
}
