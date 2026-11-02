// SPDX-License-Identifier: GPL-3.0-only
//! `client-menu-match`: the real client, started with no match flags, plays a whole match from the stock menus and
//! carries on into the next map. A person's path: Start New Server (the score limit set low beforehand, as a player can
//! set it), the server's team menu, the class menu, play, the end-of-match scoreboard, the server's map rotation, then
//! the team and class menus again in the new map and a second spawn. Every step must complete and the client must end
//! in a different map than it began in. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::verdict;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-menu-match";
/// The whole run, a hang included; a match to three kills and the map load take a minute or two.
const LIMIT: Duration = Duration::from_secs(420);
/// A menu-only run that sets or checks a stat.
const PROFILE_LIMIT: Duration = Duration::from_secs(90);

/// Types a setting at the dev console and reads back the value the client actually took.
fn set_cvar(c: &mut Driven, name: &str, value: &str) -> Result<(), String> {
    c.console(&format!("set {name} {value}"))?;
    let reply = c.ask(json!({"cmd": "cvar", "name": name}))?;
    if reply["value"].as_str() != Some(value) {
        return Err(format!(
            "set {name} {value}: the client reports {}",
            reply["value"]
        ));
    }
    Ok(())
}

/// Starts a listen server through the same two pointer clicks as a person at the main menu.
fn start_server(c: &mut Driven) -> Result<(), String> {
    c.until(
        json!({"menu.open.0": {"exists": true}}),
        Duration::from_secs(30),
        "the main menu did not open",
    )?;
    c.click("Start New Server")?;
    c.until(
        json!({"menu.open.0": "createserver"}),
        Duration::from_secs(20),
        "Start New Server did not open createserver",
    )?;
    c.click("Start")?;
    Ok(())
}

/// Joins a team and a class with the pointer, then waits for the spawn and the menus to close.
fn join(c: &mut Driven) -> Result<(), String> {
    c.until(
        json!({"menu.open.0": "team_marinesopfor"}),
        Duration::from_secs(90),
        "the team menu did not open",
    )?;
    c.click("auto_assign")?;
    c.until(
        json!({"menu.open.0": "changeclass"}),
        Duration::from_secs(30),
        "the class menu did not open",
    )?;
    c.pause(Duration::from_secs(1))?;
    c.click("Assault")?;
    c.until(
        json!({"net.spawned": true, "player.alive": true, "menu.open": {"len": 0}}),
        Duration::from_secs(120),
        "the player did not spawn",
    )?;
    Ok(())
}

/// The listen server's map name and monotonically increasing level-load count.
fn server_map(c: &mut Driven) -> Result<(String, u64), String> {
    let state = c.ask(json!({"cmd": "server", "op": "state", "what": "map"}))?;
    let map = state["map"]
        .as_str()
        .ok_or_else(|| format!("the server reported no map name: {state}"))?
        .to_owned();
    let loads = state["level_loads"]
        .as_u64()
        .ok_or_else(|| format!("the server reported no level-load count: {state}"))?;
    Ok((map, loads))
}

/// Waits for the server itself to begin another level, whether a round restart or a map rotation.
fn next_level(c: &mut Driven, previous: u64, limit: Duration) -> Result<(String, u64), String> {
    let end = Instant::now() + limit;
    loop {
        let (map, loads) = server_map(c)?;
        if loads > previous {
            return Ok((map, loads));
        }
        if Instant::now() >= end {
            return Err(format!(
                "the server's level-load count stayed {previous} in {map}"
            ));
        }
        c.pause(Duration::from_millis(250))?;
    }
}

/// Observes the stock end-of-game scoreboard and its server-supplied rows, not merely the intermission flag.
fn end_scoreboard(c: &mut Driven, limit: Duration) -> Result<(), String> {
    c.until(
        json!({"hud.intermission": true, "menu.open": {"contains": "scoreboard"},
            "scoreboard.rows.0": {"exists": true}}),
        limit,
        "the end-of-match scoreboard did not open with player rows",
    )?;
    Ok(())
}

