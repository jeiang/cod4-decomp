// SPDX-License-Identifier: GPL-3.0-only
//! Snapshot firing-event compass parity (#406). Bots stand still and use real weapon usercmds.
//! An unsilenced shot must hold its rendered mark for the independently observed audio playback length,
//! then fade for compassSoundPingFadeTime; a silenced shot must not ping. The frag case: an offhand throw raises no
//! fire event, so it pings nothing; the listen-server bot's throw is proven by the `grenade` entity on the server.
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

pub(super) fn exec(c: &mut Driven, line: &str) -> Result<(), String> {
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
pub(super) fn server_client(c: &mut Driven, slot: u64) -> Result<Value, String> {
    let r = c.ask(json!({"cmd": "server", "op": "state", "what": "clients"}))?;
    r["clients"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["slot"].as_u64() == Some(slot)).cloned())
        .ok_or_else(|| format!("the server has no client {slot}"))
}

/// What a failure needs of a server client record: its weapons, life, health and position.
fn body(p: &Value) -> String {
    format!(
        "weapon {} weapons {} alive {} health {} spawn_count {} deaths {} shots {} origin {}",
        p["weapon_name"],
        p["weapons"],
        p["alive"],
        p["health"],
        p["spawn_count"],
        p["deaths"],
        p["shots"],
        p["origin"]
    )
}

/// The slot of a live bot on team `team` (1 axis, 2 allies, as both the client and the server number them).
pub(super) fn enemy_bot(c: &mut Driven, team: u64) -> Result<u64, String> {
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

/// The distances `place` tries, in turn, round the person (`devtele … near`): in front first, then behind (a negative
/// distance), then further out. `teleport_near` picks the same spot for the same person, view and distance, so only
/// another distance finds another spot.
const PLACE_DISTS: [f32; 10] = [
    96.0, -96.0, 128.0, -128.0, 160.0, -160.0, 192.0, -192.0, 224.0, -224.0,
];
/// How far the bot may stand from the wanted distance to the person, flat: `teleport_near` puts it exactly there.
const PLACE_SLACK: f64 = 8.0;

/// Where `place` left the bot: its server record at a checked spot, or why the attempt is lost.
enum Placed {
    At(Value),
    Lost(String),
}

/// Whether a placement at `dist` held, from the bot's record before `devtele` (`pre`), its two samples after it (right
/// away and one poll later) and the person's record: the same life, alive, no health lost and `dist` from the person
/// flat. A spot in a hurt volume kills or hurts the bot (#584).
fn spot_verdict(
    dist: f32,
    pre: &Value,
    samples: [&Value; 2],
    person: &Value,
) -> Result<(), String> {
    let f = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
    let at = |p: &Value, i: usize| f(&p["origin"][i]);
    // False for a missing (NaN) value too.
    let within = |x: f64, max: f64| x.abs() <= max;
    for (k, s) in samples.iter().enumerate() {
        let when = ["right after devtele", "at the next poll"][k];
        if s["alive"] != json!(true) || s["spawn_count"] != pre["spawn_count"] {
            return Err(format!("the bot died {when}"));
        }
        let (health, was) = (f(&s["health"]), f(&pre["health"]));
        if health.is_nan() || health < was {
            return Err(format!(
                "the bot lost health {when} ({} -> {})",
                pre["health"], s["health"]
            ));
        }
        let flat = (at(s, 0) - at(person, 0)).hypot(at(s, 1) - at(person, 1));
        if !within(flat - f64::from(dist.abs()), PLACE_SLACK) {
            return Err(format!(
                "the bot stands {flat:.1} units from the person {when}, not {}",
                dist.abs()
            ));
        }
    }
    Ok(())
}

/// Puts the living bot about a distance from the person (`devtele … near`, floor and a clear sight line) and checks
/// the spot with [`spot_verdict`]. A spot with no floor in sight, or not at the distance asked for, is passed over
/// for the next of [`PLACE_DISTS`], from `next` on. A spot the bot dies at, or a person who dies meanwhile, loses the
/// attempt; `next` then points past the deadly spot, so the next attempt from the same person tries another.
fn place(
    c: &mut Driven,
    bot: u64,
    me: u64,
    person: &Value,
    next: &mut usize,
) -> Result<Placed, String> {
    let mut passed = Vec::new();
    for _ in 0..PLACE_DISTS.len() {
        let dist = PLACE_DISTS[*next % PLACE_DISTS.len()];
        *next += 1;
        let pre = server_client(c, bot)?;
        if pre["alive"] != json!(true) {
            return Ok(Placed::Lost(format!(
                "bot {bot} was dead before its placement at {dist}: {}; passed over: [{}]",
                body(&pre),
                passed.join("; ")
            )));
        }
        // The hook's own refusal ("no spot … with floor and a clear sight line") only rules out this distance.
        if let Err(e) = exec(c, &format!("devtele {bot} near {me} {dist}")) {
            passed.push(format!("dist {dist}: {e}"));
            continue;
        }
        let first = server_client(c, bot)?;
        c.pause(Duration::from_millis(500))?;
        let second = server_client(c, bot)?;
        let me_now = server_client(c, me)?;
        if me_now["spawn_count"] != person["spawn_count"] || me_now["alive"] != json!(true) {
            return Ok(Placed::Lost(format!(
                "the person died while the bot was placed at {dist} (deaths {} -> {}, health {})",
                person["deaths"], me_now["deaths"], me_now["health"]
            )));
        }
        let Err(why) = spot_verdict(dist, &pre, [&first, &second], &me_now) else {
            return Ok(Placed::At(second));
        };
        let note = format!(
            "dist {dist}: {why}; person origin {}; bot before devtele: {}; right after: {}; at the next poll: {}",
            me_now["origin"],
            body(&pre),
            body(&first),
            body(&second)
        );
        if second["alive"] != json!(true) || second["spawn_count"] != pre["spawn_count"] {
            return Ok(Placed::Lost(format!(
                "bot {bot} died at its spot: {note}; passed over: [{}]",
                passed.join("; ")
            )));
        }
        passed.push(note);
    }
    Ok(Placed::Lost(format!(
        "bot {bot}: no spot round the person held: [{}]",
        passed.join("; ")
    )))
}

/// Waits for bot `bot` to be alive: one killed in an earlier attempt is between lives for a few seconds.
fn live_bot(c: &mut Driven, bot: u64, label: &str) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let p = server_client(c, bot)?;
        if p["alive"] == json!(true) {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("{label}: bot {bot} never respawned: {}", body(&p)));
        }
        c.pause(Duration::from_millis(500))?;
    }
}

