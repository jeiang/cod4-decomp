// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-custom-class`: a person with a saved profile builds a custom class in the menus and plays from it, driven
//! through the control channel with the player's own pointer, keys and console (#376). Typed at the console, custom
//! class 5 is named `P90` (`customclass5`, as players rename classes). Create a Class > custom slot 5 > Primary
//! Weapon > Sub Machine Guns > P90 is walked in the stock menus: the P90 row's click sets the class's P90 with no
//! attachment and must open the SMG attachment popup. Special Grenades as perk 1 is typed as a `statset` fixture, as
//! is the experience (a stat no menu writes). The profile's stats (read back with `statGetInDvar`, as a player would)
//! must hold the class. The stats reach the server before the person begins, so the class loadout holds the P90, the
//! perk and experience survive the join (the server's stand-ins are never taken for news), a class change in the first
//! seconds of a round applies at once, and every other player the server announces is drawn. Needs a display and the
//! install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{sprint_problem, verdict, weapon_prefix};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-custom-class";

/// Custom class 5 (its stats start at 240): the P90's row of the stats table as the primary weapon (`stat + 1`) and no
/// attachment (`stat + 2`), both set by the P90 row's click in the SMG popup, and Special Grenades x 3 as perk 1
/// (`stat + 5`; the join once replaced it with a stand-in), typed as a fixture (see the exception in [`build_class`]).
/// The experience (`2301`) has no menu, so the stage types it.
const PRIMARY: (u32, i64) = (241, 14);
const ATTACHMENT: (u32, i64) = (242, 0);
const PERK1: (u32, i64) = (245, 176);
const EXPERIENCE: (u32, i64) = (2301, 5000);

/// The localize keys of the menu items the stage clicks that have no name of their own.
const KEYS: [&str; 2] = ["MPUI_SUB_MACHINE_GUNS", "WEAPON_P90"];

/// The install's localized strings for `keys` (without `@`), read from its own zones as the menus read them (the
/// localized zones of `ui::assets::UI_ZONES`, a later zone's string replacing an earlier one's), so a check finds a
/// menu item by the text the player reads in the install's language.
pub(super) fn localized(install: &Path, keys: &[&str]) -> Result<HashMap<String, String>, String> {
    use assets::zone::{Asset, DecodeFilter, XAssetType, Zone};
    struct Strings;
    impl DecodeFilter for Strings {
        fn keep(&self, ty: XAssetType) -> bool {
            matches!(ty, XAssetType::Localize)
        }
    }
    let mut out = HashMap::new();
    for zone in ["localized_code_post_gfx_mp", "localized_common_mp"] {
        let path = assets::paths::zone_file(install, "", zone)
            .ok_or_else(|| format!("the install has no zone {zone}"))?;
        let file = assets::fs::buffered(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let z = Zone::open(file).map_err(|e| format!("{zone}: {e}"))?;
        z.decode(&Strings, |a| {
            if let Asset::Localize(l) = a
                && let (Some(n), Some(v)) = (&l.name, &l.value)
                && let Some(k) = keys.iter().find(|k| k.eq_ignore_ascii_case(n))
            {
                out.insert((*k).to_owned(), v.to_string());
            }
        })
        .map_err(|e| format!("{zone}: {e}"))?;
    }
    match keys.iter().find(|k| !out.contains_key(**k)) {
        Some(k) => Err(format!("the install has no localized string {k}")),
        None => Ok(out),
    }
}

/// The run's state in the shape [`verdict`] reads (the `client`, `report`, `hud` and `render` state sections): the
/// steps, the graphics device's health (`client.gpu_health`), the renderer's record, what the network side counted
/// over the run (the `report` section, `NetPlay::report`) and what the HUD showed (`hud.report`). The sections as read
/// are written to `file`.
pub(super) fn session_report(c: &mut Driven, file: &str) -> Result<Value, String> {
    let r = c.report(file, &["client", "report", "hud", "render"])?;
    if r["client"]["gpu_health"].is_null() {
        return Err(format!(
            "the state document has no client.gpu_health: {}",
            r["client"]
        ));
    }
    Ok(json!({
        "steps": r["steps"],
        "gpu_health": r["client"]["gpu_health"],
        "render": r["render"],
        "net": r["report"],
        "hud": r["hud"]["report"],
    }))
}

/// A profile stat as the player reads one: `statGetInDvar` typed at the console copies it into a dvar.
pub(super) fn stat(c: &mut Driven, index: u32) -> Result<i64, String> {
    let dvar = format!("zz_stat{index}");
    c.console(&format!("statGetInDvar {index} {dvar}"))?;
    let v = c.ask(json!({"cmd": "cvar", "name": dvar}))?;
    v["value"]
        .as_str()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| format!("statGetInDvar {index} left {dvar} at {}", v["value"]))
}

