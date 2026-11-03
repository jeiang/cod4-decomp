// SPDX-License-Identifier: GPL-3.0-only
//! `client-gfx`: the graphics settings of the options menus reach the window and the renderer. The real client sets
//! the dvars the menu writes, applies them with `vid_restart` (vsync off, a 4:3 screen), starts a match (the most
//! antialiasing the adapter has when asked for 16x, lowered normal and specular map quality, no soft smoke edges or water
//! animation, no specular, depth of field, glow or shadows, four dynamic lights, the texture filter options; with it
//! on, aiming down the sights must blur the picture, and a hip-fired view must not), then turns the match's settings
//! back with a second `vid_restart` in the running match. Every `gfxis=` check reads the renderer's and the surface's
//! own state. A frame with a bright test light in front of the player must also be clearly brighter than the same
//! frame without it (the dynamic light pass). At the end the graphics device is destroyed under the match, as a driver
//! reset does it, and the client must make a new one and draw the world again. Last, a test laser sight in the mp_crash
//! garage, aimed at the floor, must draw its post light (`gfx_laser_light`, #516) only with `cg_laserLight` on, and
//! that light must brighten the floor where the laser strikes.
//!
//! The cgame and renderer dvars of #369 are checked on the way: untouched registration defaults are observed before
//! writes, and the menu-started match resets cheat dvars and refuses protected writes. A `devmap` match then checks
//! live near/far/depthhack, outdoor, sun, film, glow and blur settings without restarting the renderer, including
//! sun-flare source defaults, overrides and persistence after their gate closes, and the `r_blur` filter shaped for
//! the 4:3 `r_aspectRatio` (`RB_VirtualToSceneRadius`, #402). In that match walking bobs the eye, and after
//! `set bg_bobMax 0` on the host walking does not (#408). The view model, HUD, shellshock, unlit scene, frame counter,
//! subtitles, blood and brass retain their player-path checks. The blood checks count the impacts of their own
//! injected hits, so the match's bots stand still from there on (`bot_idle 1`): a bot's real hit on a player inside
//! a check's second played one more. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-gfx";
const LIMIT: Duration = Duration::from_secs(300);

