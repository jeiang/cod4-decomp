// SPDX-License-Identifier: GPL-3.0-only
//! `client-heli`: the real client sees a helicopter, driven through the control channel (#376). The player joins
//! through the server's team and class menus with the pointer. A bot is given the helicopter reward (`devhardpoint`,
//! typed at the host's console as setup) and calls it in itself; once the server counts it, the player is held behind
//! the helicopter looking at it (`devheli view`, setup: a camera no player can take) and a screenshot is saved. The
//! client must have drawn at least one `VEHICLE` entity, and the picture must differ from the same view with vehicles
//! left out (`drawvehicles 0`, typed at the console). The HUD must have known an enemy helicopter (the compass mark
//! itself, owner-draw 155/156, is only placed in the unused `compass_old` by the stock menus and is refused by the
//! owner-draw allow-list: #371), and once the player is given radar (`devradar`, setup) it must have swept and shown
//! the enemies it caught. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_hardpoint::{join, server_stat, until_stat};
use super::client_models::{decode, no_display};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-heli";
/// `--no-autojoin`: the server's team and class menus are the stage's to answer, with the pointer.
const ARGS: &[&str] = &[
    "--listen",
    "--bots",
    "3",
    "--no-autojoin",
    "--duration",
    "300",
];
/// Share of the picture that must differ with the helicopter in it, and by how much per channel.
const MIN_CHANGED: f64 = 0.002;
const CHANNEL_DELTA: i32 = 24;
/// How near the chase camera holds the player to the helicopter: `devheli view` floats them 700 units behind it and
/// 120 below, and the snapshots trail the helicopter's flight a little.
const VIEW_REACH: f64 = 1000.0;

/// The distance between two `[x, y, z]` arrays of the state document.
fn distance(a: &Value, b: &Value) -> Option<f64> {
    let (a, b) = (a.as_array()?, b.as_array()?);
    let mut sum = 0.0;
    for i in 0..3 {
        let d = a.get(i)?.as_f64()? - b.get(i)?.as_f64()?;
        sum += d * d;
    }
    Some(sum.sqrt())
}

/// Waits up to `limit` for the player to be within [`VIEW_REACH`] of a vehicle the newest snapshot carries; returns
/// the distance.
fn near_vehicle(c: &mut Driven, limit: Duration) -> Result<f64, String> {
    let end = Instant::now() + limit;
    loop {
        let me = c.lookup("player.origin")?;
        let entities = c.state("entities")?;
        let nearest = entities["list"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["kind"] == "vehicle")
            .filter_map(|e| distance(&me, &e["origin"]))
            .min_by(f64::total_cmp);
        if let Some(d) = nearest.filter(|d| *d < VIEW_REACH) {
            return Ok(d);
        }
        if Instant::now() > end {
            return Err(match nearest {
                Some(d) => format!(
                    "the player stayed {d:.0} units from the nearest helicopter (want under {VIEW_REACH:.0})"
                ),
                None => "the snapshots carried no helicopter".to_owned(),
            });
        }
        c.pause(Duration::from_millis(200))?;
    }
}

/// The share of pixels of `a` that differ from `b` by more than [`CHANNEL_DELTA`] in some channel.
fn changed_share(a: &Path, b: &Path) -> Result<f64, String> {
    let (a, b) = (decode(a)?, decode(b)?);
    if (a.0, a.1) != (b.0, b.1) {
        return Err("the two screenshots differ in size".into());
    }
    let changed =
        a.2.as_chunks::<3>()
            .0
            .iter()
            .zip(b.2.as_chunks::<3>().0.iter())
            .filter(|(p, q)| {
                p.iter()
                    .zip(q.iter())
                    .any(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() > CHANNEL_DELTA)
            })
            .count();
    Ok(changed as f64 / (a.0 as usize * a.1 as usize) as f64)
}

