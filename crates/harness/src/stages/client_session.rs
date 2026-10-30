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

/// The arguments that give a client run the harness's own fully unlocked profile in `config_dir`, so it never reads or
/// writes the tester's profile, config or the install's profiles (those are listed, never active).
pub(super) fn profile_args(config_dir: &Path) -> [&std::ffi::OsStr; 3] {
    [
        "--config-dir".as_ref(),
        config_dir.as_os_str(),
        "--harness-profile".as_ref(),
    ]
}

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
        .args(profile_args(config_dir))
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
            "client exited with {status}{} (see {}/client.log)",
            last_error(&dir.join("client.log")),
            dir.display()
        ));
    }
    let bytes =
        fs::read(dir.join("ui-script.json")).map_err(|e| format!("no ui-script.json: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// The last line the client logged at ERROR or above, as `: <line>`, so a failed run says what it died of.
fn last_error(log: &Path) -> String {
    let Ok(bytes) = fs::read(log) else {
        return String::new();
    };
    let tail = &bytes[bytes.len().saturating_sub(8 * 1024)..];
    let text = String::from_utf8_lossy(tail);
    let line = text
        .lines()
        .rfind(|l| l.contains("ERROR") || l.contains("WARN"))
        .map(str::trim);
    match line {
        Some(l) => format!(": {l}"),
        None => String::new(),
    }
}

/// Why a run's graphics device is a failure, or `None`: any error it reported outside a scope is. A device that was
/// lost and made again is no failure of the run (it is in the report), but one that could not be is.
pub(super) fn gpu_problem(health: &Value) -> Option<String> {
    let errors: Vec<&str> = health["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    (!errors.is_empty()).then(|| format!("graphics errors: {}", errors.join("; ")))
}

/// Material names the renderer may fail to prepare in a stock run, each with the reason it is benign. Empty: a
/// material whose draws are skipped is a defect, so a new entry needs the name and why it is sound to leave it.
const ALLOWED_MATERIAL_FAILURES: &[&str] = &[];

/// Why a run's renderer does not vouch for it, or `None`: the client's `render` record must be there, and every
/// material it could not prepare (no techset, no usable technique, a pipeline that failed to build) is a draw the
/// player never saw. Those were counted and never asserted (#375). Images that did not load are the stage's to
/// report; a UI image is checked by the menus' `missing_images`.
pub(super) fn render_problem(report: &Value) -> Option<String> {
    let Some(failures) = report["render"]["material_failures"].as_object() else {
        return Some(
            "the run reported no render record, so material failures were not checked".into(),
        );
    };
    let bad: Vec<String> = failures
        .iter()
        .filter(|(name, _)| !ALLOWED_MATERIAL_FAILURES.contains(&name.as_str()))
        .map(|(name, why)| format!("{name} ({})", why.as_str().unwrap_or("?")))
        .collect();
    (!bad.is_empty()).then(|| {
        format!(
            "{} material(s) the renderer could not prepare, their draws skipped: {}",
            bad.len(),
            bad.join(", ")
        )
    })
}

/// Why a run's steps do not vouch for it, or `None`: every step the script ran must have passed, and a report without
/// steps proves nothing (a run that ended before its script reached a step).
pub(super) fn steps_problem(report: &Value) -> Option<String> {
    let Some(steps) = report["steps"].as_array().filter(|s| !s.is_empty()) else {
        return Some("the run reported no steps, so nothing was exercised".into());
    };
    let failed: Vec<String> = steps
        .iter()
        .filter(|s| s["ok"] != Value::Bool(true))
        .map(|s| format!("{} ({})", s["step"], s["note"]))
        .collect();
    (!failed.is_empty()).then(|| format!("steps failed: {}", failed.join(", ")))
}

/// Why a run failed, or `None` when every step passed and the player spawned.
pub(super) fn verdict(report: &Value) -> Option<String> {
    if let Some(w) = gpu_problem(&report["gpu_health"]) {
        return Some(w);
    }
    if let Some(w) = render_problem(report) {
        return Some(w);
    }
    if let Some(w) = steps_problem(report) {
        return Some(w);
    }
    if report["net"]["spawned"] != Value::Bool(true) {
        return Some("the player never spawned (left floating as a spectator)".into());
    }
    if let Some(w) = players_problem(&report["net"]) {
        return Some(w);
    }
    if let Some(w) = torso_problem(&report["net"]) {
        return Some(w);
    }
    hud_problem(&report["hud"])
}

/// Why the other players were not in the picture, or `None`: every player the server announced must have been built
/// and drawn in some frame. A mode without teams once drew nobody, and one that drew some of them was left to pass
/// with `drawn == 0` checked only; a partial draw is the same bug seen earlier.
fn players_problem(net: &Value) -> Option<String> {
    let seen = net["players_seen_max"].as_u64().unwrap_or(0);
    let drawn = net["players_drawn_max"].as_u64().unwrap_or(0);
    if let Some(f) = net["player_faults"].as_array().filter(|f| !f.is_empty()) {
        return Some(format!("other players could not be drawn: {f:?}"));
    }
    if seen == 0 {
        return Some("the server announced no other player to draw".into());
    }
    (drawn < seen).then(|| {
        format!("{drawn} of the {seen} other players the server announced were ever drawn")
    })
}

/// Why the bots' weapon handling never showed on their bodies, or `None`: when another player was seen reloading,
/// swinging the knife or throwing a grenade, some upper-body clip must have played on one, and the report must carry
/// the counters at all.
fn torso_problem(net: &Value) -> Option<String> {
    let acted = net["remote_weapon_frames"].as_u64().unwrap_or(0);
    let Some(clips) = net["torso_clips"].as_array() else {
        return Some("the run reported no torso clips counter".into());
    };
    (acted > 0 && clips.is_empty())
        .then(|| format!("other players handled weapons for {acted} frames but none ever played a torso animation"))
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
    // A weapon with no magazine (the C4) is drawn with its count instead, and reports -1.
    match hud["weapon"]["clip"].as_i64() {
        Some(-1) => {}
        Some(0) | None => return Some("the HUD has no ammunition for the held weapon".into()),
        Some(_) => {}
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
        ("menus", &["--bots", "3"][..], MENU_STEPS),
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
        assert_eq!(
            players_problem(&json!({"players_seen_max": 0, "players_drawn_max": 0})).unwrap(),
            "the server announced no other player to draw"
        );
        assert_eq!(
            players_problem(
                &json!({"players_seen_max": 3, "players_drawn_max": 3, "player_faults": []})
            ),
            None
        );
        // A partial draw is the same bug caught earlier.
        assert!(
            players_problem(
                &json!({"players_seen_max": 3, "players_drawn_max": 2, "player_faults": []})
            )
            .unwrap()
            .contains("2 of the 3")
        );
        assert!(
            players_problem(
                &json!({"players_seen_max": 3, "players_drawn_max": 0, "player_faults": []})
            )
            .unwrap()
            .contains("0 of the 3")
        );
        assert!(
            players_problem(&json!({"players_seen_max": 3, "players_drawn_max": 3, "player_faults": ["no body"]}))
                .unwrap()
                .contains("no body")
        );
    }

    #[test]
    fn a_torso_clip_is_required_wherever_players_were_drawn() {
        assert!(torso_problem(&json!({})).unwrap().contains("counter"));
        let seen = |frames: u64, clips: Value| {
            torso_problem(
                &json!({"players_drawn_max": 2, "remote_weapon_frames": frames, "torso_clips": clips}),
            )
        };
        assert_eq!(
            seen(0, json!([])),
            None,
            "a scene in which nobody handled a weapon cannot check the torso animation"
        );
        assert_eq!(seen(5, json!(["pt_reload_stand_rifle"])), None);
        assert!(seen(5, json!([])).unwrap().contains("torso"));
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

    #[test]
    fn reported_graphics_errors_fail_a_run_and_a_recovered_loss_does_not() {
        assert_eq!(gpu_problem(&Value::Null), None);
        assert_eq!(
            gpu_problem(&json!({"device_losses": ["while running: Destroyed"], "errors": []})),
            None
        );
        let bad = json!({"device_losses": [], "errors": ["Buffer is invalid"]});
        assert!(gpu_problem(&bad).is_some());
    }

    #[test]
    fn a_material_the_renderer_could_not_prepare_fails_and_no_record_fails() {
        assert_eq!(
            render_problem(&json!({"render": {"material_failures": {}, "image_failures": []}})),
            None
        );
        let why = render_problem(&json!({"render": {"material_failures": {
            "mc/foo": "techset \"x\" has techniques [1], wanted [0]"}}}))
        .unwrap();
        assert!(
            why.contains("mc/foo") && why.contains("wanted [0]"),
            "{why}"
        );
        assert!(
            render_problem(&json!({"steps": []}))
                .unwrap()
                .contains("no render record")
        );
        assert!(
            render_problem(&json!({"render": {"image_failures": []}}))
                .unwrap()
                .contains("no render record")
        );
    }
}
