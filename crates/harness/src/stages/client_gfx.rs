// SPDX-License-Identifier: GPL-3.0-only
//! `client-gfx`: the graphics settings of the options menus reach the window and the renderer. The real client sets
//! the dvars the menu writes, applies them with `vid_restart` (vsync off, a 4:3 screen), starts a match (4x
//! antialiasing, no specular, depth of field, glow or shadows, four dynamic lights), then turns the match's settings back with a second
//! `vid_restart` in the running match. Every `gfxis=` check reads the renderer's and the surface's own state. A frame
//! with a bright test light in front of the player must also be clearly brighter than the same frame without it (the
//! dynamic light pass). Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-gfx";
const LIMIT: Duration = Duration::from_secs(300);

const STEPS: &str = "set=set r_vsync 0,set=set r_aspectRatio standard,set=set r_aaSamples 4,\
set=set r_specular 0,set=set r_dof_enable 0,set=set r_glow_allowed 0,set=set sm_enable 0,set=set r_dlightLimit 4,\
vidrestart,wait=1,gfxis=uncapped 1,gfxis=aspect 1.33,\
set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,\
menu=team_marinesopfor:90,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=2,\
gfxis=aa 4,gfxis=specular 0,gfxis=dof 0,gfxis=glow 0,gfxis=shadows 0,shot=gfx-on,\
set=set r_fullscreen 1,vidrestart,wait=2,gfxis=fullscreen 1,\
set=set r_gamma 0.5,wait=1,shot=gamma-lo,set=set r_gamma 2,wait=1,shot=gamma-hi,set=set r_gamma 0.8,\
set=set r_fullscreen 0,vidrestart,wait=2,gfxis=fullscreen 0,\
fxdemo=distortion/distortion_tank_muzzleflash,wait=2,shotd=distort-on,\
set=set r_distortion 0,vidrestart,wait=2,shot=distort-off,set=set r_distortion 1,vidrestart,wait=2,fxdemo=off,\
dlight=0,wait=1,shot=dlight-off,dlight=1,wait=1,gfxis=dlights 1,shot=dlight-on,dlight=0,\
set=set r_aaSamples 1,set=set r_specular 1,set=set r_dof_enable 1,set=set r_glow_allowed 1,set=set sm_enable 1,\
set=set r_aspectRatio auto,vidrestart,wait=2,\
gfxis=aa 1,gfxis=specular 1,gfxis=dof 1,gfxis=glow 1,gfxis=shadows 1,gfxis=aspect 0.00,shot=gfx-off";

/// Share of pixels a light must brighten, and by how much (of 255, in luma).
const LIT_SHARE: f64 = 0.05;
const LIT_STEP: i32 = 24;
/// How much brighter (in luma) the picture at `r_gamma` 2 must be than at 0.5.
const GAMMA_STEP: f64 = 20.0;
/// Channel value from which a pixel counts as white, and how much whiter the middle may get with distortion on.
const WHITE: u8 = 250;
const WHITE_MARGIN: f64 = 0.15;

/// Share of pixels of `on` whose luma is more than [`LIT_STEP`] above that of `off` (same size).
fn brightened_share(off: &[u8], on: &[u8]) -> f64 {
    let luma =
        |p: &[u8]| (i32::from(p[0]) * 77 + i32::from(p[1]) * 150 + i32::from(p[2]) * 29) >> 8;
    let n = off.len() / 3;
    let lit = off
        .as_chunks::<3>()
        .0
        .iter()
        .zip(on.as_chunks::<3>().0)
        .filter(|(a, b)| luma(&b[..]) - luma(&a[..]) > LIT_STEP)
        .count();
    lit as f64 / n.max(1) as f64
}

/// The frame with the test flash light brightens a real share of the picture compared with the frame without it.
fn dynamic_light_lit(dir: &std::path::Path) -> Result<(), String> {
    use super::client_models::decode;
    let (w, h, off) = decode(&dir.join("dlight-off.png"))?;
    let (w2, h2, on) = decode(&dir.join("dlight-on.png"))?;
    if (w, h) != (w2, h2) {
        return Err("dynamic light screenshots differ in size".into());
    }
    let share = brightened_share(&off, &on);
    if share < LIT_SHARE {
        return Err(format!(
            "a dynamic light brightened only {:.1}% of the picture (need {:.0}%)",
            share * 100.0,
            LIT_SHARE * 100.0
        ));
    }
    Ok(())
}

