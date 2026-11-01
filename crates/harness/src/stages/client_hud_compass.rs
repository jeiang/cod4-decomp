// SPDX-License-Identifier: GPL-3.0-only
//! Snapshot firing-event compass parity (#406). Bots stand still and use real weapon usercmds.
//! An unsilenced shot must hold its rendered mark for the independently observed audio playback length,
//! then fade for compassSoundPingFadeTime; a silenced shot must not ping. The frag case (an offhand throw raises
//! no fire event, so no ping) returns once the listen-server bot throws (#486); until then the client unit test in
//! `netplay/hud.rs` covers the `WEAPTYPE_GRENADE` rule.
//! Reference: KisakCOD MP 1.8 (GPL-3.0), cgame/cg_weapons.cpp:2552-2558,
//! cgame_mp/cg_compassfriendlies_mp.cpp:246,589-600. 1.7 unverified.
use crate::control::Driven;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The unsilenced and silenced weapons of one family, so only the silencer differs.
const UNSILENCED: &str = "mp5_mp";
const SILENCED: &str = "mp5_silencer_mp";
/// How long the bot holds the trigger: a few rounds, as the server test of the hook (`dev_bot_fire.rs`) does.
const BURST_MS: u32 = 500;
/// Long enough for the sound hold and fade to end.
const SETTLE: Duration = Duration::from_secs(5);
/// Attempts at one case: the match can still kill the idle bot (#494 saw it respawn mid-case).
const ATTEMPTS: u32 = 3;

/// The HUD's cumulative compass counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    fire_pings: u64,
    flag_pings: u64,
    marks: u64,
}

/// One case's verdict from the counters before and after it, given the shots the server counted through
/// `FireWeapon`.
fn verdict(
    case: &str,
    pings: bool,
    before: Counts,
    after: Counts,
    shots: u64,
) -> Result<(), String> {
    if shots == 0 {
        return Err(format!("{case}: the bot never fired"));
    }
    if pings {
        if after.fire_pings <= before.fire_pings || after.marks <= before.marks {
            return Err(format!(
                "{case}: want a compass ping, counters {before:?} -> {after:?}, {shots} shots"
            ));
        }
    } else if after != before {
        return Err(format!(
            "{case}: want no compass ping, counters {before:?} -> {after:?}, {shots} shots"
        ));
    }
    Ok(())
}

fn exec(c: &mut Driven, line: &str) -> Result<(), String> {
    c.ask(json!({"cmd": "server", "op": "exec", "line": line}))?;
    Ok(())
}

fn counts(c: &mut Driven) -> Result<Counts, String> {
    let hud = c.state("hud")?;
    let r = &hud["report"];
    let n = |k: &str| {
        r[k].as_u64()
            .ok_or_else(|| format!("hud.report.{k} is missing"))
    };
    Ok(Counts {
        fire_pings: n("enemy_fire_pings")?,
        flag_pings: n("enemy_flag_pings")?,
        marks: n("enemy_firing_marks")?,
    })
}

/// The server's record of client `slot`.
fn server_client(c: &mut Driven, slot: u64) -> Result<Value, String> {
    let r = c.ask(json!({"cmd": "server", "op": "state", "what": "clients"}))?;
    r["clients"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["slot"].as_u64() == Some(slot)).cloned())
        .ok_or_else(|| format!("the server has no client {slot}"))
}

/// What a failure needs of a server client record: its weapons, life and position.
fn body(p: &Value) -> String {
    format!(
        "weapon {} weapons {} alive {} spawn_count {} deaths {} shots {} origin {}",
        p["weapon_name"],
        p["weapons"],
        p["alive"],
        p["spawn_count"],
        p["deaths"],
        p["shots"],
        p["origin"]
    )
}

/// The slot of a live bot on team `team` (1 axis, 2 allies, as both the client and the server number them).
fn enemy_bot(c: &mut Driven, team: u64) -> Result<u64, String> {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let r = c.ask(json!({"cmd": "server", "op": "state", "what": "clients"}))?;
        let found = r["clients"].as_array().and_then(|a| {
            a.iter()
                .find(|p| {
                    p["bot"] == json!(true)
                        && p["team"].as_u64() == Some(team)
                        && p["alive"] == json!(true)
                })
                .and_then(|p| p["slot"].as_u64())
        });
        if let Some(slot) = found {
            return Ok(slot);
        }
        if Instant::now() > end {
            return Err(format!("no live bot on team {team}"));
        }
        c.pause(Duration::from_millis(500))?;
    }
}

