// SPDX-License-Identifier: GPL-3.0-only
//! `client-missile`: the real client sees a thrown grenade, driven through the control channel (#376). The player
//! joins through the server's team and class menus with the pointer and throws a frag with the key bound to `+frag`:
//! held until the grenade is primed, then let go. The HUD's grenade count must drop, the client must draw the
//! projectile's model while it flies and rests, receive its explosion event and play an explosion sound cue. Needs a
//! display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_hardpoint::{join, report_above, until_stat};
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::Duration;

const NAME: &str = "client-missile";
/// `--no-autojoin`: the server's team and class menus are the stage's to answer, with the pointer.
const ARGS: &[&str] = &[
    "--listen",
    "--bots",
    "1",
    "--no-autojoin",
    "--duration",
    "300",
];
/// Everyone in the match: the one bot of `ARGS` and the player.
const PLAYERS: u64 = 2;
/// `OFFHAND_HOLD` and on (`sim::pm`'s weapon states): the grenade is past `OFFHAND_PREPARE`, where letting go of a
/// hold-to-throw grenade puts it away, so letting go now throws it.
const PRIMED: u64 = 17;

fn count(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// The match, step by step; `Ok` carries what the finished run fell short of.
fn drive(c: &mut Driven, out: &mut StageReport) -> Result<Vec<String>, String> {
    let secs = Duration::from_secs;
    join(c, "auto_assign", "Assault")?;
    // The prematch countdown freezes everyone's controls (stock `freezeControls`), which strips the grenade buttons
    // from the player's commands while the weapons stay enabled: a `+frag` held then does nothing until the countdown
    // ends. The throw waits until a frag is carried and the server has every player, the player too, in the game with
    // the controls free (its `playing` statistic, as of now: the player's death or a new freeze takes it back down).
    c.step("a frag in hand", |c| {
        c.until(
            json!({"player.weapons_enabled": true, "hud.report.frag": {"gt": 0}}),
            secs(45),
            "the player never carried a frag with the weapons free",
        )?;
        until_stat(c, "playing", PLAYERS, secs(60)).map(|_| ())
    })?;
    let frags = c.lookup("hud.report.frag")?.as_u64().unwrap_or(0);
    let blasts = count(&c.lookup("events.world.explosion")?);
    let cues = count(&c.lookup("sound.explosion_cues")?);
    c.step("throw the frag with +frag", |c| {
        c.bind("+frag", "down")?;
        // Held until the grenade is primed, not a fixed pulse that may land while the weapon is still raising.
        let primed = c.until(
            json!({"player.weapon_state": {"ge": PRIMED}}),
            secs(5),
            "the frag bind never primed a grenade",
        );
        c.bind("+frag", "up")?;
        primed?;
        c.until(
            json!({"hud.report.frag": {"lt": frags}}),
            secs(10),
            "the grenade count did not drop after the throw",
        )
        .map(|_| ())
    })?;
    // Waits that run out still leave counts to judge; the checks below name what fell short. The grenade is drawn
    // while it flies and rests, before it blows.
    let drawn = report_above(c, "projectiles_max_drawn", 0, secs(5))?;
    c.wait(&json!({"events.world.explosion": {"gt": blasts}}), secs(15))?;
    // The cue is mixed on the frame the event arrives; a moment later it has been counted.
    c.pause(secs(1))?;
    let blasts_now = count(&c.lookup("events.world.explosion")?);
    let sound = c.state("sound")?;
    let cues_now = count(&sound["explosion_cues"]);
    out.metrics.insert("projectiles_drawn".into(), drawn as f64);
    out.metrics
        .insert("explosion_events".into(), blasts_now as f64);
    out.metrics.insert("explosion_cues".into(), cues_now as f64);
    let mut problems = Vec::new();
    if drawn == 0 {
        problems.push("the client drew no projectile model for the thrown grenade".into());
    }
    if blasts_now <= blasts {
        problems.push("the client never received the grenade's explosion".into());
    }
    // The mix is what is checked here (`Driven` runs the client with `--no-sound`, so no sound card is needed), and a
    // sound bank that never loaded leaves the explosion silent without failing anything.
    if sound["ready"] != json!(true) {
        problems.push(format!(
            "the sound tables never loaded, so the explosion's cue was never checked: {}",
            sound["error"]
        ));
    } else if cues_now <= cues {
        problems.push("the explosion made no sound cue".into());
    }
    Ok(problems)
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
    let dir = ctx.dir.join("missile");
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => return Ok(out.with_status(Status::Failed).with_reason(e)),
    };
    let result = drive(&mut c, &mut out);
    // The steps and what the player, the HUD, the events, the sound and the run's counters held, kept whether or not
    // the run got through.
    let kept = c.report(
        "state.json",
        &["player", "hud", "events", "sound", "report", "console"],
    );
    let status = c.finish(Duration::from_secs(30));
    out.files.push("missile/state.json".into());
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