const STEPS: &str = "cvaris=r_znear 4,cvaris=r_znear_depthhack 0.1,cvaris=r_zfar 0,\
cvaris=r_outdoor 1,cvaris=r_outdoorAwayBias 32,cvaris=r_outdoorDownBias 0,cvaris=r_outdoorFeather 8,\
cvaris=r_filmUseTweaks 0,cvaris=r_filmTweakEnable 0,cvaris=r_filmTweakBrightness 0,cvaris=r_filmTweakContrast 1.4,\
cvaris=r_filmTweakDesaturation 0.2,cvaris=r_filmTweakInvert 0,\
cvaris=r_filmTweakLightTint 1.1 1.05 0.85,cvaris=r_filmTweakDarkTint 0.7 0.85 1,\
cvaris=r_glowUseTweaks 0,cvaris=r_glowTweakEnable 0,cvaris=r_glowTweakRadius0 5,\
cvaris=r_glowTweakBloomIntensity0 1,cvaris=r_glowTweakBloomCutoff 0.5,cvaris=r_blur 0,\
cvaris=r_sun_from_dvars 0,cvaris=r_lightTweakSunLight 1,cvaris=r_lightTweakSunColor 0 1 0 1,\
cvaris=r_lightTweakSunDirection 0 0 0,cvaris=r_lightTweakAmbient 0.1,cvaris=r_lightTweakAmbientColor 1 0 0 1,\
cvaris=r_lightTweakDiffuseFraction 0.5,cvaris=r_lightTweakSunDiffuseColor 0 0 1 1,\
cvaris=r_sunsprite_shader sun,cvaris=r_sunsprite_size 16,cvaris=r_sunflare_shader sun_flare,\
cvaris=r_sunflare_min_size 0,cvaris=r_sunflare_max_size 2500,cvaris=r_sunflare_max_alpha 1,\
cvaris=r_sunflare_fadein 1,cvaris=r_sunflare_fadeout 1,cvaris=r_sun_fx_position 0 0 0,\
cgis=window_position 40 60,set=set r_vsync 0,set=set r_aspectRatio standard,set=set r_aaSamples 16,\
set=set r_picmip_manual 1,set=set r_picmip 0,set=set r_picmip_bump 1,set=set r_picmip_spec 1,set=set r_zFeather 0,set=set r_drawWater 0,\
set=set r_specular 0,set=set r_dof_enable 0,set=set r_glow_allowed 0,set=set sm_enable 0,set=set r_dlightLimit 4,\
set=set r_texFilterAnisoMin 2,set=set r_texFilterMipMode 2,\
vidrestart,wait=1,gfxis=uncapped 1,gfxis=aspect 1.33,\
set=set snd_khz 22,sawreply='22' is not a valid value for dvar 'snd_khz':5,\
set=set snd_enableEq 0,sawreply='0' is not a valid value for dvar 'snd_enableEq':5,\
set=set cg_drawLagometer 1,sawreply='1' is not a valid value for dvar 'cg_drawLagometer':5,\
set=set cg_drawGun 0,set=set r_znear 0.5,cvaris=r_znear 0.5,\
set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,\
menu=team_marinesopfor:90,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=2,\
gfxis=aa max,gfxis=picmip 0 1 1,gfxis=zfeather 0,gfxis=water 0,gfxis=specular 0,gfxis=dof 0,gfxis=blur 0,gfxis=glow 0,gfxis=shadows 0,\
gfxis=aniso 2 4,gfxis=mipmode 2,shot=gfx-on,\
gfxis=nearfar 4 0,gfxis=depthhack_near 0.1,gfxis=outdoor 1 32 0 8,gfxis=blur_radius 0,cvaris=r_znear 4,\
set=set r_znear 0.5,sawreply=r_znear is cheat protected.:5,cvaris=r_znear 4,gfxis=nearfar 4 0,\
set=set r_zfar 2000,sawreply=r_zfar is cheat protected.:5,cvaris=r_zfar 0,\
set=set r_znear_depthhack 0.25,sawreply=r_znear_depthhack is cheat protected.:5,gfxis=depthhack_near 0.1,\
set=set r_blur 3,sawreply=r_blur is cheat protected.:5,gfxis=blur_radius 0,\
set=set r_filmUseTweaks 1,sawreply=r_filmUseTweaks is cheat protected.:5,cvaris=r_filmUseTweaks 0,\
set=set r_glowUseTweaks 1,sawreply=r_glowUseTweaks is cheat protected.:5,cvaris=r_glowUseTweaks 0,\
set=set r_sun_from_dvars 1,sawreply=r_sun_from_dvars is cheat protected.:5,cvaris=r_sun_from_dvars 0,\
set=set r_lightTweakAmbient 0.9,sawreply=r_lighttweakambient is read only.:5,\
cgis=gun 1,set=set cg_drawGun 0,sawreply=cg_drawGun is cheat protected.:5,wait=1,cgis=gun 1,\
set=set sv_cheats 1,sawreply=sv_cheats is write protected.:5,\
cgis=fps 0,set=set cg_drawFPS 1,wait=1,cgis=fps 1,shot=fps,set=set cg_drawFPS Off,wait=1,cgis=fps 0,\
subtitle=MP_WAITING,wait=0.5,cgis=subtitle 1,wait=4,cgis=subtitle 0,\
set=set cg_subtitles 0,subtitle=MP_WAITING,wait=0.5,cgis=subtitle 0,set=set cg_subtitles 1,\
devmap=mp_crash,ingame=120,console=god,wait=1,\
gfxis=nearfar 4 0,gfxis=depthhack_near 0.1,gfxis=outdoor 1 32 0 8,\
set=set r_znear 0.5,set=set r_zfar 2000,set=set r_znear_depthhack 0.25,\
set=set r_outdoor 0,set=set r_outdoorAwayBias 4,set=set r_outdoorDownBias 2,set=set r_outdoorFeather 16,\
set=set r_sun_from_dvars 1,wait=1,gfxis=sun_flare 1 16 0 1250 1 1000 1000 sun sun_flare,\
set=set r_lightTweakSunLight 4,set=set r_lightTweakSunColor 0 0 2,set=set r_lightTweakSunDirection 10 20 0,\
wait=1,gfxis=nearfar 0.5 2000,gfxis=depthhack_near 0.25,gfxis=outdoor 0 4 2 16,\
gfxis=sun_tweak 4 0 0 1 10 20 0,\
set=set r_znear 1,set=set r_zfar 4000,set=set r_znear_depthhack 0.2,\
set=set r_outdoor 1,set=set r_outdoorAwayBias 12,set=set r_outdoorDownBias 3,set=set r_outdoorFeather 6,\
set=set r_lightTweakSunLight 3,set=set r_lightTweakSunColor 1 0 0,set=set r_lightTweakSunDirection 0 90 0,\
wait=1,gfxis=nearfar 1 4000,gfxis=depthhack_near 0.2,gfxis=outdoor 1 12 3 6,gfxis=sun_tweak 3 1 0 0 0 90 0,\
set=set r_lightTweakSunLight 1,set=set r_lightTweakSunColor 1 1 1,set=set r_lightTweakSunDirection 0 0 0,\
set=set r_sunsprite_shader sun,set=set r_sunflare_shader sun_flare,set=set r_sunsprite_size 24,\
set=set r_sunflare_min_size 8,set=set r_sunflare_max_size 1200,set=set r_sunflare_max_alpha 0.6,\
set=set r_sunflare_fadein 0.25,set=set r_sunflare_fadeout 0.5,set=set r_sun_fx_position 0 90 0,\
wait=1,gfxis=sun_flare 1 24 4 600 0.6 250 500 sun sun_flare,gfxis=sun_fx_position 0 1 0,\
set=set r_sun_from_dvars 0,set=set r_sunsprite_size 40,set=set r_sunflare_max_size 1800,\
wait=1,gfxis=sun_flare 1 24 4 600 0.6 250 500 sun sun_flare,gfxis=sun_fx_position 0 1 0,\
set=set r_znear 4,set=set r_zfar 0,set=set r_znear_depthhack 0.1,\
set=set r_outdoorAwayBias 32,set=set r_outdoorDownBias 0,set=set r_outdoorFeather 8,\
wait=1,gfxis=nearfar 4 0,gfxis=depthhack_near 0.1,gfxis=outdoor 1 32 0 8,\
gfxis=blur_radius 0,set=set r_blur 3,wait=1,gfxis=blur_radius 3,gfxis=blur_shape 1.33,\
set=set r_blur 4,wait=1,gfxis=blur_radius 4,set=set r_blur 0,wait=1,gfxis=blur_radius 0,\
cgis=gun 1,set=set cg_drawGun 0,wait=1,cgis=gun 0,shot=no-gun,set=set cg_drawGun 1,wait=1,cgis=gun 1,\
cgis=hud2d 1,set=set cg_draw2D 0,wait=1,cgis=hud2d 0,shot=no-2d,set=set cg_draw2D 1,wait=1,cgis=hud2d 1,\
shock=8000,wait=1,cgis=shock 1,set=set cg_drawShellshock 0,wait=1,cgis=shock 0,set=set cg_drawShellshock 1,wait=1,cgis=shock 1,\
server=set bot_idle 1,wait=1,fxmark,set=set cg_blood 0,hitflesh,wait=1,cgis=fx:bullet_impact_noblood +1,cgis=fx:bullet_impact_flesh +0,\
set=set cg_blood 1,hitflesh,wait=1,cgis=fx:bullet_impact_flesh +1,cgis=fx:bullet_impact_noblood +1,\
set=set cg_brass 0,wait=0.1,fxmark,holdkey=mouse1,wait=1,releasekey=mouse1,cgis=fx:shell_eject +0,set=set cg_brass 1,\
cgis=fullbright 0,set=set r_fullbright 1,wait=1,cgis=fullbright 1,shot=fullbright,set=set r_fullbright 0,wait=1,cgis=fullbright 0,\
cgis=centertime 3000,set=set cg_centertime 1.5,wait=1,cgis=centertime 1500,set=set cg_centertime 3,\
set=set cg_crosshairAlpha 0,set=set cg_crosshairAlphaMin 0,wait=1,cgis=crosshair_alpha 0.00,\
set=set cg_crosshairAlphaMin 0.75,wait=1,cgis=crosshair_alpha 0.75,holdkey=mouse2,releasekey=mouse2,wait=1,cgis=crosshair_alpha hidden,\
holdkey=mouse2,releasekey=mouse2,wait=1,cgis=crosshair_alpha 0.75,set=set cg_crosshairAlpha 1,set=set cg_crosshairAlphaMin 0.5,\
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
set=set r_filmUseTweaks 1,set=set r_filmTweakEnable 1,wait=1,\
gfxis=film_tweak 0 1.4 0.2 0 1.1 1.05 0.85 0.7 0.85 1,\
set=set r_filmTweakContrast 1.5,set=set r_filmTweakBrightness 0.1,set=set r_filmTweakDesaturation 0.25,set=set r_filmTweakInvert 1,\
set=set r_filmTweakLightTint 0.5 1 2,set=set r_filmTweakDarkTint 0 0 0,wait=1,\
gfxis=film_tweak 0.1 1.5 0.25 1 0.5 1 2 0 0 0,cgis=film_sample 0.10 1.50 0.25,\
set=set r_filmUseTweaks 0,set=set r_filmTweakEnable 0,\
set=set r_glowUseTweaks 1,set=set r_glowTweakEnable 1,wait=1,gfxis=glow_tweak 1 5 1 0.5,\
set=set r_glowTweakRadius0 7,set=set r_glowTweakBloomIntensity0 1.5,set=set r_glowTweakBloomCutoff 0.3,wait=1,\
gfxis=glow_tweak 1 7 1.5 0.3,\
set=set r_glowTweakRadius0 3,set=set r_glowTweakBloomIntensity0 2,set=set r_glowTweakBloomCutoff 0.6,wait=1,\
gfxis=glow_tweak 1 3 2 0.6,set=set r_glowUseTweaks 0,set=set r_glowTweakEnable 0,\
fxmark,set=set cg_bloodLimitMsec 2000,set=set cg_bloodLimit 1,hitflesh,hitflesh,wait=1,cgis=fx:bullet_impact_flesh +1,\
hitflesh,wait=1.2,cgis=fx:bullet_impact_flesh +1,hitflesh,cgis=fx:bullet_impact_flesh +2,\
set=set cg_bloodLimitMsec 1,wait=0.1,hitflesh,wait=0.1,hitflesh,cgis=fx:bullet_impact_flesh +4,set=set cg_bloodLimit 0,set=set cg_bloodLimitMsec 330,\
set=set cl_noprint 1,wait=1,console=echo hidden_print_369,wait=1,notsaw=hidden_print_369,\
set=set cl_noprint 0,wait=1,console=echo visible_print_369,saw=visible_print_369:5,\
set=set vid_xpos 40,set=set vid_ypos 60,vidrestart,wait=2,cgis=window_position 40 60,set=set vid_xpos 10000,set=set vid_ypos -10000,vidrestart,wait=2,cgis=window_position 4096 -4096,set=set vid_xpos 40,set=set vid_ypos 60,\
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
gfxis=aa 1,gfxis=specular 1,gfxis=dof 1,gfxis=blur 0,holdkey=mouse2,releasekey=mouse2,wait=2,gfxis=blur 1,\
holdkey=mouse2,releasekey=mouse2,wait=1,gfxis=glow 1,gfxis=shadows 1,gfxis=aspect 0.00,shot=gfx-off,\
bobmark,holdkey=w,wait=1.5,releasekey=w,holdkey=s,wait=1.5,releasekey=s,cgis=eyebob 1,\
console=set bg_bobMax 0,wait=1,bobmark,holdkey=w,wait=1.5,releasekey=w,holdkey=s,wait=1.5,releasekey=s,cgis=eyebob 0,\
console=set bg_bobMax 8,wait=1,\
losedevice,renderer=90,ingame=120,wait=2,gfxis=aa 1,shot=recovered,\
console=setviewpos 200.8 2267.5 323.5 175.3 20,wait=1,laser=1,set=set cg_laserLight 0,wait=1,gfxis=postlights 0,\
shot=laser-off,set=set cg_laserLight 1,wait=1,gfxis=postlights 1+,shot=laser-on,laser=0";

