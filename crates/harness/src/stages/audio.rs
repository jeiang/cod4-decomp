// SPDX-License-Identifier: GPL-3.0-or-later
//! `audio`: runs `cod4e --audio-selftest` on the real alias tables. With no window and no sound card it plays
//! stock sounds through the mixer and asserts what a match depends on: a shot on the left is louder in the
//! left ear and a shot on the right in the right, volume falls with distance, a sound beyond its range does not
//! start, a channel never exceeds its voice cap, footsteps are positioned, and the map's ambience and music
//! stream out of the IWDs. Needs the install; skips cleanly without it.
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;
use std::process::Command;

const NAME: &str = "audio";

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
        .args(["--audio-selftest", "--map", "mp_crossfire", "--out"])
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
    r.files.push("audio.json".into());
    if out.status.success() {
        if let Ok(Value::Object(m)) = serde_json::from_slice(&out.stdout) {
            for (k, v) in m {
                if let Some(n) = v.as_f64() {
                    r.metrics.insert(format!("audio.{k}"), n);
                }
            }
        }
    } else {
        r.reason = Some(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(r)
}
