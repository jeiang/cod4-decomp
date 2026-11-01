// SPDX-License-Identifier: GPL-3.0-only
//! Snapshot firing-event compass parity (#406). Bots stand still and use real weapon usercmds.
//! An unsilenced shot must hold its rendered mark for the independently observed audio playback length,
//! then fade for compassSoundPingFadeTime; silenced shots and a proven offhand throw must not ping.
//! Reference: KisakCOD MP 1.8 (GPL-3.0), cgame/cg_weapons.cpp:2552-2558,
//! cgame_mp/cg_compassfriendlies_mp.cpp:246,589-600, bgame/bg_weapons.cpp:3061-3081. 1.7 unverified.
use crate::control::Driven;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The unsilenced and silenced weapons of one family, so only the silencer differs.
const UNSILENCED: &str = "mp5_mp";
const SILENCED: &str = "mp5_silencer_mp";
/// How long the bot holds the trigger: a few rounds, as the server test of the hook (`dev_bot_fire.rs`) does.
const BURST_MS: u32 = 500;
/// How long the frag key is held before it is let go and the grenade flies.
const FRAG_MS: u32 = 1000;
/// Long enough for the sound hold and fade to end.
const SETTLE: Duration = Duration::from_secs(5);

/// The HUD's cumulative compass counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    fire_pings: u64,
    flag_pings: u64,
    marks: u64,
}

/// What the server and the client saw of the bot's attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Fired {
    /// Shots the server counted through `FireWeapon`.
    shots: u64,
    /// Proof the offhand action actually happened.
    missile: bool,
}

/// One case's verdict from the counters before and after it.
fn verdict(
    case: &str,
    pings: bool,
    before: Counts,
    after: Counts,
    fired: Fired,
    grenade: bool,
) -> Result<(), String> {
    if grenade && !fired.missile {
        return Err(format!("{case}: no grenade throw observed ({fired:?})"));
    }
    if !grenade && fired.shots == 0 {
        return Err(format!("{case}: the bot never fired ({fired:?})"));
    }
    if pings {
        if after.fire_pings <= before.fire_pings || after.marks <= before.marks {
            return Err(format!(
                "{case}: want a compass ping, counters {before:?} -> {after:?}, server {fired:?}"
            ));
        }
    } else if after != before {
        return Err(format!(
            "{case}: want no compass ping, counters {before:?} -> {after:?}, server {fired:?}"
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

/// Lets `d` pass while sampling the server's entities for a thrown grenade (classname `grenade`). The server's view,
/// not the client's: a bot put beyond a wall throws out of the person's PVS.
fn watch_for_missile(c: &mut Driven, d: Duration) -> Result<bool, String> {
    let end = Instant::now() + d;
    let mut seen = false;
    while Instant::now() < end {
        let ents = c.ask(json!({"cmd": "server", "op": "state", "what": "entities"}))?;
        seen |= ents
            .as_array()
            .is_some_and(|l| l.iter().any(|e| e["classname"] == json!("grenade")));
        c.pause(Duration::from_millis(100))?;
    }
    Ok(seen)
}

/// One case: the bot shoots `weapon` (or throws a frag for `"frag"`); the compass must ping or not.
fn case(
    c: &mut Driven,
    bot: u64,
    label: &str,
    weapon: &str,
    pings: bool,
) -> Result<String, String> {
    let grenade = weapon == "frag";
    if !grenade {
        // Give and raise the weapon first, so the burst below is its fire and not the raise.
        exec(c, &format!("devbotfire {bot} {weapon} 0"))?;
        c.pause(Duration::from_secs(2))?;
    }
    c.pause(SETTLE)?;
    let before = counts(c)?;
    let was = server_client(c, bot)?;
    let ms = if grenade { FRAG_MS } else { BURST_MS };
    exec(c, &format!("devbotfire {bot} {weapon} {ms}"))?;
    let missile = watch_for_missile(c, Duration::from_millis(u64::from(ms) + 1500))?;
    c.pause(Duration::from_secs(5))?;
    let after = counts(c)?;
    let now = server_client(c, bot)?;
    let fired = Fired {
        shots: now["shots"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(was["shots"].as_u64().unwrap_or(0)),
        missile,
    };
    verdict(label, pings, before, after, fired, grenade).map_err(|e| {
        format!(
            "{e}; bot {bot} before: weapon {} alive {} retained {}; after: weapon {} weapons {} alive {} \
             origin {} retained {}",
            was["weapon"],
            was["alive"],
            was["command_frame"]["retained"],
            now["weapon"],
            now["weapons"],
            now["alive"],
            now["origin"],
            now["command_frame"]["retained"]
        )
    })?;
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
    Ok(format!(
        "{label}: {} fire pings, {} flag pings, {} marks drawn, last ping {length} ms{}",
        after.fire_pings - before.fire_pings,
        after.flag_pings - before.flag_pings,
        after.marks - before.marks,
        match (grenade, fired.missile) {
            (false, _) => "",
            (true, true) => ", grenade thrown",
            (true, false) => ", no grenade seen",
        }
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

/// Unsilenced shots ping the compass for the fire sound's length; silenced shots and frags do not ping at all.
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
    let result = (|| {
        let bot = enemy_bot(c, enemy)?;
        let origin = c.lookup("player.origin")?;
        let at = |i: usize| origin[i].as_f64().unwrap_or(0.0);
        exec(
            c,
            &format!("devtele {bot} {} {} {}", at(0) + 96.0, at(1), at(2)),
        )?;
        let mut notes = Vec::new();
        notes.push(case(c, bot, "unsilenced", UNSILENCED, true)?);
        // `case` compares the independently played sound with the hold and final drawn mark lifetime.
        notes.push(case(c, bot, "silenced", SILENCED, false)?);
        notes.push(case(c, bot, "grenade", "frag", false)?);
        Ok(notes)
    })();
    exec(c, "set bot_idle 0")?;
    Ok(format!("compass: {}", result?.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOT: Fired = Fired {
        shots: 2,
        missile: false,
    };

    fn c(fire_pings: u64, flag_pings: u64, marks: u64) -> Counts {
        Counts {
            fire_pings,
            flag_pings,
            marks,
        }
    }

    #[test]
    fn an_unsilenced_shot_must_ping_and_draw_a_mark() {
        assert!(verdict("u", true, c(0, 0, 0), c(1, 1, 30), SHOT, false).is_ok());
        assert!(verdict("u", true, c(0, 0, 0), c(1, 0, 0), SHOT, false).is_err());
        assert!(verdict("u", true, c(0, 0, 0), c(0, 1, 30), SHOT, false).is_err());
    }

    #[test]
    fn a_silenced_shot_or_frag_must_not_ping_at_all() {
        let quiet = SHOT;
        assert!(verdict("s", false, c(1, 1, 9), c(1, 1, 9), quiet, false).is_ok());
        assert!(verdict("s", false, c(1, 1, 9), c(1, 2, 9), quiet, false).is_err());
        let frag = Fired {
            shots: 0,
            missile: true,
        };
        assert!(verdict("g", false, c(0, 0, 0), c(0, 0, 0), frag, true).is_ok());
        assert!(verdict("g", false, c(0, 0, 0), c(0, 0, 4), frag, true).is_err());
    }

    #[test]
    fn an_action_that_never_happened_fails() {
        assert!(verdict("s", false, c(0, 0, 0), c(0, 0, 0), Fired::default(), false).is_err());
        assert!(verdict("g", false, c(0, 0, 0), c(0, 0, 0), Fired::default(), true).is_err());
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