/// Share of pixels a light must brighten, and by how much (of 255, in luma).
const LIT_SHARE: f64 = 0.05;
const LIT_STEP: i32 = 24;
/// Where a test laser aimed along the view strikes the garage floor, as fractions of the picture (x from, x to, y from,
/// y to, y down): just below the crosshair, clear of the gun and the HUD, which move between frames on their own.
const LASER_STRIKE: [f64; 4] = [0.45, 0.55, 0.45, 0.6];
/// Share of [`LASER_STRIKE`]'s pixels the laser's post light must brighten, and by how much (of 255, in luma): the
/// light is a few units wide, so a patch of it (measured about 7%, and nothing without the light).
const LASER_LIT_SHARE: f64 = 0.015;
const LASER_LIT_STEP: i32 = 8;
/// How much brighter (in luma) the picture at `r_gamma` 2 must be than at 0.5.
const GAMMA_STEP: f64 = 20.0;
/// Channel value from which a pixel counts as white, and how much whiter the middle may get with distortion on.
const WHITE: u8 = 250;
const WHITE_MARGIN: f64 = 0.15;
/// A picture with a world in it has more light than this on average (of 255).
const BLANK_LUMA: f64 = 5.0;

/// Pixels of `on` whose luma is more than `step` above that of `off` (same size).
fn brightened(off: &[u8], on: &[u8], step: i32) -> usize {
    let luma =
        |p: &[u8]| (i32::from(p[0]) * 77 + i32::from(p[1]) * 150 + i32::from(p[2]) * 29) >> 8;
    off.as_chunks::<3>()
        .0
        .iter()
        .zip(on.as_chunks::<3>().0)
        .filter(|(a, b)| luma(&b[..]) - luma(&a[..]) > step)
        .count()
}