/// What `raise` came to: the bot holds the weapon in the life it holds it in, or the attempt is lost.
enum Raised {
    Held(Value),
    Lost(String),
}

/// Places the bot ([`place`]) and has it give and raise `weapon`, so the burst after it is the weapon's fire and not
/// its raise. A bot that dies after its placement loses the attempt (#584: put back on the same spot, a bot that
/// spot kills only died again until the deadline); a living bot that does not raise it within the hook's own wait
/// fails. Returns the spawn count the bot holds it in.
fn raise(
    c: &mut Driven,
    (bot, me): (u64, u64),
    (person, next): (&Value, &mut usize),
    label: &str,
    weapon: &str,
) -> Result<Raised, String> {
    live_bot(c, bot, label)?;
    let placed = match place(c, bot, me, person, next)? {
        Placed::At(p) => p,
        Placed::Lost(why) => return Ok(Raised::Lost(why)),
    };
    exec(c, &format!("devbotfire {bot} {weapon} 0"))?;
    let (life, issued) = (placed["spawn_count"].clone(), Instant::now());
    loop {
        c.pause(Duration::from_millis(500))?;
        let held = server_client(c, bot)?;
        if held["alive"] != json!(true) || held["spawn_count"] != life {
            return Ok(Raised::Lost(format!(
                "bot {bot} died at its spot raising {weapon}: placed: {}; now: {}",
                body(&placed),
                body(&held)
            )));
        }
        if held["weapon_name"] == weapon {
            return Ok(Raised::Held(life));
        }
        // The hook asks for the weapon for 5 s (`DEV_RAISE_WAIT_MS` in `server/src/bot.rs`).
        if issued.elapsed() > Duration::from_secs(6) {
            return Err(format!(
                "{label}: bot {bot} never raised {weapon}: {}; placed: {}",
                body(&held),
                body(&placed)
            ));
        }
    }
}

