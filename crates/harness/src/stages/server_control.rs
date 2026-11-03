// SPDX-License-Identifier: GPL-3.0-only
//! `server-control`: the real headless server's dev control channel (#378). The channel is off unless `--control`
//! asks for it; this run asks, reads the live map and cvars, adds bots through the server's console, waits for and
//! asserts their authoritative client state, checks script errors, and quits through the same JSON connection.
use super::client_dedicated::{Dedicated, locate_server};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::json;
use std::fs;
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "server-control";

fn drive(c: &mut Driven, report: &mut StageReport) -> Result<(), String> {
    let ping = c.ask(json!({"cmd": "ping"}))?;
    if ping["server"].as_str() != Some("cod4e-server") {
        return Err(format!("the dedicated server did not answer ping: {ping}"));
    }
    let map = c.state("map")?;
    if map["map"].as_str() != Some("mp_crash") || map["running"].as_bool() != Some(true) {
        return Err(format!("the server is not running mp_crash: {map}"));
    }
    let public = c.ask(json!({"cmd": "cvar", "name": "sv_public"}))?;
    if public["value"].is_null() {
        return Err(format!("the server did not report sv_public: {public}"));
    }

    c.ask(json!({"cmd": "exec", "line": "bots 3"}))?;
    c.until(
        json!({"clients.clients.2": {"exists": true}}),
        Duration::from_secs(120),
        "three bots did not arrive on the dedicated server",
    )?;
    let clients = c.state("clients")?;
    let listed = clients["clients"].as_array().cloned().unwrap_or_default();
    if listed.len() != 3
        || listed
            .iter()
            .any(|client| client["bot"].as_bool() != Some(true))
    {
        return Err(format!(
            "the authoritative client list is not three bots: {clients}"
        ));
    }
    report
        .metrics
        .insert("clients".to_owned(), listed.len() as f64);
    c.ask(json!({"cmd": "assert", "state": {"clients.clients": {"len": 3}}}))?;

    let script = c.state("script")?;
    let errors = script["errors"]
        .as_array()
        .map_or(u64::MAX, |list| list.len() as u64);
    report
        .metrics
        .insert("script_errors".to_owned(), errors as f64);
    if errors != 0 {
        return Err(format!("the server reported {errors} script errors"));
    }
    c.ask(json!({"cmd": "quit"}))?;
    Ok(())
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if !install.is_dir() {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    }
    let Some(binary) = locate_server() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e-server binary not found next to the harness"));
    };

    let dir = ctx.dir.join("server");
    let log = dir.join("server.log");
    let addr_file = dir.join("control.addr");
    fs::create_dir_all(&dir)?;
    let _ = fs::remove_file(&addr_file);
    let mut report = StageReport::new(NAME, Status::Passed);
    let (server, _) = match Dedicated::boot_with_control(&binary, install, &dir, &log, &addr_file) {
        Ok(started) => started,
        Err(e) => return Ok(report.with_status(Status::Failed).with_reason(e)),
    };
    let outcome = (|| {
        // The server binds its channel after the map loads, which can be after `boot_with_control` returns.
        let deadline = Instant::now() + Duration::from_secs(30);
        let addr = loop {
            if let Ok(s) = fs::read_to_string(&addr_file)
                && let Some(line) = s.lines().next().filter(|l| l.contains(':'))
            {
                break line.trim().to_owned();
            }
            if Instant::now() > deadline {
                return Err(format!("the server never wrote {}", addr_file.display()));
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        let addr = addr.as_str();
        report.notes.push(format!("channel {addr}"));
        let mut c = Driven::connect(addr, &dir)?;
        drive(&mut c, &mut report)
    })();
    let status = server.quit();
    if let Err(e) = outcome {
        report.status = Status::Failed;
        report.reason = Some(e);
    } else if !status.is_some_and(|s| s.success()) {
        report.status = Status::Failed;
        report.reason = Some(format!(
            "the dedicated server did not exit cleanly: {status:?}"
        ));
    }
    Ok(report)
}