/// Share of pixels of `on` whose luma is more than [`LIT_STEP`] above that of `off` (same size).
fn brightened_share(off: &[u8], on: &[u8]) -> f64 {
    brightened(off, on, LIT_STEP) as f64 / (off.len() / 3).max(1) as f64
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

/// A laser sight with `cg_laserLight` on lights what it strikes (`gfx_laser_light`, #516): the frame with the post light
/// brightens a patch of the floor where the laser strikes ([`LASER_STRIKE`]) against the same laser with
/// `cg_laserLight` off. Returns the pixels it brightened.
fn laser_light_lit(dir: &std::path::Path) -> Result<usize, String> {
    use super::client_models::decode;
    let (w, h, off) = decode(&dir.join("laser-off.png"))?;
    let (w2, h2, on) = decode(&dir.join("laser-on.png"))?;
    if (w, h) != (w2, h2) {
        return Err("laser light screenshots differ in size".into());
    }
    let (w, h) = (w as usize, h as usize);
    let off = crop(w, h, &off, LASER_STRIKE);
    let lit = brightened(&off, &crop(w, h, &on, LASER_STRIKE), LASER_LIT_STEP);
    let need = (LASER_LIT_SHARE * (off.len() / 3) as f64).ceil() as usize;
    if lit < need {
        return Err(format!(
            "a laser sight's post light brightened {lit} pixels where it struck (need {need})"
        ));
    }
    Ok(lit)
}

/// The pixels of a packed RGB picture inside `[x from, x to, y from, y to]`, as fractions of its size.
fn crop(w: usize, h: usize, px: &[u8], [x0, x1, y0, y1]: [f64; 4]) -> Vec<u8> {
    let at = |f: f64, n: usize| (f * n as f64) as usize;
    let (x0, x1) = (at(x0, w), at(x1, w));
    (at(y0, h)..at(y1, h))
        .flat_map(|y| &px[(y * w + x0) * 3..(y * w + x1) * 3])
        .copied()
        .collect()
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
    let config_dir = ctx.dir.join("config");
    let profile = config_dir.join("players/profiles/harness");
    std::fs::create_dir_all(&profile)?;
    std::fs::write(
        profile.join("config_mp.cfg"),
        "seta vid_xpos 40\nseta vid_ypos 60\n",
    )?;
    let mut out = StageReport::new(NAME, Status::Passed);
    match run_client(
        &client,
        install,
        &ctx.dir,
        &config_dir,
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
                "laser-off.png",
                "laser-on.png",
            ] {
                out.files.push(f.into());
            }
            let laser = laser_light_lit(&ctx.dir);
            let problems: Vec<String> = verdict(&report)
                .into_iter()
                .chain(dynamic_light_lit(&ctx.dir).err())
                .chain(gamma_moves_brightness(&ctx.dir).err())
                .chain(distortion_samples_the_scene(&ctx.dir).err())
                .chain(device_recovers(&ctx.dir, &report).err())
                .chain(laser.as_ref().err().cloned())
                .collect();
            if let Ok(lit) = laser {
                out.notes.push(format!(
                    "a laser sight's post light brightened {lit} pixels where it struck"
                ));
            }
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
    use super::{brightened_share, crop, mean_luma, white_share};

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

    #[test]
    fn crop_keeps_the_pixels_inside_its_fractions() {
        // 4x3 pixels, each its own index: columns 1..3 of row 1.
        let px: Vec<u8> = (0..12u8).flat_map(|i| [i; 3]).collect();
        assert_eq!(
            crop(4, 3, &px, [0.25, 0.75, 0.34, 0.67]),
            vec![5, 5, 5, 6, 6, 6]
        );
    }
}
