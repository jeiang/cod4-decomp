// SPDX-License-Identifier: GPL-3.0-only
//! `client-session`: the real client gets a person into the world, both ways. Run 1 is the direct
//! `--listen` path (no menus: the default team and class answers); run 2 starts from the main menu and clicks
//! through the stock menus (Start New Server, the server's team menu, the class menu, Assault). Each run must
//! complete every step and report that the local player spawned. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-session";
const LIMIT: Duration = Duration::from_secs(240);

const DIRECT: &[&str] = &["--listen", "--bots", "3"];
const DIRECT_STEPS: &str = "ingame=120,wait=3,shot=direct";
const MENU_STEPS: &str = "set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,\
click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=3,shot=menus";

/// Domination: its flags are objectives, which the minimap must mark.
const OBJECTIVE_ARGS: &[&str] = &["--listen", "--gametype", "dom", "--bots", "3"];
const OBJECTIVE_STEPS: &str = "ingame=120,wait=8,shot=objectives";

/// Runs the client with `args` and `--ui-script steps`; returns the parsed `ui-script.json`.
pub(super) fn run_client(
    client: &Path,
    install: &Path,
    dir: &Path,
    config_dir: &Path,
    args: &[&str],
    steps: &str,
    limit: Duration,
) -> Result<Value, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log = File::create(dir.join("client.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--size", "1280x720", "--no-sound", "--bots", "3"])
        .args(args)
        // The player profile and config live next to the run's output, never in the tester's own folders.
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--ui-script")
        .arg(steps)
        .arg("--out")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| e.to_string())?;
    let end = Instant::now() + limit;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("client hung (killed after {} s)", limit.as_secs()));
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

/// Why a run failed, or `None` when every step passed and the player spawned.
pub(super) fn verdict(report: &Value) -> Option<String> {
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
    if report["net"]["spawned"] != Value::Bool(true) {
        return Some("the player never spawned (left floating as a spectator)".into());
    }
    if let Some(w) = players_problem(&report["net"]) {
        return Some(w);
    }
    hud_problem(&report["hud"])
}

/// Why the other players were not in the picture, or `None`: every player the server announced must have been built
/// and drawn in some frame (a mode without teams once drew nobody).
fn players_problem(net: &Value) -> Option<String> {
    let seen = net["players_seen_max"].as_u64().unwrap_or(0);
    let drawn = net["players_drawn_max"].as_u64().unwrap_or(0);
    if let Some(f) = net["player_faults"].as_array().filter(|f| !f.is_empty()) {
        return Some(format!("other players could not be drawn: {f:?}"));
    }
    (seen > 0 && drawn == 0)
        .then(|| format!("{seen} other players were announced and none was ever drawn"))
}

/// What a spawned player's HUD must have: health replicated from the server, the weapon's ammunition, the map
/// image the minimap draws, and the stock owner-draw pieces (low-health overlay, magazine, compass) actually drawn.
fn hud_problem(hud: &Value) -> Option<String> {
    if hud["live"] != Value::Bool(true) {
        return Some("the HUD facts never went live".into());
    }
    if hud["health"].as_i64().unwrap_or(0) <= 0 || hud["max_health"].as_i64().unwrap_or(0) <= 0 {
        return Some(format!(
            "health did not reach the client ({} of {})",
            hud["health"], hud["max_health"]
        ));
    }
    if hud["weapon"]["clip"].as_i64().unwrap_or(-1) <= 0 {
        return Some("the HUD has no ammunition for the held weapon".into());
    }
    if hud["map"].as_str().is_none_or(str::is_empty) {
        return Some("the map's minimap (setMiniMap) never reached the client".into());
    }
    // Overlay, magazine graphic, minimap image, player arrow.
    for id in ["112", "117", "159", "150"] {
        if hud["drawn"][id].as_u64().unwrap_or(0) == 0 {
            return Some(format!("owner-draw {id} never drew"));
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
    let mut problems = Vec::new();
    for (label, args, steps) in [
        ("direct", DIRECT, DIRECT_STEPS),
        ("menus", &[][..], MENU_STEPS),
        ("objectives", OBJECTIVE_ARGS, OBJECTIVE_STEPS),
    ] {
        let dir = ctx.dir.join(label);
        match run_client(
            &client,
            install,
            &dir,
            &dir.join("config"),
            args,
            steps,
            LIMIT,
        ) {
            Ok(report) => {
                out.files.push(format!("{label}/ui-script.json"));
                out.files.push(format!("{label}/{label}.png"));
                if let Some(w) = verdict(&report) {
                    problems.push(format!("{label}: {w}"));
                } else if label == "objectives"
                    && report["hud"]["objective_marks"].as_u64().unwrap_or(0) == 0
                {
                    problems.push("objectives: the minimap drew no objective marks".into());
                }
                if let Some(v) = report["net"]["snapshots"].as_f64() {
                    out.metrics.insert(format!("{label}.snapshots"), v);
                }
            }
            Err(e) => problems.push(format!("{label}: {e}")),
        }
    }
    out.notes
        .push("direct --listen and the stock-menu path each spawned the local player; the minimap marked domination's flags".into());
    if !problems.is_empty() {
        out.status = Status::Failed;
        out.reason = Some(problems.join("; "));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({"live": true, "health": 100, "max_health": 100, "weapon": {"clip": 30},
            "map": "compass_map_mp_crash", "drawn": {"112": 5, "117": 5, "159": 5, "150": 5}})
    }

    #[test]
    fn announced_players_must_be_drawn() {
        assert_eq!(players_problem(&json!({})), None);
        assert_eq!(
            players_problem(
                &json!({"players_seen_max": 3, "players_drawn_max": 2, "player_faults": []})
            ),
            None
        );
        assert!(
            players_problem(
                &json!({"players_seen_max": 3, "players_drawn_max": 0, "player_faults": []})
            )
            .unwrap()
            .contains("none was ever drawn")
        );
        assert!(
            players_problem(&json!({"players_seen_max": 3, "players_drawn_max": 3, "player_faults": ["no body"]}))
                .unwrap()
                .contains("no body")
        );
    }

    #[test]
    fn a_working_hud_passes_and_each_missing_piece_is_named() {
        assert_eq!(hud_problem(&good()), None);
        let mut h = good();
        h["health"] = json!(0);
        assert!(hud_problem(&h).unwrap().contains("health"));
        let mut h = good();
        h["map"] = Value::Null;
        assert!(hud_problem(&h).unwrap().contains("minimap"));
        let mut h = good();
        h["drawn"] = json!({"112": 5, "117": 5, "150": 5});
        assert!(hud_problem(&h).unwrap().contains("159"));
        let mut h = good();
        h["weapon"]["clip"] = json!(0);
        assert!(hud_problem(&h).unwrap().contains("ammunition"));
    }
}
