// SPDX-License-Identifier: GPL-3.0-only
//! `client-gfx`: the graphics settings of the options menus reach the window and the renderer. The real client sets
//! the dvars the menu writes, applies them with `vid_restart` (vsync off, a 4:3 screen), starts a match (the most
//! antialiasing the adapter has when asked for 16x, lowered normal and specular map quality, no soft smoke edges or water
//! animation, no specular, depth of field, glow or shadows, four dynamic lights, the texture filter options; with it
//! on, aiming down the sights must blur the picture, and a hip-fired view must not), then turns the match's settings
//! back with a second `vid_restart` in the running match. Every `gfxis=` check reads the renderer's and the surface's
//! own state. A frame with a bright test light in front of the player must also be clearly brighter than the same
//! frame without it (the dynamic light pass). At the end the graphics device is destroyed under the match, as a driver
//! reset does it, and the client must make a new one and draw the world again.
//!
//! The cgame dvars of #369 are checked on the way: a sound or HUD option this build has one value of is refused with
//! the stock domain line; the menu-started match runs without cheats, so a cheat dvar set at the main menu is reset
//! at the join and a write in the match is refused as cheat protected; a `devmap` match then has them act (the view
//! model, the 2D HUD, the shellshock blend and the unlit scene go off and come back), beside the frame rate counter,
//! subtitles, blood and brass. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-gfx";
const LIMIT: Duration = Duration::from_secs(300);

