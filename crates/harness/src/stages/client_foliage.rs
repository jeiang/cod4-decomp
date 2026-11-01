// SPDX-License-Identifier: GPL-3.0-only
//! `client-foliage`: the mp_farm tree at the #358 viewpoint has no black shadow-caster cards in the camera draw.
//!
//! KisakCOD's camera draw builders skip `CAMERA_REGION_NONE` (3), while its shadow draws keep those materials. The
//! stage captures shadows on/off at one held pose and checks that neither capture shows the black cards in the crown.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::{gpu_problem, render_problem, run_client, steps_problem};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::path::Path;
use std::time::Duration;

const NAME: &str = "client-foliage";
const LIMIT: Duration = Duration::from_secs(180);
const STEPS_ON: &str = "renderer,wait=2,gfxis=shadows 1,shot=foliage-on";
const STEPS_OFF: &str = "renderer,wait=2,gfxis=shadows 0,shot=foliage-off";
/// The middle of the crown, including the black cutout cards of #358. Fractions of the picture, left/top to
/// right/bottom, so the check does not depend on the window size.
const CANOPY: (f32, f32, f32, f32) = (0.20, 0.00, 0.80, 0.35);
/// Luma at or below which a pixel is one of the black shadow-caster silhouettes rather than a shaded leaf.
const BLACK: i32 = 24;
const MAX_BLACK_SHARE: f64 = 0.04;
const MIN_CANOPY_LUMA: f64 = 30.0;

fn luma(p: &[u8]) -> i32 {
    (i32::from(p[0]) * 77 + i32::from(p[1]) * 150 + i32::from(p[2]) * 29) >> 8
}

fn region(w: u32, h: u32, r: (f32, f32, f32, f32)) -> (u32, u32, u32, u32) {
    (
        (r.0 * w as f32) as u32,
        (r.1 * h as f32) as u32,
        (r.2 * w as f32) as u32,
        (r.3 * h as f32) as u32,
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    black_on: f64,
    black_off: f64,
    canopy_luma_off: f64,
}

fn measure(w: u32, h: u32, on: &[u8], off: &[u8]) -> Metrics {
    let mut m = Metrics::default();
    let (x0, y0, x1, y1) = region(w, h, CANOPY);
    let (mut black_on, mut black_off, mut sum, mut n) = (0usize, 0usize, 0u64, 0usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let at = ((y * w + x) * 3) as usize;
            let (a, b) = (luma(&on[at..at + 3]), luma(&off[at..at + 3]));
            black_on += usize::from(a <= BLACK);
            black_off += usize::from(b <= BLACK);
            sum += b as u64;
            n += 1;
        }
    }
    m.black_on = black_on as f64 / n.max(1) as f64;
    m.black_off = black_off as f64 / n.max(1) as f64;
    m.canopy_luma_off = sum as f64 / n.max(1) as f64;
    m
}

fn foliage_verdict(dir: &Path, report: &mut StageReport) -> Result<(), String> {
    let (w, h, on) = decode(&dir.join("on/foliage-on.png"))?;
    let (w2, h2, off) = decode(&dir.join("off/foliage-off.png"))?;
    if (w, h) != (w2, h2) {
        return Err("foliage shadow-on and shadow-off screenshots differ in size".into());
    }
    let m = measure(w, h, &on, &off);
    report.metrics.insert("canopy.black_on".into(), m.black_on);
    report
        .metrics
        .insert("canopy.black_off".into(), m.black_off);
    report
        .metrics
        .insert("canopy.luma_off".into(), m.canopy_luma_off);
    let mut why = Vec::new();
    if m.canopy_luma_off < MIN_CANOPY_LUMA {
        why.push(format!(
            "the canopy without shadows has mean luma {:.1}; the world is not drawn",
            m.canopy_luma_off
        ));
    }
    for (mode, share) in [("on", m.black_on), ("off", m.black_off)] {
        if share > MAX_BLACK_SHARE {
            why.push(format!(
                "shadows {mode}: {:.1}% of the canopy is near-black (at most {:.0}%); shadow-only cards are in the camera draw",
                share * 100.0,
                MAX_BLACK_SHARE * 100.0
            ));
        }
    }
    (!why.is_empty())
        .then(|| why.join("; "))
        .map_or(Ok(()), Err)
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
    // Flythrough has no gameplay shell to apply vid_restart. Start each held pose with the public shadow-mode
    // option instead, and assert the renderer's actual setting before capturing it.
    for (mode, steps) in [("on", STEPS_ON), ("off", STEPS_OFF)] {
        let mut args = vec![
            "--map",
            "mp_farm",
            "--flythrough",
            "--fly-at",
            "60",
            "--duration",
            "30",
            "--size",
            "1920x1080",
        ];
        if mode == "off" {
            args.extend(["--shadows", "off"]);
        }
        let dir = ctx.dir.join(mode);
        match run_client(
            &client,
            install,
            &dir,
            &dir.join("config"),
            &args,
            steps,
            LIMIT,
        ) {
            Ok(report) => {
                out.files.extend([
                    format!("{mode}/ui-script.json"),
                    format!("{mode}/foliage-{mode}.png"),
                ]);
                if let Some(problem) = gpu_problem(&report["gpu_health"])
                    .or_else(|| render_problem(&report))
                    .or_else(|| steps_problem(&report))
                {
                    out.status = Status::Failed;
                    out.reason = Some(problem);
                    return Ok(out);
                }
            }
            Err(e) => {
                out.status = Status::Failed;
                out.reason = Some(e);
                return Ok(out);
            }
        }
    }
    if let Err(e) = foliage_verdict(&ctx.dir, &mut out) {
        out.status = Status::Failed;
        out.reason = Some(e);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_foliage_passes_but_black_cards_fail() {
        let (w, h) = (200, 200);
        let clean = vec![128u8; (w * h * 3) as usize];
        let m = measure(w, h, &clean, &clean);
        assert_eq!(
            m,
            Metrics {
                canopy_luma_off: 128.0,
                ..Metrics::default()
            }
        );
        assert!(m.black_on <= MAX_BLACK_SHARE);

        let mut cards = clean.clone();
        for y in 0..70 {
            for x in 40..160 {
                cards[((y * w + x) * 3) as usize..((y * w + x) * 3 + 3) as usize]
                    .copy_from_slice(&[12; 3]);
            }
        }
        let m = measure(w, h, &cards, &cards);
        assert!(m.black_on > MAX_BLACK_SHARE && m.black_off > MAX_BLACK_SHARE);
    }
}
