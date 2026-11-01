// SPDX-License-Identifier: GPL-3.0-only
//! `client-hud`: what the client draws on top of the stock HUD menus during a match, driven through the dev control
//! channel (#378) the way a person plays (#376). The real client starts its own listen server with nine bots and the
//! stage picks a team and a class with the pointer, walks and aims with the keys and the mouse ([`Walker`]), changes
//! class from the in-game menu while alive (the stock `_menus.gsc` answers with a bare-key bold message, which must
//! come out localized), types a chat line, calls a vote and adds bots through the console, puts the night vision
//! goggles on and off with the action slot 1 bind, holds the scoreboard up with the Tab bind, throws a grenade with
//! its key, waits for a message window to have a line up and for a bot to kill it (the killcam), comes back from the
//! dead, and plays on until it has had a bot in its sights and fired. What the HUD got to show over the run
//! (`hud.stats` of the state document, the counters `hud_draw` of `ui-script.json` held) is the verdict: the
//! scoreboard must have listed the players, the message windows must have drawn lines, the script hud elements must
//! have arrived, the goggles must have drawn their overlay and d-pad icon (and the frame with them on must show the
//! overlay over the world with the HUD still above it), the killcam must have played behind the body with its skip
//! prompt over it, a teammate must have had a name over the head and the player under the crosshair a name of their
//! own, and nothing may be on screen as a bare string key. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::{render_problem, steps_problem};
use crate::control::{Driven, Walker};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
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
/// The overlay's picture is a screen of fine horizontal scanlines (about 7 rows apart in a 720-row capture), so a
/// region it covers gains row-to-row ripple in its row means, whatever the world behind it shows; a bot walking by
/// or the view's own change move the row means far less. The screen is cut into `NV_GRID` x `NV_GRID` cells.
const NV_GRID: u32 = 4;
/// The picture's vignette is near black in the corners, where the lines cannot show: all but those may miss.
const NV_MIN_CELLS: usize = 12;
/// Rows each side of a row whose mean the ripple is measured from.
const NV_RADIUS: usize = 4;
/// A cell shows the lines when its ripple with the goggles on is this much above the still view's, plus twice the
/// ripple's own change between the two goggles-off captures.
const NV_MIN_RISE: f64 = 1.5;
/// The minimap of the compass in the driven client's 1280x720 window (left, top, right, bottom as fractions): HUD,
/// drawn after the overlay (`CG_Draw2D` draws the overlay first), so the lines must not show on it.
const NV_HUD: (f32, f32, f32, f32) = (0.01, 0.05, 0.12, 0.24);
/// The most the minimap's ripple may rise with the goggles on.
const NV_HUD_MAX_RISE: f64 = 1.0;

fn luma(p: &[u8]) -> i32 {
    (i32::from(p[0]) * 77 + i32::from(p[1]) * 150 + i32::from(p[2]) * 29) >> 8
}

/// The mean distance of each row's mean luma over `x0..x1` from the mean of the `NV_RADIUS` rows on each side.
fn ripple(img: &[u8], w: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> f64 {
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
    away.iter().sum::<f64>() / away.len().max(1) as f64
}

/// What the goggles changed between the matched captures.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NvView {
    /// Cells of the grid that gained the overlay's scanlines with the goggles on.
    cells_changed: usize,
    /// How much the minimap's ripple rose with the goggles on.
    hud_rise: f64,
}

/// Compares the RGB pictures `off`, `still` (both goggles off) and `on`, each `w` x `h`.
fn nv_measure(w: u32, h: u32, off: &[u8], still: &[u8], on: &[u8]) -> NvView {
    let mut m = NvView::default();
    for cy in 0..NV_GRID {
        for cx in 0..NV_GRID {
            let r = (
                cx * w / NV_GRID,
                cy * h / NV_GRID,
                (cx + 1) * w / NV_GRID,
                (cy + 1) * h / NV_GRID,
            );
            let (a, b, c) = (ripple(off, w, r), ripple(still, w, r), ripple(on, w, r));
            if c - b >= NV_MIN_RISE + 2.0 * (b - a).abs() {
                m.cells_changed += 1;
            }
        }
    }
    let (l, t, r, b) = NV_HUD;
    let at = |f: f32, n: u32| ((f * n as f32) as u32).min(n);
    let hud = (at(l, w), at(t, h), at(r, w), at(b, h));
    m.hud_rise = ripple(on, w, hud) - ripple(still, w, hud);
    m
}

