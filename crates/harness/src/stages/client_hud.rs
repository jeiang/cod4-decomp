// SPDX-License-Identifier: GPL-3.0-only
//! `client-hud`: what the client draws on top of the stock HUD menus during a match, driven through the dev control
//! channel (#378) the way a person plays (#376). The real client starts its own listen server with nine bots and the
//! stage picks a team and a class with the pointer, walks and aims with the keys and the mouse ([`Walker`]), changes
//! class from the in-game menu while alive (the stock `_menus.gsc` answers with a bare-key bold message, which must
//! come out localized), types a chat line, calls a vote and adds bots through the console, puts the night vision
//! goggles on and off with the action slot 1 bind, holds the scoreboard up with the Tab bind (it opens centred on the
//! viewer's row and a wheel notch moves it `cg_scoreboardScrollStep` lines), cooks a grenade with its
//! key (the crosshair's centre dot must grow with the fuse) and throws it, waits for a message window to have a line
//! up, has an enemy bot put beside it kill it (the killcam, through the bot's eyes with the body in sight), comes back
//! from the dead, plays on until it has had a bot in its sights and fired, then holds its sights on an enemy and on a
//! teammate until the crosshair takes each one's team colour.
//! What the HUD got to show over the run (`hud.stats` of the state document) is the verdict: the scoreboard must have
//! listed the players, the message windows must have drawn lines, the script hud elements must
//! have arrived, the goggles must have drawn their overlay and d-pad icon (and the frame with them on must show the
//! overlay over the world with the HUD still above it), the killcam must have played behind the body with its skip
//! prompt over it, a teammate must have had a name over the head and the player under the crosshair a name of their
//! own, and nothing may be on screen as a bare string key. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::{render_problem, steps_problem};
use super::voice_speaker;
use crate::control::{Driven, Walker};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-hud";
/// `--no-autojoin`: the server's team and class menus are the stage's to answer, with the pointer.
const ARGS: &[&str] = &[
    "--listen",
    "--bots",
    "9",
    "--duration",
    "400",
    "--no-autojoin",
];
/// The stock team menu of a listen server, and the item of it that lets the server pick.
const TEAM_MENU: &str = "team_marinesopfor";
const CLASS_MENU: &str = "changeclass";
/// Players once the console has asked for twelve bots.
const PLAYERS: usize = 13;
/// The bold-message window of the feed (`hud::feed::BOLD`).
const BOLD: usize = 1;
/// Seconds after the spawn before a class change says anything: the stock grace period (`scr_war_graceperiod`, 15 s)
/// runs from the match's start, which is before the spawn.
const GRACE_SECS: f64 = 20.0;

/// The night vision step's matched captures, the player held still: goggles off, off again a second later (what the
/// view changes by on its own) and on.
const NV_OFF: &str = "night_vision_off";
const NV_STILL: &str = "night_vision_still";
const NV_ON: &str = "night_vision";
/// A screen region as fractions: left, top, right, bottom.
type Frac = (f32, f32, f32, f32);
/// The overlay's picture is a screen of fine horizontal scanlines (about 7 rows apart in a 720-row capture), so a
/// region it covers gains row-to-row ripple in its row means. The lines darken what is behind them by a share, so
/// the ripple they add grows with the scene's brightness: the gain is measured against the region's mean luma. A
/// half of the screen averages over enough of the scene that a dark corner, a bot walking by or the view's own drift
/// do not hide it (per-cell counts did, #494), and the four halves together still tell a full-screen overlay from
/// one over a quarter of it.
const NV_HALVES: [(&str, Frac); 4] = [
    ("left", (0.0, 0.0, 0.5, 1.0)),
    ("right", (0.5, 0.0, 1.0, 1.0)),
    ("top", (0.0, 0.0, 1.0, 0.5)),
    ("bottom", (0.0, 0.5, 1.0, 1.0)),
];

/// The viewer's place among their team's scoreboard rows, 1 first (`CenterViewOnClient`'s `clientLine`), from the
/// state document's `hud.live`. A spectator has none: the list always opens at the top for them.
fn viewer_line(live: &Value) -> Result<usize, String> {
    let own = live["own"].as_u64();
    let team = live["own_team"].as_u64();
    if team == Some(3) || team.is_none() {
        return Err(format!(
            "the viewer is on no team ({team:?}): nothing to centre on"
        ));
    }
    let rows = live["score_rows"]
        .as_array()
        .filter(|r| !r.is_empty())
        .ok_or("the scoreboard has no rows")?;
    Ok(1 + rows
        .iter()
        .filter(|r| r[1].as_u64() == team)
        .take_while(|r| r[0].as_u64() != own)
        .count())
}

/// A `cg_scoreboardHeight` whose panel holds `view_max` rows under the first banner at the stock item and banner
/// heights: the panel less its 3 + 2 + 14 bottom and 3 + 2 + 24 + 1 + 1 top, the banner, one row and 4 of air, in
/// 18 + 4 steps (`CenterViewOnClient`), mid-step.
fn scoreboard_height(view_max: usize) -> usize {
    19 + 31 + 35 + 18 + 4 + 22 * view_max + 11
}

/// Rows each side of a row whose mean the ripple is measured from.
const NV_RADIUS: usize = 4;
/// The least gain, as a share of the half's mean luma, after twice the ripple's own change between the two
/// goggles-off captures is taken off. Runs with the overlay measured 0.035 to 0.055 in every half.
const NV_MIN_GAIN: f64 = 0.015;
/// The minimap of the compass in the driven client's 1280x720 window (left, top, right, bottom as fractions): HUD,
/// drawn after the overlay (`CG_Draw2D` draws the overlay first), so the lines must not show on it.
const NV_HUD: Frac = (0.01, 0.05, 0.12, 0.24);
/// The most the minimap's ripple may rise with the goggles on.
const NV_HUD_MAX_RISE: f64 = 1.0;

fn luma(p: &[u8]) -> i32 {
    (i32::from(p[0]) * 77 + i32::from(p[1]) * 150 + i32::from(p[2]) * 29) >> 8
}

/// The ripple of a region (the mean distance of each row's mean luma over `x0..x1` from the mean of the
/// `NV_RADIUS` rows on each side) and its mean luma.
fn ripple(img: &[u8], w: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> (f64, f64) {
    let rows: Vec<f64> = (y0..y1)
        .map(|y| {
            let sum: i64 = (x0..x1)
                .map(|x| {
                    let at = ((y * w + x) * 3) as usize;
                    i64::from(luma(&img[at..at + 3]))
                })
                .sum();
            sum as f64 / f64::from((x1 - x0).max(1))
        })
        .collect();
    let span = 2 * NV_RADIUS + 1;
    let away: Vec<f64> = rows
        .windows(span)
        .map(|win| (win[NV_RADIUS] - win.iter().sum::<f64>() / span as f64).abs())
        .collect();
    (
        away.iter().sum::<f64>() / away.len().max(1) as f64,
        rows.iter().sum::<f64>() / rows.len().max(1) as f64,
    )
}

/// What the goggles changed between the matched captures.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NvView {
    /// The smallest scanline gain of the four halves, and which half it was.
    min_gain: f64,
    weakest: &'static str,
    /// How much the minimap's ripple rose with the goggles on.
    hud_rise: f64,
}

/// Compares the RGB pictures `off`, `still` (both goggles off) and `on`, each `w` x `h`.
fn nv_measure(w: u32, h: u32, off: &[u8], still: &[u8], on: &[u8]) -> NvView {
    let at = |f: f32, n: u32| ((f * n as f32) as u32).min(n);
    let rect = |(l, t, r, b): Frac| (at(l, w), at(t, h), at(r, w), at(b, h));
    let mut m = NvView {
        min_gain: f64::INFINITY,
        ..NvView::default()
    };
    for (name, half) in NV_HALVES {
        let r = rect(half);
        let ((a, _), (b, mean), (c, _)) =
            (ripple(off, w, r), ripple(still, w, r), ripple(on, w, r));
        let gain = (c - b - 2.0 * (b - a).abs()) / mean.max(1.0);
        if gain < m.min_gain {
            (m.min_gain, m.weakest) = (gain, name);
        }
    }
    let hud = rect(NV_HUD);
    m.hud_rise = ripple(on, w, hud).0 - ripple(still, w, hud).0;
    m
}

/// What is wrong with the goggles' frame, or `None`.
fn nv_problem(m: &NvView) -> Option<String> {
    let mut bad = Vec::new();
    if m.min_gain < NV_MIN_GAIN {
        bad.push(format!(
            "the goggles' scanlines gained {:.3} of the mean luma in the {} half of the screen (need {NV_MIN_GAIN}): the overlay does not cover it",
            m.min_gain, m.weakest
        ));
    }
    if m.hud_rise > NV_HUD_MAX_RISE {
        bad.push(format!(
            "the minimap's row ripple rose {:.1} with the goggles on (at most {NV_HUD_MAX_RISE}): the overlay is over the HUD",
            m.hud_rise
        ));
    }
    (!bad.is_empty()).then(|| bad.join("; "))
}

/// Decodes the step's three captures from `dir` and measures them.
fn night_vision_view(dir: &std::path::Path) -> Result<NvView, String> {
    let load = |n: &str| decode(&dir.join(format!("{n}.png")));
    let ((w, h, off), (w2, h2, still), (w3, h3, on)) =
        (load(NV_OFF)?, load(NV_STILL)?, load(NV_ON)?);
    if (w, h) != (w2, h2) || (w, h) != (w3, h3) {
        return Err("the captures differ in size".into());
    }
    Ok(nv_measure(w, h, &off, &still, &on))
}

/// How far from the player a target bot is put for the crosshair colour steps, tried in turn. The server picks the
/// spot round the player, along their view first, where the bot stands on floor in plain sight of them
/// (`devtele <bot> near <client> <dist>`); a fixed offset could put it behind a wall or inside geometry.
const CROSSHAIR_REACH: [f64; 3] = [200.0, 150.0, 250.0];
/// How long the walker gets to turn onto one placed bot and hold it past the fade-in.
const CROSSHAIR_SPOT_LIMIT: Duration = Duration::from_secs(25);

/// Puts a live bot of the wanted side next to the player and drives the walker, aimed at that bot alone, until
/// `state` holds (#494). Left to the match, the walker turned to the nearest bot of the side, which with every bot
/// held still (`bot_idle 1`) could stand behind a wall for the whole step. The walker still has to turn onto the
/// bot itself and the HUD has to draw the tint; every spot failing fails the step.
fn hold_on_placed(
    c: &mut Driven,
    w: &mut Walker,
    enemy: bool,
    state: &Value,
    what: &str,
) -> Result<(), String> {
    use super::client_hud_compass::{enemy_bot, exec, server_client};
    let mine = c.lookup("player.team")?.as_u64().unwrap_or(0);
    let team = match (mine, enemy) {
        (1, true) => 2,
        (2, true) => 1,
        (1 | 2, false) => mine,
        _ => return Err(format!("the player is on no team ({mine})")),
    };
    let me = c
        .lookup("player.client")?
        .as_u64()
        .ok_or("the player has no client slot")?;
    let mut tried = Vec::new();
    for reach in CROSSHAIR_REACH {
        let bot = enemy_bot(c, team)?;
        exec(c, &format!("devtele {bot} near {me} {reach}"))?;
        w.target = Some(bot);
        let r = w.drive_until(c, state.clone(), CROSSHAIR_SPOT_LIMIT, what);
        w.target = None;
        match r {
            Ok(_) => return Ok(()),
            Err(e) => {
                // What the client and the server had at the end: the placed bot as the snapshot carried it, the
                // player's aim and crosshair bits, and the bot as the server has it.
                let ents = c.state("entities")?;
                let seen = ents["list"]
                    .as_array()
                    .and_then(|l| {
                        l.iter()
                            .find(|e| e["kind"] == "player" && e["client"].as_u64() == Some(bot))
                    })
                    .map(|e| {
                        json!({"origin": e["origin"], "team": e["team"], "dead": e["dead"],
                            "stance": e["stance"], "actor": e["actor"]})
                    });
                let p = c.state("player")?;
                let server = server_client(c, bot)?;
                tried.push(format!(
                    "bot {bot} at {reach} units: {e}; client sees bot {seen:?}; player origin {} aim {} \
                     crosshair flags {}; server bot origin {} alive {} team {}",
                    p["origin"],
                    p["aim_angles"],
                    p["crosshair_flags"],
                    server["origin"],
                    server["alive"],
                    server["team"]
                ));
            }
        }
    }
    Err(tried.join("; "))
}

