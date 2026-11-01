// SPDX-License-Identifier: GPL-3.0-only
//! The compass firing ping (#406), checked inside the default `client-hud` stage right after the spawn. With the bots
//! told to stand still (`bot_idle 1`), one enemy bot is put next to the person and made to shoot through its own
//! usercmds (`devbotfire`): an unsilenced `mp5_mp` burst, a silenced `mp5_silencer_mp` burst, and a frag thrown with
//! the frag key. The client's own HUD counters say what the compass did with each: the unsilenced shot pings
//! (`enemy_fire_pings` and drawn `enemy_firing_marks` rise, `last_fire_ping_ms` is the fire sound's played length or
//! the 2000 ms fallback), the silenced shot and the frag ping nothing, neither through the fire event nor through a
//! server-forged `PING` flag (`enemy_flag_pings`). The server's client record confirms the bot did fire
//! (`shots`) and that only the unsilenced shot marked it for enemy compasses (`last_fire_time`).
use crate::control::Driven;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The unsilenced and silenced weapons of one family, so only the silencer differs.
const UNSILENCED: &str = "mp5_mp";
const SILENCED: &str = "mp5_silencer_mp";
/// How long the bot holds the trigger: a few rounds, as the server test of the hook (`dev_bot_fire.rs`) does.
const BURST_MS: u32 = 500;
/// How long the frag key is held before it is let go and the grenade flies.
const FRAG_MS: u32 = 400;
/// Long enough for a ping to fade and for the server's shot window (and the forged `PING` flag it sends) to close.
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
    /// Whether the bot's `last_fire_time` moved (the shot shows on enemy compasses).
    marked: bool,
    /// Whether a missile entity reached the client.
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
    if grenade {
        if !fired.missile {
            return Err(format!("{case}: the bot's frag never reached the client"));
        }
    } else if fired.shots == 0 {
        return Err(format!("{case}: the bot never fired ({fired:?})"));
    }
    if pings {
        if after.fire_pings <= before.fire_pings || after.marks <= before.marks || !fired.marked {
            return Err(format!(
                "{case}: want a compass ping, counters {before:?} -> {after:?}, server {fired:?}"
            ));
        }
    } else if after != before || fired.marked {
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

/// Lets `d` pass while sampling the client's entities for a missile.
fn watch_for_missile(c: &mut Driven, d: Duration) -> Result<bool, String> {
    let end = Instant::now() + d;
    let mut seen = false;
    while Instant::now() < end {
        let ents = c.state("entities")?;
        seen |= ents["list"]
            .as_array()
            .is_some_and(|l| l.iter().any(|e| e["kind"] == json!("missile")));
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
    c.pause(Duration::from_secs(1))?;
    let after = counts(c)?;
    let now = server_client(c, bot)?;
    let fired = Fired {
        shots: now["shots"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(was["shots"].as_u64().unwrap_or(0)),
        marked: now["last_fire_time"] != was["last_fire_time"],
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
    let length = c.lookup("hud.report.last_fire_ping_ms")?;
    Ok(format!(
        "{label}: {} fire pings, {} flag pings, {} marks drawn, last ping {length} ms",
        after.fire_pings - before.fire_pings,
        after.flag_pings - before.flag_pings,
        after.marks - before.marks
    ))
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
        let ping = c
            .lookup("hud.report.last_fire_ping_ms")?
            .as_i64()
            .unwrap_or(0);
        if ping <= 0 {
            return Err(format!(
                "unsilenced: the ping lasts {ping} ms, want the fire sound's length or 2000"
            ));
        }
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
        marked: true,
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
        let quiet = Fired {
            marked: false,
            ..SHOT
        };
        assert!(verdict("s", false, c(1, 1, 9), c(1, 1, 9), quiet, false).is_ok());
        assert!(verdict("s", false, c(1, 1, 9), c(1, 2, 9), quiet, false).is_err());
        assert!(verdict("s", false, c(1, 1, 9), c(1, 1, 9), SHOT, false).is_err());
        let frag = Fired {
            shots: 0,
            marked: false,
            missile: true,
        };
        assert!(verdict("g", false, c(0, 0, 0), c(0, 0, 0), frag, true).is_ok());
        assert!(verdict("g", false, c(0, 0, 0), c(0, 0, 4), frag, true).is_err());
    }

    #[test]
    fn a_case_where_nothing_was_fired_fails() {
        assert!(verdict("s", false, c(0, 0, 0), c(0, 0, 0), Fired::default(), false).is_err());
        assert!(verdict("g", false, c(0, 0, 0), c(0, 0, 0), Fired::default(), true).is_err());
    }
}