/// The match, step by step; `Ok` carries what the finished run fell short of.
fn drive(c: &mut Driven, out: &mut StageReport) -> Result<Vec<String>, String> {
    let secs = Duration::from_secs;
    join(c, "auto_assign", "Assault")?;
    // Setup, not play: a bot is given the helicopter reward as its kill streak would give it (`devhardpoint`, typed at
    // the host's console). The bot's own brain selects its action slot 4 weapon some seconds after spawning when it is
    // not fighting, and calls the helicopter in; the server must count it.
    c.step("a bot calls the helicopter in (setup)", |c| {
        c.console("devhardpoint bot helicopter_mp")?;
        until_stat(c, "helicopters", 1, secs(120)).map(|_| ())
    })?;
    c.pause(secs(2))?;
    // Setup: the player is held behind the helicopter, looking at it, every frame (`devheli view`): a camera no player
    // can take. The snapshots must then put the player beside the helicopter they carry.
    let reach = c.step("chase camera behind the helicopter (setup)", |c| {
        c.console("devheli view human")?;
        near_vehicle(c, secs(10))
    })?;
    out.notes.push(format!(
        "the chase camera held the player {reach:.0} units from the helicopter"
    ));
    c.pause(secs(1))?;
    let heli = c.capture("heli")?;
    c.step("drawvehicles 0 at the console", |c| {
        c.console("drawvehicles 0")
    })?;
    c.pause(secs(1))?;
    let bare = c.capture("bare")?;
    c.pause(secs(1))?;
    // Setup: the bots pick a stealth class, which carries the GPS jammer (`specialty_gpsjammer`, bit 0 of `ps.perks`): a
    // jammed player never shows on the radar in the stock game either, so the perk is cleared off the bots (`devperk`),
    // and the player is given a UAV's radar without the streak that earns one (`devradar`). The sweep must then catch
    // and mark the enemies: the checks below.
    c.step("radar on (setup)", |c| {
        c.console("devperk bot 0")?;
        c.console("devradar human 1")?;
        // A wait that runs out still leaves counts to judge; the checks below name what fell short.
        c.wait(
            &json!({
                "hud.report.radar_lines": {"gt": 0},
                "hud.report.radar_pings": {"gt": 0},
                "hud.report.radar_marks": {"gt": 0},
            }),
            secs(15),
        )?;
        Ok(())
    })?;
    let count = |v: &Value| v.as_u64().unwrap_or(0);
    let helicopters = server_stat(c, "helicopters")?;
    let shots = server_stat(c, "heli_shots")?;
    let report = c.state("report")?;
    let hud = c.state("hud")?;
    let drawn = count(&report["vehicles_max_drawn"]);
    out.metrics.insert("helicopters".into(), helicopters as f64);
    out.metrics.insert("heli_shots".into(), shots as f64);
    out.metrics.insert("vehicles_drawn".into(), drawn as f64);
    let mut problems = Vec::new();
    // The HUD's vehicle list, which the compass marks are drawn from (owner-draw 155/156: not drawn yet, the menu
    // places them only in the unused `compass_old` and the allow-list refuses them, #371) must have held the
    // helicopter, and with the player's radar on the sweep must have caught and marked the enemies it passed.
    for (key, what) in [
        ("vehicles_max", "the HUD never knew of the helicopter"),
        ("radar_lines", "the minimap never drew the radar sweep"),
        ("radar_pings", "the radar sweep never caught an enemy"),
        (
            "radar_marks",
            "the minimap never marked an enemy the radar caught",
        ),
    ] {
        let n = count(&hud["report"][key]);
        out.metrics.insert(key.into(), n as f64);
        if n == 0 {
            problems.push(what.into());
        }
    }
    if drawn == 0 {
        problems.push(format!(
            "the client drew no vehicle (unloaded models: {})",
            report["vehicles_unloaded"]
        ));
    }
    match changed_share(&heli, &bare) {
        Ok(share) => {
            out.metrics.insert("changed_pixel_share".into(), share);
            out.notes.push(format!(
                "the helicopter changes {:.2}% of the picture",
                share * 100.0
            ));
            if share < MIN_CHANGED {
                problems.push(format!(
                    "the picture changed by only {:.3}% without vehicles (need {:.1}%): is the helicopter in view?",
                    share * 100.0,
                    MIN_CHANGED * 100.0
                ));
            }
        }
        Err(e) => problems.push(e),
    }
    Ok(problems)
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
    let dir = ctx.dir.join("heli");
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => return Ok(out.with_status(Status::Failed).with_reason(e)),
    };
    let result = drive(&mut c, &mut out);
    // The steps and what the HUD, the run's counters and the console held, kept whether or not the run got through.
    let kept = c.report("state.json", &["player", "hud", "report", "console"]);
    let status = c.finish(Duration::from_secs(30));
    out.files
        .extend(["heli/state.json", "heli/heli.png", "heli/bare.png"].map(String::from));
    let mut problems = match result {
        Ok(problems) => problems,
        Err(e) => vec![e],
    };
    if let Err(e) = kept {
        problems.push(format!("the run's state was not kept: {e}"));
    }
    if !status.is_some_and(|s| s.success()) {
        problems.push(format!("the client did not exit successfully: {status:?}"));
    }
    if !problems.is_empty() {
        return Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")));
    }
    Ok(out)
}
