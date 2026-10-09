// SPDX-License-Identifier: GPL-3.0-only
//! `client-fx`: runs `cod4e --fx-selftest` on the real effects. With no window and no GPU it plays stock effects
//! against a stock map and asserts what a match shows: an explosion draws sprites and then ends, a bullet into the
//! floor leaves a decal clipped to it, a shot plays its muzzle flash and ejects a shell, a burst of tracer rounds draws
//! one beam each in the stock tracer material, and the vision and shock files the scripts name are in the install and
//! change the picture. Needs the install; skips cleanly without it.
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;
use std::process::Command;

const NAME: &str = "client-fx";

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = super::client_flythrough::locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let out = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--fx-selftest", "--map", "mp_crash", "--out"])
        .arg(&ctx.dir)
        .output()?;
    let mut r = StageReport::new(
        NAME,
        if out.status.success() {
            Status::Passed
        } else {
            Status::Failed
        },
    );
    r.exit_code = out.status.code();
    r.files.push("fx.json".into());
    if out.status.success() {
        if let Ok(Value::Object(m)) = serde_json::from_slice(&out.stdout) {
            for (k, v) in m {
                if let Some(n) = v.as_f64() {
                    r.metrics.insert(format!("fx.{k}"), n);
                }
            }
        }
        // The burst of five tracer rounds is five beams in flight.
        if r.metrics.get("fx.tracers_in_flight") != Some(&5.0) {
            r.status = Status::Failed;
            r.reason = Some(format!(
                "a burst of five tracer rounds should draw five beams, the report says {:?}",
                r.metrics.get("fx.tracers_in_flight")
            ));
        }
    } else {
        r.reason = Some(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(r)
}
