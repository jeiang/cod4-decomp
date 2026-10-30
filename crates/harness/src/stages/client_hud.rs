// SPDX-License-Identifier: GPL-3.0-only
//! `client-hud`: what the client draws on top of the stock HUD menus during a match, driven through the dev control
//! channel (#378) the way a person plays (#376). The real client starts its own listen server with nine bots and the
//! stage picks a team and a class with the pointer, walks and aims with the keys and the mouse ([`Walker`]), changes
//! class from the in-game menu while alive (the stock `_menus.gsc` answers with a bare-key bold message, which must
//! come out localized), types a chat line, calls a vote and adds bots through the console, holds the scoreboard up
//! with the Tab bind, throws a grenade with its key, waits for a message window to have a line up and for a bot to
//! kill it (the killcam), and comes back from the dead. What the HUD got to show over the run (`hud.stats` of the
//! state document, the counters `hud_draw` of `ui-script.json` held) is the verdict: the scoreboard must have listed
//! the players, the message windows must have drawn lines, the script hud elements must have arrived, the killcam
//! must have played behind the body, a teammate must have had a name over the head and the player under the crosshair
//! a name of their own, and nothing may be on screen as a bare string key. Needs a display and the install; skips
//! without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{render_problem, steps_problem};
use crate::control::{Driven, Walker};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::Duration;

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
    notes.push(format!(
        "the stage's player turned with {}; {} ticks, {} shots",
        w.turning(),
        w.ticks,
        w.shots
    ));
    // The player must have got the sights on a bot and pulled the trigger: a walker that never aligns proves
    // nothing about the crosshair names.
    if w.shots == 0 {
        return Err(format!(
            "the stage's player never fired in {} ticks (turned with {})",
            w.ticks,
            w.turning()
        ));
    }
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
            verdict(&report)
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
                "head_icon_materials": ["headiconyouinkillcam"], "chat": 1
            },
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
    }
}
