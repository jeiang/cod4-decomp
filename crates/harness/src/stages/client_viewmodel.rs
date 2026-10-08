// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-viewmodel`: runs `cod4e --viewmodel-tour` on three stock maps. The first-person weapon and hands are drawn
//! headless from the spawn points and the paths between them, with and without, and the pixels they change must not
//! be dominated by one colour channel (a wrong reflection probe once painted them solid red) and the spawn views must
//! be neither black nor blown out. Needs the install and a GPU adapter; skips cleanly without.
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::process::Command;

const NAME: &str = "client-viewmodel";
const MAPS: [&str; 3] = ["mp_backlot", "mp_crash", "mp_strike"];
const VIEWS: &str = "20";

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = super::client_flythrough::locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let mut r = StageReport::new(NAME, Status::Passed);
    let mut problems = Vec::new();
    for map in MAPS {
        let dir = ctx.dir.join(map);
        let out = Command::new(&client)
            .arg("--install")
            .arg(install)
            .args(["--viewmodel-tour", VIEWS, "--map", map, "--out"])
            .arg(&dir)
            .output()?;
        let err = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() && err.contains("no suitable GPU adapter") {
            return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no GPU adapter"));
        }
        r.files.push(format!("{map}/viewmodel.json"));
        if !out.status.success() {
            problems.push(err.trim().to_owned());
        }
    }
    if !problems.is_empty() {
        r.status = Status::Failed;
        r.reason = Some(problems.join("\n"));
    }
    Ok(r)
}
