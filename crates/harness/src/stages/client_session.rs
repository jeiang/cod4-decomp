// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-session`: the real client gets a person into the world, both ways. Run 1 is the direct
//! `--listen` path (no menus: the default team and class answers); run 2 starts from the main menu and clicks
//! through the stock menus (Start New Server, the server's team menu, the class menu, Assault). Each run must
//! complete every step and report that the local player spawned. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-session";
const LIMIT: Duration = Duration::from_secs(240);

const DIRECT: &[&str] = &["--listen", "--bots", "3"];
const DIRECT_STEPS: &str = "ingame=120,wait=3,shot=direct";
const MENU_STEPS: &str = "click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,\
click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=3,shot=menus";

/// Runs the client with `args` and `--ui-script steps`; returns the parsed `ui-script.json`.
fn run_client(
    client: &Path,
    install: &Path,
    dir: &Path,
    args: &[&str],
    steps: &str,
) -> Result<Value, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log = File::create(dir.join("client.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--size", "1280x720", "--no-sound", "--bots", "3"])
        .args(args)
        .arg("--ui-script")
        .arg(steps)
        .arg("--out")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| e.to_string())?;
    let end = Instant::now() + LIMIT;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("client hung (killed after {} s)", LIMIT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err(format!(
            "client exited with {status} (see {}/client.log)",
            dir.display()
        ));
    }
    let bytes =
        fs::read(dir.join("ui-script.json")).map_err(|e| format!("no ui-script.json: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// Why a run failed, or `None` when every step passed and the player spawned.
fn verdict(report: &Value) -> Option<String> {
    let failed: Vec<String> = report["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["ok"] != Value::Bool(true))
        .map(|s| format!("{} ({})", s["step"], s["note"]))
        .collect();
    if !failed.is_empty() {
        return Some(format!("steps failed: {}", failed.join(", ")));
    }
    if report["net"]["spawned"] != Value::Bool(true) {
        return Some("the player never spawned (left floating as a spectator)".into());
    }
    None
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
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut problems = Vec::new();
    for (label, args, steps) in [
        ("direct", DIRECT, DIRECT_STEPS),
        ("menus", &[][..], MENU_STEPS),
    ] {
        let dir = ctx.dir.join(label);
        match run_client(&client, install, &dir, args, steps) {
            Ok(report) => {
                out.files.push(format!("{label}/ui-script.json"));
                out.files.push(format!("{label}/{label}.png"));
                if let Some(w) = verdict(&report) {
                    problems.push(format!("{label}: {w}"));
                }
                if let Some(v) = report["net"]["snapshots"].as_f64() {
                    out.metrics.insert(format!("{label}.snapshots"), v);
                }
            }
            Err(e) => problems.push(format!("{label}: {e}")),
        }
    }
    out.notes
        .push("direct --listen and the stock-menu path each spawned the local player".into());
    if !problems.is_empty() {
        out.status = Status::Failed;
        out.reason = Some(problems.join("; "));
    }
    Ok(out)
}