const STEPS: &str = "set=set r_vsync 0,set=set r_aspectRatio standard,set=set r_aaSamples 16,\
set=set r_picmip_manual 1,set=set r_picmip 0,set=set r_picmip_bump 1,set=set r_picmip_spec 1,set=set r_zFeather 0,set=set r_drawWater 0,\
set=set r_specular 0,set=set r_dof_enable 0,set=set r_glow_allowed 0,set=set sm_enable 0,set=set r_dlightLimit 4,\
set=set r_texFilterAnisoMin 2,set=set r_texFilterMipMode 2,\
vidrestart,wait=1,gfxis=uncapped 1,gfxis=aspect 1.33,\
set=set snd_khz 22,sawreply='22' is not a valid value for dvar 'snd_khz':5,\
set=set snd_enableEq 0,sawreply='0' is not a valid value for dvar 'snd_enableEq':5,\
set=set cg_drawLagometer 1,sawreply='1' is not a valid value for dvar 'cg_drawLagometer':5,\
set=set cg_drawGun 0,\
set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,\
menu=team_marinesopfor:90,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=2,\
gfxis=aa max,gfxis=picmip 0 1 1,gfxis=zfeather 0,gfxis=water 0,gfxis=specular 0,gfxis=dof 0,gfxis=blur 0,gfxis=glow 0,gfxis=shadows 0,\
gfxis=aniso 2 4,gfxis=mipmode 2,shot=gfx-on,\
cgis=gun 1,set=set cg_drawGun 0,sawreply=cg_drawGun is cheat protected.:5,wait=1,cgis=gun 1,\
set=set sv_cheats 1,sawreply=sv_cheats is write protected.:5,\
cgis=fps 0,set=set cg_drawFPS 1,wait=1,cgis=fps 1,shot=fps,set=set cg_drawFPS Off,wait=1,cgis=fps 0,\
subtitle=MP_WAITING,wait=0.5,cgis=subtitle 1,wait=4,cgis=subtitle 0,\
set=set cg_subtitles 0,subtitle=MP_WAITING,wait=0.5,cgis=subtitle 0,set=set cg_subtitles 1,\
devmap=mp_crash,ingame=120,wait=2,\
cgis=gun 1,set=set cg_drawGun 0,wait=1,cgis=gun 0,shot=no-gun,set=set cg_drawGun 1,wait=1,cgis=gun 1,\
cgis=hud2d 1,set=set cg_draw2D 0,wait=1,cgis=hud2d 0,shot=no-2d,set=set cg_draw2D 1,wait=1,cgis=hud2d 1,\
shock=8000,wait=1,cgis=shock 1,set=set cg_drawShellshock 0,wait=1,cgis=shock 0,set=set cg_drawShellshock 1,wait=1,cgis=shock 1,\
fxmark,set=set cg_blood 0,hitflesh,wait=1,cgis=fx:bullet_impact_noblood +1,cgis=fx:bullet_impact_flesh +0,\
set=set cg_blood 1,hitflesh,wait=1,cgis=fx:bullet_impact_flesh +1,cgis=fx:bullet_impact_noblood +1,\
set=set cg_brass 0,holdkey=mouse1,wait=1,releasekey=mouse1,cgis=fx:shell_eject 0,set=set cg_brass 1,\
cgis=fullbright 0,set=set r_fullbright 1,wait=1,cgis=fullbright 1,shot=fullbright,set=set r_fullbright 0,wait=1,cgis=fullbright 0,\
cgis=centertime 3000,set=set cg_centertime 1.5,wait=1,cgis=centertime 1500,set=set cg_centertime 3,\
set=set cg_crosshairAlpha 0,set=set cg_crosshairAlphaMin 0,wait=1,cgis=crosshair_alpha 0.00,\
set=set cg_crosshairAlphaMin 0.75,wait=1,cgis=crosshair_alpha 0.75,set=set cg_crosshairAlpha 1,set=set cg_crosshairAlphaMin 0.5,\
cgis=heli_camera -1000 50 15 0 1900 2100 2400 4 2,\
set=set cg_heliKillCamDist 500,set=set cg_heliKillCamFov 40,set=set cg_heliKillCamZDist 80,\
set=set cg_heliKillCamNearBlurStart 10,set=set cg_heliKillCamNearBlurEnd 200,\
set=set cg_heliKillCamFarBlurStart 150,set=set cg_heliKillCamFarBlurDist 400,\
set=set cg_heliKillCamNearBlur 6,set=set cg_heliKillCamFarBlur 3,wait=1,cgis=heli_camera -500 80 40 10 1300 1650 2050 6 3,\
set=set cg_airstrikeKillCamDist 300,set=set cg_airstrikeKillCamFov 60,set=set cg_airstrikeKillCamCloseZDist 40,\
set=set cg_airstrikeKillCamNearBlurStart 20,set=set cg_airstrikeKillCamNearBlurEnd 120,\
set=set cg_airstrikeKillCamFarBlurStart 200,set=set cg_airstrikeKillCamFarBlurDist 500,\
set=set cg_airstrikeKillCamNearBlur 7,set=set cg_airstrikeKillCamFarBlur 5,wait=1,cgis=air_camera 340 60 20 220 540 1040 7 5,\
set=set r_brightness 0.25,wait=1,cgis=brightness 0.25,set=set r_brightness 0,wait=1,cgis=brightness 0.00,\
set=set r_contrast 2,set=set r_desaturation 0,wait=1,cgis=film_sample 0.00 2.00 0.25,\
set=set r_desaturation 2,wait=1,cgis=film_sample 0.00 2.00 0.75,\
set=set r_contrast 1,set=set r_desaturation 1,wait=1,cgis=film_sample 0.00 1.00 0.50,\
fxmark,set=set cg_bloodLimitMsec 2000,set=set cg_bloodLimit 1,hitflesh,hitflesh,wait=1,cgis=fx:bullet_impact_flesh +1,\
hitflesh,wait=1.2,cgis=fx:bullet_impact_flesh +1,hitflesh,cgis=fx:bullet_impact_flesh +2,\
set=set cg_bloodLimitMsec 1,wait=0.1,hitflesh,wait=0.1,hitflesh,cgis=fx:bullet_impact_flesh +4,set=set cg_bloodLimit 0,set=set cg_bloodLimitMsec 330,\
set=set cl_noprint 1,wait=1,console=echo hidden_print_369,wait=1,notsaw=hidden_print_369,\
set=set cl_noprint 0,wait=1,console=echo visible_print_369,saw=visible_print_369:5,\
set=set vid_xpos 40,set=set vid_ypos 60,vidrestart,wait=2,cgis=window_position 40 60,\
set=set com_maxfps 20,wait=1,pacereset,wait=2,gfxis=pace 1,set=set com_maxfps 0,\
set=set r_fullscreen 1,vidrestart,wait=2,gfxis=fullscreen 1,\
set=set r_gamma 0.5,wait=1,shot=gamma-lo,set=set r_gamma 2,wait=1,shot=gamma-hi,set=set r_gamma 0.8,\
set=set r_fullscreen 0,vidrestart,wait=2,gfxis=fullscreen 0,\
fxdemo=distortion/distortion_tank_muzzleflash,wait=2,shotd=distort-on,\
set=set r_distortion 0,vidrestart,wait=2,shot=distort-off,set=set r_distortion 1,vidrestart,wait=2,fxdemo=off,\
dlight=0,wait=1,shot=dlight-off,dlight=1,wait=1,gfxis=dlights 1+,shot=dlight-on,dlight=0,\
set=set r_aaSamples 1,set=set r_specular 1,set=set r_dof_enable 1,set=set r_glow_allowed 1,set=set sm_enable 1,\
set=set r_zFeather 1,set=set r_drawWater 1,set=set r_texFilterAnisoMin 1,set=set r_texFilterMipMode Unchanged,\
set=set r_aspectRatio auto,vidrestart,wait=2,gfxis=zfeather 1,gfxis=water 1,\
gfxis=aa 1,gfxis=specular 1,gfxis=dof 1,gfxis=blur 0,holdkey=mouse2,wait=2,gfxis=blur 1,releasekey=mouse2,gfxis=glow 1,gfxis=shadows 1,gfxis=aspect 0.00,shot=gfx-off,\
losedevice,renderer=90,ingame=120,wait=2,gfxis=aa 1,shot=recovered";

