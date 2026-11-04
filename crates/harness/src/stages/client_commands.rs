// SPDX-License-Identifier: GPL-3.0-only
//! `client-commands`: the commands a bind, a menu button or a console line runs in a match, each driven as the
//! player does it through the control channel (typed at the console, a bound key pressed, a menu clicked) and
//! checked by its effect (issues #364, #376). The Pause key's stock `toggle cl_paused` is refused as read only
//! (multiplayer has no pause); looking down then a bound `centerview` levels the view; `record` says where it writes
//! and `stoprecord` leaves a demo file; Escape > Call Vote > Restart Map calls a vote the server announces; the B key
//! opens the quick message menu and two keys say a team line the server relays; `setFromLocString` localizes into a
//! cvar; `cmd` forwards its words to the server; a word nothing knows is `Unknown command` at the front end and, in a
//! match, forwarded to the server, whose answer names it. `writeconfig` exports archived settings and binds to the
//! named file, not the active profile; missing or extra arguments only print usage. The console commands of #365:
//! bad arguments print the stock usage lines (`bind`, `unbind`, `set`, `seta` through a bind, `reset`,
//! `setfromdvar`, `con_showchannel`, `openscriptmenu`); `bind k`, `bindlist`, `reset`, `togglep`,
//! `cmdlist <filter>` and `clear` show or do what the original's do; hiding the `error` channel keeps a
//! not-implemented command's warning (#527) off the console and showing it brings it back.
//! `devgui` has no recovered retail 1.7 command registration (#528): typed at the front end it takes the same
//! unknown-command path as any unknown word, rather than the missing-command warning. The stock commands of
//! #479 answer as the original's: `dvar_int` registers a ranged dvar that refuses a value outside it, `setenv` sets
//! and shows a variable, `clientinfo` reports state 0 at the front end and 9 in a match, `configstrings` and a bare
//! `serverstatus` need a server, `fs_openedList`, `path` and `fdir` list the search path, `writedefaults` names its
//! file, `localservers` scans, and in the match `serverstatus` lists the players, `snd_setEqQ` refuses an unknown
//! channel and `snd_playLocal` an unknown alias; in the match `viewpos` prints the eye, the stance commands take the
//! player prone and crouched typed and bound alike, a `+word` that is no button is `Unknown command` typed and bound
//! alike, `openmenu`/`closemenu` open and close a menu, and
//! `openscriptmenu team_marinesopfor axis` moves the player to the other team. A dvar named like a server command
//! (`give`) is shown typed and bound, not forwarded; `connect <server> reconnect` joins from the front end (an
//! address nothing answers on opens the error popup) and a plain `connect` in a match is refused with the stock
//! `Already connected to a server. Disconnect first`, opening nothing. Launched with `snd_levelFadeTime 100`, the
//! initial level's sound fades in from silence over that non-default time (#407). The renderer commands of #527:
//! `imagelist` lists the loaded images with their totals and `imagelist all` warns with #581, as its generated
//! images are not listed; `reloadmaterialtextures` prints its usage, refuses a material nothing loaded and reloads
//! a loaded one's images in the menus' texture cache (evicted, then loaded again); in the match `r_savesun` and
//! `r_loadsun` print their usage, `r_loadsun` is refused without cheats, a name leaving the user folder is refused
//! and, with `sv_cheats` forced on at the server, a saved sun file loads back into the dvars and the world's flare,
//! a missing one warns and an incomplete one sets the dvars it names but leaves the world's flare. Needs a display
//! and the install; skips without.
use super::client_custom_class::{HOME, localized, session_report, until_top};
use super::client_flythrough::locate_client;
use super::client_frontend::cvar_is;
use super::client_models::no_display;
use super::client_session::{check_menu, verdict};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// The menu cache's `gfx.ui_texture_cache` fact: images evicted by reloads and image loads attempted.
fn ui_texture_cache(c: &mut Driven) -> Result<(u64, u64), String> {
    let v = c.lookup("gfx.ui_texture_cache")?;
    let text = v.as_str().unwrap_or_default();
    let mut n = text.split_whitespace().map(str::parse::<u64>);
    match (n.next(), n.next(), n.next()) {
        (Some(Ok(evicted)), Some(Ok(loads)), None) => Ok((evicted, loads)),
        _ => Err(format!("gfx.ui_texture_cache is {v}, not two counts")),
    }
}

/// The world flare's maximum alpha, the fifth number of the `gfx.sun_flare` fact.
fn flare_alpha(c: &mut Driven) -> Result<f64, String> {
    let v = c.lookup("gfx.sun_flare")?;
    v.as_str()
        .and_then(|s| s.split_whitespace().nth(4))
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| format!("gfx.sun_flare is {v}, without a maximum alpha"))
}

/// Forces `sv_cheats` at the listen server, as `devmap` sets it, and waits for the client to take it from the
/// systeminfo.
fn cheats(c: &mut Driven, value: &str) -> Result<(), String> {
    c.ask(
        json!({"cmd": "server", "op": "cvar", "name": "sv_cheats", "value": value, "force": true}),
    )?;
    let end = Instant::now() + Duration::from_secs(15);
    while c.ask(json!({"cmd": "cvar", "name": "sv_cheats"}))?["value"] != value {
        if Instant::now() > end {
            return Err(format!(
                "the client never took sv_cheats {value} from the server"
            ));
        }
        c.pause(Duration::from_millis(250))?;
    }
    Ok(())
}

