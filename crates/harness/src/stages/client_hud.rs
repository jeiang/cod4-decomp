// SPDX-License-Identifier: GPL-3.0-only
//! `client-hud`: what the client draws on top of the stock HUD menus during a match. The real client plays a team
//! deathmatch against bots on its own listen server with the scripted player, holds the scoreboard up, waits for a
//! message window to have a line up and for a bot to kill it (the killcam), and reports what the HUD got to show
//! (`hud_draw` of `ui-script.json`). The scoreboard must have listed the players, the message windows must have drawn
//! lines, the script hud elements must have arrived, the killcam must have played, a teammate must have had a name
//! over the head and the player under the crosshair a name of their own, and a chat line typed and a vote called through the console must come back from the server. Needs a display and the
//! install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-hud";
const LIMIT: Duration = Duration::from_secs(420);
const ARGS: &[&str] = &["--listen", "--bots", "9", "--autoplay", "--duration", "400"];
const STEPS: &str = "ingame=120,wait=5,loc=MP_CHANGE_CLASS_NEXT_SPAWN:PLATFORM_HOLD_TO_PLANT_EXPLOSIVES,chat=hello from the script,saw=hello from the script:20,console=callvote kick bot5,saw=called a vote:20,throw=g:30,wait=2,scores=on,wait=4,shot=scoreboard,scores=off,feed=120,wait=1,shot=feed,\
killcam=300,shot=killcam_first,wait=1,shot=killcam_next,wait=1,shot=killcam,respawn=120";

