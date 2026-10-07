// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-models`: the client's `--show-models` scene (stock player models posed in a row, the first-person
//! hands and weapon in front) is rendered twice from the same camera, once with players and once without. The
//! stage passes only if the player rows change a meaningful share of the upper part of the picture: skinned
//! models are drawn, with textures, at the right place. Needs a display and the install; skips cleanly without.
use super::client_flythrough::locate_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-models";
const PLAYERS: usize = 7;
const SECS: u64 = 3;
const LIMIT: Duration = Duration::from_secs(180);
/// Share of upper-picture pixels that must differ, and by how much per channel.
const MIN_CHANGED: f64 = 0.01;
const CHANNEL_DELTA: i32 = 32;

fn decode(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let dec = png::Decoder::new(io::BufReader::new(
        File::open(path).map_err(|e| format!("{}: {e}", path.display()))?,
    ));
    let mut r = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("png too large")?];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let ch = info.color_type.samples();
    if ch < 3 {
        return Err("screenshot is not colour".into());
    }
    let mut rgb = Vec::with_capacity((info.width * info.height * 3) as usize);
    for px in buf[..info.buffer_size()].chunks_exact(ch) {
        rgb.extend_from_slice(&px[..3]);
    }
    Ok((info.width, info.height, rgb))
}

/// Runs the scene with `players` players and returns the screenshot's path.
fn shoot(
    client: &Path,
    install: &Path,
    dir: &Path,
    players: usize,
) -> Result<std::path::PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log = File::create(dir.join("client.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--map", "mp_crash", "--show-models", "--model-count"])
        .arg(players.to_string())
        .args(["--flythrough", "--duration"])
        .arg(SECS.to_string())
        .args([
            "--screenshot",
            "--shadows",
            "off",
            "--size",
            "1280x720",
            "--out",
        ])
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
        return Err(format!("client exited with {status} (see client.log)"));
    }
    let shot = dir.join("screenshot.png");
    shot.is_file()
        .then_some(shot)
        .ok_or_else(|| "client wrote no screenshot".to_owned())
}

/// `Some(report)` when the client cannot open a window here: skipped without a display, failed when it breaks.
pub fn no_display(client: &Path, name: &str) -> io::Result<Option<StageReport>> {
    let listing = Command::new(client)
        .args(["--list-display-modes", "--json"])
        .stdin(Stdio::null())
        .output()?;
    if listing.status.success() {
        return Ok(None);
    }
    let err = String::from_utf8_lossy(&listing.stderr);
    let line = err
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    Ok(Some(if line.starts_with("no display") {
        StageReport::new(name, Status::Skipped).with_reason(line.to_owned())
    } else {
        StageReport::new(name, Status::Failed)
            .with_reason(format!("cod4e --list-display-modes failed: {line}"))
    }))
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
    let shots = shoot(&client, install, &ctx.dir.join("players"), PLAYERS)
        .and_then(|a| Ok((a, shoot(&client, install, &ctx.dir.join("empty"), 0)?)));
    let (with, without) = match shots {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let (a, b) = match (decode(&with), decode(&without)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(e));
        }
    };
    let mut report = StageReport::new(NAME, Status::Passed);
    if (a.0, a.1) != (b.0, b.1) {
        return Ok(report_fail(report, "the two screenshots differ in size"));
    }
    let (w, h) = (a.0 as usize, a.1 as usize);
    // The viewmodel sits low in the picture; the player row is above it.
    let rows = h * 55 / 100;
    let mut changed = 0usize;
    for px in 0..rows * w {
        let d = (0..3)
            .map(|c| (i32::from(a.2[px * 3 + c]) - i32::from(b.2[px * 3 + c])).abs())
            .max()
            .unwrap_or(0);
        if d > CHANNEL_DELTA {
            changed += 1;
        }
    }
    let share = changed as f64 / (rows * w) as f64;
    report
        .metrics
        .insert("players.changed_pixel_share".into(), share);
    report.notes.push(format!(
        "{PLAYERS} players change {:.1}% of the upper picture",
        share * 100.0
    ));
    report.files.extend([
        "players/screenshot.png".into(),
        "empty/screenshot.png".into(),
    ]);
    if share < MIN_CHANGED {
        return Ok(report_fail(
            report,
            &format!(
                "the players changed only {:.2}% of the upper picture (need {:.0}%): are they drawn?",
                share * 100.0,
                MIN_CHANGED * 100.0
            ),
        ));
    }
    Ok(report)
}

fn report_fail(mut r: StageReport, why: &str) -> StageReport {
    r.status = Status::Failed;
    r.reason = Some(why.to_owned());
    r
}
