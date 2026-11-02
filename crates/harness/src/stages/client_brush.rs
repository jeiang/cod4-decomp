// SPDX-License-Identifier: GPL-3.0-only
//! `client-brush`: the real client on a stock map with `script_brushmodel`s (mp_cargoship's cargo hold debris), driven
//! through the control channel (#376). The player joins through the server's team and class menus with the pointer;
//! the server sends the brush models as `BRUSH` entities and the client must hand each one with surfaces to the
//! renderer. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_hardpoint::{join, report_above};
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-brush";
/// `--no-autojoin`: the server's team and class menus are the stage's to answer, with the pointer.
const ARGS: &[&str] = &[
    "--listen",
    "--map",
    "mp_cargoship",
    "--bots",
    "1",
    "--no-autojoin",
    "--duration",
    "300",
];

/// The match, step by step; `Ok` carries what the finished run fell short of.
fn drive(c: &mut Driven, out: &mut StageReport) -> Result<Vec<String>, String> {
    join(c, "auto_assign", "Assault")?;
    // The snapshots after the spawn carry the hold's brush models; a poll that runs out still leaves counts to judge.
    let seen = report_above(c, "brush_models_seen", 0, Duration::from_secs(10))?;
    let drawn = c.lookup("report.brush_models_max")?.as_u64().unwrap_or(0);
    out.metrics
        .insert("brush_models_drawn".into(), drawn as f64);
    out.metrics.insert("brush_models_seen".into(), seen as f64);
    let mut problems = Vec::new();
    // Every `script_brushmodel` the server announced to the client must have reached the renderer.
    if seen == 0 {
        problems.push("the server announced no script_brushmodel to draw".into());
    } else if drawn < seen {
        problems.push(format!(
            "{drawn} of the {seen} script_brushmodels the server announced were drawn"
        ));
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
    let dir = ctx.dir.join("brush");
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &dir, ARGS) {
        Ok(c) => c,
        Err(e) => return Ok(out.with_status(Status::Failed).with_reason(e)),
    };
    let result = drive(&mut c, &mut out);
    // The steps, the player and the run's counters, kept whether or not the run got through.
    let kept = c.report("state.json", &["player", "report", "console"]);
    let status = c.finish(Duration::from_secs(30));
    out.files.push("brush/state.json".into());
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