fn run_client(client: &Path, install: &Path, dir: &Path) -> Result<Value, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log = File::create(dir.join("client.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--size", "1280x720", "--no-sound"])
        .args(ARGS)
        .arg("--ui-script")
        .arg(STEPS)
        .arg("--out")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| e.to_string())?;
    let end = Instant::now() + LIMIT;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("client hung (killed after {} s)", LIMIT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err(format!(
            "client exited with {status} (see {}/client.log)",
            dir.display()
        ));
    }
    let bytes =
        fs::read(dir.join("ui-script.json")).map_err(|e| format!("no ui-script.json: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// What is wrong with the HUD of a finished run, or `None`.
pub(crate) fn verdict(report: &Value) -> Option<String> {
    let failed: Vec<String> = report["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["ok"] != Value::Bool(true))
        .map(|s| format!("{} ({})", s["step"], s["note"]))
        .collect();
    if !failed.is_empty() {
        return Some(format!("steps failed: {}", failed.join(", ")));
    }
    let h = &report["hud_draw"];
    let n = |k: &str| h[k].as_u64().unwrap_or(0);
    let sum = |k: &str| {
        h[k].as_array()
            .map_or(0, |a| a.iter().filter_map(Value::as_u64).sum::<u64>())
    };
    let mut bad = Vec::new();
    if n("elems_max") == 0 {
        bad.push("no script hud elements arrived");
    }
    if n("scoreboard_frames") == 0 || n("scoreboard_rows_max") < 4 {
        bad.push("the scoreboard listed fewer than 4 players");
    }
    if sum("messages") + n("obituaries") == 0 {
        bad.push("no print or kill line arrived");
    }
    if n("chat") == 0 {
        bad.push("the chat line the player typed never came back");
    }
    if sum("window_lines") == 0 {
        bad.push("the message windows drew no line");
    }
    if n("killcam_frames") == 0 {
        bad.push("no killcam frame");
    }
    // A name is held to account only when the match gave the HUD something to name; `untested` says when not.
    if n("friends_in_sight_frames") > 0 && n("names_max") == 0 {
        bad.push("a teammate was in sight but no overhead name was drawn");
    }
    if n("crosshair_due_frames") > 0 && n("crosshair_name_frames") == 0 {
        bad.push("the crosshair was on a player but never named them");
    }
    // The killcam shows the viewer's own body, which carries the "you" marker over its head.
    let marked = h["head_icon_materials"]
        .as_array()
        .is_some_and(|m| m.iter().any(|v| v == "headiconyouinkillcam"));
    if n("killcam_frames") > 0 && !marked {
        bad.push("the killcam drew no head icon over the viewer's body");
    }
    // A message or hint that is still a string key (MP_CHANGE_CLASS_NEXT_SPAWN) was not localized.
    let keys: Vec<&str> = [&h["unresolved"], &report["hud"]["unresolved_text"]]
        .into_iter()
        .filter_map(|v| v.as_array())
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let unresolved = format!("unlocalized text on screen: {}", keys.join(", "));
    if !keys.is_empty() {
        bad.push(&unresolved);
    }
    // The grenade and d-pad icons are weapon materials; one that is missing draws as a white square.
    let icons: Vec<&str> = report["missing_images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|n| n.starts_with("hud_"))
        .collect();
    let missing = format!("missing HUD icons: {}", icons.join(", "));
    if !icons.is_empty() {
        bad.push(&missing);
    }
    (!bad.is_empty()).then(|| bad.join(", "))
}

/// What the run gave the names nothing to do with, so those checks said nothing.
pub(crate) fn untested(report: &Value) -> Vec<&'static str> {
    let h = &report["hud_draw"];
    let n = |k: &str| h[k].as_u64().unwrap_or(0);
    let mut out = Vec::new();
    if n("friends_in_sight_frames") == 0 {
        out.push("overhead names untested: no teammate was in sight");
    }
    if n("crosshair_due_frames") == 0 {
        out.push("crosshair names untested: the crosshair was never on a player in range");
    }
    out
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
    let dir = ctx.dir.join("hud");
    match run_client(&client, install, &dir) {
        Ok(report) => {
            for f in [
                "ui-script.json",
                "scoreboard.png",
                "feed.png",
                "killcam_first.png",
                "killcam_next.png",
                "killcam.png",
            ] {
                out.files.push(format!("hud/{f}"));
            }
            for k in [
                "elems_max",
                "waypoints_max",
                "scoreboard_rows_max",
                "obituaries",
                "killcam_frames",
            ] {
                if let Some(v) = report["hud_draw"][k].as_f64() {
                    out.metrics.insert(format!("hud.{k}"), v);
                }
            }
            out.notes
                .extend(untested(&report).into_iter().map(String::from));
            if let Some(why) = verdict(&report) {
                out.status = Status::Failed;
                out.reason = Some(why);
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    out.notes.push(
        "scoreboard rows, kill and print lines, script hud elements and the killcam, as the HUD drew them".into(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({
            "steps": [{"step": "ingame=120", "ok": true}],
            "hud_draw": {
                "elems_max": 12, "scoreboard_frames": 90, "scoreboard_rows_max": 10,
                "messages": [2, 0, 0, 0], "obituaries": 3, "window_lines": [40, 0, 0, 0],
                "killcam_frames": 120, "names_max": 3, "crosshair_name_frames": 40,
                "friends_in_sight_frames": 50, "crosshair_due_frames": 60,
                "head_icon_materials": ["headiconyouinkillcam"], "chat": 1
            }
        })
    }

    #[test]
    fn a_run_that_showed_everything_passes() {
        assert_eq!(verdict(&good()), None);
    }

    #[test]
    fn each_missing_piece_is_named() {
        let mut r = good();
        r["hud_draw"]["window_lines"] = json!([0, 0, 0, 0]);
        r["hud_draw"]["killcam_frames"] = json!(0);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("message windows") && why.contains("killcam"),
            "{why}"
        );
        let mut r = good();
        r["hud_draw"]["names_max"] = json!(0);
        r["hud_draw"]["crosshair_name_frames"] = json!(0);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("overhead name") && why.contains("crosshair"),
            "{why}"
        );
        // Nothing in sight to name: not a failure, but said to be untested.
        r["hud_draw"]["friends_in_sight_frames"] = json!(0);
        r["hud_draw"]["crosshair_due_frames"] = json!(0);
        assert_eq!(verdict(&r), None);
        assert_eq!(untested(&r).len(), 2);
        let mut r = good();
        r["hud_draw"]["head_icon_materials"] = json!([]);
        assert!(verdict(&r).unwrap().contains("head icon"));
        let mut r = good();
        r["hud_draw"]["unresolved"] = json!(["MP_CHANGE_CLASS_NEXT_SPAWN"]);
        r["hud"] = json!({"unresolved_text": ["PLATFORM_HOLD_TO_PLANT_EXPLOSIVES"]});
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("MP_CHANGE_CLASS_NEXT_SPAWN")
                && why.contains("PLATFORM_HOLD_TO_PLANT_EXPLOSIVES"),
            "{why}"
        );
        let mut r = good();
        r["hud_draw"]["scoreboard_rows_max"] = json!(2);
        assert!(verdict(&r).unwrap().contains("scoreboard"));
        let mut r = good();
        r["hud_draw"]["chat"] = json!(0);
        assert!(verdict(&r).unwrap().contains("chat"));
        let mut r = good();
        r["steps"] = json!([{"step": "killcam=300", "ok": false, "note": "timed out"}]);
        assert!(verdict(&r).unwrap().contains("killcam=300"));
    }
}