/// Puts the bot at `spot` (`devtele` arguments) and has it give and raise `weapon`, so the burst after it is the
/// weapon's fire and not its raise. Only a bot seen dead or respawned (a fresh inventory, somewhere else) is put back
/// and given it again; a living bot that does not raise it within the hook's own wait fails. Returns the spawn count
/// the bot holds it in.
fn raise(c: &mut Driven, bot: u64, spot: &str, label: &str, weapon: &str) -> Result<Value, String> {
    let end = Instant::now() + Duration::from_secs(30);
    let (mut placed, mut issued) = (Value::Null, Instant::now());
    loop {
        let p = server_client(c, bot)?;
        if p["alive"] == json!(true) && p["spawn_count"] != placed {
            exec(c, &format!("devtele {bot} {spot}"))?;
            exec(c, &format!("devbotfire {bot} {weapon} 0"))?;
            (placed, issued) = (p["spawn_count"].clone(), Instant::now());
        }
        c.pause(Duration::from_millis(500))?;
        let held = server_client(c, bot)?;
        let same_life = held["alive"] == json!(true) && held["spawn_count"] == placed;
        if same_life && held["weapon_name"] == weapon {
            return Ok(placed);
        }
        // The hook asks for the weapon for 5 s (`DEV_RAISE_WAIT_MS` in `server/src/bot.rs`).
        if (same_life && issued.elapsed() > Duration::from_secs(6)) || Instant::now() > end {
            return Err(format!(
                "{label}: bot {bot} never raised {weapon}: {}",
                body(&held)
            ));
        }
    }
}

/// One case: the bot, at `spot`, shoots `weapon`; the compass must ping or not. An attempt in which the bot
/// respawned (something in the match killed it; the person's pings and its weapon are then another life's) proves
/// nothing and is made again, up to [`ATTEMPTS`] times.
fn case(
    c: &mut Driven,
    bot: u64,
    spot: &str,
    label: &str,
    weapon: &str,
    pings: bool,
) -> Result<String, String> {
    let mut lost = Vec::new();
    for attempt in 1..=ATTEMPTS {
        let placed = raise(c, bot, spot, label, weapon)?;
        c.pause(SETTLE)?;
        let before = counts(c)?;
        let was = server_client(c, bot)?;
        // Killed while settling: the burst would be another life's, without the weapon.
        if was["spawn_count"] != placed
            || was["weapon_name"] != weapon
            || was["alive"] != json!(true)
        {
            lost.push(format!(
                "attempt {attempt} lost the weapon while settling: {}",
                body(&was)
            ));
            continue;
        }
        exec(c, &format!("devbotfire {bot} {weapon} {BURST_MS}"))?;
        c.pause(Duration::from_millis(u64::from(BURST_MS) + 1500))?;
        c.pause(Duration::from_secs(5))?;
        let after = counts(c)?;
        let now = server_client(c, bot)?;
        if now["spawn_count"] != was["spawn_count"] || now["alive"] != json!(true) {
            lost.push(format!(
                "attempt {attempt} before: {}; after: {}",
                body(&was),
                body(&now)
            ));
            continue;
        }
        return finish(c, bot, label, pings, (before, after), (&was, &now))
            .map(|s| retried(s, &lost));
    }
    Err(format!(
        "{label}: bot {bot} died during every attempt: {}",
        lost.join("; ")
    ))
}

/// A passing case's note, naming the attempts the bot died in.
fn retried(note: String, lost: &[String]) -> String {
    if lost.is_empty() {
        note
    } else {
        format!(
            "{note} (bot died in {} earlier attempts: {})",
            lost.len(),
            lost.join("; ")
        )
    }
}

