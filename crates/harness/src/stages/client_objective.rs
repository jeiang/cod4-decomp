// SPDX-License-Identifier: GPL-3.0-only
//! `client-objective`: the real client plays Search and Destroy against bots and plants the bomb, driven through the
//! control channel (#376). The player picks the Marines and a class in the server's menus with the pointer. As setup
//! the bots are held still (`bot_idle`) and the player is put in the bomb's pickup trigger (`devtele`), which must hand
//! them the bomb, then in the bomb zone, where the zone's hint must be drawn (owner-draw 72). The player holds the key
//! bound to `+activate`: the hint's shot is saved, the hint must be localized (the stock zone's hint is the bare string
//! key `PLATFORM_HOLD_TO_PLANT_EXPLOSIVES`, which must not reach the screen as a key), and the server must count the
//! plant. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_hardpoint::{drawn, join, until_stat};
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "client-objective";
/// `--no-autojoin`: the server's team and class menus are the stage's to answer, with the pointer.
const ARGS: &[&str] = &[
    "--listen",
    "--gametype",
    "sd",
    "--bots",
    "4",
    "--no-autojoin",
    "--duration",
    "240",
];
/// Above the map: where the stock `_gameobjects` carry object moves its pickup trigger while the bomb is carried.
const LIFTED_Z: f64 = 5000.0;

/// The `trigger_multiple` entities of the server that are lifted above the map ([`LIFTED_Z`]).
fn lifted_triggers(c: &mut Driven) -> Result<usize, String> {
    // The server's entity list is an array, which the channel's reply carries as its `value`.
    let r = c.ask(json!({"cmd": "server", "op": "state", "what": "entities"}))?;
    let list = r["value"]
        .as_array()
        .ok_or("the listen server answered with no entity list")?;
    Ok(list
        .iter()
        .filter(|e| {
            e["classname"] == "trigger_multiple"
                && e["origin"][2].as_f64().is_some_and(|z| z > LIFTED_Z)
        })
        .count())
}

/// Waits up to `limit` for the server's variable `name` to read `want`.
fn server_cvar_until(
    c: &mut Driven,
    name: &str,
    want: &str,
    limit: Duration,
) -> Result<(), String> {
    let end = Instant::now() + limit;
    loop {
        let v = c.ask(json!({"cmd": "server", "op": "cvar", "name": name}))?["value"].clone();
        if v == json!(want) {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("the server's {name} stayed {v} (want {want:?})"));
        }
        c.pause(Duration::from_millis(250))?;
    }
}

/// Holding `+activate` in the zone: the hint's shot, then the server's plant count.
fn plant(c: &mut Driven) -> Result<u64, String> {
    c.pause(Duration::from_secs(2))?;
    c.capture("hint")?;
    until_stat(c, "plants", 1, Duration::from_secs(15))
}

/// The match, step by step; `Ok` carries what the finished run fell short of.
fn drive(c: &mut Driven, out: &mut StageReport) -> Result<Vec<String>, String> {
    let secs = Duration::from_secs;
    // mp_crash's stock SD setup makes the Marines the attackers. Pick that team through its menu instead of letting
    // auto-assign sometimes put the player on defense, where no pickup trigger exists and a plant cannot be exercised.
    join(c, "Marines", "Assault")?;
    // Setup: the bots stand still, so a firefight does not end the round before the plant; no player can make them.
    // Typed at the host's console, the variable is the listen server's, which must then hold it.
    c.step("bots idle (setup)", |c| {
        c.console("set bot_idle 1")?;
        server_cvar_until(c, "bot_idle", "1", secs(5))
    })?;
    // Setup: the player is put in the bomb's pickup trigger (`devtele`) instead of walking there. Touching it is the
    // game's own pickup, which must take the bomb: the carry object lifts its trigger above the map.
    c.step("take the bomb (setup: devtele to its pickup)", |c| {
        let before = lifted_triggers(c)?;
        c.console("devtele human sd_bomb_pickup")?;
        let end = Instant::now() + secs(10);
        while lifted_triggers(c)? <= before {
            if Instant::now() > end {
                return Err("the player in the pickup trigger never took the bomb".into());
            }
            c.pause(Duration::from_millis(250))?;
        }
        Ok(())
    })?;
    // Setup: then put in the bomb zone (`devtele`), where the zone's use hint must draw.
    c.step("in the bomb zone (setup: devtele)", |c| {
        let before = drawn(c, "72")?;
        c.console("devtele human bombzone")?;
        c.until(
            json!({"hud.report.drawn.72": {"gt": before}}),
            secs(10),
            "the zone's hint (owner-draw 72) never drew in the bomb zone",
        )
        .map(|_| ())
    })?;
    let plants = c.step("hold +activate in the zone", |c| {
        c.bind("+activate", "down")?;
        let r = plant(c);
        c.bind("+activate", "up")?;
        r
    })?;
    c.pause(secs(1))?;
    out.metrics.insert("plants".into(), plants as f64);
    out.metrics
        .insert("hint_draws".into(), drawn(c, "72")? as f64);
    // The step above failed the run unless the server counted the plant; what is left is the text on screen.
    Ok(unresolved_problem(&c.state("hud")?).into_iter().collect())
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
    let dir = ctx.dir.join("plant");
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => return Ok(out.with_status(Status::Failed).with_reason(e)),
    };
    let result = drive(&mut c, &mut out);
    // The steps and what the player, the HUD and the console held, kept whether or not the run got through.
    let kept = c.report("state.json", &["player", "hud", "console"]);
    let status = c.finish(Duration::from_secs(30));
    out.files
        .extend(["plant/state.json", "plant/hint.png"].map(String::from));
    let mut problems = match result {
        Ok(problems) => problems,
        Err(e) => vec![e],
    };
    if let Err(e) = kept {
        problems.push(format!("the run's state was not kept: {e}"));
    }
    if !status.is_some_and(|s| s.success()) {
        problems.push(format!("the client did not exit successfully: {status:?}"));
    }
    if !problems.is_empty() {
        return Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")));
    }
    Ok(out)
}

/// Why the HUD showed text as a string key, or `None`, given the state document's `hud`: the hint
/// (`report.unresolved_text`) and the messages (`stats.unresolved`) must have been localized.
fn unresolved_problem(hud: &Value) -> Option<String> {
    let keys: Vec<&str> = [
        &hud["report"]["unresolved_text"],
        &hud["stats"]["unresolved"],
    ]
    .into_iter()
    .filter_map(|v| v.as_array())
    .flatten()
    .filter_map(Value::as_str)
    .collect();
    (!keys.is_empty()).then(|| format!("unlocalized text on screen: {}", keys.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hint_left_as_a_key_is_named() {
        assert_eq!(
            unresolved_problem(
                &json!({"report": {"unresolved_text": []}, "stats": {"unresolved": []}})
            ),
            None
        );
        let why = unresolved_problem(
            &json!({"report": {"unresolved_text": ["PLATFORM_HOLD_TO_PLANT_EXPLOSIVES"]}, "stats": {"unresolved": ["MP_X"]}}),
        )
        .unwrap();
        assert!(why.contains("PLATFORM_HOLD_TO_PLANT_EXPLOSIVES") && why.contains("MP_X"));
    }
}
