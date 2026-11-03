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
use super::client_session::{gpu_problem, render_problem, steps_problem};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "client-sd-rounds";
/// The whole run, a hang included: three rounds of up to a time limit, a bomb timer and the end delays each, plus the
/// load.
const LIMIT: Duration = Duration::from_secs(600);

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

fn start_server(c: &mut Driven) -> Result<(), String> {
    c.until(
        json!({"menu.open": {"contains": "main"}}),
        Duration::from_secs(30),
        "the main menu did not open",
    )?;
    c.click("Start New Server")?;
    c.until(
        json!({"menu.open": {"contains": "createserver"}}),
        Duration::from_secs(20),
        "Start New Server did not open createserver",
    )?;
    c.click("Start")?;
    Ok(())
}

fn join(c: &mut Driven) -> Result<(), String> {
    c.until(
        json!({"menu.open": {"contains": "team_marinesopfor"}}),
        Duration::from_secs(90),
        "the team menu did not open",
    )?;
    c.click("auto_assign")?;
    c.until(
        json!({"menu.open": {"contains": "changeclass"}}),
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

fn own_team(c: &mut Driven) -> Result<u64, String> {
    let team = c.lookup("hud.live.own_team")?;
    match team.as_u64() {
        Some(team @ (1 | 2)) => Ok(team),
        _ => Err(format!("the player is on no playing team: {team}")),
    }
}

fn same_team(c: &mut Driven, team: u64) -> Result<(), String> {
    c.until(
        json!({"hud.live.own_team": team}),
        Duration::from_secs(5),
        "the player's team did not survive the round",
    )?;
    Ok(())
}

fn objective(c: &mut Driven) -> Result<String, String> {
    let reply = c.ask(json!({"cmd": "cvar", "name": "cg_objectiveText"}))?;
    let text = reply["value"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("cg_objectiveText is empty: {reply}"))?;
    Ok(text.to_owned())
}

/// Waits for the server-set objective cvar to be the first round's role again, or the other role.
fn objective_until(c: &mut Driven, first: &str, same: bool) -> Result<String, String> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let now = objective(c)?;
        if (now == first) == same {
            return Ok(now);
        }
        if Instant::now() >= end {
            return Err(format!(
                "cg_objectiveText is {now:?}; expected {} the first round's {first:?}",
                if same {
                    "the same as"
                } else {
                    "different from"
                }
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

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

fn next_level(c: &mut Driven, previous: u64) -> Result<(String, u64), String> {
    let end = Instant::now() + Duration::from_secs(150);
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

/// The levels the client has begun (`net.levels`: map loads and `map_restart` game states).
fn client_levels(c: &mut Driven) -> Result<u64, String> {
    c.lookup("net.levels")?
        .as_u64()
        .ok_or_else(|| "the client reports no net.levels".into())
}

/// Waits until the client itself has begun a level after `before`: the server's count moving says the restart was
/// sent, this says the client took it, so the spawn and menu checks that follow are of the new round, not the old.
fn client_level_after(c: &mut Driven, before: u64, round: &str) -> Result<u64, String> {
    c.until(
        json!({"net.levels": {"gt": before}}),
        Duration::from_secs(60),
        &format!("the client never began round {round} (net.levels stayed {before})"),
    )?;
    client_levels(c)
}

/// Waits for the final intermission of round three's level with the `scoreboard` menu open, player rows on it and the
/// player still on `team`, all in one reading. The end-of-game popups the stock scripts open hold the `scoreboard` menu
/// back for most of the intermission, and the level rotates a few seconds after they close: a team read after a
/// separate scoreboard wait could see the next map's spectator instead (the #542 serial run did). The matched state is
/// written to `end-scoreboard.json`.
fn end_scoreboard(c: &mut Driven, team: u64, levels: u64) -> Result<(), String> {
    let state = json!({"hud.intermission": true, "net.levels": levels,
        "menu.open": {"contains": "scoreboard"}, "scoreboard.rows.0": {"exists": true},
        "hud.live.own_team": team});
    let reply = c.until(
        state.clone(),
        Duration::from_secs(150),
        "the final intermission of round three's level did not show the scoreboard with player rows and the player's team",
    )?;
    let evidence = json!({"matched": state, "reply": reply});
    std::fs::write(
        c.dir.join("end-scoreboard.json"),
        serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// Plays all three rounds and returns the persistent team and the three objective roles observed.
fn play(c: &mut Driven) -> Result<(u64, String, String, String), String> {
    c.step("set round limits", |c| {
        for (name, value) in [
            ("scr_sd_roundlimit", "3"),
            ("scr_sd_timelimit", "0.5"),
            ("scr_sd_roundswitch", "1"),
            ("scr_sd_scorelimit", "0"),
            ("scr_sd_winlimit", "0"),
            ("ui_netGametypeName", "sd"),
        ] {
            set_cvar(c, name, value)?;
        }
        Ok(())
    })?;
    c.step("Start New Server", start_server)?;
    c.step("join first round", join)?;
    let team = c.step("team=save", own_team)?;
    let first = c.step("cvar=save cg_objectiveText", objective)?;
    let (map, mut loads) = server_map(c)?;
    let mut levels = client_levels(c)?;
    c.pause(Duration::from_secs(2))?;
    c.capture("round1")?;

    loads = c.step("round=next:150", |c| {
        let (new_map, new_loads) = next_level(c, loads)?;
        if new_map != map {
            return Err(format!("round two changed the map: {map} -> {new_map}"));
        }
        Ok(new_loads)
    })?;
    levels = c.step("ingame=120", |c| {
        let now = client_level_after(c, levels, "two")?;
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            Duration::from_secs(120),
            "the player did not spawn in round two",
        )?;
        Ok(now)
    })?;
    c.step("nomenu=5", |c| {
        c.until(
            json!({"menu.open": {"len": 0}}),
            Duration::from_secs(5),
            "a menu blocked the automatic round-two spawn",
        )?;
        Ok(())
    })?;
    c.step("team=same:5", |c| same_team(c, team))?;
    let second = c.step("cvar=other cg_objectiveText:5", |c| {
        objective_until(c, &first, false)
    })?;
    c.pause(Duration::from_secs(2))?;
    c.capture("round2")?;

    c.step("round=next:150", |c| {
        let (new_map, _) = next_level(c, loads)?;
        if new_map != map {
            return Err(format!("round three changed the map: {map} -> {new_map}"));
        }
        Ok(())
    })?;
    let levels = c.step("ingame=120", |c| {
        let now = client_level_after(c, levels, "three")?;
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            Duration::from_secs(120),
            "the player did not spawn in round three",
        )?;
        Ok(now)
    })?;
    c.step("nomenu=5", |c| {
        c.until(
            json!({"menu.open": {"len": 0}}),
            Duration::from_secs(5),
            "a menu blocked the automatic round-three spawn",
        )?;
        Ok(())
    })?;
    c.step("team=same:5", |c| same_team(c, team))?;
    let third = c.step("cvar=same cg_objectiveText:5", |c| {
        objective_until(c, &first, true)
    })?;
    c.pause(Duration::from_secs(2))?;
    c.capture("round3")?;

    c.step("menu=scoreboard:150", |c| end_scoreboard(c, team, levels))?;
    c.capture("end")?;
    Ok((team, first, second, third))
}

/// Why the run failed, or `None`: rendering stayed healthy, every Driven step passed, the player spawned, and the
/// steps that make the check were all there (two restarts, the role swapped once and back once, the team checked after
/// each restart and at the final intermission, which `menu=scoreboard` reads with it). The run ends at the final
/// scoreboard, where no weapon is held, so the HUD is not judged here.
fn rounds_verdict(report: &Value) -> Option<String> {
    if let Some(w) = gpu_problem(&report["client"]["gpu_health"]) {
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
        ("team=same", 2),
        ("menu=scoreboard", 1),
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
        &["client", "menu", "hud", "scoreboard", "net", "render"],
    );
    let status = c.finish(Duration::from_secs(30));
    out.files.extend(
        [
            "control-run.json",
            "end-scoreboard.json",
            "round1.png",
            "round2.png",
            "round3.png",
            "end.png",
        ]
        .map(str::to_owned),
    );
    let why = match (played, report) {
        (Err(e), _) => Some(e),
        (Ok(_), Err(e)) => Some(format!("the run's report could not be read: {e}")),
        (Ok((team, first, second, third)), Ok(report)) => rounds_verdict(&report).or_else(|| {
            out.notes.push(format!(
                "three rounds on team {team}: objective role {first} -> {second} -> {third}"
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

    fn report(steps: &[(&str, bool, &str)]) -> Value {
        let steps: Vec<Value> = steps
            .iter()
            .map(|(s, ok, note)| json!({"step": s, "ok": ok, "note": note}))
            .collect();
        json!({
            "steps": steps,
            "render": {"material_failures": {}, "image_failures": []},
            "client": {"gpu_health": {"errors": []}},
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
            ("menu=scoreboard:150", true, "team 2"),
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
