// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-hardpoint`: the real client calls in an airstrike. The player is given it as a killstreak reward
//! (`devhardpoint`), presses `+actionslot 4` (the d-pad slot selects the weapon), picks a point on the
//! full-screen map with the mouse movement and confirms with the attack key. The d-pad pieces (166, 168, 169)
//! and the map pick (186) must have drawn, and the server must have run the strike. Needs a display and the
//! install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-hardpoint";
const LIMIT: Duration = Duration::from_secs(240);
const ARGS: &[&str] = &["--listen", "--bots", "3"];
const STEPS: &str = "ingame=120,wait=2,server=devhardpoint human airstrike_mp,wait=2,shot=dpad,\
set=+actionslot 4,wait=3,shot=pick,set=+attack,wait=0.3,set=-attack,wait=6";

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
    let dir = ctx.dir.join("strike");
    let fail = |r: StageReport, why: String| r.with_status(Status::Failed).with_reason(why);
    let report = match run_client(
        &client,
        install,
        &dir,
        &dir.join("config"),
        ARGS,
        STEPS,
        LIMIT,
    ) {
        Ok(r) => r,
        Err(e) => return Ok(fail(StageReport::new(NAME, Status::Passed), e)),
    };
    let mut out = StageReport::new(NAME, Status::Passed);
    out.files.extend(
        [
            "strike/ui-script.json",
            "strike/dpad.png",
            "strike/pick.png",
        ]
        .map(String::from),
    );
    let strikes = report["objectives"]["airstrikes"].as_u64().unwrap_or(0);
    out.metrics.insert("airstrikes".into(), strikes as f64);
    let mut problems = Vec::new();
    if let Some(steps) = report["steps"].as_array() {
        for s in steps
            .iter()
            .filter(|s| s["ok"] != serde_json::Value::Bool(true))
        {
            problems.push(format!("step {} failed ({})", s["step"], s["note"]));
        }
    }
    for id in ["166", "168", "169", "186"] {
        let n = report["hud"]["drawn"][id].as_u64().unwrap_or(0);
        out.metrics.insert(format!("drawn.{id}"), n as f64);
        if n == 0 {
            problems.push(format!("owner-draw {id} never drew"));
        }
    }
    if strikes == 0 {
        problems.push("the airstrike was never called in".into());
    }
    if !problems.is_empty() {
        return Ok(fail(out, problems.join("; ")));
    }
    Ok(out)
}
