// SPDX-License-Identifier: GPL-3.0-only
//! Stage 3: drives the `cod4e` client through a mp_crash flythrough once per
//! display mode, recording video, a screenshot and per-frame timings.
//!
//! The client contract (see the stage docs in README.md):
//! `cod4e --list-display-modes --json` describes the GPU and monitors; each
//! run is `cod4e --install .. --map mp_crash --flythrough ..` writing
//! `frames.raw.csv`, a video, `screenshot.png` and `client.json` into `--out`.
use super::client_session::profile_args;
use crate::perf::{FrameRecorder, FrameSample};
use crate::stage::{StageCtx, StageReport, Status};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-flythrough";
const DEFAULT_SECS: u64 = 12;
/// Frames per second a recording must keep for its footage to be reviewable. The encoder runs on the host's spare
/// cores and returns its buffers to the render thread, so its loss rate follows the frame size and the load on the
/// machine (measured 0.6-1.7% at a 2536x1416 window, 34.9-37.8% at 5120x1440 with the host to itself, up to 56% at
/// 1920x1080 beside two other runs); a recording below this is not footage.
const MIN_VIDEO_FPS: f64 = 8.0;
/// The client's per-frame phase columns beside `frames.csv` (`client/src/app.rs` `PHASES`).
const PHASES_CSV: &str = "frames.phases.csv";
/// How long past the flythrough a child may take (startup, zone load, video
/// finish) before it is killed.
const DEFAULT_GRACE: Duration = Duration::from_secs(120);
const RAW_CSV: &str = "frames.raw.csv";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Monitor {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub primary: bool,
    pub native: Mode,
    #[serde(default)]
    pub modes: Vec<Mode>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DisplayModes {
    #[serde(default)]
    pub gpu: Value,
    pub monitors: Vec<Monitor>,
    #[serde(default)]
    pub present_modes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fullscreen {
    Windowed,
    Borderless,
    Exclusive,
}

impl Fullscreen {
    fn as_str(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless",
            Self::Exclusive => "exclusive",
        }
    }
}

/// One planned client run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPlan {
    pub slug: String,
    pub mode: Mode,
    pub fullscreen: Fullscreen,
    pub map: &'static str,
    /// `--present` mode; the capped runs use the default (vsync).
    pub present: &'static str,
}

/// Stock maps besides mp_crash that get a run of their own, so shadows, fog and post effects show on more than one
/// map (these have glow in their vision files).
pub const EXTRA_MAPS: [&str; 2] = ["mp_bog", "mp_crash_snow"];

/// The bounded, deterministic runs for one monitor: borderless at native,
/// windowed 1080p at the highest refresh (when it fits), windowed 720p, and
/// an exclusive run at native size with another refresh when one is listed.
pub fn plan_runs(m: &Monitor) -> Vec<RunPlan> {
    let n = m.native;
    let mut runs = vec![RunPlan {
        slug: "native-borderless".into(),
        mode: n,
        fullscreen: Fullscreen::Borderless,
        map: "mp_crash",
        present: "auto",
    }];
    let top = m
        .modes
        .iter()
        .map(|x| x.refresh_mhz)
        .chain([n.refresh_mhz])
        .max()
        .unwrap_or(n.refresh_mhz);
    for (w, h, slug, refresh) in [
        (1920, 1080, "1920x1080-windowed", top),
        (1280, 720, "1280x720-windowed", n.refresh_mhz),
    ] {
        if w <= n.width && h <= n.height {
            runs.push(RunPlan {
                slug: slug.into(),
                mode: Mode {
                    width: w,
                    height: h,
                    refresh_mhz: refresh,
                },
                fullscreen: Fullscreen::Windowed,
                map: "mp_crash",
                present: "auto",
            });
        }
    }
    let alt = m
        .modes
        .iter()
        .filter(|x| x.width == n.width && x.height == n.height && x.refresh_mhz != n.refresh_mhz)
        .map(|x| x.refresh_mhz)
        .max();
    if let Some(r) = alt {
        runs.push(RunPlan {
            slug: format!("exclusive-{}x{}-{}mhz", n.width, n.height, r),
            mode: Mode {
                refresh_mhz: r,
                ..n
            },
            fullscreen: Fullscreen::Exclusive,
            map: "mp_crash",
            present: "auto",
        });
    }
    // Uncapped: how fast the frame really is at native size.
    for present in ["immediate", "mailbox"] {
        runs.push(RunPlan {
            slug: format!("native-{present}"),
            mode: n,
            fullscreen: Fullscreen::Borderless,
            map: "mp_crash",
            present,
        });
    }
    let (w, h) = if n.width >= 1920 && n.height >= 1080 {
        (1920, 1080)
    } else {
        (1280, 720)
    };
    for map in EXTRA_MAPS {
        runs.push(RunPlan {
            slug: format!("{map}-{w}x{h}"),
            mode: Mode {
                width: w,
                height: h,
                refresh_mhz: n.refresh_mhz,
            },
            fullscreen: Fullscreen::Windowed,
            map,
            present: "auto",
        });
    }
    runs
}

