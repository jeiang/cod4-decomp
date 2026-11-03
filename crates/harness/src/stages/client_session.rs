// SPDX-License-Identifier: GPL-3.0-only
//! `client-session`: the real client gets a person into the world, both ways. Run 1 is the direct
//! `--listen` path (no menus: the default team and class answers); run 2 starts from the main menu and clicks
//! through the stock menus (Start New Server, the server's team menu, the class menu, Assault). Each run must
//! complete every step and report that the local player spawned. Run 4 is the original's launch line (#397): the
//! client starts with `+set cg_fov 75 +exec <file> +name <nick> +connect <server>` beside its own `--` options and
//! must join the `cod4e-server` started for it under that name, with the variable set over the profile's config and
//! the file run, read back through the control channel. Needs a display and the install; skips without.
//!
//! The player spawns before the prematch countdown ends, and the countdown hides the weapon and compass HUD, so a
//! step that needs that HUD waits for it to have drawn rather than for the spawn. The players check compares who the
//! server announced with who was ever drawn, by identity, from the run's `report` (not a count at one moment).
use super::client_custom_class::session_report;
use super::client_dedicated::{Dedicated, lists_player, locate_server};
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-session";

const DIRECT: &[&str] = &["--listen", "--bots", "3"];
/// Domination: its flags are objectives, which the minimap must mark.
const OBJECTIVE_ARGS: &[&str] = &["--listen", "--gametype", "dom", "--bots", "3"];

/// The arguments that give a client run the harness's own fully unlocked profile in `config_dir`, so it never reads or
/// writes the tester's profile, config or the install's profiles (those are listed, never active).
pub(crate) fn profile_args(config_dir: &Path) -> [&std::ffi::OsStr; 3] {
    [
        "--config-dir".as_ref(),
        config_dir.as_os_str(),
        "--harness-profile".as_ref(),
    ]
}

/// The visible items of the top menu must include every `see` label and none of the `nosee` labels.
pub(super) fn check_menu(c: &mut Driven, see: &[&str], nosee: &[&str]) -> Result<(), String> {
    let menu = c.state("menu")?;
    let top = menu["open"]
        .as_array()
        .and_then(|open| open.last())
        .and_then(Value::as_str)
        .ok_or("no menu is open")?;
    let has = |want: &str| {
        menu["items"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["menu"].as_str() == Some(top)
                    && item["visible"] == Value::Bool(true)
                    && (item["name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(want))
                        || item["text"].as_str().is_some_and(|text| {
                            !text.is_empty() && text.eq_ignore_ascii_case(want)
                        }))
            })
        })
    };
    let wrong: Vec<String> = see
        .iter()
        .filter(|want| !has(want))
        .map(|want| format!("missing {want}"))
        .chain(
            nosee
                .iter()
                .filter(|want| has(want))
                .map(|want| format!("unexpected {want}")),
        )
        .collect();
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{top}: {}; open {}",
            wrong.join(", "),
            menu["open"]
        ))
    }
}

