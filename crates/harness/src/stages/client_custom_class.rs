// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-custom-class`: a person with a saved profile builds a custom class in the menus and plays from it, driven
//! through the control channel with the player's own pointer, keys and console (#376). Typed at the console, custom
//! class 5 is named `P90` (`customclass5`, as players rename classes) and the profile is given experience (a stat no
//! menu writes, so it stays a typed `statset` fixture). Create a Class > custom slot 5 then sets the class with the
//! stock menus: Primary Weapon > Sub Machine Guns > P90 with no attachment and no camouflage, and Special Grenades as
//! perk 1; the profile's stats (read back with `statGetInDvar`, as a player would) must hold what the menus wrote.
//! The stats reach the server before the person begins, so the class loadout holds the P90, the perk and experience
//! survive the join (the server's stand-ins are never taken for news), a class change in the first seconds of a round
//! applies at once, and every other player the server announces is drawn. Needs a display and the install; skips
//! without.
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

/// What the menus must write for custom class 5 (its stats start at 240): the P90's row of the stats table as the
/// primary weapon (`stat + 1`), no attachment (`stat + 2`) and Special Grenades x 3 as perk 1 (`stat + 5`; the join once
/// replaced it with a stand-in). The experience (`2301`) has no menu, so the stage types it.
const PRIMARY: (u32, i64) = (241, 14);
const ATTACHMENT: (u32, i64) = (242, 0);
const PERK1: (u32, i64) = (245, 176);
const EXPERIENCE: (u32, i64) = (2301, 5000);

/// The localize keys of the menu items the stage clicks that have no name of their own.
const KEYS: [&str; 5] = [
    "MPUI_PERK_1",
    "MPUI_SUB_MACHINE_GUNS",
    "WEAPON_P90",
    "MPUI_NONE",
    "PERKS_SPECIAL_GRENADES_X_3",
];

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

/// Clicks `item` of the top menu and waits for `menu` to come up on top.
fn open_by(c: &mut Driven, item: &str, menu: &str) -> Result<(), String> {
    c.click(item)?;
    until_top(c, menu, Duration::from_secs(10)).map_err(|e| format!("clicking {item}: {e}"))?;
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
    c.step("name custom class 5 and give the profile experience", |c| {
        c.console("set customclass5 P90")?;
        c.console(&format!("statset {} {}", EXPERIENCE.0, EXPERIENCE.1))?;
        let name = c.ask(json!({"cmd": "cvar", "name": "customclass5"}))?;
        if name["value"] != "P90" {
            return Err(format!("customclass5 is {}", name["value"]));
        }
        Ok(())
    })?;
    c.step("Create a Class > custom slot 5", |c| {
        until_top(c, "main", Duration::from_secs(60))?;
        open_by(c, "Create a Class", "pc_cac_popup")?;
        open_by(c, "P90", "menu_cac_sniper")
    })?;
    c.step("primary weapon: P90, no attachment, no camouflage", |c| {
        open_by(c, "primary_weapon_selection", "sniper_popup_cac_primary")?;
        open_by(
            c,
            &format!("SMG|{}", t("MPUI_SUB_MACHINE_GUNS")),
            "sniper_popup_cac_smg",
        )?;
        open_by(
            c,
            &format!("p90|{}", t("WEAPON_P90")),
            "sniper_attachment_popup_smg",
        )?;
        open_by(
            c,
            &format!("none|{}", t("MPUI_NONE")),
            "sniper_popup_cac_camo",
        )?;
        c.click(&format!("camo_none|{}", t("MPUI_NONE")))?;
        until_top(c, "menu_cac_sniper", Duration::from_secs(5))
    })?;
    c.step("perk 1: special grenades", |c| {
        open_by(c, t("MPUI_PERK_1"), "sniper_popup_cac_perk1")?;
        c.click(&format!(
            "specialty_specialgrenade|{}",
            t("PERKS_SPECIAL_GRENADES_X_3")
        ))?;
        until_top(c, "menu_cac_sniper", Duration::from_secs(5))
    })?;
    c.step("the menus wrote custom class 5", |c| {
        stat_is(c, PRIMARY, "the primary weapon")?;
        stat_is(c, ATTACHMENT, "the primary attachment")?;
        stat_is(c, PERK1, "perk 1")?;
        c.key("escape", "tap")?;
        until_top(c, "main", Duration::from_secs(10))
    })
}

fn drive(c: &mut Driven, text: &HashMap<String, String>) -> Result<(), String> {
    build_class(c, text)?;
    c.step("start a server and join with custom class 5", |c| {
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
            "built custom class 5 in Create a Class (P90, no attachment, special grenades), spawned with its P90, kept the profile's perk and experience, changed class in the grace period and saw the other players"
                .into(),
        ),
    }
    Ok(out)
}
