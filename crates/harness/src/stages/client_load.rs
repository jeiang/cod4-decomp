// SPDX-License-Identifier: GPL-3.0-only
//! `client-load`: loading a map must not freeze the window. The real client, driven through the control channel
//! (#378) as a player plays it (#376), starts a match on `mp_crash` and on the largest map zone of the install with
//! `map` typed at the console (the map loads on a worker thread while the loading screen draws), and the player
//! answers the team and class menus with the pointer. The client's own record of the load (`client.load`) gives the
//! longest gap between two presented frames from the start of the load until the player spawned. The gap must stay
//! under [`MAX_GAP_MS`] (best of [`ATTEMPTS`] runs per map). Needs a display and the install; skips without.
use super::asset_load::mp_zones;
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-load";
/// The longest the window may go without presenting during a map load.
const MAX_GAP_MS: f64 = 100.0;
/// Tries per map; see the loop.
const ATTEMPTS: usize = 3;
/// A load takes seconds; a client that has not spawned this long after the map was typed is hung.
const LIMIT: Duration = Duration::from_secs(90);
const SMALL: &str = "mp_crash";
/// The listen server's menus the player answers, and the flags of each run beside the harness's.
const TEAM_MENU: &str = "team_marinesopfor";
const CLASS_MENU: &str = "changeclass";
const ARGS: &[&str] = &["--bots", "3"];

/// The biggest `mp_*` map zone (not its `_load` screen zone), by file size.
fn largest_map(install: &Path) -> Option<String> {
    mp_zones(install)
        .ok()?
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            let name_ok = stem.starts_with("mp_") && !stem.ends_with("_load");
            name_ok.then(|| (p.metadata().map_or(0, |m| m.len()), stem))
        })
        .max()
        .map(|(_, stem)| stem)
}

/// The load of `map` as the player starts it: `map` typed at the console of the main menu, the team and class menus
/// answered with the pointer, and the client's record of the load once the player spawned.
fn load(c: &mut Driven, map: &str) -> Result<Value, String> {
    let secs = Duration::from_secs;
    c.step("main menu", |c| {
        c.until(
            json!({"menu.open": {"contains": "main"}}),
            secs(90),
            "the main menu did not open",
        )
        .map(drop)
    })?;
    let typed = Instant::now();
    let left = || LIMIT.saturating_sub(typed.elapsed());
    c.step(&format!("map {map} at the console"), |c| {
        c.console(&format!("map {map}"))
    })?;
    c.step("team menu", |c| {
        c.until(
            json!({"menu.open": {"contains": TEAM_MENU}}),
            left(),
            "the team menu did not open after the load",
        )?;
        c.click("auto_assign")?;
        c.until(
            json!({"menu.open": {"contains": CLASS_MENU}}),
            secs(30),
            "the class menu did not open after the team was picked",
        )
        .map(drop)
    })?;
    c.step("class menu", |c| {
        c.pause(secs(1))?;
        c.click("Assault").map(drop)
    })?;
    c.step("spawn", |c| {
        // The client closes its record of the load on the frame the player spawns.
        c.until(
            json!({
                "net.spawned": true,
                "player.alive": true,
                "client.load.max_frame_gap_ms": {"exists": true},
            }),
            left(),
            "the player did not spawn",
        )?;
        let load = c.lookup("client.load")?;
        if load["map"] != json!(map) {
            return Err(format!("the client's last load was not of {map}: {load}"));
        }
        Ok(load)
    })
}

/// One attempt at `map` with a fresh client, its steps written to `control-run.json`: the client's record of the
/// load, or what went wrong.
fn attempt(client: &Path, install: &Path, dir: &Path, map: &str) -> Result<Value, String> {
    let mut c = Driven::start(client, install, dir, ARGS)?;
    let played = load(&mut c, map);
    let written = c.report("control-run.json", &["client", "net"]);
    let status = c.finish(Duration::from_secs(30));
    let load = played?;
    written.map_err(|e| format!("the run's report could not be written: {e}"))?;
    match status {
        Some(s) if s.success() => Ok(load),
        Some(s) => Err(format!("the client exited {s}")),
        None => Err("the client did not quit and was killed".into()),
    }
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
    let mut maps = vec![SMALL.to_owned()];
    if let Some(big) = largest_map(install)
        && big != SMALL
    {
        maps.push(big);
    }
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut problems = Vec::new();
    for map in &maps {
        // A shared machine stalls a window now and then whatever the client does; a stall of the client's own making
        // (a lock held, a first frame that builds a world) is there in every attempt. The best attempt counts.
        let mut best: Option<(f64, f64)> = None;
        let mut why = String::new();
        for attempt_no in 0..ATTEMPTS {
            let dir = ctx.dir.join(format!("{map}-{attempt_no}"));
            let result = attempt(&client, install, &dir, map);
            if dir.join("control-run.json").is_file() {
                out.files
                    .push(format!("{map}-{attempt_no}/control-run.json"));
            }
            let load = match result {
                Ok(l) => l,
                Err(e) => {
                    why = e;
                    continue;
                }
            };
            let (Some(gap), Some(ms)) =
                (load["max_frame_gap_ms"].as_f64(), load["load_ms"].as_f64())
            else {
                why = format!("the client reported no load: {load}");
                continue;
            };
            out.notes.push(format!(
                "{map} attempt {attempt_no}: loaded in {ms:.0} ms, longest frame gap {gap:.0} ms, slow frames {}",
                load["slow_gaps_ms"]
            ));
            if best.is_none_or(|(g, _)| gap < g) {
                best = Some((gap, ms));
            }
            if gap <= MAX_GAP_MS {
                break;
            }
        }
        match best {
            Some((gap, ms)) => {
                out.metrics.insert(format!("{map}.load_ms"), ms);
                out.metrics.insert(format!("{map}.max_frame_gap_ms"), gap);
                if gap > MAX_GAP_MS {
                    problems.push(format!(
                        "{map}: the window stalled {gap:.0} ms during the load in every attempt (limit {MAX_GAP_MS:.0})"
                    ));
                }
            }
            None => problems.push(format!("{map}: {why}")),
        }
    }
    if !problems.is_empty() {
        out.status = Status::Failed;
        out.reason = Some(problems.join("; "));
    }
    Ok(out)
}