/// One case: the bot, at a checked spot ([`place`]), shoots `weapon`; the compass must ping or not. An attempt in which
/// the bot died or respawned (its spot or something in the match killed it; the person's pings and its weapon are then
/// another life's) proves nothing and is made again, up to [`ATTEMPTS`] times. So is one in which the person (client
/// `me`) died: the bot's burst or another player can kill them, and a respawned person's compass holds another life's
/// pings. The bot stands about 96 units or more in front of or behind the person, in sight.
fn case(
    c: &mut Driven,
    me: u64,
    bot: u64,
    label: &str,
    weapon: &str,
    pings: bool,
) -> Result<String, String> {
    let (mut lost, mut next) = (Vec::new(), 0);
    for attempt in 1..=ATTEMPTS {
        let person = alive_person(c, me)?;
        let placed = match raise(c, (bot, me), (&person, &mut next), label, weapon)? {
            Raised::Held(life) => life,
            Raised::Lost(why) => {
                lost.push(format!("attempt {attempt}: {why}"));
                continue;
            }
        };
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
        let person_now = server_client(c, me)?;
        if now["spawn_count"] != was["spawn_count"] || now["alive"] != json!(true) {
            lost.push(format!(
                "attempt {attempt} before: {}; after: {}",
                body(&was),
                body(&now)
            ));
            continue;
        }
        if person_now["spawn_count"] != person["spawn_count"] || person_now["alive"] != json!(true)
        {
            lost.push(format!(
                "attempt {attempt}: the person died (deaths {} -> {}, health {})",
                person["deaths"], person_now["deaths"], person_now["health"]
            ));
            continue;
        }
        return finish(c, bot, label, pings, (before, after), (&was, &now))
            .map(|s| retried(s, &lost));
    }
    Err(format!(
        "{label}: the bot or the person died during every attempt: {}",
        lost.join("; ")
    ))
}

/// How long the bot holds the frag key: past the frag's hold time, so the release throws it.
const FRAG_MS: u32 = 1000;

/// Lets `d` pass while sampling the server's entities for a thrown grenade (classname `grenade`) and the bot's weapon
/// state, so a throw that never happens says where the offhand path stopped. The server's view, not the client's: a
/// grenade can leave the person's PVS.
fn watch_for_grenade(c: &mut Driven, bot: u64, d: Duration) -> Result<(bool, Vec<String>), String> {
    let end = Instant::now() + d;
    let (mut seen, mut trace) = (false, Vec::<String>::new());
    while Instant::now() < end {
        let ents = c.ask(json!({"cmd": "server", "op": "state", "what": "entities"}))?;
        seen |= grenade_in(&ents);
        let p = server_client(c, bot)?;
        let line = format!(
            "state {} weapon {} flags {} offhand {} fuse {} pm_flags {} buttons {}",
            p["weapon_state"],
            p["weapon"],
            p["weapon_flags"],
            p["offhand_index"],
            p["grenade_time_left"],
            p["pm_flags"],
            p["buttons"]
        );
        if trace.last() != Some(&line) {
            trace.push(line);
        }
        c.pause(Duration::from_millis(100))?;
    }
    Ok((seen, trace))
}

/// Whether the server's entity list (`state entities`; a section that is not an object comes back as the reply's
/// `value`) holds a thrown grenade (#486: read as the bare reply it never did, so every throw looked missing).
fn grenade_in(reply: &Value) -> bool {
    reply["value"]
        .as_array()
        .is_some_and(|l| l.iter().any(|e| e["classname"] == json!("grenade")))
}

