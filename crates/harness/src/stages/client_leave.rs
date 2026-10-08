// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-leave`: a player leaves a match through the stock menus. Start a server from the main menu, spawn,
//! open the in-game menu, confirm Leave Game: the client must be back at the front end (no connection, no world,
//! the main menu open and drawn, not a black screen). Then Quit from the main menu must end the process with
//! status 0. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;

const NAME: &str = "client-leave";

const STEPS: &str = "click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,\
click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=2,togglemenu,wait=1,\
shot=ingame,click=Leave Game,menu=popup_leavegame:5,click=Yes,home=20,wait=2,shot=home,\
click=Quit,menu=quit_popmenu:5,click=Yes,wait=5";

/// The least share of the frame the front end lights up (a black screen is 0).
const MIN_LIT: f64 = 0.02;

/// Why the run failed, or `None`.
fn verdict(report: &Value) -> Option<String> {
    let steps = report["steps"].as_array().map_or(&[][..], Vec::as_slice);
    let failed: Vec<String> = steps
        .iter()
        .filter(|s| s["ok"] != Value::Bool(true))
        .map(|s| format!("{} ({})", s["step"], s["note"]))
        .collect();
    if !failed.is_empty() {
        return Some(format!("steps failed: {}", failed.join(", ")));
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
    match run_client(&client, install, &ctx.dir, &[], STEPS) {
        Ok(report) => {
            out.files.push("ui-script.json".into());
            out.files.push("ingame.png".into());
            out.files.push("home.png".into());
            if let Some(w) = verdict(&report) {
                out.status = Status::Failed;
                out.reason = Some(w);
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    out.notes
        .push("Leave Game returned to the drawn main menu; Quit exited with status 0".into());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(lit: f64, quit: bool) -> Value {
        json!({"quit": quit, "steps": [
            {"step": "click=Quit", "ok": true},
            {"step": "shot=home", "ok": true, "lit_fraction": lit},
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
}