/// The crosshair colour steps, once the server has `sv_cheats` on: waits for the client to take it, puts the
/// player in `god` mode, then holds the sights on an enemy, on an enemy with `cg_crosshairEnemyColor 0`, and on a
/// teammate. Each target is a bot placed next to the player ([`hold_on_placed`]) rather than left to the match.
fn crosshair_colours(c: &mut Driven, w: &mut Walker) -> Result<(), String> {
    use super::client_hud_compass::exec;
    exec(c, "set bot_idle 1")?;
    let r = crosshair_colour_steps(c, w);
    exec(c, "set bot_idle 0")?;
    r
}

fn crosshair_colour_steps(c: &mut Driven, w: &mut Walker) -> Result<(), String> {
    c.step("crosshair cheats", |c| {
        let end = Instant::now() + Duration::from_secs(15);
        while c.ask(json!({"cmd": "cvar", "name": "sv_cheats"}))?["value"] != "1" {
            if Instant::now() > end {
                return Err("the client never took sv_cheats 1 from the server".to_owned());
            }
            c.pause(Duration::from_millis(250))?;
        }
        c.ask(json!({"cmd": "exec", "line": "god"})).map(|_| ())
    })?;
    // Held fire keeps an enemy alive under the sights through the fade-in; `god` keeps the player alive.
    w.hold_fire = true;
    c.step("crosshair over an enemy", |c| {
        let theirs = c.lookup("hud.stats.crosshair_team_tints.1")?;
        hold_on_placed(
            c,
            w,
            true,
            &json!({"hud.stats.crosshair_enemy_tints": {"contains": theirs}}),
            "the crosshair never took the enemy team's colour over an enemy",
        )?;
        c.capture("crosshair_enemy").map(|_| ())
    })?;
    // `cg_crosshairEnemyColor 0` keeps the reticle white over an enemy held past the fade-in.
    c.step("crosshair enemy colour off", |c| enemy_colour_off(c, w))?;
    c.step("crosshair over a teammate", |c| {
        let mine = c.lookup("hud.stats.crosshair_team_tints.0")?;
        w.friends = true;
        let r = hold_on_placed(
            c,
            w,
            false,
            &json!({"hud.stats.crosshair_friend_tints": {"contains": mine}}),
            "the crosshair never took the player's team colour over a teammate",
        );
        w.friends = false;
        r?;
        c.capture("crosshair_friend").map(|_| ())
    })
}

/// With `sv_cheats` on at the server: waits for the client to take it, sets `cg_crosshairEnemyColor 0` through the
/// console, then drives onto an enemy in range until the reticle has been due a tint for a second's worth of
/// frames, every one of which must have been white.
fn enemy_colour_off(c: &mut Driven, w: &mut Walker) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let set = c.ask(json!({"cmd": "cvar", "name": "cg_crosshairEnemyColor", "value": "0"}))?;
        if set["value"] == "0" {
            break;
        }
        if Instant::now() > end {
            return Err(format!(
                "cg_crosshairEnemyColor stayed {} after the server turned sv_cheats on",
                set["value"]
            ));
        }
        c.pause(Duration::from_millis(250))?;
    }
    // Both counters from the same state document: two reads would let frames land between them.
    let counts = |c: &mut Driven| -> Result<(u64, u64), String> {
        let s = c.ask(json!({"cmd": "state"}))?;
        let n = |k: &str| s["hud"]["stats"][k].as_u64().unwrap_or(0);
        Ok((
            n("crosshair_enemy_due_frames"),
            n("crosshair_enemy_due_white_frames"),
        ))
    };
    let (due, white) = counts(c)?;
    hold_on_placed(
        c,
        w,
        true,
        &json!({"hud.stats.crosshair_enemy_due_frames": {"gt": due + 60}}),
        "the crosshair was never held on an enemy past the fade-in with cg_crosshairEnemyColor 0",
    )?;
    c.capture("crosshair_enemy_white")?;
    let (due, white) = {
        let (d, wh) = counts(c)?;
        (d - due, wh - white)
    };
    if white != due {
        return Err(format!(
            "with cg_crosshairEnemyColor 0 the crosshair over an enemy was white on {white} of {due} frames"
        ));
    }
    Ok(())
}

