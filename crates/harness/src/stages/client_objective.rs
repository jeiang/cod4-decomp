// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-objective`: the real client plays Search and Destroy against bots and plants the bomb. The
//! stage puts the player in the bomb's pickup trigger, then in the zone, and holds +activate: the zone's hint
//! must be drawn (owner-draw 72, shot saved) and the server must count the plant. Needs a display and
//! the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-objective";
const LIMIT: Duration = Duration::from_secs(240);
const ARGS: &[&str] = &["--listen", "--gametype", "sd", "--bots", "4"];
// The attackers' bomb zone and the bomb itself; the player's team may be the defenders, in which case the
// server refuses the move and the run reports it.
const STEPS: &str = "ingame=120,server=set bot_idle 1,wait=1,server=devtele human sd_bomb_pickup,wait=3,\
server=devtele human bombzone,wait=2,set=+activate,wait=2,shot=hint,wait=8,set=-activate,wait=1";

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
    let dir = ctx.dir.join("plant");
    let out = StageReport::new(NAME, Status::Passed);
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
        Err(e) => return Ok(out.with_status(Status::Failed).with_reason(e)),
    };
    let mut out = out;
    out.files.push("plant/ui-script.json".into());
    out.files.push("plant/hint.png".into());
    let plants = report["objectives"]["plants"].as_u64().unwrap_or(0);
    let drawn = report["hud"]["drawn"]["72"].as_u64().unwrap_or(0);
    out.metrics.insert("plants".into(), plants as f64);
    out.metrics.insert("hint_draws".into(), drawn as f64);
    let mut problems = Vec::new();
    if let Some(steps) = report["steps"].as_array() {
        for s in steps
            .iter()
            .filter(|s| s["ok"] != serde_json::Value::Bool(true))
        {
            problems.push(format!("step {} failed ({})", s["step"], s["note"]));
        }
    }
    if drawn == 0 {
        problems.push("the cursor hint (owner-draw 72) never drew".into());
    }
    if plants == 0 {
        problems.push("the bomb was never planted".into());
    }
    if !problems.is_empty() {
        return Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")));
    }
    Ok(out)
}