/// Waits for the weapon in hand whose name starts with `prefix`.
pub(super) fn weapon_prefix(c: &mut Driven, prefix: &str, limit: Duration) -> Result<(), String> {
    let end = Instant::now() + limit;
    loop {
        let have = c.lookup("player.weapon")?;
        if have
            .as_str()
            .is_some_and(|weapon| weapon.starts_with(prefix))
        {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!(
                "the weapon is {have:?}, not one beginning {prefix:?}"
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

fn spawn(c: &mut Driven) -> Result<(), String> {
    c.until(
        json!({"net.spawned": true, "player.alive": true}),
        Duration::from_secs(120),
        "the player did not spawn",
    )?;
    Ok(())
}

/// One run's play, driven through the control channel.
type Play = fn(&mut Driven) -> Result<(), String>;

/// The direct start loads `mp_crash` before any menu: the in-game menu's map title must name it, not show the bare `@`
/// an unset `mapname` leaves (#372).
fn direct(c: &mut Driven) -> Result<(), String> {
    c.step("spawn", spawn)?;
    c.pause(Duration::from_secs(3))?;
    c.step("direct capture", |c| c.capture("direct").map(|_| ()))?;
    c.step("direct in-game menu", |c| {
        c.bind("togglemenu", "tap")?;
        c.until(
            json!({"menu.open.0": {"exists": true}}),
            Duration::from_secs(5),
            "the in-game menu did not open on the menu key",
        )?;
        check_menu(c, &["Crash"], &["@"])?;
        c.capture("direct_menu")?;
        c.key("escape", "tap")?;
        c.until(
            json!({"menu.open": {"len": 0}}),
            Duration::from_secs(5),
            "Escape did not close the in-game menu",
        )?;
        Ok(())
    })
}

fn menus(c: &mut Driven) -> Result<(), String> {
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
    // The prematch countdown hides the weapon and compass HUD; the player spawns before it ends, so the capture waits
    // for the HUD the menus check (overlay, magazine graphic, minimap image, player arrow) to have drawn.
    c.step("menu HUD after the prematch", |c| {
        c.until(
            json!({
                "hud.report.drawn.112": {"ge": 1},
                "hud.report.drawn.117": {"ge": 1},
                "hud.report.drawn.159": {"ge": 1},
                "hud.report.drawn.150": {"ge": 1},
            }),
            Duration::from_secs(30),
            "the weapon and compass HUD never drew after the prematch",
        )
        .map(|_| ())
    })?;
    c.pause(Duration::from_secs(1))?;
    c.step("menu capture", |c| c.capture("menus").map(|_| ()))
}

fn objectives(c: &mut Driven) -> Result<(), String> {
    c.step("objective spawn", spawn)?;
    c.pause(Duration::from_secs(8))?;
    c.step("objective capture", |c| c.capture("objectives").map(|_| ()))
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

/// Why a run's renderer does not vouch for it, or `None`: the client's `render` record must be there, every material
/// it could not prepare (no techset, no usable technique, a pipeline that failed to build) is a draw the player never
/// saw, and every image that did not load is a placeholder they saw instead. Both were counted and never asserted
/// (#375); the default suite's maps load every image, so no image is allowed to fail.
pub(super) fn render_problem(report: &Value) -> Option<String> {
    let render = &report["render"];
    let (Some(failures), Some(images)) = (
        render["material_failures"].as_object(),
        render["image_failures"].as_array(),
    ) else {
        return Some(
            "the run reported no render record, so material and image failures were not checked"
                .into(),
        );
    };
    let bad: Vec<String> = failures
        .iter()
        .map(|(name, why)| (name, why.as_str().unwrap_or("?")))
        .map(|(name, why)| format!("{name} ({why})"))
        .collect();
    if !bad.is_empty() {
        return Some(format!(
            "{} material(s) the renderer could not prepare, their draws skipped: {}",
            bad.len(),
            bad.join(", ")
        ));
    }
    (!images.is_empty()).then(|| {
        format!(
            "{} image(s) did not load, drawn as placeholders: {}",
            images.len(),
            Value::Array(images.clone())
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
/// and drawn in some frame. Compare client identities over the whole run, not simultaneous maxima: a death can make
/// one frame announce ten players and draw nine even when all ten were drawn at different times.
fn players_problem(net: &Value) -> Option<String> {
    let (Some(seen), Some(drawn)) = (
        net["players_seen"].as_array(),
        net["players_drawn"].as_array(),
    ) else {
        return Some("the run reported no per-player draw counters".into());
    };
    if let Some(f) = net["player_faults"].as_array().filter(|f| !f.is_empty()) {
        return Some(format!("other players could not be drawn: {f:?}"));
    }
    if seen.is_empty() {
        return Some("the server announced no other player to draw".into());
    }
    let missing: Vec<&Value> = seen.iter().filter(|id| !drawn.contains(id)).collect();
    (!missing.is_empty()).then(|| {
        format!(
            "{} of the {} other players the server announced were ever drawn (never drawn: {missing:?})",
            seen.len() - missing.len(),
            seen.len()
        )
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
    if let Some(w) = sprint_problem(hud) {
        return Some(w);
    }
    // Overlay, magazine graphic, minimap image, player arrow.
    for id in ["112", "117", "159", "150"] {
        if hud["drawn"][id].as_u64().unwrap_or(0) == 0 {
            return Some(format!("owner-draw {id} never drew"));
        }
    }
    None
}

/// The longer-sprint perk bit of `ps.perks` (`sim::pm::state::PERK_SPRINT`).
const PERK_SPRINT: u64 = 0x400;

/// Why the HUD's sprint budget is not the stock one, or `None`: `BG_GetMaxSprintTime` is `player_sprintTime` (stock
/// default 4 s) times the weapon's `sprintDurationScale`, times `perk_sprintMultiplier` (stock default 2) with the
/// perk, capped at 0x3FFF ms. The report carries each input next to the budget, so a budget that varies between runs
/// of the same class (#375 play-06: 4000, 7000, 8000, 14000) names what changed, and a dvar that drifted from stock
/// fails here.
pub(super) fn sprint_problem(hud: &Value) -> Option<String> {
    let f = &hud["sprint_facts"];
    let (Some(max), Some(scale), Some(time), Some(mult), Some(perks)) = (
        hud["sprint"][1].as_f64(),
        f["scale"].as_f64(),
        f["player_sprint_time"].as_f64(),
        f["perk_sprint_multiplier"].as_f64(),
        f["perks"].as_u64(),
    ) else {
        return Some("the HUD reported no sprint budget or its inputs".into());
    };
    if time != 4.0 || mult != 2.0 {
        return Some(format!(
            "player_sprintTime {time} / perk_sprintMultiplier {mult} are not the stock 4 / 2"
        ));
    }
    let perk = if perks & PERK_SPRINT != 0 { mult } else { 1.0 };
    let want = ((scale * time * 1000.0 * perk) as i64).min(0x3FFF) as f64;
    ((max - want).abs() > 1.0).then(|| {
        format!(
            "sprint budget {max} ms, but {} x scale {scale} x {time} s x perk {perk} is {want} (perks {perks:#x})",
            f["weapon"]
        )
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
    let mut problems = Vec::new();
    let runs: [(&str, &[&str], Play); 3] = [
        ("direct", DIRECT, direct),
        ("menus", &["--bots", "3"], menus),
        ("objectives", OBJECTIVE_ARGS, objectives),
    ];
    for (label, args, play) in runs {
        let dir = ctx.dir.join(label);
        let mut c = match Driven::start(&client, install, &dir, args) {
            Ok(c) => c,
            Err(e) => {
                problems.push(format!("{label}: {e}"));
                continue;
            }
        };
        let played = play(&mut c);
        let report = session_report(&mut c, "control-run.json");
        let status = c.finish(Duration::from_secs(30));
        out.files.push(format!("{label}/control-run.json"));
        for shot in [label, "direct_menu"] {
            let file = format!("{shot}.png");
            if (shot == label || label == "direct") && dir.join(&file).is_file() {
                out.files.push(format!("{label}/{file}"));
            }
        }
        match (played, report) {
            (Err(e), report) => {
                if let Ok(report) = report
                    && let Some(v) = report["net"]["snapshots"].as_f64()
                {
                    out.metrics.insert(format!("{label}.snapshots"), v);
                }
                problems.push(format!("{label}: {e}"));
            }
            (Ok(()), Err(e)) => problems.push(format!("{label}: the run's report failed: {e}")),
            (Ok(()), Ok(report)) => {
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
        }
        if !status.is_some_and(|s| s.success()) {
            problems.push(format!(
                "{label}: the client did not exit successfully: {status:?}"
            ));
        }
    }
    match command_line(ctx, &client, install) {
        Ok(note) => {
            out.files
                .extend(["cmdline/client.log".into(), "cmdline/server.log".into()]);
            out.notes.push(note);
        }
        Err(e) => problems.push(format!("command line: {e}")),
    }
    out.notes
        .push("direct --listen and the stock-menu path each spawned the local player; the minimap marked domination's flags".into());
    if !problems.is_empty() {
        out.status = Status::Failed;
        out.reason = Some(problems.join("; "));
    }
    Ok(out)
}

/// The name the command-line run joins under (the server keeps 15 characters).
const CMDLINE_NICK: &str = "CmdLineTest";

/// The original's launch line: `+set cg_fov 75 +exec <file> +name <nick> +connect 127.0.0.1:<port>`, with the
/// harness's `--` options after it (`Driven` adds `--control`, `--config-dir`, `--harness-profile`, `--out`). The
/// harness profile's config sets `cg_fov 65`, so a `+set` the configs overwrite fails; the file sets a marker only it
/// sets; the join must reach the server, which must list the player under the `+name` given.
fn command_line(ctx: &StageCtx, client: &Path, install: &Path) -> Result<String, String> {
    let server = locate_server().ok_or("cod4e-server binary not found next to the harness")?;
    let dir = ctx.dir.join("cmdline");
    let work = dir.join("server");
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let profile = dir.join("config/players/profiles/harness/config_mp.cfg");
    fs::create_dir_all(profile.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&profile, "seta cg_fov 65\n").map_err(|e| e.to_string())?;
    let cfg = dir.join("cmdline.cfg");
    fs::write(&cfg, "set zz_cmdline_exec ran\n").map_err(|e| e.to_string())?;
    let (mut dedicated, port) =
        Dedicated::boot(&server, install, &work, 0, &dir.join("server.log"))?;
    let target = format!("127.0.0.1:{port}");
    let cfg = cfg.to_string_lossy();
    let mut c = Driven::start(
        client,
        install,
        &dir,
        &[
            "+set",
            "cg_fov",
            "75",
            "+exec",
            &cfg,
            "+name",
            CMDLINE_NICK,
            "+connect",
            &target,
        ],
    )?;
    let checked = (|| -> Result<(), String> {
        c.until(
            json!({"net.connected": true}),
            Duration::from_secs(120),
            "+connect did not join the server",
        )?;
        let fov = c.ask(json!({"cmd": "cvar", "name": "cg_fov"}))?;
        if fov["value"] != json!("75") {
            return Err(format!(
                "+set cg_fov 75 did not outrank the profile's config: cg_fov is {}",
                fov["value"]
            ));
        }
        let ran = c.ask(json!({"cmd": "cvar", "name": "zz_cmdline_exec"}))?;
        if ran["value"] != json!("ran") {
            return Err(format!(
                "+exec did not run the file: zz_cmdline_exec is {}",
                ran["value"]
            ));
        }
        Ok(())
    })();
    let status = dedicated.command("status", Duration::from_secs(10));
    c.finish(Duration::from_secs(10));
    checked?;
    let status = status.ok_or("the server did not answer status")?;
    if !lists_player(&status, CMDLINE_NICK) {
        return Err(format!(
            "the server does not list {CMDLINE_NICK} after +connect: {status:?}"
        ));
    }
    dedicated.quit();
    Ok(format!(
        "+set cg_fov 75 +exec cmdline.cfg +name {CMDLINE_NICK} +connect {target}: joined under that name, cg_fov 75 \
         over the profile's 65, the file ran"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({"live": true, "health": 100, "max_health": 100, "weapon": {"clip": 30},
            "map": "compass_map_mp_crash", "drawn": {"112": 5, "117": 5, "159": 5, "150": 5},
            "sprint": [4000, 7000],
            "sprint_facts": {"weapon": "p90_mp", "scale": 1.75, "player_sprint_time": 4.0,
                "perk_sprint_multiplier": 2.0, "perks": 0}})
    }

    #[test]
    fn announced_players_must_be_drawn() {
        assert_eq!(
            players_problem(&json!({"players_seen": [], "players_drawn": []})).unwrap(),
            "the server announced no other player to draw"
        );
        assert_eq!(
            players_problem(
                &json!({"players_seen": [1, 2, 3], "players_drawn": [1, 2, 3], "player_faults": []})
            ),
            None
        );
        // Different per-frame maxima are not a missing draw: all three identities were drawn over the run.
        assert_eq!(
            players_problem(&json!({"players_seen_max": 3, "players_drawn_max": 2,
                "players_seen": [1, 2, 3], "players_drawn": [1, 2, 3], "player_faults": []})),
            None
        );
        // A partial draw is the same bug caught earlier.
        assert!(
            players_problem(
                &json!({"players_seen": [1, 2, 3], "players_drawn": [1, 2], "player_faults": []})
            )
            .unwrap()
            .contains("2 of the 3")
        );
        assert!(
            players_problem(
                &json!({"players_seen": [1, 2, 3], "players_drawn": [], "player_faults": []})
            )
            .unwrap()
            .contains("0 of the 3")
        );
        assert!(
            players_problem(
                &json!({"players_seen": [1, 2, 3], "players_drawn": [1, 2, 3],
                "player_faults": ["no body"]})
            )
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
    fn the_sprint_budget_is_the_stock_formula_of_its_recorded_inputs() {
        assert_eq!(sprint_problem(&good()), None);
        let mut h = good();
        h["sprint"] = json!([14000, 14000]);
        assert!(
            sprint_problem(&h).unwrap().contains("is 7000"),
            "doubled with no perk"
        );
        h["sprint_facts"]["perks"] = json!(0x400);
        assert_eq!(
            sprint_problem(&h),
            None,
            "doubled by the longer-sprint perk"
        );
        let mut h = good();
        h["sprint_facts"]["player_sprint_time"] = json!(7.0);
        assert!(sprint_problem(&h).unwrap().contains("not the stock"));
        let mut h = good();
        h["sprint_facts"] = Value::Null;
        assert!(sprint_problem(&h).unwrap().contains("no sprint budget"));
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
    fn a_material_or_image_the_renderer_could_not_load_fails_and_no_record_fails() {
        let render = |materials: Value, images: Value| json!({"render": {"material_failures": materials, "image_failures": images}});
        assert_eq!(render_problem(&render(json!({}), json!([]))), None);
        let why = render_problem(&render(
            json!({"mc/foo": "techset \"x\" has techniques [1], wanted [0]"}),
            json!([]),
        ))
        .unwrap();
        assert!(
            why.contains("mc/foo") && why.contains("wanted [0]"),
            "{why}"
        );
        let why = render_problem(&render(json!({}), json!(["ammo_counter"]))).unwrap();
        assert!(
            why.contains("image") && why.contains("ammo_counter"),
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
        // Cross-zone references must resolve before drawing (#453); no material failure is allowed, including a
        // comma-prefixed name with no techset.
        assert!(
            render_problem(&render(
                json!({",gfx_smk_white_atlas": "no techset"}),
                json!([])
            ))
            .is_some()
        );
        assert!(
            render_problem(&render(
                json!({"gfx_smk_white_atlas": "no techset"}),
                json!([])
            ))
            .is_some()
        );
        assert!(
            render_problem(&render(
                json!({",gfx_smk_white_atlas": "shader x failed"}),
                json!([])
            ))
            .is_some()
        );
    }
}