/// `r_savesun` and `r_loadsun` in the match: usage, the cheat refusal, a name leaving the user folder refused, then
/// with cheats on a save and load round trip checked in the dvar and the world's flare, a missing file and an
/// incomplete one (`stagepart`, written by [`run`]), which sets the dvars it names but must not reach the world.
fn sun_files(c: &mut Driven, config: &Path) -> Result<(), String> {
    typed(c, "r_savesun", "USAGE: r_savesun <sunname>")?;
    typed(c, "r_loadsun", "USAGE: r_loadsun <sunname>")?;
    typed(
        c,
        "r_loadsun stagesun",
        "You must have cheats enabled to use r_loadsun",
    )?;
    // `FS_CreatePath` refuses the write (naming the OS path) and `FS_WriteFile` says it failed, both on `files`.
    typed(
        c,
        "r_savesun ../zz_escape",
        "Failed to open sun/../zz_escape.sun",
    )?;
    let refusal = lines(c)?.into_iter().rev().find(|l| {
        l.starts_with("^3WARNING: refusing to create relative path \"")
            && l.ends_with("sun/../zz_escape.sun\"")
    });
    if refusal.is_none() {
        return Err(
            "r_savesun ../zz_escape printed no stock `refusing to create relative path` warning"
                .into(),
        );
    }
    if config.join("zz_escape.sun").exists() {
        return Err("r_savesun ../zz_escape wrote outside the sun folder".into());
    }
    cheats(c, "1")?;
    // `FS_SanitizeFilename` refuses a `../`, so the read fails with the plain stock line.
    let escape = "WARNING: couldn't load sun file 'sun/../zz_escape.sun'";
    typed(c, "r_loadsun ../zz_escape", escape)?;
    if let Some(l) = lines(c)?.into_iter().rev().find(|l| l.contains(escape))
        && !l.ends_with(escape)
    {
        return Err(format!(
            "r_loadsun ../zz_escape added to the stock line: {l:?}"
        ));
    }
    c.console("set r_sunflare_max_alpha 0.37")?;
    // A leading separator stays inside `sun/`: stock writes `sun//stagesun.sun`, the file `r_loadsun stagesun` reads.
    c.console("r_savesun /stagesun")?;
    c.pause(Duration::from_millis(500))?;
    let saved = config.join("sun/stagesun.sun");
    let text = std::fs::read_to_string(&saved)
        .map_err(|e| format!("r_savesun left no {}: {e}", saved.display()))?;
    if text.lines().count() != 21 || !text.lines().any(|l| l == "r_sunflare_max_alpha \"0.37\"") {
        return Err(format!(
            "{} is not the 21 sun dvars with the set alpha: {text:?}",
            saved.display()
        ));
    }
    let map = flare_alpha(c)?;
    if (map - 0.37).abs() < 1e-4 {
        return Err("the map's flare already has the alpha the file sets".into());
    }
    c.console("set r_sunflare_max_alpha 0.81")?;
    c.console("r_loadsun stagesun")?;
    cvar_is(c, "r_sunflare_max_alpha", "0.37")?;
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let alpha = flare_alpha(c)?;
        if (alpha - 0.37).abs() < 1e-4 {
            break;
        }
        if Instant::now() > end {
            return Err(format!(
                "r_loadsun did not set the world's flare alpha to 0.37 (it is {alpha}, the map's {map})"
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
    typed(
        c,
        "r_loadsun zz_nosun",
        "WARNING: couldn't load sun file 'sun/zz_nosun.sun'",
    )?;
    typed(
        c,
        "r_loadsun stagepart",
        "^1ERROR: the following dvars were not specified in file 'sun/stagepart.sun'",
    )?;
    // `Com_PrintError` lines show on the `error` channel while `system` is hidden; each missing dvar is one.
    c.console("con_showchannel error")?;
    c.console("con_hidechannel system")?;
    typed(c, "r_loadsun stagepart", "^1Error:   r_sunsprite_shader")?;
    c.console("con_showchannel system")?;
    cvar_is(c, "r_sunflare_max_alpha", "0.9")?;
    c.frames(5)?;
    let alpha = flare_alpha(c)?;
    if (alpha - 0.37).abs() >= 1e-4 {
        return Err(format!(
            "the incomplete sun file reached the world's flare: alpha {alpha}, not 0.37"
        ));
    }
    cheats(c, "0")
}

const NAME: &str = "client-commands";
const DEMO: &str = "stagedemo";

/// An address nothing answers on.
const NOBODY: &str = "127.0.0.1:28998";

/// The console's lines now, oldest first.
fn lines(c: &mut Driven) -> Result<Vec<String>, String> {
    let l = c.lookup("console.lines")?;
    Ok(l.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect())
}

/// The lines of `after` that `before` did not have: the console keeps its latest lines only, so the old ones may
/// have scrolled off the front.
fn fresh<'a>(before: &[String], after: &'a [String]) -> &'a [String] {
    let kept = (0..=before.len())
        .find(|&d| after.starts_with(&before[d..]))
        .map_or(0, |d| before.len() - d);
    &after[kept..]
}

/// Runs `act` and waits up to `secs` for a new console line holding `text`; with `reply`, a line that is not the
/// echo of a typed one (`] ...`), so the words came back from the server.
fn says(
    c: &mut Driven,
    act: impl FnOnce(&mut Driven) -> Result<(), String>,
    text: &str,
    reply: bool,
    secs: u64,
) -> Result<(), String> {
    let before = lines(c)?;
    act(c)?;
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let after = lines(c)?;
        if fresh(&before, &after)
            .iter()
            .any(|l| l.contains(text) && !(reply && l.starts_with("] ")))
        {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!(
                "no new console {}line with {text:?} within {secs} s; last lines {:?}",
                if reply { "reply " } else { "" },
                &after[after.len().saturating_sub(6)..]
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

/// `line` typed at the console must print a line holding `text`.
fn typed(c: &mut Driven, line: &str, text: &str) -> Result<(), String> {
    says(c, |c| c.console(line), text, false, 5)
}

/// `key` bound to `command` at the console and pressed: the command runs as a bind does it. Binds run only with no
/// menu up (the menus take the keys), and the press waits a frame for the console to have closed.
fn bound(c: &mut Driven, key: &str, command: &str) -> Result<(), String> {
    c.console(&format!("bind {key} \"{command}\""))?;
    c.frames(2)?;
    c.key(key, "tap")
}

/// The top menu's name, or none.
fn top(c: &mut Driven) -> Result<Option<String>, String> {
    let open = c.lookup("menu.open")?;
    Ok(open
        .as_array()
        .and_then(|o| o.last())
        .and_then(Value::as_str)
        .map(str::to_owned))
}

fn no_menu(c: &mut Driven, secs: u64, what: &str) -> Result<(), String> {
    c.until(
        json!({"menu.open": {"len": 0}}),
        Duration::from_secs(secs),
        what,
    )
    .map(|_| ())
}

fn spawned(c: &mut Driven, secs: u64) -> Result<(), String> {
    c.until(
        json!({"net.spawned": true, "player.alive": true}),
        Duration::from_secs(secs),
        "the player did not spawn",
    )
    .map(|_| ())
}

fn pitch_in(c: &mut Driven, lo: f64, hi: f64) -> Result<(), String> {
    let p = c.lookup("view.pitch")?;
    match p.as_f64() {
        Some(p) if (lo..=hi).contains(&p) => Ok(()),
        _ => Err(format!("the view's pitch is {p}, not within {lo}..{hi}")),
    }
}

/// The `props` cgame fact: moving props, props `phys_stop` stopped, and whether the prop `hitprop` struck moves.
fn props(c: &mut Driven) -> Result<(u64, String), String> {
    let v = c.lookup("cgame.props")?;
    let text = v.as_str().ok_or(format!("cgame.props is {v}"))?;
    match text.split_whitespace().collect::<Vec<_>>()[..] {
        [_, stopped, probe] => Ok((
            stopped
                .parse()
                .map_err(|_| format!("cgame.props is {text}"))?,
            probe.to_owned(),
        )),
        _ => Err(format!("cgame.props is {text}")),
    }
}

/// Waits up to `secs` for the `props` fact to satisfy `ok`.
fn props_until(
    c: &mut Driven,
    secs: u64,
    what: &str,
    ok: impl Fn(u64, &str) -> bool,
) -> Result<u64, String> {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let (stopped, probe) = props(c)?;
        if ok(stopped, &probe) {
            return Ok(stopped);
        }
        if Instant::now() > end {
            return Err(format!(
                "{what} within {secs} s: {stopped} stopped, the struck prop moving {probe}"
            ));
        }
        c.pause(Duration::from_millis(50))?;
    }
}

fn ragdoll_counts<const N: usize>(c: &mut Driven, fact: &str) -> Result<[u64; N], String> {
    let value = c.lookup(fact)?;
    let numbers: Vec<u64> = value
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{fact}: {value}"))?;
    numbers
        .try_into()
        .map_err(|_| format!("{fact}: {value}, expected {N} counts"))
}

fn ragdoll_wait<const N: usize>(
    c: &mut Driven,
    fact: &str,
    description: &str,
    ok: impl Fn([u64; N]) -> bool,
) -> Result<[u64; N], String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let counts = ragdoll_counts(c, fact)?;
        if ok(counts) {
            return Ok(counts);
        }
        if Instant::now() >= end {
            return Err(format!("{description}: {fact} is {counts:?}"));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

/// Definition edits through typed console lines; a real death's corpse proves the same definition drives physics.
fn ragdoll_commands(c: &mut Driven) -> Result<(), String> {
    const DEF: &str = "cgame.ragdoll";
    const BODIES: &str = "cgame.ragdoll_bodies";
    let startup: [u64; 5] = ragdoll_counts(c, DEF)?;
    if startup[0] < 2 {
        return Err(format!(
            "startup ragdoll.cfg did not define bones: {startup:?}"
        ));
    }
    cheats(c, "1")?;
    c.ask(json!({"cmd":"server","op":"exec","line":"set bot_idle 1"}))?;
    c.console("ragdoll_max_life 0.2")?;
    let kill = |c: &mut Driven| -> Result<(), String> {
        spawned(c, 120)?;
        let me = c
            .lookup("player.client")?
            .as_u64()
            .ok_or("no player client")?;
        c.ask(json!({"cmd":"server","op":"exec","line":format!("devkill {me} {me}")}))?;
        c.until(
            json!({"player.dead":true}),
            Duration::from_secs(10),
            "devkill did not kill",
        )?;
        Ok(())
    };
    kill(c)?;
    let settled: [u64; 4] = ragdoll_wait(c, BODIES, "no resting corpse", |n: [u64; 4]| n[1] > 0)?;
    c.console("RaGdOlL_ClEaR 1")?;
    ragdoll_wait(c, DEF, "clear kept definition fields", |n: [u64; 5]| {
        n == [0; 5]
    })?;
    ragdoll_wait(
        c,
        BODIES,
        "clear did not reset the resting corpse",
        |n: [u64; 4]| n[0] == 0 && n[2] > settled[2] && n[3] > settled[3],
    )?;
    spawned(c, 120)?;
    let made = c
        .lookup("fx.ragdolls")?
        .as_u64()
        .ok_or("no ragdoll creation counter")?;
    kill(c)?;
    spawned(c, 120)?;
    if c.lookup("fx.ragdolls")?.as_u64() != Some(made) {
        return Err("a corpse created physics after ragdoll_clear".into());
    }
    c.console("ragdoll_bone 1 j_mainroot j_spinelower 4.5 0.5 20 0.7 -1 0 capsule")?;
    ragdoll_wait(c, DEF, "root bone not added", |n: [u64; 5]| {
        n == [1, 0, 0, 0, 0]
    })?;
    typed(
        c,
        "ragdoll_bone 1 j_neck j_head 3.6 0.5 5 0.6 7 0 capsule",
        "Ragdoll: Child bones must come after parent bones: 7",
    )?;
    c.console("ragdoll_bone 1 j_spinelower j_spineupper 4.5 0.5 20 0.7 0 0 capsule")?;
    ragdoll_wait(c, DEF, "second bone not added", |n: [u64; 5]| {
        n == [2, 0, 0, 0, 0]
    })?;
    c.console("ragdoll_joint 1 1 hinge")?;
    ragdoll_wait(c, DEF, "joint not added", |n: [u64; 5]| {
        n == [2, 1, 0, 0, 0]
    })?;
    typed(
        c,
        "ragdoll_joint 1 0 hinge",
        "Ragdoll: Joint referenced bone with no parent (0)",
    )?;
    c.console("ragdoll_limit 1 0 x 0.5 -10 10")?;
    ragdoll_wait(c, DEF, "limit not added", |n: [u64; 5]| {
        n == [2, 1, 1, 0, 0]
    })?;
    typed(
        c,
        "ragdoll_limit 1 9 x 0 -10 10",
        "Ragdoll: Angular limit added to nonexistent joint 9",
    )?;
    c.console("ragdoll_selfpair 1 0 1")?;
    ragdoll_wait(c, DEF, "selfpair not added", |n: [u64; 5]| {
        n == [2, 1, 1, 1, 0]
    })?;
    typed(
        c,
        "ragdoll_selfpair 1 0 99",
        "Ragdoll: Bad self collision pair bone 99",
    )?;
    // Restore the stock shape for the no-reset checks, on a settled body.
    c.console("exec ragdoll.cfg")?;
    ragdoll_wait(
        c,
        DEF,
        "exec did not restore the startup shape",
        |n: [u64; 5]| n == startup,
    )?;
    kill(c)?;
    let resting: [u64; 4] = ragdoll_wait(
        c,
        BODIES,
        "restored definition made no corpse",
        |n: [u64; 4]| n[1] > 0,
    )?;
    if c.lookup("fx.ragdolls")?.as_u64().unwrap_or(0) <= made {
        return Err("exec ragdoll.cfg did not restore corpse physics".into());
    }
    // Stock definition 1 has all nine lerp slots occupied. Use definition 0 to exercise both appends without
    // resetting any body; its bone names are actual zone script strings, not merely accepted spellings.
    c.console("ragdoll_clear 0")?;
    c.pause(Duration::from_millis(500))?;
    c.console("ragdoll_bone 0 j_mainroot j_spinelower 4.5 0.5 20 0.7 -1 0 capsule")?;
    c.pause(Duration::from_millis(500))?;
    let before: [u64; 4] = ragdoll_wait(
        c,
        BODIES,
        "definition-0 edit lost the resting body",
        |n: [u64; 4]| n[1] > 0,
    )?;
    c.console("ragdoll_baselerp_bone 0 j_head 500")?;
    c.console("ragdoll_pin_bone 0 j_head 0")?;
    typed(
        c,
        "ragdoll_pin_bone 0 zz_never_interned 0",
        "Ragdoll: Couldn't find pinned bone named zz_never_interned",
    )?;
    typed(
        c,
        "ragdoll_pin_bone 0 j_head 99",
        "Ragdoll: Pinned bone has invalid parent index 99",
    )?;
    let zero: [u64; 5] = ragdoll_counts(c, "cgame.ragdoll0")?;
    if zero != [1, 0, 0, 0, 2] {
        return Err(format!("baselerp/pin did not append: {zero:?}"));
    }
    let after: [u64; 4] = ragdoll_counts(c, BODIES)?;
    if after[2..] != before[2..] {
        return Err(format!(
            "baselerp/pin reset bodies: {before:?} -> {after:?}"
        ));
    }
    if resting[1] == 0 {
        return Err("no resting body for no-reset check".into());
    }
    c.console("exec ragdoll.cfg")?;
    ragdoll_wait(
        c,
        DEF,
        "final exec did not restore startup definition",
        |n: [u64; 5]| n == startup,
    )?;
    c.console("ragdoll_max_life 4.5")?;
    spawned(c, 120)?;
    c.ask(json!({"cmd":"server","op":"exec","line":"set bot_idle 0"}))?;
    cheats(c, "0")
}

/// A prop the `hitprop` dev hook struck falls; typed `Phys_Stop` (command names match without regard to case) stops
/// it where it is (`Phys_Stop_f`); typed `phys_go` leaves it there (`Phys_Go_f` re-enables collision only), and the
/// next force moves it again (#529).
fn phys(c: &mut Driven) -> Result<(), String> {
    c.ask(json!({"cmd": "dev", "op": "hitprop", "arg": ""}))?;
    let before = props_until(c, 2, "the struck prop did not move", |_, p| p == "1")?;
    let shown = lines(c)?;
    c.console("Phys_Stop")?;
    props_until(c, 2, "phys_stop did not stop the moving prop", |s, p| {
        s > before && p == "0"
    })?;
    c.pause(Duration::from_secs(1))?;
    props_until(c, 0, "the stopped prop moved again", |_, p| p == "0")?;
    c.console("phys_go")?;
    c.pause(Duration::from_millis(500))?;
    props_until(c, 0, "phys_go woke the stopped prop", |_, p| p == "0")?;
    let after = lines(c)?;
    // Other lines (the kill feed, chat) may come and go meanwhile; a warning or `Unknown command` names the word.
    if let Some(l) = fresh(&shown, &after)
        .iter()
        .find(|l| !l.starts_with("] ") && l.to_ascii_lowercase().contains("phys_"))
    {
        return Err(format!(
            "phys_stop and phys_go print nothing in stock, but the console said {l:?}"
        ));
    }
    c.ask(json!({"cmd": "dev", "op": "hitprop", "arg": ""}))?;
    props_until(c, 2, "a force did not move the stopped prop", |_, p| {
        p == "1"
    })
    .map(drop)
}

/// `writeconfig` from the console: the named exports, the usage lines, and the active profile read back as it was.
fn writeconfig(c: &mut Driven, active: &Path) -> Result<(), String> {
    c.step("writeconfig exports and usage", |c| {
        until_top(c, HOME, Duration::from_secs(60))?;
        c.console("seta writeconfig_marker exported")?;
        c.console("bind f10 \"say named_config_export\"")?;
        c.console("writeconfig commands-export")?;
        c.console("writeconfig commands-extension.cfg")?;
        typed(c, "writeconfig", "Usage: writeconfig <filename>")?;
        typed(
            c,
            "writeconfig commands-invalid extra",
            "Usage: writeconfig <filename>",
        )?;
        // Read the active file back before native shutdown saves it: if writeconfig wrote the profile instead,
        // this exec reads "exported", not "baseline".
        c.console(&format!("exec \"{}\"", active.display()))?;
        cvar_is(c, "writeconfig_marker", "baseline")
    })
}

/// The console commands at the front end: usage lines, binds, cvars, channels, and a connect nothing answers.
fn front_end(c: &mut Driven) -> Result<(), String> {
    c.step(
        "ragdoll definitions and script-string lookup at the front end",
        |c| {
            c.console("ragdoll_clear 0")?;
            typed(
                c,
                "ragdoll_pin_bone 0 zz_never_interned 0",
                "Ragdoll: Couldn't find pinned bone named zz_never_interned",
            )?;
            c.console("ragdoll_bone 0 j_mainroot j_spinelower 4.5 0.5 20 0.7 -1 0 capsule")?;
            ragdoll_wait(
                c,
                "cgame.ragdoll0",
                "frontend definition not edited",
                |n: [u64; 5]| n == [1, 0, 0, 0, 0],
            )
            .map(|_| ())
        },
    )?;
    c.step("front-end console commands", |c| {
        typed(
            c,
            "zz_unknown_at_front",
            "Unknown command \"zz_unknown_at_front\"",
        )?;
        typed(
            c,
            "bind",
            "bind <key> [command] : attach a command to a key",
        )?;
        c.console("bind f11 \"say bound_a\"")?;
        typed(c, "bind f11", "\"f11\" = \"say bound_a\"")?;
        typed(c, "bind zz_nokey x", "\"zz_nokey\" isn't a valid key")?;
        typed(c, "unbind", "unbind <key> : remove commands from a key")?;
        typed(c, "set", "USAGE: set <variable> <value>")?;
        typed(c, "set zz.bad 1", "invalid variable name: zz.bad")?;
        typed(c, "reset", "USAGE: reset <variable>")?;
        typed(
            c,
            "setfromdvar zz_a",
            "USAGE: setfromdvar <dest_dvar> <source_dvar>",
        )?;
        typed(c, "con_showchannel", "<channel> may include wildcards */?")?;
        typed(
            c,
            "openscriptmenu",
            "EXAMPLE: openscriptmenu ingame changeweapon",
        )?;
        c.console("set zz_reset first")?;
        c.console("set zz_reset second")?;
        c.console("reset zz_reset")?;
        cvar_is(c, "zz_reset", "first")?;
        c.console("set zz_tp 0")?;
        typed(c, "togglep zz_tp", "zz_tp toggled to 1")?;
        cvar_is(c, "zz_tp", "1")?;
        says(c, |c| c.console("bindlist"), "f11 \"say bound_a\"", true, 5)?;
        says(
            c,
            |c| c.console("cmdlist togglecr*"),
            "togglecrouch",
            true,
            5,
        )?;
        if !lines(c)?.iter().any(|l| l.contains("1 commands")) {
            return Err("cmdlist togglecr* did not count 1 commands".into());
        }
        c.console("clear")?;
        if let Some(l) = lines(c)?.iter().find(|l| l.contains("zz_unknown_at_front")) {
            return Err(format!("clear left {l:?} on the console"));
        }
        Ok(())
    })?;
    c.step("the error channel hidden and shown", |c| {
        typed(c, "con_hidechannel error", "Hiding channel: error")?;
        let before = lines(c)?;
        c.console("r_smc_flush")?;
        c.pause(Duration::from_secs(1))?;
        let after = lines(c)?;
        if let Some(l) = fresh(&before, &after)
            .iter()
            .find(|l| l.contains("not implemented"))
        {
            return Err(format!("the hidden error channel still printed {l:?}"));
        }
        typed(c, "con_showchannel error", "Adding channel: error")?;
        typed(c, "r_smc_stats", "not implemented")
    })?;
    c.step("devgui takes the unknown-command path", |c| {
        typed(c, "devgui", "Unknown command \"devgui\"")
    })?;
    c.step("the renderer commands at the front end", |c| {
        let reply = |c: &mut Driven, line: &str, text: &str| {
            let line = line.to_owned();
            says(c, move |c| c.console(&line), text, true, 5)
        };
        reply(c, "imagelist", " total images")?;
        let after = lines(c)?;
        let total = after
            .iter()
            .rev()
            .find_map(|l| l.strip_suffix(" total images"))
            .and_then(|n| n.trim().parse::<u32>().ok());
        if !total.is_some_and(|n| n > 0) {
            return Err(format!("imagelist counted no images: {total:?}"));
        }
        if !after
            .last()
            .is_some_and(|l| l.starts_with("Related commands: meminfo, imagelist"))
        {
            return Err(format!(
                "imagelist did not end with its related commands: {:?}",
                after.last()
            ));
        }
        reply(c, "imagelist all", "issue=581")?;
        reply(
            c,
            "reloadmaterialtextures",
            "Usage: reloadmaterialtextures <materialname>",
        )?;
        reply(
            c,
            "reloadmaterialtextures zz_nomaterial",
            "ReloadMaterialTextures: Material 'zz_nomaterial' is not currently loaded.",
        )?;
        // The menus' pointer, which every front-end frame draws from the UI zone's `ui_cursor` material (the
        // filled rectangles of `white` take no image from the cache).
        let (evicted, loads) = ui_texture_cache(c)?;
        c.console("reloadmaterialtextures ui_cursor")?;
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let now = ui_texture_cache(c)?;
            if now.0 > evicted && now.1 > loads {
                break;
            }
            if Instant::now() > end {
                return Err(format!(
                    "reloadmaterialtextures ui_cursor did not evict and load again the menus' images: {:?} before, {now:?} after",
                    (evicted, loads)
                ));
            }
            c.pause(Duration::from_millis(100))?;
        }
        if let Some(l) = lines(c)?.iter().find(|l| l.contains("Material 'ui_cursor'")) {
            return Err(format!("reloadmaterialtextures ui_cursor was refused: {l:?}"));
        }
        Ok(())
    })?;
    c.step("the stock commands of #479 at the front end", |c| {
        let reply = |c: &mut Driven, line: &str, text: &str| {
            let line = line.to_owned();
            says(c, move |c| c.console(&line), text, true, 5)
        };
        c.console("dvar_int zz_cmdtest 5 0 10")?;
        reply(c, "zz_cmdtest", "\"zz_cmdtest\" is \"5\"")?;
        reply(c, "zz_cmdtest 50", "Domain is any integer from 0 to 10")?;
        cvar_is(c, "zz_cmdtest", "5")?;
        reply(
            c,
            "dvar_int zz_cmdtest2 1 9 2",
            "min 9 should not be greater than max 2",
        )?;
        c.console("setenv ZZ_ENV a b")?;
        reply(c, "setenv ZZ_ENV", "ZZ_ENV=a b")?;
        reply(c, "clientinfo", "Client Information")?;
        if !lines(c)?.iter().any(|l| l.contains("state: 0")) {
            return Err("clientinfo at the front end did not report state: 0".into());
        }
        reply(c, "configstrings", "Not connected to a server.")?;
        reply(c, "serverstatus", "Usage: serverstatus [server]")?;
        reply(c, "fs_openedList", "Opened IWD Names: iw_")?;
        if let Some(l) = lines(c)?.iter().find(|l| l.contains("Names: Opened IWD")) {
            return Err(format!("fs_openedList ran its lines together: {l:?}"));
        }
        reply(c, "path", "Current search path:")?;
        reply(c, "fdir zz_none*", "0 files listed")?;
        reply(c, "writedefaults zz_defaults", "Writing zz_defaults.cfg.")?;
        reply(
            c,
            "localservers",
            "Scanning for servers on the local network...",
        )
    })?;
    c.step("connect to an address nothing answers on", |c| {
        c.console(&format!("connect {NOBODY} reconnect"))?;
        until_top(c, "error_popmenu", Duration::from_secs(30))?;
        cvar_is(c, "com_errorMessage", &format!("{NOBODY} did not answer"))?;
        c.key("escape", "tap")?;
        c.pause(Duration::from_secs(1))?;
        cvar_is(c, "com_errorMessage", "")?;
        c.until(
            json!({"client.in_world": false}),
            Duration::from_secs(5),
            "the client did not come back to the front end",
        )?;
        until_top(c, HOME, Duration::from_secs(5))?;
        let net = c.lookup("net")?;
        if net.is_null() {
            Ok(())
        } else {
            Err(format!("the failed connect left a connection: {net}"))
        }
    })
}

/// The commands in the match: the player's binds and keys, the console, the menus, and the server's answers.
fn in_match(c: &mut Driven, config: &Path, custom3: &str) -> Result<(), String> {
    c.step("start a server and join the Marines", |c| {
        c.console("set ui_netGametypeName war")?;
        cvar_is(c, "ui_netGametypeName", "war")?;
        c.click("Start New Server")?;
        until_top(c, "createserver", Duration::from_secs(20))?;
        c.click("Start")?;
        until_top(c, "team_marinesopfor", Duration::from_secs(90))?;
        c.pause(Duration::from_secs(1))?;
        c.click("Marines")?;
        until_top(c, "changeclass", Duration::from_secs(30))?;
        c.pause(Duration::from_secs(1))?;
        c.click("Assault")?;
        spawned(c, 120)?;
        c.pause(Duration::from_secs(3))?;
        c.until(
            json!({"hud.live.own_team": 2}),
            Duration::from_secs(5),
            "the player is not a Marine",
        )
        .map(|_| ())
    })?;
    c.step("ragdoll console edits survive loading the match", |c| {
        let n: [u64; 5] = ragdoll_counts(c, "cgame.ragdoll0")?;
        if n != [1, 0, 0, 0, 0] {
            return Err(format!("map load replaced frontend definition: {n:?}"));
        }
        Ok(())
    })?;
    // The `cgame` section is not one a `wait` builds (`control::PARTS`): it is read by name until it answers.
    c.step("the level's sound faded in over snd_levelFadeTime", |c| {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let fade = c.lookup("cgame.levelfade")?;
            if fade == "in 100" {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(format!(
                    "the level's sound did not fade in over 100 ms: cgame.levelfade is {fade}"
                ));
            }
            c.pause(Duration::from_millis(100))?;
        }
    })?;
    c.step("the Pause key is refused", |c| {
        says(
            c,
            |c| {
                c.key("pause", "down")?;
                c.key("pause", "up")
            },
            "cl_paused is read only.",
            false,
            5,
        )?;
        cvar_is(c, "cl_paused", "0")
    })?;
    c.step("look down, then a bound centerview", |c| {
        c.console("bind f9 centerview")?;
        c.key("downarrow", "down")?;
        c.pause(Duration::from_secs(1))?;
        c.key("downarrow", "up")?;
        c.pause(Duration::from_millis(500))?;
        pitch_in(c, -90.0, -20.0)?;
        c.capture("lookdown")?;
        c.key("f9", "down")?;
        c.key("f9", "up")?;
        c.pause(Duration::from_millis(500))?;
        pitch_in(c, -3.0, 3.0)?;
        c.capture("centered").map(|_| ())
    })?;
    c.step("record and stoprecord", |c| {
        typed(c, &format!("record {DEMO}"), "recording to")?;
        c.pause(Duration::from_secs(2))?;
        typed(c, "stoprecord", "Stopped demo.")?;
        typed(c, "stoprecord", "Not recording a demo.")
    })?;
    c.step("phys_stop and phys_go on a struck prop", phys)?;
    c.step("r_savesun and r_loadsun", |c| sun_files(c, config))?;
    c.step("words the server answers", |c| {
        says(
            c,
            |c| c.console("cmd say line_through_cmd"),
            "line_through_cmd",
            true,
            6,
        )?;
        says(
            c,
            |c| c.console("zz_unknown_in_match"),
            "zz_unknown_in_match",
            true,
            6,
        )?;
        says(c, |c| c.console("viewpos"), ") : ", true, 5)?;
        says(c, |c| c.console("clientinfo"), "state: 9", true, 5)?;
        says(c, |c| c.console("configstrings"), "17: ", true, 5)?;
        says(
            c,
            |c| c.console("serverstatus"),
            "num: score: ping: name:",
            true,
            10,
        )?;
        says(
            c,
            |c| c.console("snd_setEqQ zz_chan 0 0 1"),
            "Unknown channel name (zz_chan)",
            true,
            5,
        )?;
        says(
            c,
            |c| c.console("snd_playLocal zz_noalias"),
            "Couldn't find sound alias \"zz_noalias\".",
            true,
            5,
        )
    })?;
    // A bound key runs its command only with no menu up: at the front end the menus take the keys.
    c.step("seta through a bind prints its usage", |c| {
        says(
            c,
            |c| bound(c, "f8", "seta zz_x"),
            "USAGE: seta <variable> <value>",
            false,
            5,
        )
    })?;
    c.step("a dvar named give, typed and bound", |c| {
        c.console("set give sentinel")?;
        typed(c, "give", "\"give\" is \"sentinel\"")?;
        c.console("set give sentinel_bound")?;
        says(
            c,
            |c| bound(c, "f5", "give"),
            "\"give\" is \"sentinel_bound\"",
            false,
            5,
        )
    })?;
    c.step("the stance commands, typed and bound", |c| {
        let limit = Duration::from_secs(5);
        c.console("toggleprone")?;
        c.until(
            json!({"player.prone": true}),
            limit,
            "toggleprone did not go prone",
        )?;
        c.console("toggleprone")?;
        c.until(
            json!({"player.prone": false, "player.crouched": false}),
            limit,
            "toggleprone again did not stand",
        )?;
        bound(c, "f7", "togglecrouch")?;
        c.until(
            json!({"player.crouched": true}),
            limit,
            "the bound togglecrouch did not crouch",
        )?;
        c.key("f7", "tap")?;
        c.until(
            json!({"player.crouched": false, "player.prone": false}),
            limit,
            "the bound togglecrouch again did not stand",
        )
        .map(|_| ())
    })?;
    c.step("a +word that is no button, typed and bound", |c| {
        typed(c, "+zz_typed", "Unknown command \"+zz_typed\"")?;
        c.console("bind f6 +zz_bound")?;
        says(
            c,
            |c| {
                c.key("f6", "down")?;
                c.key("f6", "up")
            },
            "Unknown command \"+zz_bound\"",
            false,
            5,
        )
    })?;
    c.step("openmenu and closemenu", |c| {
        c.console("openmenu quickmessage")?;
        until_top(c, "quickmessage", Duration::from_secs(5))?;
        c.console("closemenu quickmessage")?;
        no_menu(c, 5, "closemenu left a menu open")
    })?;
    c.step("connect in a match is refused", |c| {
        typed(
            c,
            &format!("connect {NOBODY}"),
            "Already connected to a server. Disconnect first",
        )?;
        c.pause(Duration::from_secs(3))?;
        no_menu(c, 1, "the refused connect opened a menu")?;
        c.until(
            json!({"net.connected": true}),
            Duration::from_secs(1),
            "the refused connect dropped the match",
        )
        .map(|_| ())
    })?;
    c.step("openscriptmenu moves the player to the other team", |c| {
        c.console("openscriptmenu team_marinesopfor axis")?;
        c.until(
            json!({"hud.live.own_team": 1}),
            Duration::from_secs(15),
            "the player did not join the Opfor",
        )?;
        until_top(c, "changeclass", Duration::from_secs(10))?;
        c.pause(Duration::from_secs(1))?;
        c.click("Assault")?;
        spawned(c, 60)?;
        c.pause(Duration::from_secs(3))
    })?;
    c.step("setFromLocString localizes into a cvar", |c| {
        c.console("setFromLocString customclass3 @CLASS_CUSTOM3")?;
        c.pause(Duration::from_millis(500))?;
        cvar_is(c, "customclass3", custom3)
    })?;
    c.step("Escape > Call Vote > Restart Map", |c| {
        c.bind("togglemenu", "tap")?;
        c.pause(Duration::from_secs(1))?;
        check_menu(c, &["Call Vote"], &[])?;
        c.click("Call Vote")?;
        until_top(c, "callvote", Duration::from_secs(5))?;
        c.pause(Duration::from_secs(1))?;
        check_menu(c, &["Restart Map", "Next Map"], &[])?;
        c.capture("callvote")?;
        says(
            c,
            |c| c.click("Restart Map").map(|_| ()),
            "called a vote",
            false,
            6,
        )?;
        no_menu(c, 5, "the vote left a menu open")?;
        c.pause(Duration::from_secs(1))
    })?;
    c.step("the quick message menu says a team line", |c| {
        c.key("b", "down")?;
        c.key("b", "up")?;
        until_top(c, "quickmessage", Duration::from_secs(5))?;
        c.pause(Duration::from_millis(500))?;
        c.capture("quickmessage")?;
        let before = lines(c)?;
        c.key("1", "tap")?;
        until_top(c, "quickcommands", Duration::from_secs(5))?;
        c.pause(Duration::from_millis(500))?;
        c.capture("quickcommands")?;
        c.key("1", "tap")?;
        no_menu(c, 5, "the quick command left a menu open")?;
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            let after = lines(c)?;
            if fresh(&before, &after).iter().any(|l| l.contains("(Team)")) {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(format!(
                    "the server relayed no team line; top menu {:?}",
                    top(c)?
                ));
            }
            c.pause(Duration::from_millis(100))?;
        }
    })?;
    // Intentional deaths change the spawn point: run these after the stance/view/quick-message assertions.
    c.step(
        "ragdoll commands edit definitions and reset resting corpses",
        ragdoll_commands,
    )
}

fn drive(c: &mut Driven, active: &Path, config: &Path, custom3: &str) -> Result<(), String> {
    writeconfig(c, active)?;
    front_end(c)?;
    in_match(c, config, custom3)
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
    let custom3 = match localized(install, &["CLASS_CUSTOM3"]) {
        Ok(t) => t["CLASS_CUSTOM3"].clone(),
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    // `Driven` gives the client the profile under `<dir>/config`.
    let config = ctx.dir.join("config");
    let active = config.join("players/profiles/harness/config_mp.cfg");
    std::fs::create_dir_all(active.parent().unwrap())?;
    std::fs::write(&active, "seta writeconfig_marker baseline\n")?;
    let exports = ["commands-export.cfg", "commands-extension.cfg"];
    for file in exports.into_iter().chain(["commands-invalid.cfg"]) {
        match std::fs::remove_file(config.join("players").join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // The saved sun file is the stage's to write; the incomplete one names one of the 21 sun dvars only.
    let sun = config.join("sun");
    std::fs::create_dir_all(&sun)?;
    match std::fs::remove_file(sun.join("stagesun.sun")) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::write(sun.join("stagepart.sun"), "r_sunflare_max_alpha \"0.9\"\n")?;
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(
        &client,
        install,
        &ctx.dir,
        &["--bots", "9", "+set", "snd_levelFadeTime", "100"],
    ) {
        Ok(c) => c,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let played = drive(&mut c, &active, &config, &custom3);
    let report = session_report(&mut c, "control-run.json");
    let status = c.finish(Duration::from_secs(30));
    out.files.push("control-run.json".into());
    for f in [
        "lookdown",
        "centered",
        "callvote",
        "quickmessage",
        "quickcommands",
    ] {
        if ctx.dir.join(format!("{f}.png")).is_file() {
            out.files.push(format!("{f}.png"));
        }
    }
    let demo = config.join("demos").join(format!("{DEMO}.c4edemo"));
    let demo_len = std::fs::metadata(&demo).map(|m| m.len()).unwrap_or(0);
    let export_problem = exports
        .iter()
        .find_map(|file| {
            let path = config.join("players").join(file);
            match std::fs::read_to_string(&path) {
                Ok(text)
                    if text.starts_with("// generated by cod4e\n")
                        && text
                            .lines()
                            .any(|l| l == "seta writeconfig_marker \"exported\"")
                        && text.lines().any(|l| l == "unbindall")
                        && text
                            .lines()
                            .any(|l| l == "bind \"f10\" \"say named_config_export\"") =>
                {
                    out.files.push(format!("config/players/{file}"));
                    None
                }
                Ok(_) => Some(format!(
                    "{} lacks the exported archived cvar or bind",
                    path.display()
                )),
                Err(e) => Some(format!("named writeconfig export {}: {e}", path.display())),
            }
        })
        .or_else(|| {
            ["commands-extension.cfg.cfg", "commands-invalid.cfg"]
                .iter()
                .find(|file| config.join("players").join(file).exists())
                .map(|file| format!("writeconfig unexpectedly created {file}"))
        });
    let problem = match (played, report) {
        (Err(e), _) | (Ok(()), Err(e)) => Some(e),
        (Ok(()), Ok(report)) => verdict(&report),
    }
    .or(export_problem)
    .or_else(|| {
        (demo_len == 0).then(|| {
            format!(
                "record left no demo at {} (the console said it was recording)",
                demo.display()
            )
        })
    })
    .or_else(|| {
        (!status.is_some_and(|s| s.success()))
            .then(|| format!("the client did not exit successfully: {status:?}"))
    });
    match problem {
        Some(w) => {
            out.status = Status::Failed;
            out.reason = Some(w);
        }
        None => out.notes.push(format!(
            "Pause refused, centerview levelled the view, record wrote {demo_len} bytes, the vote and the quick message reached the server, setFromLocString and cmd ran; writeconfig exported both named configs with the archived cvar and bind, left the active profile unchanged until its normal save, and rejected invalid arity; imagelist listed the images, reloadmaterialtextures reloaded the menus' pointer, and a saved sun file loaded back into the world's flare"
        )),
    }
    Ok(out)
}
