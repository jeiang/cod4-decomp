// SPDX-License-Identifier: GPL-3.0-only
//! `client-sd-rounds`: the real client starts a three-round Search and Destroy from the stock menus (Start New Server
//! with the round limit, round switch and a short round time set beforehand, as a player can set them), picks a team
//! and a class once, and plays through the round restarts. A person's path, as #362 found it broken: after each
//! round the stock scripts' `map_restart(true)` keeps every client's `pers` array (`SV_MapRestart` hands the kept
//! `scriptId` back to `ClientConnect`; `G_ShutdownGame` frees it only without `savepersist`), so
//! `Callback_PlayerConnect` finds the team and class and spawns the player without the team menu. The sides
//! (`game["switchedsides"]`, which `sd.gsc` `onRoundSwitch` toggles and `onStartGameType` applies to
//! `game["attackers"]`) swap after rounds one and two, not after the last: the objective text the server sets on the
//! client (`cg_objectiveText`, `updateObjectiveText`) names the other role in round two and the first one again in
//! round three. The team survives into the final scoreboard. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{gpu_problem, run_client, steps_problem};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;
use std::time::Duration;

const NAME: &str = "client-sd-rounds";
/// The whole run, a hang included: three rounds of up to a time limit, a bomb timer and the end delays each, plus the
/// load.
const LIMIT: Duration = Duration::from_secs(600);

/// A three-round match with the sides switching every round, half-minute rounds, no score or win limit.
const LIMITS: &str = "set=set scr_sd_roundlimit 3,set=set scr_sd_timelimit 0.5,set=set scr_sd_roundswitch 1,\
    set=set scr_sd_scorelimit 0,set=set scr_sd_winlimit 0";
/// Join a team and a class from the server's menus, and wait until the player is in the world.
const JOIN: &str = "menu=team_marinesopfor:90,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120";
/// A later round: the restart, the spawn with no menu in the way, the same team.
const NEXT: &str = "round=next:150,ingame=120,nomenu=5,team=same:5";

fn steps() -> String {
    format!(
        "{LIMITS},set=set ui_netGametypeName sd,click=Start New Server,menu=createserver:20,click=Start,{JOIN},\
         wait=2,team=save,cvar=save cg_objectiveText,shot=round1,\
         {NEXT},cvar=other cg_objectiveText:5,wait=2,shot=round2,\
         {NEXT},cvar=same cg_objectiveText:5,wait=2,shot=round3,\
         menu=scoreboard:150,team=same:5,wait=1,shot=end"
    )
}

/// Why the run failed, or `None`: the graphics device stayed healthy, every step passed, the player spawned, and the
/// steps that make the check were all there (two restarts, the role swapped once and back once, the team checked after
/// each). The run ends at the final scoreboard, where no weapon is held, so the HUD is not judged here.
fn rounds_verdict(report: &Value) -> Option<String> {
    if let Some(w) = gpu_problem(&report["gpu_health"]) {
        return Some(w);
    }
    if let Some(w) = steps_problem(report) {
        return Some(w);
    }
    if report["net"]["spawned"] != Value::Bool(true) {
        return Some("the player never spawned (left floating as a spectator)".into());
    }
    let count = |prefix: &str| {
        report["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["step"].as_str().is_some_and(|s| s.starts_with(prefix)))
            .count()
    };
    for (prefix, want) in [
        ("round=next", 2),
        ("cvar=other cg_objectiveText", 1),
        ("cvar=same cg_objectiveText", 1),
        ("team=same", 3),
    ] {
        let have = count(prefix);
        if have != want {
            return Some(format!(
                "the run has {have} `{prefix}` steps, not {want}: the rounds were not all checked"
            ));
        }
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
    let cfg = ctx.dir.join("config");
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
                "round1.png",
                "round2.png",
                "round3.png",
                "end.png",
            ] {
                out.files.push(f.into());
            }
            if let Some(w) = rounds_verdict(&report) {
                out.status = Status::Failed;
                out.reason = Some(w);
            } else {
                let notes: Vec<String> = report["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|s| {
                        s["step"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("team=") || s.starts_with("cvar="))
                    })
                    .map(|s| format!("{}: {}", s["step"], s["note"].as_str().unwrap_or("")))
                    .collect();
                out.notes.push(format!(
                    "three rounds on the same team, the role swapped and swapped back: {}",
                    notes.join("; ")
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

    fn report(steps: &[(&str, bool, &str)]) -> Value {
        let steps: Vec<Value> = steps
            .iter()
            .map(|(s, ok, note)| json!({"step": s, "ok": ok, "note": note}))
            .collect();
        json!({
            "steps": steps,
            "gpu_health": {"errors": []},
            // The run ends at the scoreboard: no weapon is held then.
            "net": {"spawned": true},
            "hud": {"live": true, "health": 100, "weapon": null},
            "map": "mp_backlot",
        })
    }

    fn full() -> Vec<(&'static str, bool, &'static str)> {
        vec![
            ("ingame=120", true, ""),
            ("team=save", true, "team 2"),
            ("cvar=save cg_objectiveText", true, "OBJECTIVES_SD_ATTACKER"),
            ("round=next:150", true, "level 0 -> 1"),
            ("ingame=120", true, ""),
            ("nomenu=5", true, ""),
            ("team=same:5", true, "team 2"),
            (
                "cvar=other cg_objectiveText:5",
                true,
                "OBJECTIVES_SD_DEFENDER",
            ),
            ("round=next:150", true, "level 1 -> 2"),
            ("ingame=120", true, ""),
            ("nomenu=5", true, ""),
            ("team=same:5", true, "team 2"),
            (
                "cvar=same cg_objectiveText:5",
                true,
                "OBJECTIVES_SD_ATTACKER",
            ),
            ("menu=scoreboard:150", true, ""),
            ("team=same:5", true, "team 2"),
        ]
    }

    #[test]
    fn every_round_checked_passes() {
        assert_eq!(rounds_verdict(&report(&full())), None);
    }

    #[test]
    fn a_lost_team_or_an_unswapped_role_fails() {
        let mut lost = full();
        lost[6] = ("team=same:5", false, "timed out; team is \"3\"");
        assert!(rounds_verdict(&report(&lost)).is_some());
        let mut same_role = full();
        same_role[7] = (
            "cvar=other cg_objectiveText:5",
            false,
            "timed out; cg_objectiveText is \"OBJECTIVES_SD_ATTACKER\"",
        );
        assert!(rounds_verdict(&report(&same_role)).is_some());
    }

    #[test]
    fn a_run_that_stopped_short_of_the_rounds_fails() {
        let short = &full()[..3];
        assert!(rounds_verdict(&report(short)).is_some());
    }
}
