// SPDX-License-Identifier: GPL-3.0-only
//! Runs `cod4e --input-selftest`: the client's input layer driven by synthetic events, pass/fail. Needs neither a
//! display, a GPU nor the install, and has no timing in it.
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::process::Command;

const NAME: &str = "client-input";
/// The checks the selftest prints (about 20 today): fewer pass lines than this means it barely ran.
const MIN_CHECKS: usize = 10;

pub fn run(_: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = super::client_flythrough::locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let out = Command::new(client).arg("--input-selftest").output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let passed = stdout.lines().filter(|l| l.starts_with("pass ")).count();
    let failed: Vec<&str> = stdout.lines().filter(|l| l.starts_with("FAIL ")).collect();
    let mut r = StageReport::new(
        NAME,
        if out.status.success() {
            Status::Passed
        } else {
            Status::Failed
        },
    );
    r.exit_code = out.status.code();
    r.metrics.insert("checks_passed".into(), passed as f64);
    r.metrics
        .insert("checks_failed".into(), failed.len() as f64);
    r.notes = stdout
        .lines()
        .filter(|l| l.starts_with("raw mouse"))
        .map(str::to_owned)
        .collect();
    if !out.status.success() {
        r.reason = Some(format!(
            "{} ({})",
            failed.join("; "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    } else if passed < MIN_CHECKS {
        // The selftest prints one line per check and exits 0 when none failed, so a run that printed no `pass`
        // line (or almost none) exited 0 having checked nothing (#375).
        r.status = Status::Failed;
        r.reason = Some(format!(
            "the selftest passed only {passed} of the {MIN_CHECKS}+ checks it has"
        ));
    }
    Ok(r)
}