/// Mean luma (0..255) of packed RGB pixels.
fn mean_luma(px: &[u8]) -> f64 {
    let n = px.len() / 3;
    let sum: u64 = px
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| (u64::from(p[0]) * 77 + u64::from(p[1]) * 150 + u64::from(p[2]) * 29) >> 8)
        .sum();
    sum as f64 / n.max(1) as f64
}

/// Brightness is `r_gamma`'s to change: the same view at 2.0 is clearly brighter than at 0.5.
fn gamma_moves_brightness(dir: &std::path::Path) -> Result<(), String> {
    use super::client_models::decode;
    let (_, _, lo) = decode(&dir.join("gamma-lo.png"))?;
    let (_, _, hi) = decode(&dir.join("gamma-hi.png"))?;
    let (lo, hi) = (mean_luma(&lo), mean_luma(&hi));
    if hi < lo + GAMMA_STEP {
        return Err(format!(
            "r_gamma 2.0 gave mean luma {hi:.1} against {lo:.1} at 0.5 (need {GAMMA_STEP} more)"
        ));
    }
    Ok(())
}

/// Share of the middle third of an RGB picture whose pixels are (nearly) white.
fn white_share(w: usize, h: usize, px: &[u8]) -> f64 {
    let (mut white, mut n) = (0usize, 0usize);
    for y in h / 3..2 * h / 3 {
        for x in w / 3..2 * w / 3 {
            let p = &px[(y * w + x) * 3..][..3];
            white += usize::from(p.iter().all(|&c| c >= WHITE));
            n += 1;
        }
    }
    white as f64 / n.max(1) as f64
}

/// A heat-haze effect drawn with distortion on shows the scene through it, not the white a material gets when the
/// scene was never copied: the middle of the picture is no whiter than the same effect with distortion off.
fn distortion_samples_the_scene(dir: &std::path::Path) -> Result<(), String> {
    use super::client_models::decode;
    let (w, h, on) = decode(&dir.join("distort-on.png"))?;
    let (w2, h2, off) = decode(&dir.join("distort-off.png"))?;
    if (w, h) != (w2, h2) {
        return Err("distortion screenshots differ in size".into());
    }
    let (on, off) = (
        white_share(w as usize, h as usize, &on),
        white_share(w as usize, h as usize, &off),
    );
    if on > off + WHITE_MARGIN {
        return Err(format!(
            "a distortion material drew white: {:.0}% of the middle is white against {:.0}% without distortion",
            on * 100.0,
            off * 100.0
        ));
    }
    Ok(())
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
    match run_client(
        &client,
        install,
        &ctx.dir,
        &ctx.dir.join("config"),
        &["--bots", "3"],
        STEPS,
        LIMIT,
    ) {
        Ok(report) => {
            for f in [
                "ui-script.json",
                "gfx-on.png",
                "gfx-off.png",
                "dlight-off.png",
                "dlight-on.png",
                "gamma-lo.png",
                "gamma-hi.png",
                "distort-on.png",
                "distort-off.png",
            ] {
                out.files.push(f.into());
            }
            let problems: Vec<String> = verdict(&report)
                .into_iter()
                .chain(dynamic_light_lit(&ctx.dir).err())
                .chain(gamma_moves_brightness(&ctx.dir).err())
                .chain(distortion_samples_the_scene(&ctx.dir).err())
                .collect();
            if !problems.is_empty() {
                out.status = Status::Failed;
                out.reason = Some(problems.join("; "));
            } else {
                out.notes.push(
                    "vsync, aspect, antialiasing, specular, depth of field, glow and shadows took the menu's values, in the menu and in a running match".into(),
                );
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{brightened_share, mean_luma, white_share};

    #[test]
    fn white_share_counts_the_middle_third_only() {
        // 3x3 pixels: only the middle one is looked at.
        let mut px = vec![255u8; 27];
        assert_eq!(white_share(3, 3, &px), 1.0);
        px[12..15].copy_from_slice(&[10, 255, 255]);
        assert_eq!(white_share(3, 3, &px), 0.0);
    }

    #[test]
    fn mean_luma_weighs_green_most() {
        assert_eq!(mean_luma(&[0, 255, 0, 0, 0, 0]), 74.5);
    }

    #[test]
    fn counts_only_pixels_that_got_clearly_brighter() {
        let off = [10, 10, 10, 10, 10, 10, 200, 200, 200, 50, 50, 50];
        let on = [90, 90, 90, 20, 20, 20, 255, 255, 255, 50, 50, 50];
        assert_eq!(brightened_share(&off, &on), 0.5);
    }
}