/// The monitor to test: the primary one, else the first.
pub fn pick_monitor(d: &DisplayModes) -> Option<&Monitor> {
    d.monitors.iter().find(|m| m.primary).or(d.monitors.first())
}

pub struct Options {
    pub client: PathBuf,
    pub secs: u64,
    pub grace: Duration,
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse().ok()
}

pub(crate) fn locate_client() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COD4E_CLIENT") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe
        .parent()?
        .join(format!("cod4e{}", std::env::consts::EXE_SUFFIX));
    p.is_file().then_some(p)
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    run_with(
        ctx,
        &Options {
            client,
            secs: env_u64("COD4E_FLYTHROUGH_SECS").unwrap_or(DEFAULT_SECS),
            grace: env_u64("COD4E_FLYTHROUGH_GRACE_SECS")
                .map_or(DEFAULT_GRACE, Duration::from_secs),
        },
    )
}

pub fn run_with(ctx: &StageCtx, opts: &Options) -> io::Result<StageReport> {
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let listing = Command::new(&opts.client)
        .args(["--list-display-modes", "--json"])
        .stdin(Stdio::null())
        .output()?;
    if !listing.status.success() {
        let err = String::from_utf8_lossy(&listing.stderr);
        let line = err
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        let status = if line.starts_with("no display") {
            Status::Skipped
        } else {
            Status::Failed
        };
        let why = if line.is_empty() {
            format!("cod4e --list-display-modes exited with {}", listing.status)
        } else {
            line.to_owned()
        };
        return Ok(StageReport::new(NAME, status).with_reason(why));
    }
    let modes: DisplayModes = serde_json::from_slice(&listing.stdout).map_err(|e| {
        io::Error::other(format!(
            "cannot parse cod4e --list-display-modes output: {e}"
        ))
    })?;
    let Some(monitor) = pick_monitor(&modes) else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no display: no monitors"));
    };

    let mut report = StageReport::new(NAME, Status::Passed);
    let mut performed = Vec::new();
    let mut failures = Vec::new();
    for plan in plan_runs(monitor) {
        let outcome = run_one(ctx, opts, install, &plan, &mut report)?;
        performed.push(json!({
            "slug": plan.slug,
            "width": plan.mode.width,
            "height": plan.mode.height,
            "refresh_mhz": plan.mode.refresh_mhz,
            "fullscreen": plan.fullscreen.as_str(),
            "map": plan.map,
            "present": plan.present,
            "outcome": match &outcome { Ok(()) => "ok".to_owned(), Err(e) => e.clone() },
        }));
        match outcome {
            Ok(()) => report.notes.push(format!("{}: ok", plan.slug)),
            Err(e) => {
                report.notes.push(format!("{}: {e}", plan.slug));
                failures.push(plan.slug);
            }
        }
    }
    if !failures.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(format!("failed runs: {}", failures.join(", ")));
    }
    report.environment = Some(json!({
        "gpu": modes.gpu,
        "display_modes": {
            "monitors": monitors_json(&modes),
            "present_modes": modes.present_modes,
            "runs": performed,
        },
    }));
    Ok(report)
}