/// Runs one menu-only client process. The arbitrary stat is a dev fixture written with the menus' own typed
/// `statset`; `statgetindvar` then reads the live profile back into a cvar, whose value is asserted.
fn profile_run(
    client: &Path,
    install: &Path,
    dir: &Path,
    label: &str,
    write: bool,
) -> Result<(), String> {
    let duration = PROFILE_LIMIT.as_secs().to_string();
    let mut c = Driven::start(client, install, dir, &["--duration", &duration])?;
    let outcome = (|| -> Result<(), String> {
        if write {
            c.step("profile dev fixture", |c| {
                c.console("statset 2301 77")?;
                c.pause(Duration::from_secs(1))
            })?;
        }
        c.step("profile stat readback", |c| {
            c.console("statgetindvar 2301 harness_profile_stat")?;
            let reply = c.ask(json!({"cmd": "cvar", "name": "harness_profile_stat"}))?;
            if reply["value"].as_str() != Some("77") {
                return Err(format!(
                    "profile stat 2301 read back as {}, not 77",
                    reply["value"]
                ));
            }
            Ok(())
        })?;
        c.report(&format!("{label}.json"), &["console", "cvars", "menu"])?;
        Ok(())
    })();
    let status = c.finish(Duration::from_secs(30));
    outcome?;
    if !status.is_some_and(|s| s.success()) {
        return Err(format!(
            "the profile client did not exit successfully: {status:?}"
        ));
    }
    Ok(())
}

/// Plays the match and returns the two map names observed at the server.
fn play(c: &mut Driven) -> Result<(String, String), String> {
    c.step("set match limits", |c| {
        for (name, value) in [
            ("scr_war_scorelimit", "3"),
            ("scr_war_timelimit", "4"),
            ("ui_netGametypeName", "war"),
        ] {
            set_cvar(c, name, value)?;
        }
        Ok(())
    })?;
    c.step("Start New Server", start_server)?;
    c.step("join first map", join)?;
    let (from, loads) = server_map(c)?;
    if !from.starts_with("mp_") {
        return Err(format!("the first server map is not a map name: {from:?}"));
    }
    c.pause(Duration::from_secs(2))?;
    c.capture("match")?;
    c.step("end-of-match scoreboard", |c| {
        end_scoreboard(c, Duration::from_secs(240))?;
        c.pause(Duration::from_secs(1))?;
        c.capture("scoreboard").map(|_| ())
    })?;
    let to = c.step("map rotation", |c| {
        let (to, _) = next_level(c, loads, Duration::from_secs(120))?;
        if !to.starts_with("mp_") || to == from {
            return Err(format!(
                "the server did not rotate to another map: {from} -> {to}"
            ));
        }
        Ok(to)
    })?;
    c.step("join rotated map", join)?;
    let (server_map, _) = server_map(c)?;
    let client_map = c.lookup("client.map")?;
    if server_map != to || client_map.as_str() != Some(to.as_str()) {
        return Err(format!(
            "the client did not follow the rotation: expected {to}, server {server_map}, client {client_map}"
        ));
    }
    c.pause(Duration::from_secs(3))?;
    c.capture("rotated")?;
    Ok((from, to))
}

