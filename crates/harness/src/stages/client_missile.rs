// SPDX-License-Identifier: GPL-3.0-only
//! `client-missile`: the real client sees a thrown grenade. The player throws a frag (`devgrenade`); the client must
//! draw its projectile model while it flies and rests, receive its explosion event and play an explosion sound
//! cue. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-missile";
const LIMIT: Duration = Duration::from_secs(300);
const ARGS: &[&str] = &["--listen", "--bots", "1"];
// A 1.5 s fuse: the grenade is in the air and on the floor for a while, then blows.
const STEPS: &str = "ingame=120,wait=2,server=devgrenade human frag_grenade_mp 500 1500,wait=5";

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
    let dir = ctx.dir.join("missile");
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
        Err(e) => {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(e));
        }
    };
    let mut out = StageReport::new(NAME, Status::Passed);
    out.files.push("missile/ui-script.json".into());
    let count = |v: &serde_json::Value| v.as_u64().unwrap_or(0);
    let drawn = count(&report["net"]["projectiles_max_drawn"]);
    let blasts = count(&report["net"]["events"]["explosion"]);
    let sound_ready = report["sound"]["ready"] == serde_json::Value::Bool(true);
    let cues = count(&report["sound"]["explosion_cues"]);
    out.metrics.insert("projectiles_drawn".into(), drawn as f64);
    out.metrics.insert("explosion_events".into(), blasts as f64);
    out.metrics.insert("explosion_cues".into(), cues as f64);
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
        problems.push("the client drew no projectile model for the thrown grenade".into());
    }
    if blasts == 0 {
        problems.push("the client never received the grenade's explosion".into());
    }
    if sound_ready && cues == 0 {
        problems.push("the explosion made no sound cue".into());
    }
    if !problems.is_empty() {
        return Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")));
    }
    Ok(out)
}
