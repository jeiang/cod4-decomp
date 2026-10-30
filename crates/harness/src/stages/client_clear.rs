// SPDX-License-Identifier: GPL-3.0-only
//! `client-clear`: pixels nothing draws are black, as the stock engine leaves them, not the map's fog colour (#356).
//!
//! The flythrough's path runs straight between spawn points and does not collide, so some of its poses stand inside
//! a wall or outside the sealed world, where part of the view reaches no surface at all. Stock never clears the colour
//! buffer in a shipped configuration (`r_clear` is `dev-only blink`, `R_GetClearColor`), so such pixels keep what the
//! target held, black on the reference host; the renderer used to clear to the fog colour, which drew pale flat
//! polygons that looked like holes in the world. The stage holds the client at two such poses (the issue's mp_crash
//! and mp_backlot viewpoints) and samples the picture: every probe in the undrawn region must be dark, and a probe on
//! drawn geometry must not be, so a blank frame cannot pass. Needs a display and the install; skips without.
//!
//! The poses are not reachable through the player's path (no `noclip`, and `setviewpos` is held only outside a match),
//! so the client's `--fly-at` holds them: the same fixed camera the flythrough stage draws.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::profile_args;
use crate::stage::{StageCtx, StageReport, Status};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-clear";
const LIMIT: Duration = Duration::from_secs(240);
/// Seconds the client holds the pose before its screenshot (the first frames warm pipelines).
const SECS: u64 = 4;
/// Luma (of 255) at most in the undrawn region, and at least on the drawn geometry.
const DARK: f64 = 40.0;
const DRAWN: f64 = 60.0;
/// Side of the square of pixels a probe averages.
const BLOCK: u32 = 7;

/// One held pose of the flythrough: the map, the time of its path, probes in the undrawn region and one on geometry.
/// Probes are in normalized device coordinates (x right, y up, -1..1), so the picture's size does not matter.
struct Pose {
    map: &'static str,
    fly_at: &'static str,
    void: &'static [(f32, f32)],
    drawn: (f32, f32),
}

/// The viewpoints of #356: `mp_crash` at 120 s (origin -83.8 1549.0 313.1, yaw -88.3, pitch -5.5) looks down into a
/// hole left of a pillar; `mp_backlot` at 120 s (277.3 1728.0 149.2, yaw -14.3, pitch -3.2) has one across the lower
/// left. The probes sit well inside each region; the drawn probe is on a sunlit wall.
const POSES: &[Pose] = &[
    Pose {
        map: "mp_crash",
        fly_at: "120",
        void: &[(-0.25, -0.82), (-0.28, -0.92), (-0.01, -0.76)],
        drawn: (0.6, 0.3),
    },
    Pose {
        map: "mp_backlot",
        fly_at: "120",
        void: &[(-0.62, -0.73), (-0.81, -0.89)],
        drawn: (0.5, 0.0),
    },
];