/// The frag case: the bot, at a checked spot ([`place`]), throws a frag through the offhand path. The throw must be
/// proven (a `grenade` entity on the server, no `FireWeapon` shot) and must not ping the compass: an offhand throw
/// raises no fire event (`WEAPTYPE_GRENADE`). The frag can hurt or kill the person or the bot itself; a death changes
/// no fire or flag ping, so a seen grenade settles the case either way, and only a bot that respawned without one
/// (another life, another inventory) makes the attempt again. A placement that does not hold uses up an attempt too.
fn frag_case(c: &mut Driven, me: u64, bot: u64) -> Result<String, String> {
    let (mut lost, mut next) = (Vec::new(), 0);
    for attempt in 1..=ATTEMPTS {
        let person = alive_person(c, me)?;
        live_bot(c, bot, "grenade")?;
        if let Placed::Lost(why) = place(c, bot, me, &person, &mut next)? {
            lost.push(format!("attempt {attempt}: {why}"));
            continue;
        }
        c.pause(SETTLE)?;
        let before = counts(c)?;
        let was = server_client(c, bot)?;
        if was["alive"] != json!(true) {
            lost.push(format!(
                "attempt {attempt}: the bot died settling: {}",
                body(&was)
            ));
            continue;
        }
        exec(c, &format!("devbotfire {bot} frag {FRAG_MS}"))?;
        let (thrown, trace) =
            watch_for_grenade(c, bot, Duration::from_millis(u64::from(FRAG_MS) + 1500))?;
        c.pause(SETTLE)?;
        let after = counts(c)?;
        let now = server_client(c, bot)?;
        if !thrown && now["spawn_count"] != was["spawn_count"] {
            lost.push(format!(
                "attempt {attempt} before: {}; after: {}; trace [{}]",
                body(&was),
                body(&now),
                trace.join(" | ")
            ));
            continue;
        }
        let shots = now["shots"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(was["shots"].as_u64().unwrap_or(0));
        frag_verdict(thrown, shots, before, after).map_err(|e| {
            format!(
                "{e}; bot {bot} before: {}; after: {}; trace [{}]",
                body(&was),
                body(&now),
                trace.join(" | ")
            )
        })?;
        return Ok(retried(
            format!("grenade: thrown, 0 shots, counters {before:?} -> {after:?}"),
            &lost,
        ));
    }
    Err(format!(
        "grenade: the bot died during every attempt: {}",
        lost.join("; ")
    ))
}

/// The frag case's verdict: a grenade entity proves the throw, which is no `FireWeapon` shot and pings nothing.
fn frag_verdict(thrown: bool, shots: u64, before: Counts, after: Counts) -> Result<(), String> {
    if !thrown {
        return Err("grenade: no grenade entity on the server; the bot never threw".into());
    }
    if shots != 0 {
        return Err(format!(
            "grenade: the throw counted {shots} FireWeapon shots"
        ));
    }
    if after != before {
        return Err(format!(
            "grenade: want no compass ping, counters {before:?} -> {after:?}"
        ));
    }
    Ok(())
}

/// The person's server state once they are alive (a killed person respawns within a few seconds), and the client has
/// seen it. A person at health 0 is dying, not alive: the frag can kill them on the frame the server still reports
/// `alive`, and a step after this one would then find them dead.
fn alive_person(c: &mut Driven, me: u64) -> Result<Value, String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let p = server_client(c, me)?;
        if p["alive"] == json!(true) && p["health"].as_f64().is_some_and(|h| h > 0.0) {
            c.until(
                json!({"player.alive": true}),
                Duration::from_secs(10),
                "the client never saw the person alive again",
            )?;
            return Ok(p);
        }
        if Instant::now() > end {
            return Err(format!("the person did not respawn: {}", body(&p)));
        }
        c.pause(Duration::from_millis(500))?;
    }
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
        let gone = c
            .lookup("hud.report.last_fire_gone_ms")?
            .as_i64()
            .ok_or("missing first frame without the mark")?;
        duration_verdict(played, hold, (mark, gone), 2000)?;
    }
    let length = c.lookup("hud.report.last_fire_ping_ms")?;
    let played = c.lookup("hud.report.last_fire_played_ms")?;
    let mark = c.lookup("hud.report.last_fire_mark_ms")?;
    let gone = c.lookup("hud.report.last_fire_gone_ms")?;
    Ok(format!(
        "{label}: {} fire pings, {} flag pings, {} marks drawn, hold {length} ms, audio {played} ms, mark lifetime {mark}-{gone} ms",
        after.fire_pings - before.fire_pings,
        after.flag_pings - before.flag_pings,
        after.marks - before.marks,
    ))
}

/// The mark's lifetime lies between its age on the last frame that drew it (`mark`) and on the first that did not
/// (`gone`, 0 when unseen). At a few frames a second those are hundreds of milliseconds apart (#499), so the wanted
/// lifetime must fall in that span, give or take a few 60 Hz frames of snapshot and draw scheduling; never the
/// 351 ms fixed-hold error.
fn duration_verdict(
    played: i64,
    hold: i64,
    (mark, gone): (i64, i64),
    fade: i64,
) -> Result<(), String> {
    let want = played + fade;
    if hold != played || mark > want + 200 || mark.max(gone) < want - 200 {
        return Err(format!(
            "fire timing: played {played} ms, held {hold} ms, mark {mark} ms, gone {gone} ms; want {want} ms"
        ));
    }
    Ok(())
}