/// Why the run failed, or `None`: every Driven step passed, rendering stayed healthy, the player spawned, the
/// scoreboard and rotation steps both ran, and the client finished in the different map the server loaded.
fn menu_match_verdict(report: &Value, from: &str, to: &str) -> Option<String> {
    // The original cumulative HUD and network reports, now read through the control channel.
    let legacy = json!({
        "steps": report["steps"],
        "net": report["report"],
        "hud": report["hud"]["report"],
        "render": report["render"],
        "gpu_health": report["client"]["gpu_health"],
    });
    if let Some(w) = verdict(&legacy) {
        return Some(w);
    }
    for prefix in [
        "end-of-match scoreboard",
        "map rotation",
        "join rotated map",
    ] {
        let count = report["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| {
                s["step"]
                    .as_str()
                    .is_some_and(|step| step.starts_with(prefix))
            })
            .count();
        if count != 1 {
            return Some(format!(
                "the run has {count} `{prefix}` steps, not 1: the whole match was not exercised"
            ));
        }
    }
    if !from.starts_with("mp_") || !to.starts_with("mp_") {
        return Some(format!(
            "the rotation does not name two maps: {from} -> {to}"
        ));
    }
    if from == to || report["client"]["map"].as_str() != Some(to) {
        return Some(format!(
            "the map did not rotate ({from} -> {to}, ended in {})",
            report["client"]["map"]
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
    // Two processes, one isolated profile folder: the second start must read what the first wrote.
    let profile = ctx.dir.join("profile");
    for (label, write) in [("profile-write", true), ("profile-read", false)] {
        if let Err(e) = profile_run(&client, install, &profile, label, write) {
            out.status = Status::Failed;
            out.reason = Some(format!("{label}: {e}"));
            return Ok(out);
        }
        out.files.push(format!("profile/{label}.json"));
    }

    let duration = LIMIT.as_secs().to_string();
    let mut c = match Driven::start(
        &client,
        install,
        &ctx.dir,
        &["--bots", "4", "--duration", &duration],
    ) {
        Ok(c) => c,
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
            return Ok(out);
        }
    };
    out.notes.push(format!("channel {}", c.addr));
    let played = play(&mut c);
    let report = c.report(
        "control-run.json",
        &[
            "client",
            "menu",
            "hud",
            "scoreboard",
            "net",
            "report",
            "render",
        ],
    );
    let status = c.finish(Duration::from_secs(30));
    out.files.extend(
        [
            "control-run.json",
            "match.png",
            "scoreboard.png",
            "rotated.png",
        ]
        .map(str::to_owned),
    );
    let why = match (played, report) {
        (Err(e), _) => Some(e),
        (Ok(_), Err(e)) => Some(format!("the run's report could not be read: {e}")),
        (Ok((from, to)), Ok(report)) => menu_match_verdict(&report, &from, &to).or_else(|| {
            out.notes.push(format!(
                "played to the score limit, saw the scoreboard, rotated from {from} into {to} and spawned again"
            ));
            None
        }),
    };
    if let Some(why) = why {
        out.status = Status::Failed;
        out.reason = Some(why);
    } else if !status.is_some_and(|s| s.success()) {
        out.status = Status::Failed;
        out.reason = Some(format!("the client did not exit successfully: {status:?}"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(map: &str) -> Value {
        json!({
            "steps": [
                {"step": "join first map", "ok": true, "note": ""},
                {"step": "end-of-match scoreboard", "ok": true, "note": ""},
                {"step": "map rotation", "ok": true, "note": ""},
                {"step": "join rotated map", "ok": true, "note": ""}
            ],
            "net": {"spawned": true},
            "report": {"spawned": true, "players_seen_max": 5, "players_drawn_max": 5,
                "players_seen": [1, 2, 3, 4, 5], "players_drawn": [1, 2, 3, 4, 5],
                "remote_weapon_frames": 90, "torso_clips": ["pt_reload_stand_rifle"]},
            "hud": {"report": {"live": true, "health": 100, "max_health": 100,
                "weapon": {"clip": 30}, "map": "compass_map_x", "sprint": [4000, 4000],
                "sprint_facts": {"weapon": "m16_gl_mp", "scale": 1.0, "player_sprint_time": 4.0,
                    "perk_sprint_multiplier": 2.0, "perks": 0},
                "drawn": {"112": 1, "117": 1, "159": 1, "150": 1}}},
            "client": {"map": map, "gpu_health": {"errors": []}},
            "render": {"material_failures": {}, "image_failures": []},
        })
    }

    #[test]
    fn rotation_must_change_the_map_and_end_in_it() {
        assert_eq!(menu_match_verdict(&report("mp_b"), "mp_a", "mp_b"), None);
        assert!(menu_match_verdict(&report("mp_a"), "mp_a", "mp_a").is_some());
        assert!(menu_match_verdict(&report("mp_c"), "mp_a", "mp_b").is_some());
        assert!(
            menu_match_verdict(&report("mp_b"), "2", "mp_b")
                .unwrap()
                .contains("two maps")
        );
    }

    #[test]
    fn a_run_without_the_rotation_step_fails() {
        let mut r = report("mp_b");
        r["steps"].as_array_mut().unwrap().remove(2);
        assert!(
            menu_match_verdict(&r, "mp_a", "mp_b").is_some(),
            "no rotation step must not pass"
        );
    }
}
