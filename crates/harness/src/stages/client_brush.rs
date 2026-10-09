// SPDX-License-Identifier: GPL-3.0-only
//! `client-brush`: the real client on a stock map with `script_brushmodel`s (mp_cargoship's cargo hold debris). The
//! server sends them as `BRUSH` entities and the client must hand each one with surfaces to the renderer. Needs a
//! display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-brush";
const LIMIT: Duration = Duration::from_secs(300);
const ARGS: &[&str] = &["--listen", "--map", "mp_cargoship", "--bots", "1"];
const STEPS: &str = "ingame=120,wait=5";

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
    let dir = ctx.dir.join("brush");
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
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let mut out = StageReport::new(NAME, Status::Passed);
    out.files.push("brush/ui-script.json".into());
    let drawn = report["net"]["brush_models_max"].as_u64().unwrap_or(0);
    out.metrics
        .insert("brush_models_drawn".into(), drawn as f64);
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
        problems.push("the client drew no brush model on a map that has them".into());
    }
    if !problems.is_empty() {
        return Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")));
    }
    Ok(out)
}