/// Unsilenced shots ping the compass for the fire sound's length; silenced shots and a thrown frag do not ping at all.
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
    let mut person = server_client(c, me)?;
    let mut bot_home = None;
    let result: Result<Vec<String>, String> = (|| {
        let bot = enemy_bot(c, enemy)?;
        bot_home = Some((bot, server_client(c, bot)?["origin"].clone()));
        // `case` compares the independently played sound with the hold and final drawn mark lifetime.
        let notes = vec![
            case(c, me, bot, "unsilenced", UNSILENCED, true)?,
            case(c, me, bot, "silenced", SILENCED, false)?,
            frag_case(c, me, bot)?,
        ];
        // A death `case` repeated an attempt for is in its note, and the frag may kill the person; the release below
        // must not add one.
        person = alive_person(c, me)?;
        Ok(notes)
    })();
    // Put the bot back where it stood before letting it think again: released next to the person, an enemy with a
    // raised SMG can kill them at the start of the next step.
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
            "the person died releasing the bot: before {person}, after {after}"
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
        assert!(duration_verdict(1649, 1649, (3640, 3656), 2000).is_ok());
        assert!(duration_verdict(1649, 2000, (4000, 4016), 2000).is_err());
        assert!(duration_verdict(1649, 1649, (2000, 2016), 2000).is_err());
        assert!(duration_verdict(0, 0, (1990, 0), 2000).is_ok());
        assert!(duration_verdict(0, 2000, (4000, 4016), 2000).is_err());
    }

    #[test]
    fn lifetime_spans_a_slow_frame_but_not_a_short_mark() {
        // #494: the last drawn frame 224 ms short of the fade's end, the next frame 300 ms later past it.
        assert!(duration_verdict(1649, 1649, (3425, 3725), 2000).is_ok());
        assert!(duration_verdict(1649, 1649, (3425, 0), 2000).is_err());
        assert!(duration_verdict(1649, 1649, (3000, 3100), 2000).is_err());
        assert!(duration_verdict(1649, 1649, (3900, 4200), 2000).is_err());
    }

    #[test]
    fn a_grenade_is_found_in_the_replys_value() {
        let reply = json!({"id": 3, "ok": true, "value": [
            {"classname": "player", "number": 0},
            {"classname": "grenade", "number": 70},
        ]});
        assert!(grenade_in(&reply));
        let none = json!({"id": 4, "ok": true, "value": [{"classname": "player", "number": 0}]});
        assert!(!grenade_in(&none));
    }

    #[test]
    fn a_frag_must_be_thrown_count_no_shot_and_ping_nothing() {
        assert!(frag_verdict(true, 0, c(2, 1, 9), c(2, 1, 9)).is_ok());
        assert!(frag_verdict(false, 0, c(2, 1, 9), c(2, 1, 9)).is_err());
        assert!(frag_verdict(true, 1, c(2, 1, 9), c(2, 1, 9)).is_err());
        assert!(frag_verdict(true, 0, c(2, 1, 9), c(3, 1, 9)).is_err());
        assert!(frag_verdict(true, 0, c(2, 1, 9), c(2, 2, 9)).is_err());
        assert!(frag_verdict(true, 0, c(2, 1, 9), c(2, 1, 10)).is_err());
    }

    fn bot(alive: bool, life: u64, health: i64, at: [f64; 3]) -> Value {
        json!({"alive": alive, "spawn_count": life, "health": health, "origin": at})
    }

    #[test]
    fn a_placement_holds_only_alive_in_the_same_life_unhurt_and_at_the_distance() {
        let (pre, person) = (
            bot(true, 3, 100, [0.0, 0.0, 0.0]),
            bot(true, 1, 100, [0.0, 0.0, 64.0]),
        );
        let s = bot(true, 3, 100, [0.0, -96.0, 32.5]);
        assert!(spot_verdict(-96.0, &pre, [&s, &s], &person).is_ok());
        // #584: dead on the floor of a hurt volume, or hurt there.
        let dead = bot(false, 3, 0, [0.0, -96.0, 32.5]);
        assert!(spot_verdict(-96.0, &pre, [&s, &dead], &person).is_err());
        let hurt = bot(true, 3, 90, [0.0, -96.0, 32.5]);
        assert!(spot_verdict(-96.0, &pre, [&s, &hurt], &person).is_err());
        // Respawned elsewhere between the read and the teleport: alive in another life.
        let other = bot(true, 4, 100, [0.0, -96.0, 32.5]);
        assert!(spot_verdict(-96.0, &pre, [&other, &other], &person).is_err());
        // Not where the hook was asked to put it.
        let far = bot(true, 3, 100, [0.0, -300.0, 32.5]);
        assert!(spot_verdict(-96.0, &pre, [&far, &far], &person).is_err());
    }
}
