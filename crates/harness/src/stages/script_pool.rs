// SPDX-License-Identifier: GPL-3.0-or-later
//! `script-pool`: a long headless Search and Destroy match and a long team deathmatch with map
//! rotations, with bots. The script variable pool (the original's 0xFFFE-value limit) must
//! return to the same level every round: what a round leaves behind is what ends a server
//! that runs for days (ticket #141: about 3,100 values per round, the match failing after 15).
use crate::stage::{StageCtx, StageReport, Status};
use server::server::Server;
use std::io;

const NAME: &str = "script-pool";
const BOTS: &str = "12";
/// Rounds of Search and Destroy.
const SD_ROUNDS: u32 = 32;
/// Level loads of the rotation: each of the two maps comes up this many times.
const TDM_LOADS: u32 = 12;
/// One simulated second at the server's 30 Hz.
const SECOND: u32 = 30;
/// Longest one level may run, in simulated seconds: a round lasts under a minute, so only a
/// game that cannot progress (a full pool stops it) takes this long.
const LEVEL_LIMIT: u32 = 20 * 60;
/// What a round may differ from the reference, in percent: kills and plants change a little.
const SLACK_PERCENT: u32 = 15;
/// Rounds the reference is taken from, after the first (which carries the boot's state).
const REFERENCE: std::ops::Range<usize> = 1..4;

/// Pool usage at the end of every level: the last sample before the next level loaded, with
/// the map it was on.
fn levels(server: &mut Server, loads: u32) -> Result<Vec<(String, u32, u32)>, String> {
    let mut ends = Vec::new();
    let mut last = (String::new(), 0, 0);
    let mut seen = server.level_loads;
    let mut since = 0;
    while since < LEVEL_LIMIT {
        server.run_frames(SECOND);
        since += 1;
        if let Some((_, e)) = server.all_script_errors.first() {
            return Err(format!("after {} levels: {e}", ends.len()));
        }
        if server.level_loads != seen {
            since = 0;
            seen = server.level_loads;
            ends.push(last.clone());
            if seen > loads {
                return Ok(ends);
            }
        }
        let (o, v) = Server::script_pool();
        last = (server.map_name().unwrap_or("?").to_owned(), o, v);
    }
    Err(format!(
        "level {} ran {LEVEL_LIMIT} simulated seconds without ending",
        ends.len()
    ))
}

/// Checks `ends` of one map against its own reference; returns the failure, if any.
fn drift(what: &str, ends: &[(u32, u32)]) -> Option<String> {
    let want = ends.get(REFERENCE)?;
    let (o, v) = want
        .iter()
        .fold((0, 0), |a, e| (a.0.max(e.0), a.1.max(e.1)));
    let (po, pv) = (o + o * SLACK_PERCENT / 100, v + v * SLACK_PERCENT / 100);
    ends.iter().enumerate().skip(REFERENCE.end).find_map(|(i, &(eo, ev))| {
        (eo > po || ev > pv).then(|| {
            format!(
                "{what}: level {i} ends with {eo} objects and {ev} values; levels {REFERENCE:?} peaked at {o} and {v}, \
                 and {SLACK_PERCENT}% over is {po} and {pv}"
            )
        })
    })
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let mut report = StageReport::new(NAME, Status::Passed);
    let fail = |report: &mut StageReport, why: String| {
        report.status = Status::Failed;
        report.reason.get_or_insert_with(|| why.clone());
        report.notes.push(why);
    };
    let sets = |extra: &[&str]| -> Vec<String> {
        ["+set", "net_port", "0"]
            .into_iter()
            .chain(extra.iter().copied())
            .map(String::from)
            .collect()
    };
    let phases: [(&str, u32, Vec<String>); 2] = [
        (
            "sd",
            SD_ROUNDS,
            // Search and Destroy's time limit is the round's length (minutes); rounds restart
            // the map until a limit that is off here.
            sets(&[
                "+set",
                "g_gametype",
                "sd",
                "+set",
                "scr_sd_timelimit",
                "0.5",
                "+set",
                "scr_sd_roundlimit",
                "0",
                "+set",
                "scr_sd_scorelimit",
                "0",
                "+set",
                "scr_sd_winlimit",
                "0",
                "+set",
                "scr_sd_roundswitch",
                "0",
                "+map",
                "mp_crash",
            ]),
        ),
        (
            "tdm",
            TDM_LOADS,
            sets(&[
                "+set",
                "g_gametype",
                "war",
                "+set",
                "scr_war_timelimit",
                "0.5",
                "+set",
                "scr_war_scorelimit",
                "0",
                "+set",
                "sv_mapRotation",
                "gametype war map mp_crash gametype war map mp_backlot",
                "+map",
                "mp_crash",
            ]),
        ),
    ];
    for (name, loads, args) in phases {
        let mut server = match Server::boot(install, &args, false) {
            Ok(s) => s,
            Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
        };
        if let Err(e) = server.exec_line(&format!("bots {BOTS}")) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(e));
        }
        let ends = match levels(&mut server, loads) {
            Ok(e) => e,
            Err(e) => {
                fail(&mut report, format!("{name}: {e}"));
                continue;
            }
        };
        if !server.all_script_errors.is_empty() {
            fail(
                &mut report,
                format!(
                    "{name}: {} script runtime errors, the first: {}",
                    server.all_script_errors.len(),
                    server.all_script_errors[0].1
                ),
            );
        }
        report
            .metrics
            .insert(format!("{name}.levels"), ends.len() as f64);
        for (i, (m, o, v)) in ends.iter().enumerate() {
            report
                .notes
                .push(format!("{name} level {i} ({m}): {o} objects, {v} values"));
        }
        // Each map is compared with its own earlier levels.
        let mut maps: Vec<&str> = ends.iter().map(|e| e.0.as_str()).collect();
        maps.sort_unstable();
        maps.dedup();
        for m in maps {
            let of: Vec<(u32, u32)> = ends
                .iter()
                .filter(|e| e.0 == m)
                .map(|e| (e.1, e.2))
                .collect();
            if let Some(&(_, v)) = of.last() {
                report
                    .metrics
                    .insert(format!("{name}.{m}.values_last"), f64::from(v));
            }
            if let Some(&(_, v)) = of.get(REFERENCE.start) {
                report
                    .metrics
                    .insert(format!("{name}.{m}.values_first"), f64::from(v));
            }
            if let Some(why) = drift(&format!("{name} on {m}"), &of) {
                fail(&mut report, why);
            }
        }
        drop(server);
        // The level's whole pool is released with the level: nothing outlives the server.
        let (o, v) = Server::script_pool();
        report
            .metrics
            .insert(format!("{name}.pool_after_shutdown_values"), f64::from(v));
        if v > 100 {
            fail(
                &mut report,
                format!("{name}: {o} objects and {v} values outlive the server"),
            );
        }
    }
    Ok(report)
}