/// Mean RGB of the [`BLOCK`]-sided square around normalized device coordinates `(nx, ny)`.
fn sample(w: u32, h: u32, rgb: &[u8], (nx, ny): (f32, f32)) -> [f64; 3] {
    let cx = ((nx + 1.0) * 0.5 * w as f32) as i64;
    let cy = ((1.0 - ny) * 0.5 * h as f32) as i64;
    let half = i64::from(BLOCK / 2);
    let mut sum = [0.0; 3];
    let mut n = 0.0;
    for y in cy - half..=cy + half {
        for x in cx - half..=cx + half {
            if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) {
                continue;
            }
            let at = ((y as u64 * u64::from(w) + x as u64) * 3) as usize;
            for c in 0..3 {
                sum[c] += f64::from(rgb[at + c]);
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n.max(1.0))
}

fn luma(rgb: [f64; 3]) -> f64 {
    (rgb[0] * 77.0 + rgb[1] * 150.0 + rgb[2] * 29.0) / 256.0
}

/// Why the picture fails the pose's probes, or `None`.
fn verdict(pose: &Pose, w: u32, h: u32, rgb: &[u8], report: &mut StageReport) -> Option<String> {
    let mut why = Vec::new();
    for (i, &p) in pose.void.iter().enumerate() {
        let c = sample(w, h, rgb, p);
        let l = luma(c);
        report
            .metrics
            .insert(format!("{}.void{i}.luma", pose.map), l);
        report.notes.push(format!(
            "{} void probe {i} at ({:.2}, {:.2}): rgb ({:.0}, {:.0}, {:.0})",
            pose.map, p.0, p.1, c[0], c[1], c[2]
        ));
        if l > DARK {
            why.push(format!(
                "{} probe {i} in the undrawn region is ({:.0}, {:.0}, {:.0}), luma {l:.0} (stock leaves it black, at most {DARK:.0})",
                pose.map, c[0], c[1], c[2]
            ));
        }
    }
    let c = sample(w, h, rgb, pose.drawn);
    let l = luma(c);
    report.metrics.insert(format!("{}.drawn.luma", pose.map), l);
    if l < DRAWN {
        why.push(format!(
            "{} probe on the wall is ({:.0}, {:.0}, {:.0}), luma {l:.0}: is the world drawn? (need {DRAWN:.0})",
            pose.map, c[0], c[1], c[2]
        ));
    }
    (!why.is_empty()).then(|| why.join("; "))
}

/// Holds the client at the pose and returns its screenshot's path.
fn shoot(
    client: &Path,
    install: &Path,
    dir: &Path,
    pose: &Pose,
) -> Result<std::path::PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log = File::create(dir.join("client.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(profile_args(&dir.join("config")))
        .args(["--map", pose.map, "--flythrough", "--fly-at", pose.fly_at])
        .args(["--duration", &SECS.to_string()])
        .args(["--screenshot", "--no-sound", "--size", "1920x1080", "--out"])
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

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if !install.is_dir() {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    }
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    let mut failures = Vec::new();
    for pose in POSES {
        let dir = ctx.dir.join(pose.map);
        let picture = shoot(&client, install, &dir, pose).and_then(|p| decode(&p));
        let (w, h, rgb) = match picture {
            Ok(p) => p,
            Err(e) => {
                return Ok(report
                    .with_status(Status::Failed)
                    .with_reason(format!("{}: {e}", pose.map)));
            }
        };
        report.files.push(format!("{}/screenshot.png", pose.map));
        if let Some(why) = verdict(pose, w, h, &rgb, &mut report) {
            failures.push(why);
        }
    }
    if !failures.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(failures.join("; "));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture whose lower left is `fill` and the rest mid grey.
    fn picture(fill: [u8; 3]) -> (u32, u32, Vec<u8>) {
        let (w, h) = (64, 32);
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                if x < w / 2 && y >= h / 2 {
                    rgb.extend_from_slice(&fill);
                } else {
                    rgb.extend_from_slice(&[128, 128, 128]);
                }
            }
        }
        (w, h, rgb)
    }

    const POSE: Pose = Pose {
        map: "test",
        fly_at: "0",
        void: &[(-0.5, -0.5)],
        drawn: (0.5, 0.5),
    };

    #[test]
    fn a_fog_coloured_hole_fails_and_a_black_one_passes() {
        let (w, h, rgb) = picture([151, 171, 172]);
        let why = verdict(
            &POSE,
            w,
            h,
            &rgb,
            &mut StageReport::new(NAME, Status::Passed),
        );
        assert!(
            why.as_deref().is_some_and(|s| s.contains("undrawn")),
            "{why:?}"
        );
        let (w, h, rgb) = picture([0, 0, 0]);
        let mut r = StageReport::new(NAME, Status::Passed);
        assert_eq!(verdict(&POSE, w, h, &rgb, &mut r), None);
        assert_eq!(r.metrics["test.void0.luma"], 0.0);
    }

    #[test]
    fn a_blank_picture_fails_on_the_drawn_probe() {
        let (w, h, _) = picture([0; 3]);
        let rgb = vec![0; (w * h * 3) as usize];
        let why = verdict(
            &POSE,
            w,
            h,
            &rgb,
            &mut StageReport::new(NAME, Status::Passed),
        );
        assert!(
            why.as_deref().is_some_and(|s| s.contains("wall")),
            "{why:?}"
        );
    }

    #[test]
    fn a_probe_averages_its_block_at_the_picture_coordinates() {
        let (w, h, rgb) = picture([10, 20, 30]);
        assert_eq!(sample(w, h, &rgb, (-0.5, -0.5)), [10.0, 20.0, 30.0]);
        assert_eq!(sample(w, h, &rgb, (0.5, 0.5)), [128.0; 3]);
        // A probe at the corner is clipped to the picture.
        assert_eq!(sample(w, h, &rgb, (-1.0, -1.0)), [10.0, 20.0, 30.0]);
    }
}
