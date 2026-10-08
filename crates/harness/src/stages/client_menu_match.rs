// SPDX-License-Identifier: GPL-3.0-only
//! `client-menu-match`: the real client, started with no match flags, plays a whole match from the stock menus and
//! carries on into the next map. A person's path: Start New Server (the score limit set low beforehand, as a player can
//! set it), the server's team menu, the class menu, play, the end-of-match scoreboard, the server's map rotation, then
//! the team and class menus again in the new map and a second spawn. Every step must complete and the client must end
//! in a different map than it began in. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;
use std::time::Duration;

const NAME: &str = "client-menu-match";
/// The whole run, a hang included; a match to three kills and the map load take a minute or two.
const LIMIT: Duration = Duration::from_secs(420);
/// A menu-only run that sets or checks a stat.
const PROFILE_LIMIT: Duration = Duration::from_secs(90);

/// Join a team and a class from the server's menus, and wait until the player is in the world.
const JOIN: &str = "menu=team_marinesopfor:90,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120";
/// The match: a three-kill limit so bots end it quickly.
const LIMITS: &str = "set=set scr_war_scorelimit 3,set=set scr_war_timelimit 4";

fn steps() -> String {
    format!(
        "{LIMITS},set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,{JOIN},wait=2,shot=match,\
         menu=scoreboard:240,wait=1,shot=scoreboard,maprotate=120,{JOIN},wait=3,shot=rotated"
    )
}

/// Why a menu-only run failed (a step did not pass), or `None`.
fn verdict_steps(report: &Value) -> Option<String> {
    let failed: Vec<String> = report["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["ok"] != Value::Bool(true))
        .map(|s| format!("{} ({})", s["step"], s["note"]))
        .collect();
    (!failed.is_empty()).then(|| format!("steps failed: {}", failed.join(", ")))
}

/// Why the run failed, or `None`: every step passed, the player spawned in the second map and the map changed.
fn menu_match_verdict(report: &Value) -> Option<String> {
    if let Some(w) = verdict(report) {
        return Some(w);
    }
    let Some(rotated) = report["steps"].as_array().into_iter().flatten().find(|s| {
        s["step"]
            .as_str()
            .is_some_and(|s| s.starts_with("maprotate"))
    }) else {
        return Some("the report has no maprotate step".into());
    };
    let note = rotated["note"].as_str().unwrap_or("");
    let Some((from, to)) = note.split_once(" -> ") else {
        return Some(format!("the map did not rotate (note {note:?})"));
    };
    if from == to || report["map"].as_str() != Some(to) {
        return Some(format!(
            "the map did not rotate (note {note:?}, ended in {})",
            report["map"]
        ));
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
    // The profile first: a stat written in one run must be there in the next, from the same config folder.
    let cfg = ctx.dir.join("config");
    for (label, steps) in [
        ("profile-write", "stat=2301 77,wait=1"),
        ("profile-read", "statis=2301 77"),
    ] {
        let dir = ctx.dir.join(label);
        let res = run_client(&client, install, &dir, &cfg, &[], steps, PROFILE_LIMIT)
            .map(|r| verdict_steps(&r));
        if let Err(e) | Ok(Some(e)) = res {
            out.status = Status::Failed;
            out.reason = Some(format!("{label}: {e}"));
            return Ok(out);
        }
    }
    match run_client(
        &client,
        install,
        &ctx.dir,
        &cfg,
        &["--bots", "4"],
        &steps(),
        LIMIT,
    ) {
        Ok(report) => {
            for f in [
                "ui-script.json",
                "match.png",
                "scoreboard.png",
                "rotated.png",
            ] {
                out.files.push(f.into());
            }
            if let Some(w) = menu_match_verdict(&report) {
                out.status = Status::Failed;
                out.reason = Some(w);
            } else {
                out.notes.push(format!(
                    "played to the score limit, saw the scoreboard, rotated into {} and spawned again",
                    report["map"]
                ));
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok_steps(extra: Value) -> Value {
        json!([{"step": "ingame=120", "ok": true, "note": ""}, extra])
    }

    #[test]
    fn rotation_must_change_the_map_and_end_in_it() {
        let r = |note: &str, map: &str| {
            json!({
                "steps": ok_steps(json!({"step": "maprotate=120", "ok": true, "note": note})),
                "net": {"spawned": true},
                "hud": {"live": true, "health": 100, "max_health": 100, "weapon": {"clip": 30},
                    "map": "compass_map_x",
                    "drawn": {"112": 1, "117": 1, "159": 1, "150": 1}},
                "map": map,
            })
        };
        assert_eq!(menu_match_verdict(&r("mp_a -> mp_b", "mp_b")), None);
        assert!(menu_match_verdict(&r("mp_a -> mp_a", "mp_a")).is_some());
        assert!(menu_match_verdict(&r("mp_a -> mp_b", "mp_c")).is_some());
    }

    #[test]
    fn a_run_without_the_rotation_step_fails() {
        let r = json!({"steps": ok_steps(json!({"step": "shot=x", "ok": true, "note": ""})),
            "net": {"spawned": true}, "hud": {}, "map": "mp_a"});
        assert!(
            menu_match_verdict(&r).is_some(),
            "no rotation step must not pass"
        );
    }
}
