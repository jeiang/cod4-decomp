// SPDX-License-Identifier: GPL-3.0-only
//! `client-load`: loading a map must not freeze the window. The real client starts a match on `mp_crash` and on the
//! largest map zone of the install from a script step (the menus' `Start New Server` path: the map loads on a worker
//! thread while the loading screen draws) and reports the longest gap between two presented frames from the start of
//! the load until the player spawned. The gap must stay under [`MAX_GAP_MS`] (best of [`ATTEMPTS`] runs per map).
//! Needs a display and the install; skips without.
use super::asset_load::mp_zones;
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::path::Path;
use std::time::Duration;

const NAME: &str = "client-load";
/// The longest the window may go without presenting during a map load.
const MAX_GAP_MS: f64 = 100.0;
/// Tries per map; see the loop.
const ATTEMPTS: usize = 3;
/// A load takes seconds; a client that has not spawned by now is hung.
const LIMIT: Duration = Duration::from_secs(90);
const SMALL: &str = "mp_crash";

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
        for attempt in 0..ATTEMPTS {
            let dir = ctx.dir.join(format!("{map}-{attempt}"));
            let steps = format!("start={map},ingame=120,wait=1");
            let report = match run_client(
                &client,
                install,
                &dir,
                &dir.join("config"),
                &[],
                &steps,
                LIMIT,
            ) {
                Ok(r) => r,
                Err(e) => {
                    why = e;
                    continue;
                }
            };
            out.files.push(format!("{map}-{attempt}/ui-script.json"));
            let load = &report["load"];
            let (Some(gap), Some(ms)) =
                (load["max_frame_gap_ms"].as_f64(), load["load_ms"].as_f64())
            else {
                why = "the client reported no load".into();
                continue;
            };
            if report["net"]["spawned"] != serde_json::Value::Bool(true) {
                why = "the player never spawned".into();
                continue;
            }
            out.notes.push(format!(
                "{map} attempt {attempt}: loaded in {ms:.0} ms, longest frame gap {gap:.0} ms, slow frames {}",
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