/// The verdict on one attempt in which the bot lived throughout.
fn finish(
    c: &mut Driven,
    bot: u64,
    label: &str,
    pings: bool,
    (before, after): (Counts, Counts),
    (was, now): (&Value, &Value),
) -> Result<String, String> {
    let shots = now["shots"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(was["shots"].as_u64().unwrap_or(0));
    verdict(label, pings, before, after, shots)
        .map_err(|e| format!("{e}; bot {bot} before: {}; after: {}", body(was), body(now)))?;
    if pings {
        let played = c
            .lookup("hud.report.last_fire_played_ms")?
            .as_i64()
            .ok_or("missing independent audio length")?;
        let hold = c
            .lookup("hud.report.last_fire_ping_ms")?
            .as_i64()
            .ok_or("missing firing hold")?;
        let mark = c
            .lookup("hud.report.last_fire_mark_ms")?
            .as_i64()
            .ok_or("missing rendered mark lifetime")?;
        duration_verdict(played, hold, mark, 2000)?;
    }
    let length = c.lookup("hud.report.last_fire_ping_ms")?;
    let played = c.lookup("hud.report.last_fire_played_ms")?;
    let mark = c.lookup("hud.report.last_fire_mark_ms")?;
    Ok(format!(
        "{label}: {} fire pings, {} flag pings, {} marks drawn, hold {length} ms, audio {played} ms, mark lifetime {mark} ms",
        after.fire_pings - before.fire_pings,
        after.flag_pings - before.flag_pings,
        after.marks - before.marks,
    ))
}

/// At 60 Hz allow several frames of snapshot/draw scheduling, but never the 351 ms fixed-hold error.
fn duration_verdict(played: i64, hold: i64, mark: i64, fade: i64) -> Result<(), String> {
    if hold != played || (mark - (played + fade)).abs() > 200 {
        return Err(format!(
            "fire timing: played {played} ms, held {hold} ms, mark {mark} ms; want {} ms",
            played + fade
        ));
    }
    Ok(())
}

/// Unsilenced shots ping the compass for the fire sound's length; silenced shots do not ping at all.
pub(super) fn check(c: &mut Driven) -> Result<String, String> {
    let team = c.lookup("player.team")?.as_u64().unwrap_or(0);
    let enemy = match team {
        1 => 2,
        2 => 1,
        _ => return Err(format!("the person is on no team ({team})")),
    };
    exec(c, "set bot_idle 1")?;
    exec(c, "set developer 1")?;
    c.ask(json!({"cmd":"cvar", "name":"compassSoundPingFadeTime", "value":"2"}))?;
    let me = c
        .lookup("player.client")?
        .as_u64()
        .ok_or("no client slot")?;
    let person = server_client(c, me)?;
    let mut bot_home = None;
    let result: Result<Vec<String>, String> = (|| {
        let bot = enemy_bot(c, enemy)?;
        bot_home = Some((bot, server_client(c, bot)?["origin"].clone()));
        let origin = c.lookup("player.origin")?;
        let at = |i: usize| origin[i].as_f64().unwrap_or(0.0);
        // 96 units along x from the person; `case` puts it back there after a respawn.
        let spot = format!("{} {} {}", at(0) + 96.0, at(1), at(2));
        // `case` compares the independently played sound with the hold and final drawn mark lifetime.
        Ok(vec![
            case(c, bot, &spot, "unsilenced", UNSILENCED, true)?,
            case(c, bot, &spot, "silenced", SILENCED, false)?,
        ])
    })();
    // Put the bot back where it stood before letting it think again: released 96 units from the person, an enemy
    // with a raised SMG can kill them at the start of the next step.
    if let Some((bot, home)) = bot_home
        && let Some(o) = home.as_array().filter(|o| o.len() == 3)
    {
        exec(c, &format!("devtele {bot} {} {} {}", o[0], o[1], o[2]))?;
    }
    exec(c, "set bot_idle 0")?;
    exec(c, "set developer 0")?;
    let after = server_client(c, me)?;
    let mut notes = result?;
    if after["deaths"] != person["deaths"] || after["alive"] != json!(true) {
        return Err(format!(
            "the person died during the compass check: before {person}, after {after}"
        ));
    }
    notes.push(format!(
        "person health {} -> {}",
        person["health"], after["health"]
    ));
    Ok(format!("compass: {}", notes.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(fire_pings: u64, flag_pings: u64, marks: u64) -> Counts {
        Counts {
            fire_pings,
            flag_pings,
            marks,
        }
    }

    #[test]
    fn an_unsilenced_shot_must_ping_and_draw_a_mark() {
        assert!(verdict("u", true, c(0, 0, 0), c(1, 1, 30), 2).is_ok());
        assert!(verdict("u", true, c(0, 0, 0), c(1, 0, 0), 2).is_err());
        assert!(verdict("u", true, c(0, 0, 0), c(0, 1, 30), 2).is_err());
    }

    #[test]
    fn a_silenced_shot_must_not_ping_at_all() {
        assert!(verdict("s", false, c(1, 1, 9), c(1, 1, 9), 2).is_ok());
        assert!(verdict("s", false, c(1, 1, 9), c(1, 2, 9), 2).is_err());
    }

    #[test]
    fn a_shot_that_never_happened_fails() {
        assert!(verdict("s", false, c(0, 0, 0), c(0, 0, 0), 0).is_err());
    }

    #[test]
    fn lifetime_rejects_a_fixed_two_seconds_and_handles_unknown_audio() {
        assert!(duration_verdict(1649, 1649, 3640, 2000).is_ok());
        assert!(duration_verdict(1649, 2000, 4000, 2000).is_err());
        assert!(duration_verdict(1649, 1649, 2000, 2000).is_err());
        assert!(duration_verdict(0, 0, 1990, 2000).is_ok());
        assert!(duration_verdict(0, 2000, 4000, 2000).is_err());
    }
}