/// Share of pixels a light must brighten, and by how much (of 255, in luma).
const LIT_SHARE: f64 = 0.05;
const LIT_STEP: i32 = 24;
/// How much brighter (in luma) the picture at `r_gamma` 2 must be than at 0.5.
const GAMMA_STEP: f64 = 20.0;
/// Channel value from which a pixel counts as white, and how much whiter the middle may get with distortion on.
const WHITE: u8 = 250;
const WHITE_MARGIN: f64 = 0.15;
/// A picture with a world in it has more light than this on average (of 255).
const BLANK_LUMA: f64 = 5.0;

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

/// A graphics device that is lost under a running match is made again: the report names the loss, and the picture
/// after it is not blank.
fn device_recovers(dir: &std::path::Path, report: &serde_json::Value) -> Result<(), String> {
    use super::client_models::decode;
    let losses = report["gpu_health"]["device_losses"]
        .as_array()
        .map_or(0, Vec::len);
    if losses == 0 {
        return Err("the device was destroyed and the client did not notice".into());
    }
    let (_, _, px) = decode(&dir.join("recovered.png"))?;
    let luma = mean_luma(&px);
    if luma < BLANK_LUMA {
        return Err(format!(
            "the picture after the device was made again has mean luma {luma:.1}"
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
                "recovered.png",
                "fps.png",
                "no-gun.png",
                "no-2d.png",
                "fullbright.png",
            ] {
                out.files.push(f.into());
            }
            let problems: Vec<String> = verdict(&report)
                .into_iter()
                .chain(dynamic_light_lit(&ctx.dir).err())
                .chain(gamma_moves_brightness(&ctx.dir).err())
                .chain(distortion_samples_the_scene(&ctx.dir).err())
                .chain(device_recovers(&ctx.dir, &report).err())
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
