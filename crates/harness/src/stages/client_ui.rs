// SPDX-License-Identifier: GPL-3.0-only
//! `client-ui`: the client's `--ui-tour` opens the stock menus one after another (front end, server setup, options,
//! the script menus a match opens) with no world, saves a screenshot of each and a report. The stage passes when
//! every menu opens, draws a meaningful share of the picture (fonts, localized text and images resolved) and no UI
//! image is missing. Needs a display and the install; skips cleanly without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-ui";
const LIMIT: Duration = Duration::from_secs(240);
/// Menus that are mostly backdrop must light at least this share of the screenshot.
const MIN_LIT: f64 = 0.02;
/// Menus that must exist in the report: the front end and the script menus of a match.
const REQUIRED: &[&str] = &[
    "main",
    "createserver",
    "team_marinesopfor",
    "class",
    "scoreboard",
];

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
    let dir = ctx.dir.join("ui");
    fs::create_dir_all(&dir)?;
    let log = File::create(dir.join("client.log"))?;
    let mut child = Command::new(&client)
        .arg("--install")
        .arg(install)
        .args(["--size", "1280x720", "--ui-tour"])
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let end = Instant::now() + LIMIT;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(StageReport::new(NAME, Status::Failed)
                .with_reason(format!("client hung (killed after {} s)", LIMIT.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Ok(StageReport::new(NAME, Status::Failed)
            .with_reason(format!("client exited with {status} (see ui/client.log)")));
    }
    let report: Value = match fs::read(dir.join("ui.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            return Ok(
                StageReport::new(NAME, Status::Failed).with_reason(format!("no ui.json: {e}"))
            );
        }
    };
    let mut out = StageReport::new(NAME, Status::Passed);
    let menus = report["menus"].as_array().cloned().unwrap_or_default();
    let mut bad = Vec::new();
    let mut lit_min = f64::MAX;
    for m in &menus {
        let name = m["menu"].as_str().unwrap_or("?");
        let lit = m["lit_fraction"].as_f64().unwrap_or(0.0);
        lit_min = lit_min.min(lit);
        if m["open"] != Value::Bool(true) || lit < MIN_LIT || m.get("error").is_some() {
            bad.push(format!(
                "{name} (open {}, lit {:.1}%)",
                m["open"],
                lit * 100.0
            ));
        }
        // The tour sets the join menu's tab colour with `setitemcolor` and reports whether the items took it.
        if m["setitemcolor"] == Value::Bool(false) {
            bad.push(format!("{name} (setitemcolor backcolor not applied)"));
        }
        if let Some(f) = m["screenshot"].as_str() {
            out.files.push(format!("ui/{f}"));
        }
    }
    for r in REQUIRED {
        if !menus.iter().any(|m| m["menu"] == *r) {
            bad.push(format!("{r} missing from the report"));
        }
    }
    let missing = report["missing_images"].as_array().map_or(0, Vec::len);
    out.metrics.insert("menus".into(), menus.len() as f64);
    out.metrics.insert(
        "lit_min".into(),
        if lit_min == f64::MAX { 0.0 } else { lit_min },
    );
    out.metrics.insert("missing_images".into(), missing as f64);
    out.files.push("ui/ui.json".into());
    out.notes.push(format!(
        "{} menus drawn, {missing} UI images missing",
        menus.len()
    ));
    if !bad.is_empty() || missing > 0 {
        out.status = Status::Failed;
        out.reason = Some(format!(
            "menus that did not draw: {}; missing images: {}",
            bad.join(", "),
            report["missing_images"]
        ));
    }
    Ok(out)
}
