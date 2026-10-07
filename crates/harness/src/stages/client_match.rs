// SPDX-License-Identifier: GPL-3.0-or-later
//! Stage 4, `client-match`: the real `cod4e` client plays a team deathmatch against bots on the server it starts
//! in-process (`--listen --autoplay`). The scripted player connects over UDP, gets a body, walks, sees the bots,
//! turns toward enemies it can see and shoots them, for 90 seconds. The stage reads the client's report and asserts
//! what makes a match playable: connected and spawned, the player moved by prediction, the others were drawn,
//! snapshots kept coming, prediction was rarely corrected, shots were fired and some hit, and the server ran without
//! script errors. Bandwidth, frame times and the server's tick cost are reported as metrics. Needs a display and the
//! install; skips cleanly without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::perf::Percentiles;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-match";
const BOTS: usize = 9;
const SECS: u64 = 90;
const LIMIT: Duration = Duration::from_secs(300);

fn num(v: &Value, path: &[&str]) -> f64 {
    path.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn fail(mut r: StageReport, why: String) -> StageReport {
    r.status = Status::Failed;
    r.reason = Some(why);
    r
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
    let dir = ctx.dir.join("match");
    fs::create_dir_all(&dir)?;
    let log = File::create(dir.join("client.log"))?;
    let mut child = Command::new(&client)
        .arg("--install")
        .arg(install)
        .args(["--map", "mp_crash", "--listen", "--bots"])
        .arg(BOTS.to_string())
        .args(["--autoplay", "--duration"])
        .arg(SECS.to_string())
        .args(["--screenshot", "--size", "1280x720", "--out"])
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
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut report = StageReport::new(NAME, Status::Passed);
    report.files.extend([
        "match/client.log".into(),
        "match/client.json".into(),
        "match/screenshot.png".into(),
    ]);
    if !status.success() {
        return Ok(fail(
            report,
            format!("client exited with {status} (see match/client.log)"),
        ));
    }
    let json: Value = match fs::read(dir.join("client.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return Ok(fail(report, format!("no client report: {e}"))),
    };
    let (net, srv) = (&json["net"], &json["server"]);
    let mut failures = Vec::new();
    let secs = SECS as f64;
    let snaps = num(net, &["snapshots"]);
    let corrected = num(net, &["prediction_corrections"]);
    let predicted = num(net, &["predictions"]);
    let (shots, hits) = (
        num(srv, &["person", "shots"]),
        num(srv, &["person", "hits"]),
    );
    if net["connected"] != true {
        failures.push("the client never connected".to_owned());
    }
    if net["spawned"] != true {
        failures.push("the server never gave the player a body".to_owned());
    }
    if num(net, &["moved"]) < 100.0 && num(net, &["path"]) < 500.0 {
        failures.push(format!(
            "the player barely moved ({:.0} units walked)",
            num(net, &["path"])
        ));
    }
    if num(net, &["players_seen_max"]) < (BOTS / 2) as f64 {
        failures.push(format!(
            "at most {} other players drawn of {BOTS} bots",
            num(net, &["players_seen_max"])
        ));
    }
    if snaps < secs * 20.0 {
        failures.push(format!("only {snaps} snapshots in {SECS} s"));
    }
    if predicted == 0.0 || corrected * 7.0 > predicted {
        failures.push(format!(
            "the server corrected {corrected} of {predicted} predictions"
        ));
    }
    if net["viewmodel_frames"].as_f64().unwrap_or(0.0) < secs {
        failures.push("the first-person weapon was hardly drawn".into());
    }
    if net["weapons_without_models"]
        .as_object()
        .is_some_and(|m| !m.is_empty())
    {
        failures.push(format!(
            "weapons without a model: {}",
            net["weapons_without_models"]
        ));
    }
    if num(srv, &["script_errors"]) > 0.0 {
        failures.push(format!(
            "{} script runtime errors on the server",
            num(srv, &["script_errors"])
        ));
    }
    if shots < 1.0 {
        failures.push("the player never fired a shot the server ran".into());
    } else if hits < 1.0 {
        failures.push(format!("{shots} shots fired, none hit an enemy"));
    }
    let snd = &net["sound"];
    let heard = |names: &[&str]| -> f64 {
        names
            .iter()
            .map(|n| num(snd, &["by_channel", n]))
            .sum::<f64>()
    };
    if snd["ready"] != true {
        failures.push(format!("the sound tables never loaded: {}", snd["error"]));
    } else {
        if num(snd, &["started"]) < 50.0 {
            failures.push(format!("only {} sounds started", num(snd, &["started"])));
        }
        if heard(&["weapon", "weapon2d"]) < 1.0 {
            failures.push("no weapon fire was heard".into());
        }
        if heard(&["body", "body2d"]) < 1.0 {
            failures.push("no footsteps were heard".into());
        }
        if heard(&["ambient", "music"]) < 1.0 {
            failures.push("neither the map's ambience nor music played".into());
        }
        if num(snd, &["lost_commands"]) > 0.0 {
            failures.push("the mixer's command queue overflowed".into());
        }
        if snd["failed"].as_array().is_some_and(|f| !f.is_empty()) {
            failures.push(format!("sound files failed: {}", snd["failed"]));
        }
    }
    // What the shots did must have reached the client as events.
    let (impacts, pains) = (
        num(net, &["events", "bullet_impact"]),
        num(net, &["events", "player_pain"]),
    );
    if shots >= 1.0 && impacts < 1.0 {
        failures.push("shots were fired but the client saw no bullet impact event".into());
    }
    if hits >= 1.0 && pains < 1.0 {
        failures.push("enemies were hit but the client saw no pain event".into());
    }
    if impacts >= 1.0 && heard(&["bulletimpact"]) < 1.0 {
        failures.push(format!(
            "{impacts} bullet impacts reached the client but none was heard: {}",
            snd["by_channel"]
        ));
    }
    // The events became effects on screen.
    let fx = &net["fx"];
    if impacts >= 1.0 && num(fx, &["played", "bullet_impact"]) < 1.0 {
        failures.push("bullet impacts happened but none played an effect".into());
    }
    if num(fx, &["played", "bullet_impact"]) >= 1.0 && num(fx, &["quads_max"]) < 1.0 {
        failures.push("impact effects played but no sprite was ever drawn".into());
    }
    let m = &mut report.metrics;
    m.insert("sound.impacts_heard".into(), heard(&["bulletimpact"]));
    m.insert("fx.quads_max".into(), num(fx, &["quads_max"]));
    m.insert("fx.decals_max".into(), num(fx, &["decals_max"]));
    m.insert("fx.live_elems_max".into(), num(fx, &["live_elems_max"]));
    m.insert("sound.started".into(), num(snd, &["started"]));
    m.insert("sound.refused".into(), num(snd, &["refused"]));
    m.insert("sound.replaced".into(), num(snd, &["replaced"]));
    m.insert("sound.underruns".into(), num(snd, &["underruns"]));
    m.insert("client.snapshots_per_s".into(), snaps / secs);
    m.insert(
        "client.bytes_in_per_s".into(),
        num(net, &["bytes_in"]) / secs,
    );
    m.insert(
        "client.bytes_out_per_s".into(),
        num(net, &["bytes_out"]) / secs,
    );
    m.insert("client.prediction_corrections".into(), corrected);
    m.insert(
        "client.players_seen_max".into(),
        num(net, &["players_seen_max"]),
    );
    m.insert("client.walked".into(), num(net, &["path"]));
    m.insert("client.event_bullet_impacts".into(), impacts);
    m.insert("client.event_pains".into(), pains);
    m.insert("match.shots".into(), shots);
    m.insert("match.hits".into(), hits);
    m.insert("match.kills".into(), num(srv, &["stats", "kills"]));
    m.insert("server.tick_ms_p50".into(), num(srv, &["tick_ms_p50"]));
    m.insert("server.tick_ms_p99".into(), num(srv, &["tick_ms_p99"]));
    m.insert(
        "server.net_ms_per_client_p50".into(),
        num(srv, &["net_ms_p50"]),
    );
    // Frame pacing of the client while playing.
    if let Ok(csv) = fs::read_to_string(dir.join("frames.raw.csv")) {
        let col = |i: usize| -> Vec<f64> {
            csv.lines()
                .skip(1)
                .filter_map(|l| l.split(',').nth(i)?.parse().ok())
                .collect()
        };
        if let Some(p) = Percentiles::from_samples(&col(0)) {
            m.insert("client.cpu_ms_p50".into(), p.p50);
            m.insert("client.cpu_ms_p99".into(), p.p99);
        }
        if let Some(p) = Percentiles::from_samples(&col(2)) {
            m.insert("client.frame_interval_ms_p50".into(), p.p50);
            m.insert("client.frame_interval_ms_p99".into(), p.p99);
        }
    }
    report.notes.push(format!(
        "{BOTS} bots, {SECS} s: {:.1} KB/s down, {:.1} KB/s up, server tick p50 {:.2} ms p99 {:.2} ms, {shots} shots {hits} hits",
        num(net, &["bytes_in"]) / secs / 1000.0,
        num(net, &["bytes_out"]) / secs / 1000.0,
        num(srv, &["tick_ms_p50"]),
        num(srv, &["tick_ms_p99"]),
    ));
    if !failures.is_empty() {
        return Ok(fail(report, failures.join("; ")));
    }
    Ok(report)
}