/// What is wrong with the HUD of a finished run, or `None`.
pub(crate) fn verdict(report: &Value) -> Option<String> {
    if let Some(w) = steps_problem(report) {
        return Some(w);
    }
    if let Some(w) = render_problem(report) {
        return Some(w);
    }
    let h = &report["hud_draw"];
    let n = |k: &str| h[k].as_u64().unwrap_or(0);
    let sum = |k: &str| {
        h[k].as_array()
            .map_or(0, |a| a.iter().filter_map(Value::as_u64).sum::<u64>())
    };
    let mut bad = Vec::new();
    if n("elems_max") == 0 {
        bad.push("no script hud elements arrived");
    }
    if n("scoreboard_frames") == 0 || n("scoreboard_rows_max") < 4 {
        bad.push("the scoreboard listed fewer than 4 players");
    }
    if sum("messages") + n("obituaries") == 0 {
        bad.push("no print or kill line arrived");
    }
    if n("chat") == 0 {
        bad.push("the chat line the player typed never came back");
    }
    if sum("window_lines") == 0 {
        bad.push("the message windows drew no line");
    }
    if n("killcam_frames") == 0 {
        bad.push("no killcam frame");
    }
    // The dead see their body from behind, and the picture is blurred by depth of field at some point (the sights'
    // blur, or the dead view's, which eases to a focus; the stock spawn call of `setDepthOfField` asks for nothing).
    // A killcam of the kill needs the death first.
    let view = &report["net"]["view"];
    let v = |k: &str| view[k].as_u64().unwrap_or(0);
    if n("killcam_frames") > 0 && v("death_view_frames") == 0 {
        bad.push("the player died but the camera never went behind the body");
    }
    if v("death_view_frames") > 0 && view["death_view_range_max"].as_f64().unwrap_or(0.0) < 8.0 {
        bad.push("the death camera hung at the head instead of behind it");
    }
    if v("dof_frames") == 0 {
        bad.push("depth of field never blurred a frame");
    }
    // A name is held to account: the match is 400 s of team deathmatch with nine bots, so a teammate stood in
    // sight and a player stood under the crosshair at some point; a run in which neither happened exercised
    // neither check and proves nothing (#375).
    if n("friends_in_sight_frames") == 0 {
        bad.push("no teammate was ever in sight, so the overhead names were never checked");
    } else if n("names_max") == 0 {
        bad.push("a teammate was in sight but no overhead name was drawn");
    }
    if n("crosshair_due_frames") == 0 {
        bad.push("the crosshair was never on a player in range, so none was ever named");
    } else if n("crosshair_name_frames") == 0 {
        bad.push("the crosshair was on a player but never named them");
    }
    // The crosshair's tint (#414): white, or the colour of the team of the player it is on, never another.
    fn strs(v: &Value) -> Vec<&str> {
        v.as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }
    let team = strs(&h["crosshair_team_tints"]);
    let only = |k: &str, i: usize| {
        strs(&h[k])
            .iter()
            .all(|t| *t == "1.000 1.000 1.000" || team.get(i) == Some(t))
    };
    if !only("crosshair_friend_tints", 0) {
        bad.push("the crosshair over a teammate was drawn in a colour other than white or the player's team's");
    }
    if !only("crosshair_enemy_tints", 1) {
        bad.push("the crosshair over an enemy was drawn in a colour other than white or the enemy team's");
    }
    if n("crosshair_stray_tint_frames") > 0 {
        bad.push("the crosshair was tinted with no player under it");
    }
    // The killcam shows the viewer's own body, which carries the "you" marker over its head.
    let marked = h["head_icon_materials"]
        .as_array()
        .is_some_and(|m| m.iter().any(|v| v == "headiconyouinkillcam"));
    if n("killcam_frames") > 0 && n("killcam_you_frames") == 0 {
        bad.push("the killcam never had the viewer's body in sight, so the \"you\" marker was never checked");
    } else if n("killcam_you_frames") > 0 && !marked {
        bad.push("the killcam drew no head icon over the viewer's body");
    }
    // A killcam shows the watcher's own live elements beside the killer's archived ones (#426): the stock
    // `_killcam.gsc` skip prompt (a `PLATFORM_PRESS_TO_...` key) must have been drawn over the replay.
    let prompt = h["killcam_live_text"].as_array().is_some_and(|t| {
        t.iter()
            .filter_map(Value::as_str)
            .any(|t| t.contains("PRESS_TO"))
    });
    if n("killcam_frames") > 0 && !prompt {
        bad.push("the killcam's skip prompt was never drawn over the replay");
    }
    // The goggles on (#427): their overlay over the screen and their icon in the d-pad slot.
    if n("night_vision_frames") == 0 {
        bad.push("the night vision goggles' overlay was never drawn");
    }
    if report["hud"]["night_vision_icon_frames"]
        .as_u64()
        .unwrap_or(0)
        == 0
    {
        bad.push("the d-pad never drew the night vision icon");
    }
    let overlay_missing = report["missing_images"]
        .as_array()
        .is_some_and(|m| m.iter().any(|v| v == "nightvision_overlay_goggles"));
    if overlay_missing {
        bad.push("the night vision overlay's picture could not be loaded");
    }
    // A message or hint that is still a string key (MP_CHANGE_CLASS_NEXT_SPAWN) was not localized.
    let keys: Vec<&str> = [&h["unresolved"], &report["hud"]["unresolved_text"]]
        .into_iter()
        .filter_map(|v| v.as_array())
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let unresolved = format!("unlocalized text on screen: {}", keys.join(", "));
    if !keys.is_empty() {
        bad.push(&unresolved);
    }
    // The grenade and d-pad icons are weapon materials; one that is missing draws as a white square.
    let icons: Vec<&str> = report["missing_images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|n| n.starts_with("hud_"))
        .collect();
    let missing = format!("missing HUD icons: {}", icons.join(", "));
    if !icons.is_empty() {
        bad.push(&missing);
    }
    (!bad.is_empty()).then(|| bad.join(", "))
}

/// The run's report in the shape the verdict reads: the steps, the HUD's counters as `hud_draw`, what the HUD shows
/// as `hud`, the view's counters under `net`, and the images the UI could not find.
fn assemble(steps: &[Value], hud: Value, net: &Value, render: &Value) -> Value {
    json!({
        "steps": steps,
        "hud_draw": hud["stats"],
        "hud": hud["report"],
        "missing_images": hud["missing_images"],
        "net": {"view": net["view"]},
        "render": render,
    })
}

/// A count of the HUD's `stats` section, read now.
fn stat(c: &mut Driven, path: &str) -> Result<u64, String> {
    Ok(c.lookup(path)?.as_u64().unwrap_or(0))
}

/// The numbers of a look block of the client's `look` section (`glow.radius`, `film.tint_dark.2`, ...), by name.
fn look_numbers(v: &Value) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for part in ["glow", "film"] {
        for (k, x) in v[part].as_object().into_iter().flatten() {
            match x {
                Value::Number(n) => out.push((format!("{part}.{k}"), n.as_f64().unwrap_or(0.0))),
                Value::Array(a) => {
                    for (i, e) in a.iter().enumerate() {
                        out.extend(e.as_f64().map(|f| (format!("{part}.{k}.{i}"), f)));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The switches of a look block: glow on, film on, film inverted.
fn look_switches(v: &Value) -> [&Value; 3] {
    [
        &v["glow"]["enabled"],
        &v["film"]["enabled"],
        &v["film"]["invert"],
    ]
}

/// One sample of a vision blend (the client's `look` section): `None` once the blend is over, else its fraction.
/// The number that moves most must sit where `sin(fraction * pi/2)` puts it (`LerpFloat` with
/// `VISIONSETLERP_TO_SMOOTH`), and the switches must already be the destination's (`LerpBool`). The second value is
/// whether this sample tells the eased curve from a straight line.
fn vision_blend_sample(look: &Value) -> Result<Option<(f64, bool)>, String> {
    let b = &look["blend"];
    let ms = b["ms"].as_f64().unwrap_or(0.0);
    let f = (look["time"].as_f64().unwrap_or(0.0) - b["start_ms"].as_f64().unwrap_or(0.0))
        / ms.max(1.0);
    if ms <= 0.0 || f > 1.0 {
        return Ok(None);
    }
    let f = f.max(0.0);
    if look_switches(&look["applied"]) != look_switches(&b["to"]) {
        return Err(format!(
            "{:.0}% into the vision blend the glow/film switches are {:?}, not the destination's {:?}",
            f * 100.0,
            look_switches(&look["applied"]),
            look_switches(&b["to"])
        ));
    }
    let (from, to, now) = (
        look_numbers(&b["from"]),
        look_numbers(&b["to"]),
        look_numbers(&look["applied"]),
    );
    let Some(((name, a), (_, z))) = from
        .iter()
        .zip(&to)
        .max_by(|(x, y), (u, v)| (y.1 - x.1).abs().total_cmp(&(v.1 - u.1).abs()))
    else {
        return Err(format!("the look section has no numbers: {look}"));
    };
    let span = z - a;
    if span.abs() < 0.05 {
        return Err(format!(
            "the vision blend moves nothing measurable (the most is {name} by {span})"
        ));
    }
    let got = now
        .iter()
        .find(|(n, _)| n == name)
        .map_or(f64::NAN, |(_, v)| *v);
    let eased = a + span * (f * std::f64::consts::FRAC_PI_2).sin();
    let tol = span.abs() * 2e-3 + 1e-4;
    if (got - eased).abs() > tol {
        return Err(format!(
            "{:.0}% into the vision blend {name} is {got}, not the sin-eased {eased} (linear would be {})",
            f * 100.0,
            a + span * f
        ));
    }
    Ok(Some((
        f,
        (0.2..0.8).contains(&f) && (eased - (a + span * f)).abs() > 4.0 * tol,
    )))
}

/// Observe an actual server tick atomically. Collecting UDP traffic briefly before the tick creates a backlog even
/// when the renderer is slower than `sv_fps`; 150 ms at the 125 Hz send cap stays under the server's 24-command take
/// per frame and its 64-command queue, so the latest received command is the latest one sent.
fn command_frame(c: &mut Driven, slot: u64, empty: bool, right: bool) -> Result<Value, String> {
    let start = Instant::now();
    let frames = |c: &mut Driven| -> Result<u64, String> {
        Ok(c.ask(json!({"cmd": "ping"}))?["frames"]
            .as_u64()
            .unwrap_or(0))
    };
    let first = frames(c)?;
    let mut last = Value::Null;
    while start.elapsed() < Duration::from_secs(30) {
        let state = c.ask(json!({
            "cmd": "server", "op": "command_frame", "collect_ms": if empty { 0 } else { 150 }
        }))?;
        let player = state["clients"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["slot"].as_u64() == Some(slot)))
            .ok_or_else(|| format!("player {slot} missing from server state: {state}"))?;
        let f = &player["command_frame"];
        last = f.clone();
        let received = f["received"].as_u64().unwrap_or(0);
        let selected = &f["selected"];
        let moving = if right {
            selected["rightmove"].as_i64().unwrap_or(0) > 0
                && selected["forwardmove"] == 0
                && selected["buttons"].as_i64().unwrap_or(0) != 0
        } else {
            selected["forwardmove"].as_i64().unwrap_or(0) > 0 && selected["rightmove"] == 0
        };
        if f["synchronous"] == true && moving && if empty { received == 0 } else { received >= 2 } {
            let input = if empty {
                &f["retained"]
            } else {
                &f["latest_received"]
            };
            let mut expected = input.clone();
            expected["server_time"] = f["frame_time"].clone();
            if selected != &expected {
                return Err(format!("sync frame selected the wrong input: {f}"));
            }
            if f["simulated_time"] != f["frame_time"]
                || f["frame_time"].as_i64().unwrap_or(0) <= f["previous_time"].as_i64().unwrap_or(0)
            {
                return Err(format!(
                    "sync frame did not advance simulation at level time: {f}"
                ));
            }
            return Ok(f.clone());
        }
        c.pause(Duration::from_millis(50))?;
    }
    let drawn = frames(c)?.saturating_sub(first);
    Err(format!(
        "no matching sync {} frame (right={right}; the client drew {drawn} frames in the wait): {last}",
        if empty { "empty" } else { "backlog" }
    ))
}

/// Frames the client drew per second over `window`, by its own frame counter.
fn frame_rate(c: &mut Driven, window: Duration) -> Result<f64, String> {
    let count = |c: &mut Driven| {
        c.lookup("client.frames")?
            .as_u64()
            .ok_or_else(|| "no client.frames".to_owned())
    };
    let (f0, t0) = (count(c)?, Instant::now());
    c.pause(window)?;
    Ok(count(c)?.saturating_sub(f0) as f64 / t0.elapsed().as_secs_f64())
}

/// The first rate over `window` of at least `want` frames a second, measured again until `limit` has passed (`Ok`), or
/// the best rate seen (`Err`). A busy host (another slot's release build) slows every frame for a while, whatever the
/// present mode: a slot run at a few frames a second drew as few with `Immediate` as with `Fifo` (#499).
fn reach_frame_rate(
    c: &mut Driven,
    want: f64,
    window: Duration,
    limit: Duration,
) -> Result<Result<f64, f64>, String> {
    let start = Instant::now();
    let mut best = 0.0f64;
    loop {
        let rate = frame_rate(c, window)?;
        if rate >= want {
            return Ok(Ok(rate));
        }
        best = best.max(rate);
        if start.elapsed() >= limit {
            return Ok(Err(best));
        }
    }
}

/// Exercise queued and retained player input through the real UDP path, independent of rendering throughput. The
/// step's `vid_restart` back to the player's `r_vsync` must give the frame rate back too, as stock `vid_restart` does
/// (#499): at least half the rate measured before the step, within 15 s.
fn synchronous_commands(c: &mut Driven, notes: &mut Vec<String>) -> Result<(), String> {
    let fps = c.ask(json!({"cmd": "cvar", "name": "com_maxfps"}))?["value"].clone();
    let vsync = c.ask(json!({"cmd": "cvar", "name": "r_vsync"}))?["value"].clone();
    let before = frame_rate(c, Duration::from_secs(1))?;
    let sync =
        c.ask(json!({"cmd": "server", "op": "cvar", "name": "g_synchronousClients"}))?["value"]
            .clone();
    let result = (|| {
        let slot = c
            .lookup("player.client")?
            .as_u64()
            .ok_or("no player slot")?;
        // Uncap sending while collecting the backlog; this does not assume the GPU can exceed the server tick rate.
        c.ask(json!({"cmd": "cvar", "name": "r_vsync", "value": "0"}))?;
        c.console("vid_restart")?;
        c.ask(json!({"cmd": "cvar", "name": "com_maxfps", "value": "125"}))?;
        c.ask(
            json!({"cmd": "server", "op": "cvar", "name": "g_synchronousClients", "value": "1"}),
        )?;
        c.bind("+forward", "down")?;
        let queued = command_frame(c, slot, false, false)?;
        notes.push(format!("synchronous latest queued input: {queued}"));
        c.bind("+forward", "up")?;
        c.bind("+moveright", "down")?;
        c.bind("+gostand", "down")?;
        let changed = command_frame(c, slot, false, true)?;
        if changed["selected"]["buttons"] == queued["selected"]["buttons"] {
            return Err(format!(
                "changed movement never changed the selected buttons: {changed}"
            ));
        }
        c.ask(json!({"cmd": "cvar", "name": "com_maxfps", "value": "2"}))?;
        let retained = command_frame(c, slot, true, true)?;
        notes.push(format!(
            "synchronous retained input at next frame time: {retained}"
        ));
        let repeated = command_frame(c, slot, true, true)?;
        notes.push(format!(
            "synchronous retained input repeated again: {repeated}"
        ));
        Ok(())
    })();
    // Restore the player's settings and release every held input even when an assertion failed.
    c.bind("+forward", "up")?;
    c.bind("+moveright", "up")?;
    c.bind("+gostand", "up")?;
    c.ask(json!({"cmd": "cvar", "name": "com_maxfps", "value": fps}))?;
    c.ask(json!({"cmd": "cvar", "name": "r_vsync", "value": vsync}))?;
    c.console("vid_restart")?;
    c.ask(json!({"cmd": "server", "op": "cvar", "name": "g_synchronousClients", "value": sync}))?;
    result?;
    let vsync = vsync.as_str().unwrap_or("?");
    match reach_frame_rate(
        c,
        before / 2.0,
        Duration::from_secs(1),
        Duration::from_secs(15),
    )? {
        Ok(after) => {
            notes.push(format!(
                "frame rate around the synchronous step: {before:.0} a second before, {after:.0} after vid_restart \
                 put r_vsync {vsync} back"
            ));
            Ok(())
        }
        Err(best) => Err(format!(
            "after vid_restart put r_vsync {vsync} back the client drew at most {best:.1} frames a second in 15 s, \
             against {before:.1} before the step"
        )),
    }
}

/// The record meter's white outline (`UI_DrawRecordLevel`, KisakCOD `ui_mp/ui_main_mp.cpp:1524-1533`) must be on
/// screen at the rectangle the meter last drew: most of its top and bottom edges near-white.
fn meter_outline(shot: &Path, rect: &Value) -> Result<(), String> {
    let r: Vec<f64> = rect
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let [x, y, w, h] = r[..] else {
        return Err(format!("the record meter reported no rectangle: {rect}"));
    };
    if w < 4.0 || h < 4.0 {
        return Err(format!(
            "the record meter's rectangle is degenerate: {rect}"
        ));
    }
    let (width, height, rgb) = super::client_models::decode(shot)?;
    let white = |px: u32, py: u32| {
        let i = ((py * width + px) * 3) as usize;
        rgb[i..i + 3].iter().all(|&c| c >= 200)
    };
    // The share of white pixels on the best row near an edge (an edge is one 640-unit, a pixel or two).
    let edge = |ey: f64| -> f64 {
        let (x0, x1) = (x.max(0.0) as u32, ((x + w) as u32).min(width));
        let rows = (ey - 1.0).max(0.0) as u32..((ey + 2.0) as u32).min(height);
        rows.map(|py| {
            let lit = (x0..x1).filter(|&px| white(px, py)).count();
            lit as f64 / f64::from(x1.saturating_sub(x0).max(1))
        })
        .fold(0.0, f64::max)
    };
    let (top, bottom) = (edge(y), edge(y + h - 1.0));
    if top < 0.8 || bottom < 0.8 {
        return Err(format!(
            "the record meter's outline is not on screen at {rect}: top edge {top:.2} white, bottom {bottom:.2}"
        ));
    }
    Ok(())
}

/// The crosshair opacity seen per predicted weapon state during one keyed action.
fn sample_crosshair(c: &mut Driven, until: Duration) -> Result<Vec<(u64, f64)>, String> {
    let end = Instant::now() + until;
    let mut seen = Vec::new();
    while Instant::now() < end {
        let p = c.state("player")?;
        if p["alive"] != true {
            return Err("the player died while checking the crosshair".into());
        }
        if let (Some(s), Some(a)) = (
            p["visual_weapon_state"].as_u64(),
            p["crosshair_alpha"].as_f64(),
        ) {
            seen.push((s, a));
        }
        c.frames(1)?;
    }
    Ok(seen)
}

/// Per weapon state, the drawn frames and those with a visible crosshair (`player.crosshair_state_frames`).
fn state_frames(c: &mut Driven) -> Result<Vec<[u64; 2]>, String> {
    let p = c.state("player")?;
    let rows = p["crosshair_state_frames"]
        .as_array()
        .ok_or("no player.crosshair_state_frames")?;
    Ok(rows
        .iter()
        .map(|r| [0, 1].map(|i| r[i].as_u64().unwrap_or(0)))
        .collect())
}

/// The crosshair opacity per predicted weapon state while one `weapnext` switches the weapon. It comes from one
/// wheel notch (`mwheelup` in `extra_binds.cfg`, not a key `bind` can press), as a person's one notch must do it.
/// A switch to a pistol drops and raises quickly, as stock does (KisakCOD MP 1.8 `bgame/bg_weapons.cpp:2491-2587`,
/// `PM_BeginWeaponChange`: `quickDropTime` for `WEAPCLASS_PISTOL`): about half a second in all, short enough for a
/// polling gap on a busy host to miss (#494). So besides the polled samples it diffs the client's per-frame counts
/// of the drop and raise states (1-4), which see every drawn frame. Also returns the trace of every polled sample
/// whose facts changed (ms since the notch, the snapshot's `weapon_index` and `weapon_state`, the predicted
/// `visual_weapon_state` and `crosshair_alpha`) and the counted frames per state. At a few frames a second the whole
/// switch falls inside one frame's prediction and is never drawn, so the notch waits (up to 30 s) for the client to
/// draw twenty a second, about ten frames of the switch: a busy host slows every frame for a while (#499).
fn sample_weapon_switch(
    c: &mut Driven,
    until: Duration,
) -> Result<(Vec<(u64, f64)>, String), String> {
    let rate = match reach_frame_rate(c, 20.0, Duration::from_millis(500), Duration::from_secs(30))?
    {
        Ok(rate) => rate,
        Err(best) => {
            return Err(format!(
                "the client drew at most {best:.1} frames a second for 30 s, too few to draw a half-second weapon \
                 switch"
            ));
        }
    };
    let counted = state_frames(c)?;
    let start = Instant::now();
    c.ask(json!({"cmd": "wheel", "y": 1.0}))?;
    let (mut seen, mut trace, mut last, mut samples) = (Vec::new(), Vec::new(), None, 0);
    while start.elapsed() < until {
        let p = c.state("player")?;
        let ms = start.elapsed().as_millis();
        if p["alive"] != true {
            return Err("the player died while checking the crosshair".into());
        }
        samples += 1;
        let facts = (
            p["weapon_index"].clone(),
            p["weapon_state"].clone(),
            p["visual_weapon_state"].clone(),
            p["crosshair_alpha"].clone(),
        );
        if last.as_ref() != Some(&facts) {
            trace.push(format!(
                "{ms} ms: weapon {} state {} visual {} alpha {}",
                facts.0, facts.1, facts.2, facts.3
            ));
            last = Some(facts);
        }
        if let (Some(s), Some(a)) = (
            p["visual_weapon_state"].as_u64(),
            p["crosshair_alpha"].as_f64(),
        ) {
            seen.push((s, a));
        }
        c.frames(1)?;
    }
    let (mut frames, mut total) = (Vec::new(), 0);
    for (s, (now, was)) in state_frames(c)?.iter().zip(&counted).enumerate() {
        let (all, shown) = (now[0].saturating_sub(was[0]), now[1].saturating_sub(was[1]));
        if all == 0 {
            continue;
        }
        total += all;
        frames.push(format!("state {s}: {all} frames, {shown} with a crosshair"));
        // A counted drop or raise frame stands for its state with the crosshair shown (1) or hidden (0).
        if (1..=4).contains(&s) {
            if shown > 0 {
                seen.push((s as u64, 1.0));
            }
            if shown < all {
                seen.push((s as u64, 0.0));
            }
        }
    }
    // At a few frames a second the whole quick switch can fall inside one frame's prediction and never be drawn;
    // the rate is in the trace so such a failure reads as the client's frame pacing, not the crosshair.
    Ok((
        seen,
        format!(
            "{samples} samples at {rate:.0} frames a second before the notch: {}; {total} frames drawn in {} ms: {}",
            trace.join(", "),
            start.elapsed().as_millis(),
            frames.join(", ")
        ),
    ))
}

/// What is wrong with crosshair samples: hidden states must be transparent, ready and firing visible.
fn crosshair_problem(seen: &[(u64, f64)], hidden: &[u64], label: &str) -> Option<String> {
    if let Some((s, a)) = seen.iter().find(|(s, a)| hidden.contains(s) && *a > 0.0) {
        return Some(format!("{label}: crosshair alpha {a} in weapon state {s}"));
    }
    if !seen.iter().any(|(s, _)| hidden.contains(s)) {
        return Some(format!("{label}: no frame in weapon states {hidden:?}"));
    }
    if let Some((s, a)) = seen.iter().find(|(s, a)| matches!(s, 0 | 5) && *a <= 0.0) {
        return Some(format!(
            "{label}: crosshair hidden in weapon state {s} ({a})"
        ));
    }
    None
}

/// #413 / KisakCOD `cgame/cg_draw_reticles.cpp:601-627`: reload, weapon raise/drop and melee hide the crosshair;
/// ready and firing keep it. Returns the weapon switch's trace for the notes.
fn crosshair_states(c: &mut Driven) -> Result<String, String> {
    let secs = Duration::from_secs;
    c.bind("+attack", "down")?;
    let fire = sample_crosshair(c, Duration::from_millis(400));
    c.bind("+attack", "up")?;
    let fire = fire?;
    if !fire.iter().any(|(s, a)| *s == 5 && *a > 0.0) {
        return Err(format!("firing showed no crosshair: {fire:?}"));
    }
    c.until(
        json!({"player.visual_weapon_state": 0}),
        secs(5),
        "the weapon did not settle after firing",
    )?;
    let mut switch = String::new();
    for (bind, hidden, label) in [
        ("+reload", &[7][..], "reload"),
        ("weapnext", &[1, 2, 3, 4][..], "raise/drop"),
        ("+melee", &[12, 13, 14][..], "melee"),
    ] {
        let seen = if bind == "weapnext" {
            let (seen, trace) = sample_weapon_switch(c, secs(4))?;
            switch = trace;
            seen
        } else {
            c.bind(bind, "tap")?;
            sample_crosshair(c, secs(4))?
        };
        if let Some(why) = crosshair_problem(&seen, hidden, label) {
            return Err(if bind == "weapnext" {
                format!("{why} ({switch})")
            } else {
                why
            });
        }
        c.until(
            json!({"player.visual_weapon_state": 0, "player.crosshair_alpha": {"gt": 0.0}}),
            secs(10),
            &format!("the crosshair did not come back after {label}"),
        )?;
    }
    Ok(format!("crosshair weapnext switch: {switch}"))
}

/// How far from the living player an enemy bot is put for the killcam's kill, tried in turn: near enough that the
/// bot sees and turns to them, on the spot round them where the two see each other (`devtele <bot> near`).
const KILLCAM_REACH: [f64; 3] = [120.0, 160.0, 200.0];
/// Time the bot is given to see the player and turn to them: its reaction is at most 600 ms and it turns at least
/// 420 degrees a second (`server/src/bot.rs`).
const KILLCAM_AIM: Duration = Duration::from_secs(2);

/// Has an enemy bot kill the player in front of it, so the killcam that follows replays the kill through eyes that
/// have the body in sight, and returns while that killcam is up with the viewer's body seen. Left to the match, a
/// bot may not find the player before it ends (#494: no kill in 300 s). The bot is put near the player where the
/// two see each other, with its own play on (`bot_idle 0`), so it turns to them, and then kills them with its
/// rifle (`devkill`, as `net-killcam` and `web-match` do; a kill it makes first does as well). The stock
/// `_killcam.gsc` plays the killcam. When it never showed the body, the next distance is tried after the respawn,
/// and when no killcam did, the step fails.
fn killcam_kill(c: &mut Driven) -> Result<(), String> {
    use super::client_hud_compass::{enemy_bot, exec, server_client};
    let secs = Duration::from_secs;
    let enemy = match c.lookup("player.team")?.as_u64() {
        Some(1) => 2,
        Some(2) => 1,
        t => return Err(format!("the player is on no team ({t:?})")),
    };
    let me = c
        .lookup("player.client")?
        .as_u64()
        .ok_or("the player has no client slot")?;
    exec(c, "set bot_idle 0")?;
    let mut tried = Vec::new();
    for reach in KILLCAM_REACH {
        c.until(
            json!({"player.alive": true, "hud.live.killcam": false, "hud.live.dead": false}),
            secs(60),
            "the player was not alive to be killed",
        )?;
        let you0 = stat(c, "hud.stats.killcam_you_frames")?;
        let bot = enemy_bot(c, enemy)?;
        exec(c, &format!("devtele {bot} near {me} {reach}"))?;
        c.pause(KILLCAM_AIM)?;
        // Where the bot stood and looked when it killed, against the player: a failure says whether it faced them.
        let (b, p) = (server_client(c, bot)?, server_client(c, me)?);
        let seen = format!(
            "bot origin {} view {} alive {} session {}, player origin {}",
            b["origin"], b["viewangles"], b["alive"], b["session"], p["origin"]
        );
        if p["alive"] == json!(true) {
            exec(c, &format!("devkill {me} {bot}"))?;
        }
        c.until(
            json!({"hud.live.killcam": true}),
            secs(30),
            "no killcam started after the bot's kill",
        )?;
        // Up while the replay plays: done once it has the body in sight, the next side once it is over without.
        let end = Instant::now() + secs(30);
        loop {
            let hud = c.state("hud")?;
            if hud["stats"]["killcam_you_frames"].as_u64().unwrap_or(0) > you0 {
                return Ok(());
            }
            if hud["live"]["killcam"] != json!(true) || Instant::now() > end {
                tried.push(format!("bot {bot} at {reach} units [{seen}]"));
                break;
            }
            c.frames(1)?;
        }
    }
    Err(format!(
        "no killcam showed the viewer's body for the you marker; killed by {}",
        tried.join(", ")
    ))
}

/// A second person joins the listen server and talks ([`voice_speaker::Speaker`]); the server relays the voice and the
/// cgame talker slots (193-196) must name them, then `cg_drawTalk` given by its stock names must hide and show them by
/// side (`CG_DrawTalkerNum`, KisakCOD `cgame_mp/cg_newDraw_mp.cpp:2287`).
fn received_voice(c: &mut Driven, install: &Path, notes: &mut Vec<String>) -> Result<(), String> {
    let secs = Duration::from_secs;
    let server = |c: &mut Driven, name: &str, value: &str| {
        c.ask(json!({"cmd":"server","op":"cvar","name":name,"value":value}))
            .map(|_| ())
    };
    // Everyone hears everyone, dead or alive, so the speaker's team and a death do not decide the check.
    for (name, value) in [
        ("sv_voice", "1"),
        ("voice_global", "1"),
        ("voice_deadChat", "1"),
    ] {
        server(c, name, value)?;
    }
    let addr = c.ask(json!({"cmd":"server","op":"state","what":"net"}))?["addr"]
        .as_str()
        .and_then(|a| a.parse::<std::net::SocketAddr>().ok())
        .ok_or("the listen server did not say its address")?;
    let speaker = voice_speaker::Speaker::start(
        std::net::SocketAddr::from(([127, 0, 0, 1], addr.port())),
        install,
    )?;
    let result = (|| {
        let start = Instant::now();
        let client = loop {
            if let Some(n) = speaker.client() {
                break n;
            }
            if start.elapsed() > secs(60) {
                return Err("the speaker never got a body on the listen server".to_owned());
            }
            c.pause(Duration::from_millis(500))?;
        };
        c.ask(json!({"cmd":"cvar","name":"cg_drawTalk","value":"ALL"}))?;
        c.until(
            json!({"hud.report.talker_marks":{"ge":1}}),
            secs(20),
            "a talking player's ownerdraw (193) never drew",
        )?;
        let talkers = c.lookup("hud.report.talkers")?;
        let me = talkers
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t[0].as_u64() == Some(u64::from(client)))
            .cloned()
            .ok_or_else(|| {
                format!("the talkers {talkers} do not list the speaker, client {client}")
            })?;
        if me[1].as_str() != Some(voice_speaker::NAME) {
            return Err(format!(
                "the talker is named {} instead of {}",
                me[1],
                voice_speaker::NAME
            ));
        }
        let enemy = me[2].as_bool().unwrap_or(false);
        notes.push(format!("speaker client {client}, enemy {enemy}"));
        let (hide, show) = if enemy {
            ("FRIENDLY", "ENEMY")
        } else {
            ("ENEMY", "FRIENDLY")
        };
        let marks = |c: &mut Driven| -> Result<u64, String> {
            Ok(c.lookup("hud.report.talker_marks")?.as_u64().unwrap_or(0))
        };
        c.ask(json!({"cmd":"cvar","name":"cg_drawTalk","value":hide}))?;
        c.pause(Duration::from_millis(500))?;
        let before = marks(c)?;
        c.pause(secs(2))?;
        let after = marks(c)?;
        let still = c.lookup("hud.report.talkers")?;
        if still.as_array().is_none_or(Vec::is_empty) {
            return Err("the speaker stopped being heard while cg_drawTalk hid it".to_owned());
        }
        if after != before {
            return Err(format!(
                "cg_drawTalk {hide} still drew the {} talker ({before} -> {after} icons)",
                if enemy { "enemy" } else { "friendly" }
            ));
        }
        c.ask(json!({"cmd":"cvar","name":"cg_drawTalk","value":show}))?;
        c.until(
            json!({"hud.report.talker_marks":{"gt":after}}),
            secs(10),
            &format!("cg_drawTalk {show} did not draw the talker"),
        )?;
        Ok(())
    })();
    drop(speaker);
    c.ask(json!({"cmd":"cvar","name":"cg_drawTalk","value":"ALL"}))?;
    for (name, value) in [
        ("sv_voice", "0"),
        ("voice_global", "0"),
        ("voice_deadChat", "0"),
    ] {
        server(c, name, value)?;
    }
    result
}
/// The match, step by step. Every step goes through the channel: a failure names the step and what was observed.
fn play(
    c: &mut Driven,
    w: &mut Walker,
    install: &Path,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let secs = Duration::from_secs;
    // The server's team menu, answered with the pointer; then the class menu.
    c.step("team menu", |c| {
        c.until(
            json!({"menu.open": {"contains": TEAM_MENU}}),
            secs(90),
            "the team menu did not open",
        )?;
        c.click("auto_assign")?;
        c.until(
            json!({"menu.open": {"contains": CLASS_MENU}}),
            secs(30),
            "the class menu did not open after the team was picked",
        )?;
        Ok(())
    })?;
    c.step("class menu", |c| {
        c.click("Assault")?;
        c.until(
            json!({"menu.open": {"len": 0}}),
            secs(10),
            "the class menu did not close after the class was picked",
        )
    })?;
    let spawned_at = c.step("spawn", |c| {
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            secs(120),
            "the player did not spawn",
        )?;
        Ok(c.lookup("client.seconds")?.as_f64().unwrap_or(0.0))
    })?;
    c.pause(secs(3))?;
    c.step("voice talking ownerdraw", |c| {
        c.ask(json!({"cmd":"server","op":"cvar","name":"sv_voice","value":"1"}))?;
        c.bind("+talk", "down")?;
        let result = c.until(
            json!({"hud.report.local_talker_marks":{"ge":1}}),
            secs(10),
            "the speaking player's ownerdraw 266 never drew",
        );
        c.bind("+talk", "up")?;
        result?;
        c.ask(json!({"cmd":"server","op":"cvar","name":"sv_voice","value":"0"}))?;
        Ok(())
    })?;
    let cvars_before = c.ask(json!({"cmd": "cvars"}))?;
    c.step("received voice talker ownerdraws", |c| {
        received_voice(c, install, notes)
    })?;
    c.step("record level ownerdraw", |c| {
        // UI_DrawRecordLevel in the stock voice options. With the slot's test-tone microphone (#488, half-scale sine:
        // a mean sample size of about 0.32 of full scale) the meter must rise; without one only its drawing is checked.
        let mic = std::env::var_os("COD4E_TEST_MIC").is_some();
        let before = c.lookup("menu.open")?;
        c.console("openmenu options_voice")?;
        let result = c.until(
            json!({"hud.report.record_meter_marks":{"ge":1}}),
            secs(10),
            "the voice options never drew the record level (265)",
        );
        let result = result.and_then(|_| {
            if !mic {
                notes.push(
                    "record level untested: no test microphone (COD4E_TEST_MIC unset; #488)".into(),
                );
                return Ok(());
            }
            c.until(
                json!({"hud.report.record_level":{"ge":0.1}}),
                secs(10),
                "the record level stayed under 0.1 with the test tone on the microphone",
            )?;
            notes.push(format!(
                "record level {}",
                c.lookup("hud.report.record_level")?
            ));
            Ok(())
        });
        let result = result.and_then(|_| {
            let shot = c.capture("record-level")?;
            let rect = c.lookup("hud.report.record_meter_rect")?;
            notes.push(format!("record meter rect {rect}"));
            meter_outline(&shot, &rect)
        });
        if result.is_err() {
            notes.push(format!("open menus: {}", c.lookup("menu.open")?));
        }
        c.console("closemenu options_voice")?;
        result?;
        // The match's menus must be as they were, or the steps after play with a menu up.
        c.pause(Duration::from_millis(500))?;
        let after = c.lookup("menu.open")?;
        if after != before {
            return Err(format!(
                "closing the voice options left {after} open, not {before}"
            ));
        }
        Ok(())
    })?;
    // What the voice steps (the speaker, the voice options' scripts) left changed in the client's settings.
    let cvars_after = c.ask(json!({"cmd": "cvars"}))?;
    if let (Some(a), Some(b)) = (cvars_before.as_object(), cvars_after.as_object()) {
        let changed: Vec<String> = b
            .iter()
            .filter(|(k, v)| !matches!(k.as_str(), "ok" | "id") && a.get(*k) != Some(*v))
            .map(|(k, v)| format!("{k}={v} (was {})", a.get(k).unwrap_or(&Value::Null)))
            .collect();
        notes.push(format!("cvars the voice steps changed: {changed:?}"));
    }
    c.step("stock server dvar registrations", |c| {
        // KisakCOD game_mp/g_main_mp.cpp:407,632-648,694-712 and server_mp/sv_init_mp.cpp:708-722,771.
        // Empty registered strings must be "", never the null returned for an absent registration.
        for (name, want) in [
            ("g_redCrosshairs", "1"),
            ("g_banIPs", ""),
            ("g_friendlyfireDist", "256"),
            ("g_friendlyNameDist", "15000"),
            ("sv_iwds", ""),
            ("sv_iwdNames", ""),
            ("sv_showCommands", "0"),
            ("sv_padPackets", "0"),
            ("g_dumpAnims", "-1"),
        ] {
            let r = c.ask(json!({"cmd": "server", "op": "cvar", "name": name}))?;
            if r["value"] != json!(want) {
                return Err(format!(
                    "{name}: expected registered default {want:?}, got {r}"
                ));
            }
        }
        Ok(())
    })?;
    c.step("minimap objectives", |c| {
        super::client_hud_minimap::step(c, notes)
    })?;
    c.step("synchronous client simulation", |c| {
        synchronous_commands(c, notes)
    })?;

    // A class change from the in-game menu while alive: the stock script answers with `iprintlnbold` of a bare
    // string key, which the HUD must show localized (a key left on screen is `unresolved`). Within the grace period
    // (`scr_war_graceperiod`, 15 s) the script gives the new loadout at once and says nothing, so the change waits
    // until the period is over.
    c.step("class change message", |c| {
        c.until(
            json!({"client.seconds": {"ge": spawned_at + GRACE_SECS}}),
            secs(GRACE_SECS as u64 + 5),
            "the clock did not run past the grace period",
        )?;
        let bold_before = stat(c, &format!("hud.stats.messages.{BOLD}"))?;
        c.bind("togglemenu", "tap")?;
        c.until(
            json!({"menu.open.0": {"exists": true}}),
            secs(10),
            "the in-game menu did not open on the menu key",
        )?;
        c.click("Choose Class")?;
        c.until(
            json!({"menu.open": {"contains": CLASS_MENU}}),
            secs(10),
            "Choose Class did not open the class menu",
        )?;
        c.click("Spec Ops")?;
        c.until(
            json!({"menu.open": {"len": 0}}),
            secs(10),
            "the class menu did not close after the class was picked",
        )?;
        c.until(
            json!({format!("hud.stats.messages.{BOLD}"): {"gt": bold_before}}),
            secs(20),
            "no bold message followed the class change",
        )?;
        c.pause(secs(1))?;
        let unresolved = c.lookup("hud.stats.unresolved")?;
        if unresolved.as_array().is_some_and(|u| !u.is_empty()) {
            return Err(format!(
                "the message came out as a string key: {unresolved}"
            ));
        }
        Ok(())
    })?;
    c.step("crosshair weapon states", |c| {
        notes.push(crosshair_states(c)?);
        Ok(())
    })?;
    c.step("compass firing pings", |c| {
        notes.push(super::client_hud_compass::check(c)?);
        c.until(
            json!({"player.alive": true, "menu.open": {"len": 0}}),
            secs(60),
            "the player was not alive again after the compass checks",
        )
        .map(|_| ())
    })?;

    c.step("key fallback aim and fire", |c| w.check_key_fallback(c))?;
    c.step("mouse input ready", |c| {
        c.until(
            json!({"client.focused": true, "client.pointer_locked": true, "client.look_gate": null}),
            secs(10),
            "the match window did not get focus and pointer lock",
        )
    })?;

    // A chat line typed as a person does, which the server must send back.
    c.step("chat", |c| {
        c.chat("hello from the script", false)?;
        c.until(
            json!({"console.text": {"contains": "hello from the script"}, "hud.stats.chat": {"ge": 1}}),
            secs(20),
            "the chat line never came back",
        )
    })?;
    c.step("team-only incoming chat", |c| {
        let before = stat(c, "hud.stats.chat")?;
        c.console("set cg_teamChatsOnly 1")?;
        c.chat("global_hidden_369", false)?;
        c.pause(secs(2))?;
        if stat(c, "hud.stats.chat")? != before {
            return Err("cg_teamChatsOnly did not suppress incoming global chat".into());
        }
        c.chat("team_visible_369", true)?;
        c.until(
            json!({"hud.stats.chat": {"gt": before}}),
            secs(10),
            "team chat was suppressed",
        )?;
        c.console("set cg_teamChatsOnly 0")?;
        c.chat("global_visible_369", false)?;
        c.until(
            json!({"hud.stats.chat": {"gt": before + 1}}),
            secs(10),
            "global chat did not return",
        )?;
        Ok(())
    })?;
    // Holding `+talk` sends the microphone (`CL_VoiceFrame`): on the Linux test host's slots scripts/virtual-audio.sh
    // gives the client a 440 Hz test-tone microphone (#488) and says so with COD4E_TEST_MIC.
    c.step("voice capture", |c| {
        if std::env::var_os("COD4E_TEST_MIC").is_none() {
            notes.push(
                "voice capture untested: no test microphone (COD4E_TEST_MIC unset; #488)".into(),
            );
            return Ok(());
        }
        let before = stat(c, "voice.sent")?;
        c.bind("+talk", "down")?;
        // Half a second of speech: 25 of the 20 ms frames (`net::voice::FRAME_SAMPLES` at 8 kHz).
        let sent = c.until(
            json!({"voice.mic_open": true, "voice.sent": {"ge": before + 25}}),
            secs(10),
            &format!("holding +talk sent no voice frames from the test microphone (sent before: {before})"),
        );
        let held = c.state("voice")?;
        c.bind("+talk", "up")?;
        sent.map_err(|e| format!("{e}; voice {held}"))?;
        // Released, nothing more is sent (the microphone stays open for a while, its audio thrown away).
        c.pause(Duration::from_millis(300))?;
        let up = stat(c, "voice.sent")?;
        c.pause(secs(1))?;
        let later = stat(c, "voice.sent")?;
        if later != up {
            return Err(format!(
                "voice frames went on after +talk was released ({up} -> {later})"
            ));
        }
        notes.push(format!(
            "voice: {} frames sent while +talk was held",
            up - before
        ));
        Ok(())
    })?;
    // A vote called, bots counted and added through the console.
    c.step("callvote", |c| {
        c.console("callvote kick bot5")?;
        c.until(
            json!({"console.text": {"contains": "called a vote"}}),
            secs(20),
            "the vote call was not announced",
        )
    })?;
    c.step("bots usage", |c| {
        c.console("bots")?;
        c.until(
            json!({"console.text": {"contains": "<count>"}}),
            secs(10),
            "the server's usage line for `bots` did not arrive",
        )
    })?;
    c.step("bots 12", |c| {
        c.console("bots 12")?;
        c.bind("+scores", "down")?;
        let r = c.until(
            json!({"hud.live.players_listed": {"ge": PLAYERS}}),
            secs(30),
            "the scoreboard did not list the bots the console asked for",
        );
        c.bind("+scores", "up")?;
        r
    })?;

    // `cg_fov` is registered at 65 (`iw3mp.exe` 1.7, KisakCOD `cg_main_mp.cpp`), and the view is that at 4:3 widened
    // for the window (Hor+).
    c.step("field of view", |c| {
        let fov = c.ask(json!({"cmd": "cvar", "name": "cg_fov"}))?;
        if fov["value"] != json!("65") {
            return Err(format!("cg_fov starts at {}, not the stock 65", fov["value"]));
        }
        let view = c.lookup("view")?;
        let (got, aspect) = (
            view["fov_x"].as_f64().unwrap_or(0.0),
            view["aspect"].as_f64().unwrap_or(0.0),
        );
        let want = 2.0 * ((65f64.to_radians() * 0.5).tan() * 0.75 * aspect).atan().to_degrees();
        if (got - want).abs() > 0.1 {
            return Err(format!(
                "the horizontal field of view is {got}, not the {want:.2} cg_fov 65 gives at aspect {aspect}"
            ));
        }
        // 1.7 registers `cg_fov` from 65 to 80: a value outside is refused and the dvar keeps its value.
        c.console("set cg_fov 90")?;
        c.pause(Duration::from_millis(300))?;
        let refused = c.ask(json!({"cmd": "cvar", "name": "cg_fov"}))?;
        c.console("set cg_fov 80")?;
        c.pause(Duration::from_millis(300))?;
        let widest = c.ask(json!({"cmd": "cvar", "name": "cg_fov"}))?;
        c.console("set cg_fov 65")?;
        if refused["value"] != json!("65") || widest["value"] != json!("80") {
            return Err(format!(
                "cg_fov is {} after `set cg_fov 90` (the 1.7 range is 65 to 80) and {} after `set cg_fov 80`",
                refused["value"], widest["value"]
            ));
        }
        Ok(())
    })?;

    // `visionsetnaked` through the console: the stock transition eases by `sin` (`VISIONSETLERP_TO_SMOOTH`) with the
    // destination's switches from the first blended frame, and a set with no file falls back to
    // `vision/default.vision` (`RawBufferOpen`), named in the look's missing list. The map's set is put back after.
    c.step("vision sets", |c| {
        let naked = c.lookup("look.naked")?.as_str().unwrap_or("").to_owned();
        c.console("visionsetnaked default_night 3")?;
        c.until(
            json!({"look.naked": "default_night", "look.blend.ms": 3000}),
            secs(10),
            "visionsetnaked default_night 3 did not start a 3 s blend on the client",
        )?;
        let (mut samples, mut telling) = (0, 0);
        let deadline = Instant::now() + secs(8);
        while Instant::now() < deadline {
            match vision_blend_sample(&c.lookup("look")?)? {
                Some((_, t)) => {
                    samples += 1;
                    telling += usize::from(t);
                }
                None => break,
            }
            c.pause(Duration::from_millis(100))?;
        }
        if telling == 0 {
            return Err(format!(
                "{samples} samples of the 3 s blend, none between 20% and 80% where the eased curve leaves the straight line"
            ));
        }
        c.pause(Duration::from_millis(300))?;
        let night = c.lookup("look.applied")?;
        c.console("visionsetnaked does_not_exist 0")?;
        c.until(
            json!({"look.missing": {"contains": "vision/does_not_exist.vision"}}),
            secs(10),
            "a vision set with no file was not reported missing",
        )?;
        c.pause(Duration::from_millis(300))?;
        let fallback = c.lookup("look.applied")?;
        c.console("visionsetnaked default 0")?;
        c.until(
            json!({"look.naked": "default"}),
            secs(10),
            "visionsetnaked default did not reach the client",
        )?;
        c.pause(Duration::from_millis(300))?;
        let default = c.lookup("look.applied")?;
        if fallback != default || fallback == night {
            return Err(format!(
                "a missing vision file did not fall back to vision/default.vision: {fallback} against default {default} (was {night})"
            ));
        }
        if !naked.is_empty() {
            c.console(&format!("visionsetnaked {naked} 0"))?;
            c.until(
                json!({"look.naked": naked}),
                secs(10),
                "the map's vision set did not come back",
            )?;
        }
        Ok(())
    })?;

    // Night vision on the stock action slot 1 bind (every stock class gets `setactionslot(1, "nightvision")`): the
    // goggles' overlay covers the screen under the HUD and the d-pad slot shows their icon
    // (`CG_DrawNightVisionOverlay`, `CG_DrawPlayerActionSlot`); the same bind takes them off. The picture changes to
    // the night channel on the first frame (multiplayer's `VisionFadeValue` is 1), and with
    // `nightVisionDisableEffects` 1 the goggles go on without it or the overlay (`CG_LookingThroughNightVision`). The
    // player stands still for the matched captures: two with the goggles off (what the view changes by on its own) and
    // one with them on.
    c.step("night vision", |c| {
        w.stop(c)?;
        c.until(
            json!({"player.alive": true, "hud.live.killcam": false}),
            secs(30),
            "the player was not alive to put the goggles on",
        )?;
        let frames = stat(c, "hud.stats.night_vision_frames")?;
        let icons = c
            .lookup("hud.report.night_vision_icon_frames")?
            .as_u64()
            .unwrap_or(0);
        c.pause(secs(1))?;
        c.capture(NV_OFF)?;
        c.pause(secs(1))?;
        c.capture(NV_STILL)?;
        c.bind("+actionslot 1", "tap")?;
        c.until(
            json!({
                "hud.live.night_vision": true,
                "hud.stats.night_vision_frames": {"gt": frames},
                "hud.report.night_vision_icon_frames": {"gt": icons},
            }),
            secs(10),
            "the goggles' overlay and d-pad icon were not drawn after the night vision bind",
        )?;
        let switch = c.lookup("look.switch")?;
        if switch["to_night"] != json!(true) || switch["first"] != switch["target"] {
            return Err(format!(
                "the goggles did not switch the picture to the night channel on the first frame: {switch}"
            ));
        }
        c.pause(secs(1))?;
        c.capture(NV_ON)?;
        if c.lookup("player.alive")? != Value::Bool(true) {
            return Err("the player died while the goggles' frames were captured".into());
        }
        c.bind("+actionslot 1", "tap")?;
        c.until(
            json!({"hud.live.night_vision": false}),
            secs(10),
            "the goggles did not come off on the second press",
        )?;
        c.console("set nightVisionDisableEffects 1")?;
        c.pause(Duration::from_millis(300))?;
        c.bind("+actionslot 1", "tap")?;
        let r = c
            .until(
                json!({"look.goggles": true}),
                secs(10),
                "the goggles did not go on with nightVisionDisableEffects 1",
            )
            .and_then(|_| {
                c.pause(Duration::from_millis(300))?;
                let (live, channel) = (c.lookup("hud.live.night_vision")?, c.lookup("look.channel")?);
                if live != json!(false) || channel != json!("naked") {
                    return Err(format!(
                        "with nightVisionDisableEffects 1 the goggles still changed the picture: night vision {live}, channel {channel}"
                    ));
                }
                Ok(())
            });
        c.bind("+actionslot 1", "tap")?;
        c.until(
            json!({"look.goggles": false}),
            secs(10),
            "the goggles did not come off with nightVisionDisableEffects 1",
        )?;
        c.console("set nightVisionDisableEffects 0")?;
        r
    })?;

    // A shell shock ends at the next spawn, as stock's does (`ClientSpawn` wipes the player state's shock and the
    // cgame's `CG_StartShellShock(.., 0, 0)` ends it), not when its own time runs out. The player throws a concussion
    // grenade at its feet through the server (`devgrenade`, the stock scripts shock it), dies (`devkill`) and
    // respawns; the look must have no shock at the first live frame.
    c.step("shock ends at spawn", |c| {
        w.stop(c)?;
        let me = c.lookup("player.client")?.as_u64().ok_or("player.client is not a number")?;
        let before = c.lookup("look.shocks")?.as_u64().unwrap_or(0);
        c.ask(json!({"cmd": "server", "op": "exec", "line": "devgrenade human concussion_grenade_mp 0 100"}))?;
        c.until(
            json!({"look.shocks": {"gt": before}}),
            secs(10),
            "a concussion grenade at the player's feet did not shell shock the client",
        )?;
        let shock = c.lookup("look.shock")?;
        let end = shock["start_ms"].as_f64().unwrap_or(0.0) + shock["duration_ms"].as_f64().unwrap_or(0.0);
        c.ask(json!({"cmd": "server", "op": "exec", "line": format!("devkill {me} {me}")}))?;
        c.until(json!({"player.dead": true}), secs(10), "devkill did not kill the player")?;
        c.until(
            json!({"player.alive": true, "hud.live.dead": false}),
            secs(60),
            "the player did not respawn after devkill",
        )?;
        let look = c.lookup("look")?;
        let now = look["time"].as_f64().unwrap_or(0.0);
        if !look["shock"].is_null() {
            return Err(format!(
                "the shell shock ({shock}) is still running after the spawn at {now} ms; stock ends it at spawn"
            ));
        }
        if now >= end {
            return Err(format!(
                "the respawn at {now} ms came after the shock's own end at {end} ms, so it proves nothing"
            ));
        }
        Ok(())
    })?;

    // A grenade cooked and thrown with its key: while it cooks the crosshair's centre dot grows with the fuse
    // (`CG_DrawReticleCenter`, #419), and the count on the HUD drops once the throw is out.
    c.step("throw", |c| {
        let frag = c.lookup("hud.report.frag")?;
        let (key, before) = match (frag.as_u64(), c.lookup("hud.report.second")?.as_u64()) {
            (Some(n), _) => ("hud.report.frag", n),
            (None, Some(n)) => ("hud.report.second", n),
            (None, None) => return Err("the HUD shows no grenade to throw".to_owned()),
        };
        let cooked = c
            .lookup("hud.stats.crosshair_cook_frames")?
            .as_u64()
            .unwrap_or(0);
        c.bind("+frag", "down")?;
        // The dot is half again its size at the first cooked frame (3.5 s left); half a second of cooking shows it.
        let cooking = c
            .until(
                json!({"player.grenade_time_left": {"gt": 0}, "hud.stats.crosshair_cook_frames": {"gt": cooked + 30}}),
                secs(10),
                "holding the grenade key cooked no grenade under a drawn crosshair",
            )
            .and_then(|_| c.capture("crosshair_cook").map(|_| ()));
        if let Err(e) = cooking {
            c.bind("+frag", "up")?;
            return Err(e);
        }
        let grown = c
            .lookup("hud.stats.crosshair_cook_growth_max")?
            .as_f64()
            .unwrap_or(0.0);
        c.bind("+frag", "up")?;
        if grown < 5.0 {
            return Err(format!(
                "the crosshair's centre grew by at most {grown} while the grenade cooked"
            ));
        }
        c.until(
            json!({key: {"lt": before}}),
            secs(30),
            "the grenade count never dropped",
        )?;
        // The cooked grenade goes off a few seconds after the throw, and one that bounces back off a wall can kill
        // the thrower: let it go off and the player be back on their feet before the scoreboard's look checks.
        c.pause(secs(4))?;
        c.until(
            json!({"hud.report.live": true, "hud.live.killcam": false, "hud.live.dead": false, "hud.report.health": {"gt": 0}}),
            secs(120),
            "the player did not come back after the cooked grenade",
        )
        .map(|_| ())
    })?;

    // The scoreboard held up by the Tab bind, with every player on it, and up on every frame while the key is down
    // (the original holds it on `cg.showScores`; a menu that took the keys would drop the bind and flicker).
    c.step("scoreboard", |c| {
        c.pause(secs(2))?;
        c.bind("+scores", "down")?;
        let held = |c: &mut Driven| -> Result<(), String> {
            c.until(
                json!({"hud.scoreboard_shown": true, "hud.live.players_listed": {"ge": PLAYERS}}),
                secs(10),
                "the scoreboard did not come up on the bind",
            )?;
            // Read both counters from the same state document, and measure frames rather than assuming the GPU
            // can render ten in two seconds. The hold must still span at least thirty actual frames.
            let before = c.ask(json!({"cmd": "state"}))?;
            let shown0 = before["hud"]["stats"]["scoreboard_frames"].as_u64().unwrap_or(0);
            let frames0 = before["client"]["frames"].as_u64().unwrap_or(0);
            c.until(
                json!({"client.frames": {"ge": frames0 + 30}}),
                secs(30),
                "fewer than thirty frames were drawn while Tab was held",
            )?;
            let after = c.ask(json!({"cmd": "state"}))?;
            let shown = after["hud"]["stats"]["scoreboard_frames"].as_u64().unwrap_or(0).saturating_sub(shown0);
            let frames = after["client"]["frames"].as_u64().unwrap_or(0).saturating_sub(frames0);
            if frames < 30 || shown * 10 < frames * 9 {
                return Err(format!(
                    "the scoreboard was up on {shown} of the {frames} frames drawn while Tab was held"
                ));
            }
            c.capture("scoreboard")?;
            // The keys stay the game's while Tab is held: the down arrow looks down (`+lookdown`) and leaves the
            // list alone; the wheel scrolls the list by `cg_scoreboardScrollStep` (stock 3) lines and goes no further,
            // so `weapnext` on it changes no weapon (`Scoreboard_HandleInput`, `CG_ScrollScoreboardDown`). Thirteen
            // players and two banners do not fit the stock panel, so the list scrolls down from its first line.
            let top0 = c.lookup("hud.scores_top")?;
            let pitch0 = c.lookup("player.angles.0")?.as_f64().unwrap_or(0.0);
            let weapon0 = c.lookup("player.weapon")?;
            c.key("downarrow", "down")?;
            c.pause(Duration::from_millis(300))?;
            c.key("downarrow", "up")?;
            c.until(
                json!({"player.angles.0": {"gt": pitch0 + 2.0}}),
                secs(5),
                "the down arrow did not look down while Tab was held",
            )?;
            if c.lookup("hud.scores_top")? != top0 {
                return Err("the down arrow scrolled the scoreboard; it is the binds' key".to_owned());
            }
            c.ask(json!({"cmd": "wheel", "y": -1.0}))?;
            c.pause(Duration::from_millis(300))?;
            let top = c.lookup("hud.scores_top")?;
            if top.as_u64() != top0.as_u64().map(|t| t + 3) {
                return Err(format!(
                    "one wheel notch moved the scoreboard from line {top0} to {top}, not by the stock step of 3"
                ));
            }
            if c.lookup("player.weapon")? != weapon0 {
                return Err("the wheel over the scoreboard reached the binds (the weapon changed)".to_owned());
            }
            Ok(())
        };
        let r = held(c);
        c.bind("+scores", "up")?;
        r
    })?;

    // Each open of the scoreboard lands on the viewer's row (`CenterViewOnClient`): with the viewer's place among
    // their team's rows past the `viewmax` rows the panel holds, the list opens at `line - viewmax / 2 + 1`. The panel
    // is shortened (`cg_scoreboardHeight`) so the viewer's row is just past it wherever the scores put them; then
    // `cg_scoreboardScrollStep` sets how far a notch goes.
    c.step("scoreboard centre", |c| {
        let mut last = String::new();
        let mut centred = false;
        for _ in 0..3 {
            let line = viewer_line(&c.ask(json!({"cmd": "state"}))?["hud"]["live"])?;
            let view_max = line - 1;
            c.console(&format!("set cg_scoreboardHeight {}", scoreboard_height(view_max)))?;
            c.pause(Duration::from_millis(300))?;
            c.bind("+scores", "down")?;
            let up = c.until(
                json!({"hud.scoreboard_shown": true, "hud.scores_top": {"ge": 1}}),
                secs(10),
                "the scoreboard did not come up on the bind",
            );
            let state = c.ask(json!({"cmd": "state"}));
            c.bind("+scores", "up")?;
            up?;
            let state = state?;
            let line_now = viewer_line(&state["hud"]["live"])?;
            let top = state["hud"]["scores_top"].as_u64().unwrap_or(0) as usize;
            let want = if line_now > view_max {
                line_now - view_max / 2 + 1
            } else {
                1
            };
            if line_now == line && top == want {
                centred = true;
                break;
            }
            last = format!(
                "the viewer is on line {line_now} of their team with {view_max} rows in view; the list opened at \
                 line {top}, not {want}"
            );
            c.until(
                json!({"hud.scoreboard_shown": false}),
                secs(10),
                "the scoreboard did not go down",
            )?;
        }
        c.console("set cg_scoreboardHeight 435")?;
        if !centred {
            return Err(last);
        }
        c.until(
            json!({"hud.scoreboard_shown": false}),
            secs(10),
            "the scoreboard did not go down",
        )?;
        c.console("set cg_scoreboardScrollStep 2")?;
        c.pause(Duration::from_millis(300))?;
        c.bind("+scores", "down")?;
        let stepped = (|| -> Result<(), String> {
            c.until(
                json!({"hud.scoreboard_shown": true, "hud.scores_top": {"ge": 1}}),
                secs(10),
                "the scoreboard did not come up on the bind",
            )?;
            let top0 = c.lookup("hud.scores_top")?.as_u64().unwrap_or(0);
            c.ask(json!({"cmd": "wheel", "y": -1.0}))?;
            c.pause(Duration::from_millis(300))?;
            let top = c.lookup("hud.scores_top")?.as_u64().unwrap_or(0);
            if top != top0 + 2 {
                return Err(format!(
                    "with cg_scoreboardScrollStep 2 one wheel notch moved the scoreboard from line {top0} to {top}"
                ));
            }
            Ok(())
        })();
        c.bind("+scores", "up")?;
        c.console("set cg_scoreboardScrollStep 3")?;
        stepped
    })?;

    // From here the stage's player walks and fights, so that bots come into sight and one of them gets the kill.
    c.step("feed", |c| {
        w.drive_until(
            c,
            json!({"hud.live.feed": true}),
            secs(120),
            "no message window had a line up",
        )?;
        c.pause(secs(1))?;
        c.capture("feed").map(|_| ())
    })?;
    c.step("killcam", |c| {
        killcam_kill(c)?;
        c.capture("killcam_first")?;
        c.pause(secs(1))?;
        c.capture("killcam_next")?;
        c.pause(secs(1))?;
        c.capture("killcam").map(|_| ())
    })?;
    // Back alive with health and out of the killcam, the low-health overlay must be off (`CG_Respawn` resets it).
    c.step("respawn", |c| {
        c.until(
            json!({"hud.report.live": true, "hud.live.killcam": false, "hud.live.dead": false, "hud.report.health": {"gt": 0}}),
            secs(120),
            "the player did not come back from the dead",
        )?;
        let alpha = c.lookup("hud.report.overlay_alpha")?.as_f64().unwrap_or(1.0);
        if alpha != 0.0 {
            return Err(format!("the low-health overlay is still at alpha {alpha} after the respawn"));
        }
        Ok(())
    })?;
    // The player must have got the sights on a bot and pulled the trigger: a walker that never aligns proves
    // nothing about the crosshair names. The steps above end on the match's events (a line in the feed, a bot's
    // kill), which can come before the walker met anyone (#464): keep playing until it has had a bot in its sights,
    // so the check fails only when aiming and firing do not work.
    c.step("fire", |c| {
        // A bot left to the match may never come into sight before the timeout (#494). Put a live enemy 200 units
        // from the player in plain sight, along their view where it can, and hold it still, so this step measures
        // the walker's aiming and trigger pull, not whether the match offered a target. The walker still has to
        // turn onto the bot and fire itself.
        if w.shots == 0 {
            use super::client_hud_compass::{enemy_bot, exec};
            let enemy = match c.lookup("player.team")?.as_u64() {
                Some(1) => 2,
                Some(2) => 1,
                t => return Err(format!("the player is on no team ({t:?})")),
            };
            let me = c
                .lookup("player.client")?
                .as_u64()
                .ok_or("the player has no client slot")?;
            let bot = enemy_bot(c, enemy)?;
            exec(c, "set bot_idle 1")?;
            exec(c, &format!("devtele {bot} near {me} 200"))?;
        }
        let end = Instant::now() + secs(60);
        let result = (|| {
            while w.shots == 0 {
                if Instant::now() > end {
                    w.stop(c)?;
                    return Err(format!(
                        "the stage's player never fired in {} ticks (turned with {})",
                        w.ticks,
                        w.turning()
                    ));
                }
                c.pause(Duration::from_millis(150))?;
                w.tick(c)?;
            }
            w.stop(c)
        })();
        super::client_hud_compass::exec(c, "set bot_idle 0")?;
        result
    })?;
    // The crosshair over a player takes their team's colour (`CG_CalcCrosshairColor`, #414). The colour steps run
    // with `sv_cheats` on at the listen server (forced, as `devmap` sets it; the client takes it from the
    // systeminfo) and the player in `god` mode, so the walker's sights stay on players rather than on killcams.
    let cheats = c.ask(json!({"cmd": "server", "op": "cvar", "name": "sv_cheats"}))?["value"]
        .as_str()
        .unwrap_or("0")
        .to_owned();
    let force = |c: &mut Driven, v: &str| {
        c.ask(
            json!({"cmd": "server", "op": "cvar", "name": "sv_cheats", "value": v, "force": true}),
        )
    };
    force(c, "1")?;
    let r = crosshair_colours(c, w);
    w.hold_fire = false;
    let restored = c
        .ask(json!({"cmd": "cvar", "name": "cg_crosshairEnemyColor", "value": "1"}))
        .and_then(|_| c.ask(json!({"cmd": "exec", "line": "god"})))
        .and_then(|_| force(c, &cheats));
    r?;
    restored?;
    notes.push(format!(
        "the stage's player turned with {}; {} ticks, {} shots",
        w.turning(),
        w.ticks,
        w.shots
    ));
    if w.mouse_ticks == 0 {
        return Err(format!(
            "the match never exercised mouse look (turned with {})",
            w.turning()
        ));
    }
    c.step("helicopter compass ownerdraw", |c| {
        // Stock 1.7 places 155 in compass_old, one of the in-match HUD menus the client draws; opening it as a menu
        // hides the HUD instead.
        c.ask(json!({"cmd":"server","op":"exec","line":"devhardpoint bot helicopter_mp"}))?;
        c.until(
            json!({"hud.report.helicopter_marks":{"ge":1}}),
            secs(120),
            "an active helicopter never drew its minimap icon (155)",
        )?;
        c.capture("helicopter-compass")?;
        Ok(())
    })?;
    Ok(())
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
    let dir = ctx.dir.join("hud");
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
            return Ok(out);
        }
    };
    out.notes.push(format!("channel {}", c.addr));
    let played =
        Walker::new(&mut c).and_then(|mut w| play(&mut c, &mut w, install, &mut out.notes));
    // The counters are read before the run ends, played out or not: a failed step still says what the HUD drew.
    let report = c
        .report("control-run.json", &["hud", "net", "render"])
        .map(|r| {
            let steps = r["steps"].as_array().cloned().unwrap_or_default();
            assemble(&steps, r["hud"].clone(), &r["net"], &r["render"])
        });
    let status = c.finish(Duration::from_secs(30));
    out.files.push("hud/control-run.json".into());
    for f in [
        "scoreboard.png",
        "feed.png",
        "night_vision_off.png",
        "night_vision_still.png",
        "night_vision.png",
        "killcam_first.png",
        "killcam_next.png",
        "killcam.png",
        "record-level.png",
        "helicopter-compass.png",
    ] {
        if dir.join(f).is_file() {
            out.files.push(format!("hud/{f}"));
        }
    }
    let why = match (played, report) {
        (Err(e), _) => Some(e),
        (Ok(()), Err(e)) => Some(format!("the run's report could not be read: {e}")),
        (Ok(()), Ok(report)) => {
            for k in [
                "elems_max",
                "waypoints_max",
                "scoreboard_rows_max",
                "obituaries",
                "killcam_frames",
            ] {
                if let Some(v) = report["hud_draw"][k].as_f64() {
                    out.metrics.insert(format!("hud.{k}"), v);
                }
            }
            verdict(&report).or_else(|| match night_vision_view(&dir) {
                Ok(m) => {
                    out.metrics.insert("nv.min_gain".into(), m.min_gain);
                    out.metrics.insert("nv.hud_rise".into(), m.hud_rise);
                    nv_problem(&m)
                }
                Err(e) => Some(format!(
                    "the night vision captures could not be compared: {e}"
                )),
            })
        }
    };
    if let Some(why) = why {
        out.status = Status::Failed;
        out.reason = Some(why);
    } else if !status.is_some_and(|s| s.success()) {
        out.status = Status::Failed;
        out.reason = Some(match status {
            Some(s) => format!("the client exited {s}"),
            None => "the client did not quit and was killed".to_owned(),
        });
    }
    out.notes.push(
        "scoreboard rows, kill and print lines, script hud elements and the killcam, as the HUD drew them while the stage's own player, keys and mouse through the control channel, played the match".into(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Value {
        json!({
            "steps": [{"step": "spawn", "ok": true}],
            "hud_draw": {
                "elems_max": 12, "scoreboard_frames": 90, "scoreboard_rows_max": 10,
                "messages": [2, 0, 0, 0], "obituaries": 3, "window_lines": [40, 0, 0, 0],
                "killcam_frames": 120, "killcam_you_frames": 30, "names_max": 3, "crosshair_name_frames": 40,
                "friends_in_sight_frames": 50, "crosshair_due_frames": 60,
                "head_icon_materials": ["headiconyouinkillcam"], "chat": 1,
                "killcam_live_text": ["&PLATFORM_PRESS_TO_SKIP"], "night_vision_frames": 60
            },
            "hud": {"night_vision_icon_frames": 60},
            "missing_images": [],
            "net": {"view": {"death_view_frames": 40, "death_view_range_max": 120.0, "dof_frames": 900}},
            "render": {"material_failures": {}, "image_failures": []},
        })
    }

    #[test]
    fn a_run_that_showed_everything_passes() {
        assert_eq!(verdict(&good()), None);
    }

    #[test]
    fn the_report_is_assembled_from_the_state_sections() {
        let hud = json!({
            "stats": {"elems_max": 3},
            "report": {"unresolved_text": ["X_Y"]},
            "missing_images": ["hud_a"],
        });
        let net = json!({"spawned": true, "view": {"dof_frames": 2}});
        let render = json!({"material_failures": {}, "image_failures": ["missing_image"]});
        let r = assemble(&[json!({"step": "spawn", "ok": true})], hud, &net, &render);
        assert_eq!(r["render"], render);
        assert_eq!(r["hud_draw"]["elems_max"], 3);
        assert_eq!(r["hud"]["unresolved_text"][0], "X_Y");
        assert_eq!(r["missing_images"][0], "hud_a");
        assert_eq!(r["net"]["view"]["dof_frames"], 2);
        assert!(r["net"].get("spawned").is_none());
    }

    #[test]
    fn each_missing_piece_is_named() {
        let mut r = good();
        r["hud_draw"]["window_lines"] = json!([0, 0, 0, 0]);
        r["hud_draw"]["killcam_frames"] = json!(0);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("message windows") && why.contains("killcam"),
            "{why}"
        );
        let mut r = good();
        r["hud_draw"]["names_max"] = json!(0);
        r["hud_draw"]["crosshair_name_frames"] = json!(0);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("overhead name") && why.contains("crosshair"),
            "{why}"
        );
        // The match gave nobody to name: a failure now, because the scenario is meant to put someone there.
        r["hud_draw"]["friends_in_sight_frames"] = json!(0);
        r["hud_draw"]["crosshair_due_frames"] = json!(0);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("no teammate was ever in sight") && why.contains("never on a player"),
            "{why}"
        );
        // The crosshair tinted in the wrong team's colour, or with nobody under it.
        let mut r = good();
        r["hud_draw"]["crosshair_team_tints"] = json!(["0.400 0.600 0.850", "0.750 0.250 0.250"]);
        r["hud_draw"]["crosshair_friend_tints"] = json!(["1.000 1.000 1.000", "0.400 0.600 0.850"]);
        r["hud_draw"]["crosshair_enemy_tints"] = json!(["0.750 0.250 0.250"]);
        assert_eq!(verdict(&r), None);
        r["hud_draw"]["crosshair_enemy_tints"] = json!(["0.400 0.600 0.850"]);
        r["hud_draw"]["crosshair_stray_tint_frames"] = json!(3);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("over an enemy") && why.contains("no player under it"),
            "{why}"
        );
        let mut r = good();
        r["net"]["view"]["death_view_frames"] = json!(0);
        r["net"]["view"]["dof_frames"] = json!(0);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("behind the body") && why.contains("depth of field"),
            "{why}"
        );
        r["net"]["view"]["death_view_frames"] = json!(5);
        r["net"]["view"]["death_view_range_max"] = json!(0.0);
        assert!(verdict(&r).unwrap().contains("at the head"));
        let mut r = good();
        r["hud_draw"]["head_icon_materials"] = json!([]);
        assert!(verdict(&r).unwrap().contains("head icon"));
        // The viewer's body never in sight in the killcam: a failure too, the killcam must show it.
        r["hud_draw"]["killcam_you_frames"] = json!(0);
        assert!(verdict(&r).unwrap().contains("never had the viewer's body"));
        let mut r = good();
        r["hud_draw"]["unresolved"] = json!(["MP_CHANGE_CLASS_NEXT_SPAWN"]);
        r["hud"] = json!({"unresolved_text": ["PLATFORM_HOLD_TO_PLANT_EXPLOSIVES"]});
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("MP_CHANGE_CLASS_NEXT_SPAWN")
                && why.contains("PLATFORM_HOLD_TO_PLANT_EXPLOSIVES"),
            "{why}"
        );
        let mut r = good();
        r["hud_draw"]["scoreboard_rows_max"] = json!(2);
        assert!(verdict(&r).unwrap().contains("scoreboard"));
        let mut r = good();
        r["hud_draw"]["chat"] = json!(0);
        assert!(verdict(&r).unwrap().contains("chat"));
        let mut r = good();
        r["steps"] = json!([{"step": "killcam", "ok": false, "note": "no bot killed the player"}]);
        assert!(verdict(&r).unwrap().contains("killcam"));
        let mut r = good();
        r["hud_draw"]["killcam_live_text"] = json!([]);
        assert!(verdict(&r).unwrap().contains("skip prompt"));
        let mut r = good();
        r["hud_draw"]["night_vision_frames"] = json!(0);
        r["hud"]["night_vision_icon_frames"] = json!(0);
        r["missing_images"] = json!(["nightvision_overlay_goggles"]);
        let why = verdict(&r).unwrap();
        assert!(
            why.contains("goggles' overlay")
                && why.contains("night vision icon")
                && why.contains("picture"),
            "{why}"
        );
    }

    const W: u32 = 320;
    const H: u32 = 180;

    /// A `W` x `H` picture: a smoothly shaded world (its luma divided by `dim`) with a flat white HUD block where the
    /// minimap is, and the same picture with the goggles on, where `overlay` darkens three rows in every seven of the
    /// world it covers (and of the HUD block when `over_hud`), as the goggles' scanlines do.
    fn pictures(
        overlay: impl Fn(u32, u32) -> bool,
        over_hud: bool,
        dim: u32,
    ) -> (Vec<u8>, Vec<u8>) {
        let (mut off, mut on) = (Vec::new(), Vec::new());
        let hud = |x: u32, y: u32| (3..38).contains(&x) && (9..43).contains(&y);
        for y in 0..H {
            for x in 0..W {
                let v = if hud(x, y) {
                    250
                } else {
                    (60 + x / 4 + y / 4) / dim
                };
                let line = y % 7 < 3 && overlay(x, y) && (!hud(x, y) || over_hud);
                off.extend([v as u8; 3]);
                on.extend([(if line { v * 7 / 10 } else { v }) as u8; 3]);
            }
        }
        (off, on)
    }

    #[test]
    fn the_goggles_frame_must_show_the_overlay_over_the_world_and_under_the_hud() {
        let measure = |overlay: &dyn Fn(u32, u32) -> bool, over_hud, dim| {
            let (off, on) = pictures(overlay, over_hud, dim);
            nv_measure(W, H, &off, &off, &on)
        };
        let good = measure(&|_, _| true, false, 1);
        assert!(good.min_gain > 0.03, "{good:?}");
        assert!(good.hud_rise.abs() < 0.01, "{good:?}");
        assert!(nv_problem(&good).is_none());
        // A dark scene shows the lines as plainly for its brightness (#494).
        let dark = measure(&|_, _| true, false, 3);
        assert!(nv_problem(&dark).is_none(), "{dark:?}");
        let none = measure(&|_, _| false, false, 1);
        assert!(none.min_gain.abs() < 0.001, "{none:?}");
        assert!(nv_problem(&none).unwrap().contains("does not cover"));
        let quarter = measure(&|x, y| x < W / 2 && y < H / 2, false, 1);
        assert!(
            nv_problem(&quarter).unwrap().contains("does not cover"),
            "{quarter:?}"
        );
        let over = measure(&|_, _| true, true, 1);
        assert!(over.hud_rise > 5.0, "{over:?}");
        assert!(nv_problem(&over).unwrap().contains("over the HUD"));
        // A view that changes as much on its own as with the goggles shows no overlay.
        let (off, on) = pictures(|_, _| true, false, 1);
        assert!(
            nv_problem(&nv_measure(W, H, &on, &off, &on))
                .unwrap()
                .contains("does not cover")
        );
    }
}