fn stat_is(c: &mut Driven, (index, want): (u32, i64), what: &str) -> Result<(), String> {
    let have = stat(c, index)?;
    if have == want {
        Ok(())
    } else {
        Err(format!("{what}: stat {index} is {have}, not {want}"))
    }
}

/// The main menu as it stands on top of the stack: the stock `main` draws the background and opens `main_text`
/// over it, which holds the buttons.
pub(super) const HOME: &str = "main_text";

/// Waits until the top menu is `name` (menu names compare without case, as the menus' `open` does).
pub(super) fn until_top(c: &mut Driven, name: &str, limit: Duration) -> Result<(), String> {
    let end = Instant::now() + limit;
    loop {
        let open = c.lookup("menu.open")?;
        let top = open
            .as_array()
            .and_then(|o| o.last())
            .and_then(Value::as_str)
            .unwrap_or("");
        if top.eq_ignore_ascii_case(name) {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!(
                "{name} was not the top menu within {} s; open {open}",
                limit.as_secs()
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

/// Clicks the top menu's button labelled `text` (the install's localized label; empty for none) or, wanting that, the
/// one named `name`: a shown item that is not only decoration, so a locked copy or a preview of the same name is
/// passed over. A list button whose label is drawn by a decoration item of its row (the class popups' weapon rows)
/// is the live item under that label. Returns what matched, for a failure to name.
fn press(c: &mut Driven, name: &str, text: &str) -> Result<String, String> {
    let menu = c.state("menu")?;
    let top = menu["open"]
        .as_array()
        .and_then(|o| o.last())
        .cloned()
        .unwrap_or(Value::Null);
    let shown: Vec<&Value> = menu["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|it| it["menu"] == top && it["visible"] == true)
        .collect();
    let rect = |it: &Value| -> Option<[f64; 4]> {
        let r: Vec<f64> = it["rect"]
            .as_array()?
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        r.try_into().ok()
    };
    let label = |it: &Value| {
        let kind = if it["decoration"] == true {
            " (decoration)"
        } else {
            ""
        };
        format!("{}/{}{kind}", it["name"], it["text"])
    };
    let by_text = |it: &Value| {
        !text.is_empty()
            && it["text"]
                .as_str()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case(text))
    };
    let by_name = |it: &Value| {
        !name.is_empty()
            && it["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    let live: Vec<&Value> = shown
        .iter()
        .copied()
        .filter(|it| it["decoration"] == false)
        .collect();
    // The live item whose rectangle holds the middle of a shown label's, the smallest such: its row's button.
    let under_label = || {
        shown
            .iter()
            .copied()
            .filter(|it| by_text(it))
            .find_map(|l| {
                let [x, y, w, h] = rect(l)?;
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                live.iter()
                    .copied()
                    .filter(|it| {
                        rect(it).is_some_and(|[bx, by, bw, bh]| {
                            bx <= cx && cx <= bx + bw && by <= cy && cy <= by + bh
                        })
                    })
                    .min_by(|a, b| {
                        let area = |it: &Value| rect(it).map_or(f64::MAX, |[_, _, w, h]| w * h);
                        area(a).total_cmp(&area(b))
                    })
            })
    };
    let Some(it) = live
        .iter()
        .copied()
        .find(|it| by_text(it))
        .or_else(|| live.iter().copied().find(|it| by_name(it)))
        .or_else(under_label)
    else {
        return Err(format!(
            "{top} shows no live button {text:?} or {name:?}; it shows {:?}",
            shown
                .iter()
                .map(|it| format!("{} at {}", label(it), it["rect"]))
                .collect::<Vec<_>>()
        ));
    };
    let matched: Vec<String> = shown
        .iter()
        .filter(|it| by_text(it) || by_name(it))
        .map(|it| label(it))
        .collect();
    let rect = it["rect"].clone();
    super::client_frontend::click_rect(c, &rect)?;
    Ok(format!("{} of {matched:?}", label(it)))
}

/// Presses the top menu's button (see [`press`]) and waits for `menu` to come up on top.
fn open_by(c: &mut Driven, name: &str, text: &str, menu: &str) -> Result<(), String> {
    let hit = press(c, name, text)?;
    until_top(c, menu, Duration::from_secs(10)).map_err(|e| format!("clicking {hit}: {e}"))?;
    c.pause(Duration::from_millis(500))
}

fn spawn(c: &mut Driven, limit: Duration) -> Result<(), String> {
    c.until(
        json!({"net.spawned": true, "player.alive": true}),
        limit,
        "the player did not spawn",
    )
    .map(|_| ())
}

/// The class built in Create a Class's menus, from the main menu back to it.
fn build_class(c: &mut Driven, text: &HashMap<String, String>) -> Result<(), String> {
    let t = |k: &str| text[k].as_str();
    c.step("name custom class 5", |c| {
        until_top(c, HOME, Duration::from_secs(60))?;
        c.console("set customclass5 P90")?;
        let name = c.ask(json!({"cmd": "cvar", "name": "customclass5"}))?;
        if name["value"] != "P90" {
            return Err(format!("customclass5 is {}", name["value"]));
        }
        Ok(())
    })?;
    c.step("Create a Class > custom slot 5", |c| {
        until_top(c, HOME, Duration::from_secs(5))?;
        open_by(c, "", "Create a Class", "pc_cac_popup")?;
        open_by(c, "", "P90", "menu_cac_sniper")
    })?;
    c.step("primary weapon: Sub Machine Guns", |c| {
        open_by(
            c,
            "primary_weapon_selection",
            "",
            "sniper_popup_cac_primary",
        )?;
        open_by(c, "SMG", t("MPUI_SUB_MACHINE_GUNS"), "sniper_popup_cac_smg")
    })?;
    // The stock P90 row is a live button only once the profile unlocks the P90 (bit 0 of stat 3014); its click sets
    // the class's primary weapon and attachment and runs `openMenuOnDvar selected_weapon_class SMG
    // sniper_attachment_popup_SMG` (#537).
    c.step("primary weapon: P90", |c| {
        open_by(c, "", t("WEAPON_P90"), "sniper_attachment_popup_smg")
    })?;
    // EXCEPTION (#376): the menu path ends at the attachment popup, so Special Grenades as perk 1 is typed as a
    // `statset` fixture, with no camouflage left as the profile has it. Escape closes the popups the way a player
    // backs out.
    c.step("perk 1, typed", |c| {
        for _ in 0..6 {
            let open = c.lookup("menu.open")?;
            let top = open
                .as_array()
                .and_then(|o| o.last())
                .and_then(Value::as_str);
            if top.is_some_and(|t| t.eq_ignore_ascii_case(HOME)) {
                break;
            }
            c.key("escape", "tap")?;
            c.pause(Duration::from_millis(500))?;
        }
        until_top(c, HOME, Duration::from_secs(5))?;
        c.console(&format!("statset {} {}", PERK1.0, PERK1.1))
    })?;
    c.step("custom class 5 holds the P90 class", |c| {
        stat_is(c, PRIMARY, "the primary weapon")?;
        stat_is(c, ATTACHMENT, "the primary attachment")?;
        stat_is(c, PERK1, "perk 1")
    })
}

fn drive(c: &mut Driven, text: &HashMap<String, String>) -> Result<(), String> {
    build_class(c, text)?;
    c.step("start a server and join with custom class 5", |c| {
        // The class menus lock every weapon above the rank the experience gives (the P90 is a late unlock), so the
        // profile keeps its top-rank experience while the class is built and is given the stage's own only now.
        c.console(&format!("statset {} {}", EXPERIENCE.0, EXPERIENCE.1))?;
        c.console("set ui_netGametypeName war")?;
        c.click("Start New Server")?;
        until_top(c, "createserver", Duration::from_secs(20))?;
        c.click("Start")?;
        until_top(c, "team_marinesopfor", Duration::from_secs(90))?;
        c.click("auto_assign")?;
        until_top(c, "changeclass", Duration::from_secs(30))?;
        c.pause(Duration::from_secs(1))?;
        c.click("P90")?;
        spawn(c, Duration::from_secs(120))?;
        weapon_prefix(c, "p90", Duration::from_secs(20))
    })?;
    c.pause(Duration::from_secs(6))?;
    c.step("custom capture", |c| c.capture("custom").map(|_| ()))?;
    // Within the grace period (`scr_war_graceperiod`) the class menu's choice applies at once.
    c.step("class change in the grace period", |c| {
        c.bind("togglemenu", "tap")?;
        c.pause(Duration::from_secs(1))?;
        c.click("Choose Class")?;
        until_top(c, "changeclass", Duration::from_secs(5))?;
        c.click("Assault")?;
        c.until(
            json!({"menu.open": {"len": 0}}),
            Duration::from_secs(5),
            "the class menu did not close after Assault",
        )?;
        weapon_prefix(c, "m16", Duration::from_secs(20))?;
        c.capture("changed").map(|_| ())
    })?;
    c.step("the profile's perk and experience survived the join", |c| {
        stat_is(c, PERK1, "perk 1 after the join")?;
        stat_is(c, EXPERIENCE, "the experience after the join")
    })
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
    let text = match localized(install, &KEYS) {
        Ok(t) => t,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let mut c = match Driven::start(&client, install, &ctx.dir, &["--bots", "6"]) {
        Ok(c) => c,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let played = drive(&mut c, &text);
    let report = session_report(&mut c, "control-run.json");
    let status = c.finish(Duration::from_secs(30));
    let mut out = StageReport::new(NAME, Status::Passed);
    out.files.push("control-run.json".into());
    for f in ["custom", "changed"] {
        if ctx.dir.join(format!("{f}.png")).is_file() {
            out.files.push(format!("{f}.png"));
        }
    }
    let problem = match (played, report) {
        (Err(e), _) | (Ok(()), Err(e)) => Some(e),
        (Ok(()), Ok(report)) => {
            verdict(&report).or_else(|| sprint_problem(&report["hud"]["sprint_spawn"]))
        }
    }
    .or_else(|| {
        (!status.is_some_and(|s| s.success()))
            .then(|| format!("the client did not exit successfully: {status:?}"))
    });
    match problem {
        Some(w) => {
            out.status = Status::Failed;
            out.reason = Some(w);
        }
        None => out.notes.push(
            "walked Create a Class to custom slot 5's SMG popup, picked the P90 (its attachment popup opened), typed perk 1, spawned with its P90, kept the profile's perk and experience, changed class in the grace period and saw the other players"
                .into(),
        ),
    }
    Ok(out)
}