fn monitors_json(d: &DisplayModes) -> Vec<Value> {
    let mode =
        |m: &Mode| json!({"width": m.width, "height": m.height, "refresh_mhz": m.refresh_mhz});
    d.monitors
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "primary": m.primary,
                "native": mode(&m.native),
                "modes": m.modes.iter().map(mode).collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[derive(Default, Deserialize)]
struct ClientJson {
    #[serde(default)]
    status: String,
    error: Option<String>,
    video: Option<VideoJson>,
    screenshot: Option<String>,
    pose: Option<PoseJson>,
    frames: Option<u64>,
    zone_load_ms: Option<f64>,
    world: Option<WorldJson>,
    mode: Option<ModeJson>,
    #[serde(default)]
    gpu_health: Value,
}

#[derive(Deserialize)]
struct ModeJson {
    /// What the window was, not what was asked for.
    fullscreen: Option<String>,
}

#[derive(Deserialize)]
struct VideoJson {
    file: String,
    frames: u64,
    dropped: u64,
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
    fps: u32,
    ring_full: u64,
    queue_full: u64,
    map_failed: u64,
    gpu_reset: u64,
    pacing_missed: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    first_slot: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    last_slot: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    first_encoded_slot: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    last_encoded_slot: Option<u64>,
    duration_ticks: u64,
}

/// Source opportunities include pacing misses, not just frames admitted to readback.
fn video_problems(
    v: &VideoJson,
    plan: &RunPlan,
    secs: u64,
    report: &mut StageReport,
) -> Vec<String> {
    let slug = &plan.slug;
    let mut problems = Vec::new();
    let scale = (1280.0 / f64::from(v.source_width))
        .min(720.0 / f64::from(v.source_height))
        .min(1.0);
    let width = ((f64::from(v.source_width) * scale).floor() as u32) & !1;
    let height = ((f64::from(v.source_height) * scale).floor() as u32) & !1;
    report.notes.push(format!(
        "{slug}: recording {}x{} -> {}x{} at {} fps (required 30 fps); frames={}, ring_full={}, queue_full={}, map_failed={}, gpu_reset={}, pacing_missed={}",
        v.source_width, v.source_height, v.width, v.height, v.fps, v.frames,
        v.ring_full, v.queue_full, v.map_failed, v.gpu_reset, v.pacing_missed
    ));
    for (name, value) in [
        ("frames", v.frames),
        ("dropped", v.dropped),
        ("ring_full", v.ring_full),
        ("queue_full", v.queue_full),
        ("map_failed", v.map_failed),
        ("gpu_reset", v.gpu_reset),
        ("pacing_missed", v.pacing_missed),
        ("duration_ticks", v.duration_ticks),
        ("width", u64::from(v.width)),
        ("height", u64::from(v.height)),
        ("source_width", u64::from(v.source_width)),
        ("source_height", u64::from(v.source_height)),
        ("target_fps", u64::from(v.fps)),
    ] {
        report
            .metrics
            .insert(format!("{slug}.video_{name}"), value as f64);
    }
    for (name, value) in [
        ("first_slot", v.first_slot),
        ("last_slot", v.last_slot),
        ("first_encoded_slot", v.first_encoded_slot),
        ("last_encoded_slot", v.last_encoded_slot),
    ] {
        if let Some(value) = value {
            report
                .metrics
                .insert(format!("{slug}.video_{name}"), value as f64);
        }
    }
    if width == 0 || height == 0 || (v.width, v.height) != (width, height) {
        problems.push(format!(
            "video dimensions {}x{} do not match bounded, aspect-preserved, even, no-upscale output {width}x{height} from {}x{}",
            v.width, v.height, v.source_width, v.source_height
        ));
    }
    if v.fps != 30 {
        problems.push(format!("video target fps is {}, expected 30", v.fps));
    }
    let dropped = v
        .ring_full
        .checked_add(v.queue_full)
        .and_then(|n| n.checked_add(v.map_failed))
        .and_then(|n| n.checked_add(v.gpu_reset));
    if dropped != Some(v.dropped) {
        problems.push("video dropped does not equal loss breakdown".into());
    }
    let fps = v.frames as f64 / secs.max(1) as f64;
    report.metrics.insert(format!("{slug}.video_fps"), fps);
    if fps < MIN_VIDEO_FPS {
        problems.push(format!(
            "the recording kept {fps:.1} fps of {secs} s: too few frames for the footage review"
        ));
    }
    let due = v
        .first_slot
        .zip(v.last_slot)
        .and_then(|(first, last)| last.checked_sub(first).and_then(|n| n.checked_add(1)));
    match due {
        Some(due) => {
            let accounted = v
                .frames
                .checked_add(v.dropped)
                .and_then(|n| n.checked_add(v.pacing_missed));
            if accounted != Some(due) {
                problems.push(
                    "video frames + dropped + pacing_missed does not equal due interval".into(),
                );
            }
            let missing = v.dropped.checked_add(v.pacing_missed);
            let share = missing.map_or(f64::INFINITY, |n| n as f64 / due as f64);
            report
                .metrics
                .insert(format!("{slug}.video_missing_share"), share);
            report.metrics.insert(
                format!("{slug}.video_dropped_share"),
                v.dropped as f64 / due as f64,
            );
            if share > 0.05 {
                problems.push(format!(
                    "video missing source opportunities exceed 5% ({:.1}%)",
                    share * 100.0
                ));
            }
            if v.first_slot.is_some_and(|first| first > 30) {
                problems.push("video first due slot exceeds 1 second startup allowance".into());
            }
            if due < secs.saturating_sub(1).saturating_mul(30) {
                problems.push("video due interval covers less than duration minus 1 second".into());
            }
            match (v.first_encoded_slot, v.last_encoded_slot) {
                (Some(first), Some(last)) if v.frames > 0 => {
                    let duration = last.checked_sub(first).and_then(|n| n.checked_add(1));
                    if duration != Some(v.duration_ticks) || v.frames > v.duration_ticks {
                        problems.push(
                            "video duration_ticks does not match encoded endpoints or frame count"
                                .into(),
                        );
                    }
                    let due_first = v.first_slot.unwrap();
                    let due_last = v.last_slot.unwrap();
                    if first < due_first || last > due_last || first > last {
                        problems.push(
                            "video encoded endpoints are outside due interval or reversed".into(),
                        );
                    } else {
                        let gap = (first - due_first) + (due_last - last);
                        if gap as f64 / due as f64 > 0.05 {
                            problems.push(
                                "video encoded endpoint gaps exceed 5% of due interval".into(),
                            );
                        }
                    }
                }
                (None, None) if v.frames == 0 && v.duration_ticks == 0 => {}
                _ => problems.push(
                    "video encoded endpoints/duration_ticks disagree with frame count".into(),
                ),
            }
        }
        None => problems.push("video due endpoints are missing, reversed, or overflow".into()),
    }
    problems
}

#[derive(Deserialize)]
struct WorldJson {
    surfaces_drawn_p50: Option<f64>,
}

/// The screenshot's camera (`client.json` `pose`, #431): the original's `setviewpos` line and `cg_fov` at the
/// frame's size reproduce the picture.
#[derive(Deserialize)]
struct PoseJson {
    map: String,
    frame: usize,
    origin: [f64; 3],
    yaw: f64,
    pitch: f64,
    cg_fov: f64,
    fov_x: f64,
    width: u32,
    height: u32,
    setviewpos: String,
}

/// The pose file beside `frames.raw.csv`: the camera each frame was drawn with.
const POSE_CSV: &str = "frames.pose.csv";

/// Why the screenshot's recorded camera is not usable to reproduce it, or `None`: the pose must name the run's map,
/// its `setviewpos` line must carry its eye and angles, `fov_x` must be `cg_fov` widened for the frame's shape
/// (Hor+), and `frames.pose.csv` must hold one row per timed frame whose row for the screenshot's frame is the same
/// camera, with the camera moving along the tour.
fn pose_problem(pose: &PoseJson, map: &str, csv: &str, frames: usize) -> Option<String> {
    const EPS: f64 = 0.011;
    let near = |a: f64, b: f64| (a - b).abs() <= EPS;
    if pose.map != map {
        return Some(format!(
            "the pose names map {:?}, the run was {map}",
            pose.map
        ));
    }
    let want = [
        pose.origin[0],
        pose.origin[1],
        pose.origin[2],
        pose.yaw,
        pose.pitch,
    ];
    let args: Vec<f64> = pose
        .setviewpos
        .strip_prefix("setviewpos ")
        .map(|a| a.split(' ').filter_map(|v| v.parse().ok()).collect())
        .unwrap_or_default();
    if args.len() != 5 || !args.iter().zip(want).all(|(&a, b)| near(a, b)) {
        return Some(format!(
            "the pose's line {:?} is not setviewpos with its eye {:?}, yaw {} and pitch {}",
            pose.setviewpos, pose.origin, pose.yaw, pose.pitch
        ));
    }
    if !(-180.0..180.0).contains(&pose.yaw) || !(-90.0..=90.0).contains(&pose.pitch) {
        return Some(format!(
            "the pose's angles yaw {} pitch {} are outside the stock ranges",
            pose.yaw, pose.pitch
        ));
    }
    let aspect = f64::from(pose.width) / f64::from(pose.height.max(1));
    let widened = 2.0 * ((pose.cg_fov.to_radians() * 0.5).tan() * 0.75 * aspect).atan();
    if !(65.0..=80.0).contains(&pose.cg_fov) || (widened.to_degrees() - pose.fov_x).abs() > 0.05 {
        return Some(format!(
            "the pose's cg_fov {} at {}x{} widens to {:.2}, not its fov_x {}",
            pose.cg_fov,
            pose.width,
            pose.height,
            widened.to_degrees(),
            pose.fov_x
        ));
    }
    let mut lines = csv.lines();
    if lines.next() != Some("frame,x,y,z,yaw,pitch,roll,cg_fov,fov_x") {
        return Some(format!("{POSE_CSV} is missing or has no header"));
    }
    let rows: Vec<Vec<f64>> = lines
        .map(|l| l.split(',').filter_map(|v| v.parse().ok()).collect())
        .collect();
    if rows.len() != frames || rows.iter().any(|r| r.len() != 9) {
        return Some(format!(
            "{POSE_CSV} has {} rows, not one complete row for each of the {frames} timed frames",
            rows.len()
        ));
    }
    let Some(row) = rows.iter().find(|r| r[0] as usize == pose.frame) else {
        return Some(format!(
            "{POSE_CSV} has no row for the screenshot's frame {}",
            pose.frame
        ));
    };
    let csv_cam = [row[1], row[2], row[3], row[4], row[5], row[7], row[8]];
    let json_cam = want.into_iter().chain([pose.cg_fov, pose.fov_x]);
    if !csv_cam.iter().zip(json_cam).all(|(&a, b)| near(a, b)) {
        return Some(format!(
            "{POSE_CSV} row for frame {} is {row:?}, not the screenshot's pose",
            pose.frame
        ));
    }
    if rows.windows(2).all(|w| w[0][1..4] == w[1][1..4]) {
        return Some(format!("{POSE_CSV}: the camera never moved along the tour"));
    }
    None
}

/// Run the client once. `Ok(Err(why))` is a failed run; `Err` is a harness
/// I/O failure.
fn run_one(
    ctx: &StageCtx,
    opts: &Options,
    install: &Path,
    plan: &RunPlan,
    report: &mut StageReport,
) -> io::Result<Result<(), String>> {
    let out = ctx.dir.join(&plan.slug);
    fs::create_dir_all(&out)?;
    let log = File::create(out.join("client.log"))?;
    let mut child = Command::new(&opts.client)
        .arg("--install")
        .arg(install)
        .args(profile_args(&ctx.dir.join("config")))
        .args([
            "--map",
            plan.map,
            "--present",
            plan.present,
            "--flythrough",
            "--duration",
        ])
        .arg(opts.secs.to_string())
        .arg("--out")
        .arg(&out)
        .args(["--video", "--screenshot", "--size"])
        .arg(format!("{}x{}", plan.mode.width, plan.mode.height))
        .arg("--refresh")
        .arg((f64::from(plan.mode.refresh_mhz) / 1000.0).to_string())
        .args(["--fullscreen", plan.fullscreen.as_str()])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(opts.secs) + opts.grace;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            report.files.push(format!("{}/client.log", plan.slug));
            return Ok(Err(format!(
                "Hung: killed after {} s",
                (opts.secs + opts.grace.as_secs())
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let result = consume(&out, plan, opts.secs, status, report)?;
    for f in ["client.log", "client.json"] {
        if out.join(f).exists() {
            report.files.push(format!("{}/{f}", plan.slug));
        }
    }
    Ok(result)
}

fn consume(
    out: &Path,
    plan: &RunPlan,
    secs: u64,
    status: std::process::ExitStatus,
    report: &mut StageReport,
) -> io::Result<Result<(), String>> {
    let slug = &plan.slug;
    let Ok(json) = fs::read(out.join("client.json")) else {
        return Ok(Err(format!(
            "client exited with {status} and wrote no client.json"
        )));
    };
    let cj: ClientJson = match serde_json::from_slice(&json) {
        Ok(c) => c,
        Err(e) => return Ok(Err(format!("bad client.json: {e}"))),
    };
    if cj.status != "ok" {
        let why = cj.error.as_deref().unwrap_or("no error given");
        return Ok(Err(format!(
            "client reported status {:?}: {why}",
            cj.status
        )));
    }
    if !status.success() {
        return Ok(Err(format!("client exited with {status}")));
    }

    let raw = out.join(RAW_CSV);
    let text = fs::read_to_string(&raw)
        .map_err(|e| io::Error::new(e.kind(), format!("{RAW_CSV}: {e}")))?;
    // The rows after the header: one per timed frame, which `frames.pose.csv` must match.
    let timed_frames = text.lines().count().saturating_sub(1);
    let mut rec = FrameRecorder::create(&out.join("frames.csv"))?;
    for (i, line) in text.lines().enumerate().skip(1) {
        match parse_frame(line) {
            Some(s) => rec.push(s)?,
            None => return Ok(Err(format!("{RAW_CSV} line {}: bad row {line:?}", i + 1))),
        }
    }
    let series = rec.finish()?;
    fs::remove_file(&raw)?;
    report.files.push(format!("{slug}/frames.csv"));
    // Where each frame's time went, which is what tells a hitch's cause.
    if out.join(PHASES_CSV).is_file() {
        report.files.push(format!("{slug}/{PHASES_CSV}"));
    }
    if series.is_empty() {
        return Ok(Err("no frames recorded".into()));
    }
    report
        .series
        .extend(series.into_iter().map(|(k, v)| (format!("{slug}.{k}"), v)));

    if let Some(f) = cj.frames {
        report.metrics.insert(format!("{slug}.frames"), f as f64);
    }
    if let Some(z) = cj.zone_load_ms {
        report.metrics.insert(format!("{slug}.zone_load_ms"), z);
    }
    if let Some(s) = cj.world.and_then(|w| w.surfaces_drawn_p50) {
        report
            .metrics
            .insert(format!("{slug}.surfaces_drawn_p50"), s);
    }

    let mut problems = Vec::new();
    // An exclusive run that was not one tests nothing it is there for.
    if plan.fullscreen == Fullscreen::Exclusive {
        let actual = cj.mode.as_ref().and_then(|m| m.fullscreen.as_deref());
        if actual != Some("exclusive") {
            problems.push(format!(
                "asked for exclusive fullscreen, the window was {}",
                actual.unwrap_or("unknown")
            ));
        }
    }
    if let Some(p) = super::client_session::gpu_problem(&cj.gpu_health) {
        problems.push(p);
    }
    if let Some(losses) = cj.gpu_health["device_losses"]
        .as_array()
        .filter(|l| !l.is_empty())
    {
        report
            .metrics
            .insert(format!("{slug}.device_losses"), losses.len() as f64);
        report.notes.push(format!(
            "{slug}: graphics device lost and recovered: {losses:?}"
        ));
    }
    match &cj.video {
        Some(v) => {
            problems.extend(video_problems(v, plan, secs, report));
            let size = fs::metadata(out.join(&v.file)).map_or(0, |m| m.len());
            if size == 0 {
                problems.push(format!("video {} missing or empty", v.file));
            } else {
                report.files.push(format!("{slug}/{}", v.file));
                report
                    .metrics
                    .insert(format!("{slug}.video_bytes"), size as f64);
            }
        }
        None => problems.push("no video".into()),
    }
    match &cj.screenshot {
        Some(s) if out.join(s).is_file() => report.files.push(format!("{slug}/{s}")),
        _ => problems.push("no screenshot".into()),
    }
    let pose_csv = fs::read_to_string(out.join(POSE_CSV)).unwrap_or_default();
    if !pose_csv.is_empty() {
        report.files.push(format!("{slug}/{POSE_CSV}"));
    }
    match &cj.pose {
        Some(p) => problems.extend(pose_problem(p, plan.map, &pose_csv, timed_frames)),
        None => problems.push("client.json has no pose for the screenshot".into()),
    }
    Ok(if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    })
}

fn parse_frame(line: &str) -> Option<FrameSample> {
    let mut f = line.split(',');
    let cpu_ms = f.next()?.trim().parse().ok()?;
    let gpu = f.next()?.trim();
    let gpu_ms = if gpu.is_empty() {
        None
    } else {
        Some(gpu.parse().ok()?)
    };
    let present_interval_ms = f.next()?.trim().parse().ok()?;
    let mem_bytes = f.next()?.trim().parse().ok()?;
    f.next().is_none().then_some(FrameSample {
        cpu_ms,
        gpu_ms,
        present_interval_ms,
        mem_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose() -> PoseJson {
        PoseJson {
            map: "mp_crash".into(),
            frame: 2,
            origin: [10.0, -20.5, 64.25],
            yaw: -90.0,
            pitch: 10.0,
            cg_fov: 65.0,
            // 65 degrees at 4:3 widened for 16:9.
            fov_x: 80.69,
            width: 1280,
            height: 720,
            setviewpos: "setviewpos 10.00 -20.50 64.25 -90.00 10.00".into(),
        }
    }

    const ROWS: &str = "frame,x,y,z,yaw,pitch,roll,cg_fov,fov_x\n\
        1,0.00,0.00,64.25,-90.00,10.00,0.00,65.00,80.69\n\
        2,10.00,-20.50,64.25,-90.00,10.00,0.00,65.00,80.69\n";

    #[test]
    fn a_screenshot_pose_must_be_its_frames_camera_and_a_setviewpos_line() {
        assert_eq!(pose_problem(&pose(), "mp_crash", ROWS, 2), None);
        let p = pose_problem(&pose(), "mp_bog", ROWS, 2).expect("wrong map");
        assert!(p.contains("mp_bog"), "{p}");
        let mut wrong_line = pose();
        wrong_line.setviewpos = "setviewpos 10.00 -20.50 64.25 90.00 10.00".into();
        assert!(pose_problem(&wrong_line, "mp_crash", ROWS, 2).is_some());
        let mut wrong_fov = pose();
        wrong_fov.fov_x = 65.0;
        assert!(pose_problem(&wrong_fov, "mp_crash", ROWS, 2).is_some());
        let moved = ROWS.replace("2,10.00,", "2,11.00,");
        let p = pose_problem(&pose(), "mp_crash", &moved, 2).expect("row differs");
        assert!(p.contains("not the screenshot's pose"), "{p}");
        assert!(pose_problem(&pose(), "mp_crash", ROWS, 3).is_some());
        let still = ROWS.replace("1,0.00,0.00,", "1,10.00,-20.50,");
        assert!(pose_problem(&pose(), "mp_crash", &still, 2).is_some());
        assert!(pose_problem(&pose(), "mp_crash", "", 2).is_some());
    }

    fn mode(w: u32, h: u32, hz: u32) -> Mode {
        Mode {
            width: w,
            height: h,
            refresh_mhz: hz * 1000,
        }
    }

    fn monitor(native: Mode, modes: &[Mode]) -> Monitor {
        Monitor {
            name: "m".into(),
            primary: true,
            native,
            modes: modes.to_vec(),
        }
    }

    /// The capped mp_crash runs.
    fn slugs(m: &Monitor) -> Vec<String> {
        plan_runs(m)
            .into_iter()
            .filter(|r| r.map == "mp_crash" && r.present == "auto")
            .map(|r| r.slug)
            .collect()
    }

    #[test]
    fn uncapped_and_other_map_runs_follow_the_capped_ones() {
        let m = monitor(mode(2560, 1440, 60), &[]);
        let runs = plan_runs(&m);
        let by = |slug: &str| runs.iter().find(|r| r.slug == slug).unwrap();
        assert_eq!(by("native-immediate").present, "immediate");
        assert_eq!(by("native-mailbox").mode, mode(2560, 1440, 60));
        for map in EXTRA_MAPS {
            let r = by(&format!("{map}-1920x1080"));
            assert_eq!((r.map, r.present), (map, "auto"));
        }
    }

    #[test]
    fn full_plan_for_a_4k_monitor_with_two_refreshes() {
        let m = monitor(
            mode(3840, 2160, 60),
            &[
                mode(3840, 2160, 60),
                mode(3840, 2160, 30),
                mode(1920, 1080, 144),
            ],
        );
        let runs = plan_runs(&m);
        assert_eq!(
            slugs(&m),
            [
                "native-borderless",
                "1920x1080-windowed",
                "1280x720-windowed",
                "exclusive-3840x2160-30000mhz"
            ]
        );
        assert_eq!(runs[0].fullscreen, Fullscreen::Borderless);
        assert_eq!(runs[0].mode, mode(3840, 2160, 60));
        // Highest refresh the monitor lists, whatever the size.
        assert_eq!(runs[1].mode, mode(1920, 1080, 144));
        assert_eq!(runs[2].mode, mode(1280, 720, 60));
        assert_eq!(runs[3].fullscreen, Fullscreen::Exclusive);
        assert_eq!(runs[3].mode, mode(3840, 2160, 30));
    }

    #[test]
    fn windowed_sizes_larger_than_native_are_skipped() {
        let m = monitor(mode(1366, 768, 60), &[mode(1366, 768, 60)]);
        assert_eq!(slugs(&m), ["native-borderless", "1280x720-windowed"]);
        let tiny = monitor(mode(1024, 600, 60), &[]);
        assert_eq!(slugs(&tiny), ["native-borderless"]);
    }

    #[test]
    fn exclusive_needs_another_refresh_at_the_native_size() {
        let same = monitor(
            mode(1920, 1080, 60),
            &[mode(1920, 1080, 60), mode(1280, 720, 120)],
        );
        assert_eq!(
            slugs(&same),
            [
                "native-borderless",
                "1920x1080-windowed",
                "1280x720-windowed"
            ]
        );
    }

    #[test]
    fn primary_monitor_wins() {
        let mut a = monitor(mode(1920, 1080, 60), &[]);
        a.primary = false;
        let mut b = monitor(mode(2560, 1440, 60), &[]);
        b.name = "b".into();
        let d = DisplayModes {
            gpu: Value::Null,
            monitors: vec![a, b],
            present_modes: vec![],
        };
        assert_eq!(pick_monitor(&d).unwrap().native.width, 2560);
    }

    #[test]
    fn frame_rows_parse_with_and_without_gpu_time() {
        let s = parse_frame("1.5,,16.7,1024").unwrap();
        assert_eq!((s.cpu_ms, s.gpu_ms, s.mem_bytes), (1.5, None, 1024));
        assert_eq!(parse_frame("1.5,0.4,16.7,1024").unwrap().gpu_ms, Some(0.4));
        assert!(parse_frame("1.5,16.7").is_none());
        assert!(parse_frame("x,,1,2").is_none());
    }
}