/// What is wrong with the goggles' frame, or `None`.
fn nv_problem(m: &NvView) -> Option<String> {
    let mut bad = Vec::new();
    if m.cells_changed < NV_MIN_CELLS {
        bad.push(format!(
            "the goggles' scanlines showed in {} of {} parts of the screen (need {NV_MIN_CELLS}): the overlay does not cover it",
            m.cells_changed,
            NV_GRID * NV_GRID
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

/// Read the real person's last simulation frame, not an expected value supplied by the scenario.
fn command_frame(c: &mut Driven, slot: u64, empty: bool, right: bool) -> Result<Value, String> {
    let start = Instant::now();
    let mut last = Value::Null;
    while start.elapsed() < Duration::from_secs(30) {
        let state = c.ask(json!({"cmd": "server", "op": "state", "what": "clients"}))?;
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
    Err(format!(
        "no matching sync {} frame (right={right}): {last}",
        if empty { "empty" } else { "backlog" }
    ))
}

/// Exercise both queued input and a frame without new input using the normal render/send cadence.
fn synchronous_commands(c: &mut Driven, notes: &mut Vec<String>) -> Result<(), String> {
    let fps = c.ask(json!({"cmd": "cvar", "name": "com_maxfps"}))?["value"].clone();
    let vsync = c.ask(json!({"cmd": "cvar", "name": "r_vsync"}))?["value"].clone();
    let sync =
        c.ask(json!({"cmd": "server", "op": "cvar", "name": "g_synchronousClients"}))?["value"]
            .clone();
    let result = (|| {
        let slot = c
            .lookup("player.client")?
            .as_u64()
            .ok_or("no player slot")?;
        // `r_vsync` reaches the surface only through `vid_restart`; while the display paces the frames, `com_maxfps`
        // caps nothing and the client keeps sending a command every displayed frame.
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
    result
}

/// The match, step by step. Every step goes through the channel: a failure names the step and what was observed.
fn play(c: &mut Driven, w: &mut Walker, notes: &mut Vec<String>) -> Result<(), String> {
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

    // Night vision on the stock action slot 1 bind (every stock class gets `setactionslot(1, "nightvision")`): the
    // goggles' overlay covers the screen under the HUD and the d-pad slot shows their icon
    // (`CG_DrawNightVisionOverlay`, `CG_DrawPlayerActionSlot`); the same bind takes them off. The player stands still
    // for the matched captures: two with the goggles off (what the view changes by on its own) and one with them on.
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
        Ok(())
    })?;

    // A grenade thrown with its key: the count on the HUD drops once the throw is out.
    c.step("throw", |c| {
        let frag = c.lookup("hud.report.frag")?;
        let (key, before) = match (frag.as_u64(), c.lookup("hud.report.second")?.as_u64()) {
            (Some(n), _) => ("hud.report.frag", n),
            (None, Some(n)) => ("hud.report.second", n),
            (None, None) => return Err("the HUD shows no grenade to throw".to_owned()),
        };
        c.bind("+frag", "down")?;
        let r = c.until(
            json!({key: {"lt": before}}),
            secs(30),
            "the grenade count never dropped",
        );
        c.bind("+frag", "up")?;
        r
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
            let shown0 = stat(c, "hud.stats.scoreboard_frames")?;
            let frames0 = stat(c, "client.frames")?;
            c.pause(secs(2))?;
            let shown = stat(c, "hud.stats.scoreboard_frames")?.saturating_sub(shown0);
            let frames = stat(c, "client.frames")?.saturating_sub(frames0);
            if frames < 10 || shown * 10 < frames * 9 {
                return Err(format!(
                    "the scoreboard was up on {shown} of the {frames} frames drawn while Tab was held"
                ));
            }
            c.capture("scoreboard")?;
            // The keys stay the game's while Tab is held: the down arrow looks down (`+lookdown`) and leaves the
            // list alone; the wheel scrolls the list and goes no further, so `weapnext` on it changes no weapon
            // (`Scoreboard_HandleInput`).
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
            if c.lookup("hud.scores_top")? == top0 {
                return Err("the wheel did not scroll the scoreboard".to_owned());
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
        // A replay can start with the victim behind a wall. Keep playing until a killcam actually offers the body
        // to the "you" marker rather than ending the run at the first replay regardless of what it showed.
        w.drive_until(
            c,
            json!({"hud.live.killcam": true, "hud.stats.killcam_you_frames": {"gt": 0}}),
            secs(300),
            "no killcam showed the viewer's body for the you marker",
        )?;
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
        let end = Instant::now() + secs(120);
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
    })?;
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
    let played = Walker::new(&mut c).and_then(|mut w| play(&mut c, &mut w, &mut out.notes));
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
                    out.metrics
                        .insert("nv.cells_changed".into(), m.cells_changed as f64);
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

    /// A `W` x `H` picture: a smoothly shaded world with a flat white HUD block where the minimap is, and the same
    /// picture with the goggles on, where `overlay` darkens three rows in every seven of the world it covers (and of
    /// the HUD block when `over_hud`), as the goggles' scanlines do.
    fn pictures(overlay: impl Fn(u32, u32) -> bool, over_hud: bool) -> (Vec<u8>, Vec<u8>) {
        let (mut off, mut on) = (Vec::new(), Vec::new());
        let hud = |x: u32, y: u32| (3..38).contains(&x) && (9..43).contains(&y);
        for y in 0..H {
            for x in 0..W {
                let v = if hud(x, y) { 250 } else { 60 + x / 4 + y / 4 };
                let line = y % 7 < 3 && overlay(x, y) && (!hud(x, y) || over_hud);
                off.extend([v as u8; 3]);
                on.extend([(if line { v * 7 / 10 } else { v }) as u8; 3]);
            }
        }
        (off, on)
    }

    #[test]
    fn the_goggles_frame_must_show_the_overlay_over_the_world_and_under_the_hud() {
        let measure = |overlay: &dyn Fn(u32, u32) -> bool, over_hud| {
            let (off, on) = pictures(overlay, over_hud);
            nv_measure(W, H, &off, &off, &on)
        };
        let good = measure(&|_, _| true, false);
        assert_eq!(good.cells_changed, 16);
        assert!(good.hud_rise.abs() < 0.01, "{good:?}");
        assert!(nv_problem(&good).is_none());
        let none = measure(&|_, _| false, false);
        assert_eq!(none.cells_changed, 0);
        assert!(nv_problem(&none).unwrap().contains("does not cover"));
        let quarter = measure(&|x, y| x < W / 2 && y < H / 2, false);
        assert_eq!(quarter.cells_changed, 4);
        let over = measure(&|_, _| true, true);
        assert!(over.hud_rise > 5.0, "{over:?}");
        assert!(nv_problem(&over).unwrap().contains("over the HUD"));
        // A view that changes as much on its own as with the goggles shows no overlay.
        let (off, on) = pictures(|_, _| true, false);
        assert_eq!(nv_measure(W, H, &on, &off, &on).cells_changed, 0);
    }
}
