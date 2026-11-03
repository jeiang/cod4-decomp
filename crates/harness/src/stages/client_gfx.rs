// SPDX-License-Identifier: GPL-3.0-only
//! `client-gfx`: the graphics settings of the options menus reach the window and the renderer. The real client is
//! driven through the control channel (#378) as a player plays it (#376): every setting is typed at the console opened
//! with its key, the match is started with the pointer through Start New Server, and the team and class menus are
//! clicked. The stage sets the dvars the menu writes, applies them with `vid_restart` (vsync off, a 4:3 screen),
//! starts a match (the most antialiasing the adapter has when asked for 16x, lowered normal and specular map quality,
//! no soft smoke edges or water animation, no specular, depth of field, glow or shadows, four dynamic lights, the
//! texture filter options; with it on, aiming down the sights must blur the picture, and a hip-fired view must not),
//! then turns the match's settings back with a second `vid_restart` in the running match. Every graphics check reads
//! the renderer's and the surface's own state (the `gfx` section of the state document), every cgame check what the
//! cgame drew or played (`cgame`, `fx.played`). A frame with a bright test light in front of the player must also be
//! clearly brighter than the same frame without it (the dynamic light pass). At the end the graphics device is
//! destroyed under the match, as a driver reset does it, and the client must make a new one and draw the world again.
//! Last, a test laser sight in the mp_crash garage, aimed at the floor, must draw its post light (`gfx_laser_light`,
//! #516) only with `cg_laserLight` on, and that light must brighten the floor where the laser strikes.
//!
//! The cgame and renderer dvars of #369 are checked on the way: untouched registration defaults are observed before
//! writes, and the menu-started match resets cheat dvars and refuses protected writes. A match started with `devmap`
//! typed at the console then checks live near/far/depthhack, outdoor, sun, film, glow and blur settings without
//! restarting the renderer, including sun-flare source defaults, overrides and persistence after their gate closes,
//! and the `r_blur` filter shaped for the 4:3 `r_aspectRatio` (`RB_VirtualToSceneRadius`, #402). In that match walking
//! bobs the eye, and after `set bg_bobMax 0` on the host walking does not (#408). The view model, HUD, shellshock,
//! unlit scene, frame counter, subtitles, blood and brass retain their player-path checks. What no key reaches (a
//! subtitle, a shellshock, a bullet striking flesh, the test light, an effect played in front of the player, the
//! device loss, the bob and frame-pace probes) is set up with the channel's `dev` hooks, each followed by what the
//! player sees. Needs a display and the install; skips without.
//! The blood and brass and blood limit checks each run with the host's `bot_idle 1` (after a one-second wait) and set
//! `bot_idle 0` afterwards, so real bot hits cannot contaminate injected-hit counts; the result notes retain each
//! `fx.impacts` entry of that window with its source, entity and weapon.
use super::client_flythrough::locate_client;
use super::client_match::FRAG_SUICIDE_ERROR;
use super::client_models::no_display;
use super::client_session::verdict;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::time::{Duration, Instant};

const NAME: &str = "client-gfx";
/// The listen server's menus a menu-started match opens for the player to answer.
const TEAM_MENU: &str = "team_marinesopfor";
const CLASS_MENU: &str = "changeclass";

/// Registration defaults observed before any write (#369).
const DEFAULTS: &[(&str, &str)] = &[
    ("r_znear", "4"),
    ("r_znear_depthhack", "0.1"),
    ("r_zfar", "0"),
    ("r_outdoor", "1"),
    ("r_outdoorAwayBias", "32"),
    ("r_outdoorDownBias", "0"),
    ("r_outdoorFeather", "8"),
    ("r_filmUseTweaks", "0"),
    ("r_filmTweakEnable", "0"),
    ("r_filmTweakBrightness", "0"),
    ("r_filmTweakContrast", "1.4"),
    ("r_filmTweakDesaturation", "0.2"),
    ("r_filmTweakInvert", "0"),
    ("r_filmTweakLightTint", "1.1 1.05 0.85"),
    ("r_filmTweakDarkTint", "0.7 0.85 1"),
    ("r_glowUseTweaks", "0"),
    ("r_glowTweakEnable", "0"),
    ("r_glowTweakRadius0", "5"),
    ("r_glowTweakBloomIntensity0", "1"),
    ("r_glowTweakBloomCutoff", "0.5"),
    ("r_blur", "0"),
    ("r_sun_from_dvars", "0"),
    ("r_lightTweakSunLight", "1"),
    ("r_lightTweakSunColor", "0 1 0 1"),
    ("r_lightTweakSunDirection", "0 0 0"),
    ("r_lightTweakAmbient", "0.1"),
    ("r_lightTweakAmbientColor", "1 0 0 1"),
    ("r_lightTweakDiffuseFraction", "0.5"),
    ("r_lightTweakSunDiffuseColor", "0 0 1 1"),
    ("r_sunsprite_shader", "sun"),
    ("r_sunsprite_size", "16"),
    ("r_sunflare_shader", "sun_flare"),
    ("r_sunflare_min_size", "0"),
    ("r_sunflare_max_size", "2500"),
    ("r_sunflare_max_alpha", "1"),
    ("r_sunflare_fadein", "1"),
    ("r_sunflare_fadeout", "1"),
    ("r_sun_fx_position", "0 0 0"),
];

/// What the options menus write before the match: vsync off, a 4:3 screen, 16x antialiasing, the picmips, no soft
/// smoke edges, water animation, specular, depth of field, glow or shadows, four dynamic lights, the texture filter.
const MENU_SETTINGS: &[&str] = &[
    "set r_vsync 0",
    "set r_aspectRatio standard",
    "set r_aaSamples 16",
    "set r_picmip_manual 1",
    "set r_picmip 0",
    "set r_picmip_bump 1",
    "set r_picmip_spec 1",
    "set r_zFeather 0",
    "set r_drawWater 0",
    "set r_specular 0",
    "set r_dof_enable 0",
    "set r_glow_allowed 0",
    "set sm_enable 0",
    "set r_dlightLimit 4",
    "set r_texFilterAnisoMin 2",
    "set r_texFilterMipMode 2",
];

/// The menus' settings turned back in the running match.
const MENU_SETTINGS_BACK: &[&str] = &[
    "set r_aaSamples 1",
    "set r_specular 1",
    "set r_dof_enable 1",
    "set r_glow_allowed 1",
    "set sm_enable 1",
    "set r_zFeather 1",
    "set r_drawWater 1",
    "set r_texFilterAnisoMin 1",
    "set r_texFilterMipMode Unchanged",
    "set r_aspectRatio auto",
];

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

/// Numbers compare within rounding and words exactly; a missing, extra or non-finite token fails. The comparison the
/// client's `cvaris=` and `gfxis=` steps made.
fn value_matches(have: &str, want: &str) -> bool {
    let mut have = have.split_whitespace();
    for want in want.split_whitespace() {
        let Some(have) = have.next() else {
            return false;
        };
        let ok = match (have.parse::<f32>(), want.parse::<f32>()) {
            (Ok(h), Ok(w)) => h.is_finite() && w.is_finite() && (h - w).abs() <= 0.005,
            _ => have == want,
        };
        if !ok {
            return false;
        }
    }
    have.next().is_none()
}

/// Types each line at the console, as the player does.
fn typed(c: &mut Driven, lines: &[&str]) -> Result<(), String> {
    for line in lines {
        c.console(line)?;
    }
    Ok(())
}

/// A variable's value as the client holds it matches `want`.
fn cvar_is(c: &mut Driven, name: &str, want: &str) -> Result<(), String> {
    let r = c.ask(json!({"cmd": "cvar", "name": name}))?;
    let have = r["value"].as_str().unwrap_or_default();
    if value_matches(have, want) {
        Ok(())
    } else {
        Err(format!("{name} is {have:?}, want {want:?}"))
    }
}

/// One fact of a section of named facts (`gfx`, `cgame`) as its text; an error where it is unavailable.
fn fact(c: &mut Driven, section: &str, name: &str) -> Result<String, String> {
    let v = c.lookup(&format!("{section}.{name}"))?;
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{section}.{name} is unavailable ({v})"))
}

/// What a graphics setting came to in the renderer or on the surface (`gfx`): `aa max` is the most samples the
/// adapter offers, `1+` at least one.
fn gfx_is(c: &mut Driven, name: &str, want: &str) -> Result<(), String> {
    let have = fact(c, "gfx", name)?;
    let want = if (name, want) == ("aa", "max") {
        fact(c, "gfx", "aa_max")?
    } else {
        want.to_owned()
    };
    let ok = match want.strip_suffix('+') {
        Some(min) => matches!(
            (have.parse::<u64>(), min.parse::<u64>()),
            (Ok(h), Ok(m)) if h >= m
        ),
        None => value_matches(&have, &want),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("gfx {name} is {have:?}, want {want:?}"))
    }
}

/// What the cgame drew last frame (`cgame`), exactly.
fn cg_is(c: &mut Driven, name: &str, want: &str) -> Result<(), String> {
    let have = fact(c, "cgame", name)?;
    if have == want {
        Ok(())
    } else {
        Err(format!("cgame {name} is {have:?}, want {want:?}"))
    }
}

/// How often each effect kind has played (`fx.played`), to count what plays from now.
fn fx_mark(c: &mut Driven) -> Result<Value, String> {
    c.lookup("fx.played")
}

/// Runs `f` with the server's bots idle (`bot_idle 1`, then a second to settle) so real bot hits cannot
/// add to the injected-hit counts, notes every impact the client recorded meanwhile (`fx.impacts`) under
/// `label`, and lets the bots move again (`bot_idle 0`) whatever `f` returned.
fn bots_idle(
    c: &mut Driven,
    notes: &mut Vec<String>,
    label: &str,
    f: impl FnOnce(&mut Driven) -> Result<(), String>,
) -> Result<(), String> {
    let idle = |c: &mut Driven, on: &str| {
        c.ask(json!({"cmd": "server", "op": "exec", "line": format!("set bot_idle {on}")}))
    };
    idle(c, "1")?;
    c.pause(Duration::from_secs(1))?;
    let start = c.lookup("fx.impacts")?.as_array().map_or(0, Vec::len);
    let result = f(c);
    let impacts = c.lookup("fx.impacts")?;
    let window = impacts.as_array().map(|v| &v[start.min(v.len())..]);
    notes.push(format!("{label} impacts: {}", json!(window)));
    idle(c, "0")?;
    result
}

/// The effect `kind` played exactly `n` times since `mark` (so a wrong kind fails as much as a missing one).
fn fx_since(c: &mut Driven, mark: &Value, kind: &str, n: u64) -> Result<(), String> {
    let now = c
        .lookup(&format!("fx.played.{kind}"))?
        .as_u64()
        .unwrap_or(0);
    let delta = now.saturating_sub(mark[kind].as_u64().unwrap_or(0));
    if delta == n {
        Ok(())
    } else {
        Err(format!(
            "{kind} played {delta} times since the mark, want {n}"
        ))
    }
}

/// The console's lines.
fn console_lines(c: &mut Driven) -> Result<Vec<String>, String> {
    Ok(c.lookup("console.lines")?
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

/// A console line with `text` within `limit`; with `reply`, not a typed line's echo (`] …`) but what the client or the
/// server answered.
fn saw(c: &mut Driven, text: &str, reply: bool, limit: Duration) -> Result<(), String> {
    let end = Instant::now() + limit;
    loop {
        let seen = console_lines(c)?
            .iter()
            .any(|l| l.contains(text) && !(reply && l.starts_with("] ")));
        if seen {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!(
                "no console line with {text:?} within {} s",
                limit.as_secs_f32()
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

/// What the client or the server answered at the console.
fn replied(c: &mut Driven, text: &str) -> Result<(), String> {
    saw(c, text, true, Duration::from_secs(5))
}

/// No console line has `text` now.
fn not_saw(c: &mut Driven, text: &str) -> Result<(), String> {
    match console_lines(c)?.iter().find(|l| l.contains(text)) {
        Some(l) => Err(format!("the console shows {l:?}")),
        None => Ok(()),
    }
}

/// One of the client's dev hooks: scenario setup no key or menu reaches.
fn dev(c: &mut Driven, op: &str, arg: &str) -> Result<(), String> {
    c.ask(json!({"cmd": "dev", "op": op, "arg": arg})).map(drop)
}

/// A mouse button pressed and let go, a frame apart.
fn press(c: &mut Driven, button: &str) -> Result<(), String> {
    c.button(button, "down")?;
    c.button(button, "up")
}

/// Holds a key for `d`.
fn hold(c: &mut Driven, key: &str, d: Duration) -> Result<(), String> {
    c.key(key, "down")?;
    c.pause(d)?;
    c.key(key, "up")
}

/// The first frame from now that copied the scene for a distortion material, captured as `<name>.png`.
fn capture_distortion(c: &mut Driven, name: &str) -> Result<(), String> {
    let shot = c.ask(json!({"cmd": "capture", "after": "distortion"}))?;
    let from = shot["path"]
        .as_str()
        .ok_or_else(|| format!("no capture path: {shot}"))?;
    let to = c.dir.join(format!("{name}.png"));
    std::fs::rename(from, &to).map_err(|e| format!("{from} -> {}: {e}", to.display()))
}

/// The listen server's team and class menus answered with the pointer, and the spawn.
fn join(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.until(
        json!({"menu.open": {"contains": TEAM_MENU}}),
        secs(90),
        "the team menu did not open",
    )?;
    c.click("auto_assign")?;
    c.until(
        json!({"menu.open": {"contains": CLASS_MENU}}),
        secs(30),
        "the class menu did not open after the team was picked",
    )?;
    c.pause(secs(1))?;
    c.click("Assault")?;
    c.until(
        json!({"net.spawned": true, "player.alive": true}),
        secs(120),
        "the player did not spawn",
    )?;
    Ok(())
}

/// Walks forward and back with the movement keys.
fn walk(c: &mut Driven) -> Result<(), String> {
    hold(c, "w", Duration::from_millis(1500))?;
    hold(c, "s", Duration::from_millis(1500))
}

/// The front end: registration defaults, the menus' settings applied with `vid_restart`, the values the dvars refuse,
/// and a match started with the pointer through Start New Server.
fn front_end(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("main menu", |c| {
        c.until(
            json!({"menu.open": {"contains": "main"}}),
            secs(90),
            "the main menu did not open",
        )
        .map(drop)
    })?;
    c.step("registered defaults", |c| {
        for (name, want) in DEFAULTS {
            cvar_is(c, name, want)?;
        }
        // The profile's `vid_xpos 40` and `vid_ypos 60` placed the window.
        cg_is(c, "window_position", "40 60")
    })?;
    c.step("menu settings applied with vid_restart", |c| {
        typed(c, MENU_SETTINGS)?;
        c.console("vid_restart")?;
        c.pause(secs(1))?;
        gfx_is(c, "uncapped", "1")?;
        gfx_is(c, "aspect", "1.33")
    })?;
    c.step("values the dvars refuse", |c| {
        for (line, reply) in [
            (
                "set snd_khz 22",
                "'22' is not a valid value for dvar 'snd_khz'",
            ),
            (
                "set snd_enableEq 0",
                "'0' is not a valid value for dvar 'snd_enableEq'",
            ),
            (
                "set cg_drawLagometer 1",
                "'1' is not a valid value for dvar 'cg_drawLagometer'",
            ),
        ] {
            c.console(line)?;
            replied(c, reply)?;
        }
        Ok(())
    })?;
    c.step("cheat dvars written before the match", |c| {
        typed(c, &["set cg_drawGun 0", "set r_znear 0.5"])?;
        cvar_is(c, "r_znear", "0.5")
    })?;
    c.step("Start New Server", |c| {
        c.console("set ui_netGametypeName war")?;
        c.click("Start New Server")?;
        c.until(
            json!({"menu.open": {"contains": "createserver"}}),
            secs(20),
            "Start New Server did not open the server settings",
        )?;
        c.click("Start")?;
        join(c)?;
        c.pause(secs(2))
    })
}

/// The menu-started match (`sv_cheats` 0): the menus' settings in the renderer, the cheat dvars reset and their writes
/// refused, the frame counter and subtitles.
fn menu_match(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("the menus' settings in the match", |c| {
        for (name, want) in [
            ("aa", "max"),
            ("picmip", "0 1 1"),
            ("zfeather", "0"),
            ("water", "0"),
            ("specular", "0"),
            ("dof", "0"),
            ("blur", "0"),
            ("glow", "0"),
            ("shadows", "0"),
            ("aniso", "2 4"),
            ("mipmode", "2"),
        ] {
            gfx_is(c, name, want)?;
        }
        c.capture("gfx-on").map(drop)
    })?;
    c.step("cheat dvars reset by the match", |c| {
        gfx_is(c, "nearfar", "4 0")?;
        gfx_is(c, "depthhack_near", "0.1")?;
        gfx_is(c, "outdoor", "1 32 0 8")?;
        gfx_is(c, "blur_radius", "0")?;
        cvar_is(c, "r_znear", "4")
    })?;
    c.step("protected writes refused", |c| {
        c.console("set r_znear 0.5")?;
        replied(c, "r_znear is cheat protected.")?;
        cvar_is(c, "r_znear", "4")?;
        gfx_is(c, "nearfar", "4 0")?;
        c.console("set r_zfar 2000")?;
        replied(c, "r_zfar is cheat protected.")?;
        cvar_is(c, "r_zfar", "0")?;
        c.console("set r_znear_depthhack 0.25")?;
        replied(c, "r_znear_depthhack is cheat protected.")?;
        gfx_is(c, "depthhack_near", "0.1")?;
        c.console("set r_blur 3")?;
        replied(c, "r_blur is cheat protected.")?;
        gfx_is(c, "blur_radius", "0")?;
        for name in ["r_filmUseTweaks", "r_glowUseTweaks", "r_sun_from_dvars"] {
            c.console(&format!("set {name} 1"))?;
            replied(c, &format!("{name} is cheat protected."))?;
            cvar_is(c, name, "0")?;
        }
        c.console("set r_lightTweakAmbient 0.9")?;
        replied(c, "r_lighttweakambient is read only.")?;
        cg_is(c, "gun", "1")?;
        c.console("set cg_drawGun 0")?;
        replied(c, "cg_drawGun is cheat protected.")?;
        c.pause(secs(1))?;
        cg_is(c, "gun", "1")?;
        c.console("set sv_cheats 1")?;
        replied(c, "sv_cheats is write protected.")
    })?;
    c.step("frame counter", |c| {
        cg_is(c, "fps", "0")?;
        c.console("set cg_drawFPS 1")?;
        c.pause(secs(1))?;
        cg_is(c, "fps", "1")?;
        c.capture("fps")?;
        c.console("set cg_drawFPS Off")?;
        c.pause(secs(1))?;
        cg_is(c, "fps", "0")
    })?;
    c.step("subtitles", |c| {
        dev(c, "subtitle", "MP_WAITING")?;
        c.pause(Duration::from_millis(500))?;
        cg_is(c, "subtitle", "1")?;
        c.pause(secs(4))?;
        cg_is(c, "subtitle", "0")?;
        c.console("set cg_subtitles 0")?;
        dev(c, "subtitle", "MP_WAITING")?;
        c.pause(Duration::from_millis(500))?;
        cg_is(c, "subtitle", "0")?;
        c.console("set cg_subtitles 1")
    })
}

/// The match started with `devmap` typed at the console: the live renderer dvars, without a renderer restart.
fn devmap_renderer(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("devmap at the console", |c| {
        c.console("devmap mp_crash")?;
        join(c)?;
        c.console("god")?;
        c.pause(secs(1))
    })?;
    c.step("near/far, depth hack, outdoor and sun tweaks", |c| {
        gfx_is(c, "nearfar", "4 0")?;
        gfx_is(c, "depthhack_near", "0.1")?;
        gfx_is(c, "outdoor", "1 32 0 8")?;
        typed(
            c,
            &[
                "set r_znear 0.5",
                "set r_zfar 2000",
                "set r_znear_depthhack 0.25",
                "set r_outdoor 0",
                "set r_outdoorAwayBias 4",
                "set r_outdoorDownBias 2",
                "set r_outdoorFeather 16",
                "set r_sun_from_dvars 1",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "sun_flare", "1 16 0 1250 1 1000 1000 sun sun_flare")?;
        typed(
            c,
            &[
                "set r_lightTweakSunLight 4",
                "set r_lightTweakSunColor 0 0 2",
                "set r_lightTweakSunDirection 10 20 0",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "nearfar", "0.5 2000")?;
        gfx_is(c, "depthhack_near", "0.25")?;
        gfx_is(c, "outdoor", "0 4 2 16")?;
        gfx_is(c, "sun_tweak", "4 0 0 1 10 20 0")?;
        typed(
            c,
            &[
                "set r_znear 1",
                "set r_zfar 4000",
                "set r_znear_depthhack 0.2",
                "set r_outdoor 1",
                "set r_outdoorAwayBias 12",
                "set r_outdoorDownBias 3",
                "set r_outdoorFeather 6",
                "set r_lightTweakSunLight 3",
                "set r_lightTweakSunColor 1 0 0",
                "set r_lightTweakSunDirection 0 90 0",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "nearfar", "1 4000")?;
        gfx_is(c, "depthhack_near", "0.2")?;
        gfx_is(c, "outdoor", "1 12 3 6")?;
        gfx_is(c, "sun_tweak", "3 1 0 0 0 90 0")
    })?;
    c.step("sun flare overrides", |c| {
        typed(
            c,
            &[
                "set r_lightTweakSunLight 1",
                "set r_lightTweakSunColor 1 1 1",
                "set r_lightTweakSunDirection 0 0 0",
                "set r_sunsprite_shader sun",
                "set r_sunflare_shader sun_flare",
                "set r_sunsprite_size 24",
                "set r_sunflare_min_size 8",
                "set r_sunflare_max_size 1200",
                "set r_sunflare_max_alpha 0.6",
                "set r_sunflare_fadein 0.25",
                "set r_sunflare_fadeout 0.5",
                "set r_sun_fx_position 0 90 0",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "sun_flare", "1 24 4 600 0.6 250 500 sun sun_flare")?;
        gfx_is(c, "sun_fx_position", "0 1 0")?;
        // The overrides persist once their gate closes.
        typed(
            c,
            &[
                "set r_sun_from_dvars 0",
                "set r_sunsprite_size 40",
                "set r_sunflare_max_size 1800",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "sun_flare", "1 24 4 600 0.6 250 500 sun sun_flare")?;
        gfx_is(c, "sun_fx_position", "0 1 0")
    })?;
    c.step("near/far and outdoor back", |c| {
        typed(
            c,
            &[
                "set r_znear 4",
                "set r_zfar 0",
                "set r_znear_depthhack 0.1",
                "set r_outdoorAwayBias 32",
                "set r_outdoorDownBias 0",
                "set r_outdoorFeather 8",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "nearfar", "4 0")?;
        gfx_is(c, "depthhack_near", "0.1")?;
        gfx_is(c, "outdoor", "1 32 0 8")
    })?;
    c.step("r_blur shaped for 4:3", |c| {
        gfx_is(c, "blur_radius", "0")?;
        c.console("set r_blur 3")?;
        c.pause(secs(1))?;
        gfx_is(c, "blur_radius", "3")?;
        gfx_is(c, "blur_shape", "1.33")?;
        c.console("set r_blur 4")?;
        c.pause(secs(1))?;
        gfx_is(c, "blur_radius", "4")?;
        c.console("set r_blur 0")?;
        c.pause(secs(1))?;
        gfx_is(c, "blur_radius", "0")
    })
}

/// A cgame fact one dvar switches: `name` is `before` with the dvar as it is, `after` once `change` is typed (the frame
/// captured as `shot` when given), and `before` again once `back` is.
fn toggled(
    c: &mut Driven,
    name: &str,
    [before, after]: [&str; 2],
    [change, back]: [&str; 2],
    shot: Option<&str>,
) -> Result<(), String> {
    let secs = Duration::from_secs;
    cg_is(c, name, before)?;
    c.console(change)?;
    c.pause(secs(1))?;
    cg_is(c, name, after)?;
    if let Some(shot) = shot {
        c.capture(shot)?;
    }
    c.console(back)?;
    c.pause(secs(1))?;
    cg_is(c, name, before)
}

/// The devmap match's cgame: view model, 2D, shellshock, blood and brass, unlit scene, centre prints, crosshair,
/// killcam cameras and the film and glow output.
fn devmap_cgame(c: &mut Driven, notes: &mut Vec<String>) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("view model", |c| {
        toggled(
            c,
            "gun",
            ["1", "0"],
            ["set cg_drawGun 0", "set cg_drawGun 1"],
            Some("no-gun"),
        )
    })?;
    c.step("2D", |c| {
        toggled(
            c,
            "hud2d",
            ["1", "0"],
            ["set cg_draw2D 0", "set cg_draw2D 1"],
            Some("no-2d"),
        )
    })?;
    c.step("shellshock", |c| {
        dev(c, "shock", "8000")?;
        c.pause(secs(1))?;
        toggled(
            c,
            "shock",
            ["1", "0"],
            ["set cg_drawShellshock 0", "set cg_drawShellshock 1"],
            None,
        )
    })?;
    c.step("blood and brass", |c| {
        bots_idle(c, notes, "blood and brass", |c| {
            let mark = fx_mark(c)?;
            c.console("set cg_blood 0")?;
            dev(c, "hitflesh", "")?;
            c.pause(secs(1))?;
            fx_since(c, &mark, "bullet_impact_noblood", 1)?;
            fx_since(c, &mark, "bullet_impact_flesh", 0)?;
            c.console("set cg_blood 1")?;
            dev(c, "hitflesh", "")?;
            c.pause(secs(1))?;
            fx_since(c, &mark, "bullet_impact_flesh", 1)?;
            fx_since(c, &mark, "bullet_impact_noblood", 1)?;
            c.console("set cg_brass 0")?;
            c.pause(Duration::from_millis(100))?;
            let mark = fx_mark(c)?;
            hold(c, "mouse1", secs(1))?;
            fx_since(c, &mark, "shell_eject", 0)?;
            c.console("set cg_brass 1")
        })
    })?;
    c.step("unlit scene", |c| {
        toggled(
            c,
            "fullbright",
            ["0", "1"],
            ["set r_fullbright 1", "set r_fullbright 0"],
            Some("fullbright"),
        )
    })?;
    c.step("centre print time", |c| {
        cg_is(c, "centertime", "3000")?;
        c.console("set cg_centertime 1.5")?;
        c.pause(secs(1))?;
        cg_is(c, "centertime", "1500")?;
        c.console("set cg_centertime 3")
    })?;
    c.step("crosshair opacity", |c| {
        typed(
            c,
            &["set cg_crosshairAlpha 0", "set cg_crosshairAlphaMin 0"],
        )?;
        c.pause(secs(1))?;
        cg_is(c, "crosshair_alpha", "0.00")?;
        c.console("set cg_crosshairAlphaMin 0.75")?;
        c.pause(secs(1))?;
        cg_is(c, "crosshair_alpha", "0.75")?;
        // Down the sights the stock gates leave no reticle; back from them it shows again.
        press(c, "mouse2")?;
        c.pause(secs(1))?;
        cg_is(c, "crosshair_alpha", "hidden")?;
        press(c, "mouse2")?;
        c.pause(secs(1))?;
        cg_is(c, "crosshair_alpha", "0.75")?;
        typed(
            c,
            &["set cg_crosshairAlpha 1", "set cg_crosshairAlphaMin 0.5"],
        )
    })?;
    c.step("killcam cameras", |c| {
        cg_is(c, "heli_camera", "-1000 50 15 0 1900 2100 2400 4 2")?;
        typed(
            c,
            &[
                "set cg_heliKillCamDist 500",
                "set cg_heliKillCamFov 40",
                "set cg_heliKillCamZDist 80",
                "set cg_heliKillCamNearBlurStart 10",
                "set cg_heliKillCamNearBlurEnd 200",
                "set cg_heliKillCamFarBlurStart 150",
                "set cg_heliKillCamFarBlurDist 400",
                "set cg_heliKillCamNearBlur 6",
                "set cg_heliKillCamFarBlur 3",
            ],
        )?;
        c.pause(secs(1))?;
        cg_is(c, "heli_camera", "-500 80 40 10 1300 1650 2050 6 3")?;
        typed(
            c,
            &[
                "set cg_airstrikeKillCamDist 300",
                "set cg_airstrikeKillCamFov 60",
                "set cg_airstrikeKillCamCloseZDist 40",
                "set cg_airstrikeKillCamNearBlurStart 20",
                "set cg_airstrikeKillCamNearBlurEnd 120",
                "set cg_airstrikeKillCamFarBlurStart 200",
                "set cg_airstrikeKillCamFarBlurDist 500",
                "set cg_airstrikeKillCamNearBlur 7",
                "set cg_airstrikeKillCamFarBlur 5",
            ],
        )?;
        c.pause(secs(1))?;
        cg_is(c, "air_camera", "340 60 20 220 540 1040 7 5")
    })?;
    c.step("brightness, contrast and desaturation", |c| {
        c.console("set r_brightness 0.25")?;
        c.pause(secs(1))?;
        cg_is(c, "brightness", "0.25")?;
        c.console("set r_brightness 0")?;
        c.pause(secs(1))?;
        cg_is(c, "brightness", "0.00")?;
        typed(c, &["set r_contrast 2", "set r_desaturation 0"])?;
        c.pause(secs(1))?;
        cg_is(c, "film_sample", "0.00 2.00 0.25")?;
        c.console("set r_desaturation 2")?;
        c.pause(secs(1))?;
        cg_is(c, "film_sample", "0.00 2.00 0.75")?;
        typed(c, &["set r_contrast 1", "set r_desaturation 1"])?;
        c.pause(secs(1))?;
        cg_is(c, "film_sample", "0.00 1.00 0.50")
    })?;
    c.step("film tweaks", |c| {
        typed(c, &["set r_filmUseTweaks 1", "set r_filmTweakEnable 1"])?;
        c.pause(secs(1))?;
        gfx_is(c, "film_tweak", "0 1.4 0.2 0 1.1 1.05 0.85 0.7 0.85 1")?;
        typed(
            c,
            &[
                "set r_filmTweakContrast 1.5",
                "set r_filmTweakBrightness 0.1",
                "set r_filmTweakDesaturation 0.25",
                "set r_filmTweakInvert 1",
                "set r_filmTweakLightTint 0.5 1 2",
                "set r_filmTweakDarkTint 0 0 0",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "film_tweak", "0.1 1.5 0.25 1 0.5 1 2 0 0 0")?;
        cg_is(c, "film_sample", "0.10 1.50 0.25")?;
        typed(c, &["set r_filmUseTweaks 0", "set r_filmTweakEnable 0"])
    })?;
    c.step("glow tweaks", |c| {
        typed(c, &["set r_glowUseTweaks 1", "set r_glowTweakEnable 1"])?;
        c.pause(secs(1))?;
        gfx_is(c, "glow_tweak", "1 5 1 0.5")?;
        typed(
            c,
            &[
                "set r_glowTweakRadius0 7",
                "set r_glowTweakBloomIntensity0 1.5",
                "set r_glowTweakBloomCutoff 0.3",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "glow_tweak", "1 7 1.5 0.3")?;
        typed(
            c,
            &[
                "set r_glowTweakRadius0 3",
                "set r_glowTweakBloomIntensity0 2",
                "set r_glowTweakBloomCutoff 0.6",
            ],
        )?;
        c.pause(secs(1))?;
        gfx_is(c, "glow_tweak", "1 3 2 0.6")?;
        typed(c, &["set r_glowUseTweaks 0", "set r_glowTweakEnable 0"])
    })?;
    c.step("blood limit", |c| {
        bots_idle(c, notes, "blood limit", |c| {
            let mark = fx_mark(c)?;
            typed(c, &["set cg_bloodLimitMsec 2000", "set cg_bloodLimit 1"])?;
            // Two hits a frame apart: the second is within the limit.
            dev(c, "hitflesh", "")?;
            dev(c, "hitflesh", "")?;
            c.pause(secs(1))?;
            fx_since(c, &mark, "bullet_impact_flesh", 1)?;
            // A third about a second later is still within it; a fourth past two seconds is not.
            dev(c, "hitflesh", "")?;
            c.pause(Duration::from_millis(1200))?;
            fx_since(c, &mark, "bullet_impact_flesh", 1)?;
            dev(c, "hitflesh", "")?;
            fx_since(c, &mark, "bullet_impact_flesh", 2)?;
            c.console("set cg_bloodLimitMsec 1")?;
            c.pause(Duration::from_millis(100))?;
            dev(c, "hitflesh", "")?;
            c.pause(Duration::from_millis(100))?;
            dev(c, "hitflesh", "")?;
            fx_since(c, &mark, "bullet_impact_flesh", 4)?;
            typed(c, &["set cg_bloodLimit 0", "set cg_bloodLimitMsec 330"])
        })
    })?;
    c.step("cl_noprint", |c| {
        c.console("set cl_noprint 1")?;
        c.pause(secs(1))?;
        c.console("echo hidden_print_369")?;
        c.pause(secs(1))?;
        not_saw(c, "hidden_print_369")?;
        c.console("set cl_noprint 0")?;
        c.pause(secs(1))?;
        c.console("echo visible_print_369")?;
        saw(c, "visible_print_369", false, secs(5))
    })
}

/// The devmap match's window and surface: position, frame cap, fullscreen, gamma, distortion, the dynamic light, the
/// menus' settings back, the eye bob and a lost device.
fn devmap_window(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("window position", |c| {
        typed(c, &["set vid_xpos 40", "set vid_ypos 60", "vid_restart"])?;
        c.pause(secs(2))?;
        cg_is(c, "window_position", "40 60")?;
        typed(
            c,
            &["set vid_xpos 10000", "set vid_ypos -10000", "vid_restart"],
        )?;
        c.pause(secs(2))?;
        cg_is(c, "window_position", "4096 -4096")?;
        typed(c, &["set vid_xpos 40", "set vid_ypos 60"])
    })?;
    c.step("frame cap", |c| {
        c.console("set com_maxfps 20")?;
        c.pause(secs(1))?;
        dev(c, "pacereset", "")?;
        c.pause(secs(2))?;
        gfx_is(c, "pace", "1")?;
        c.console("set com_maxfps 0")
    })?;
    c.step("fullscreen and gamma", |c| {
        typed(c, &["set r_fullscreen 1", "vid_restart"])?;
        c.pause(secs(2))?;
        gfx_is(c, "fullscreen", "1")?;
        c.console("set r_gamma 0.5")?;
        c.pause(secs(1))?;
        c.capture("gamma-lo")?;
        c.console("set r_gamma 2")?;
        c.pause(secs(1))?;
        c.capture("gamma-hi")?;
        c.console("set r_gamma 0.8")?;
        typed(c, &["set r_fullscreen 0", "vid_restart"])?;
        c.pause(secs(2))?;
        gfx_is(c, "fullscreen", "0")
    })?;
    c.step("distortion", |c| {
        dev(c, "fxdemo", "distortion/distortion_tank_muzzleflash")?;
        c.pause(secs(2))?;
        capture_distortion(c, "distort-on")?;
        typed(c, &["set r_distortion 0", "vid_restart"])?;
        c.pause(secs(2))?;
        c.capture("distort-off")?;
        typed(c, &["set r_distortion 1", "vid_restart"])?;
        c.pause(secs(2))?;
        dev(c, "fxdemo", "off")
    })?;
    c.step("dynamic light", |c| {
        dev(c, "dlight", "0")?;
        c.pause(secs(1))?;
        c.capture("dlight-off")?;
        dev(c, "dlight", "1")?;
        c.pause(secs(1))?;
        gfx_is(c, "dlights", "1+")?;
        c.capture("dlight-on")?;
        dev(c, "dlight", "0")
    })?;
    c.step("menu settings back with vid_restart", |c| {
        typed(c, MENU_SETTINGS_BACK)?;
        c.console("vid_restart")?;
        c.pause(secs(2))?;
        for (name, want) in [
            ("zfeather", "1"),
            ("water", "1"),
            ("aa", "1"),
            ("specular", "1"),
            ("dof", "1"),
            ("blur", "0"),
        ] {
            gfx_is(c, name, want)?;
        }
        // Down the sights depth of field blurs the picture; hip-fired it does not.
        press(c, "mouse2")?;
        c.pause(secs(2))?;
        gfx_is(c, "blur", "1")?;
        press(c, "mouse2")?;
        c.pause(secs(1))?;
        gfx_is(c, "glow", "1")?;
        gfx_is(c, "shadows", "1")?;
        gfx_is(c, "aspect", "0.00")?;
        c.capture("gfx-off").map(drop)
    })?;
    c.step("eye bob", |c| {
        dev(c, "bobmark", "")?;
        walk(c)?;
        cg_is(c, "eyebob", "1")?;
        c.console("set bg_bobMax 0")?;
        c.pause(secs(1))?;
        dev(c, "bobmark", "")?;
        walk(c)?;
        cg_is(c, "eyebob", "0")?;
        c.console("set bg_bobMax 8")?;
        c.pause(secs(1))
    })?;
    c.step("device lost and made again", |c| {
        dev(c, "losedevice", "")?;
        c.until(
            json!({"client.in_world": true}),
            secs(90),
            "the world was not drawable again",
        )?;
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            secs(120),
            "the match did not go on after the device was made again",
        )?;
        c.pause(secs(2))?;
        gfx_is(c, "aa", "1")?;
        c.capture("recovered").map(drop)
    })?;
    // A test laser sight aimed at the garage floor draws its post light only with `cg_laserLight` on (#516).
    c.step("laser post light", |c| {
        c.console("setviewpos 200.8 2267.5 323.5 175.3 20")?;
        c.pause(secs(1))?;
        dev(c, "laser", "1")?;
        c.console("set cg_laserLight 0")?;
        c.pause(secs(1))?;
        gfx_is(c, "postlights", "0")?;
        c.capture("laser-off")?;
        c.console("set cg_laserLight 1")?;
        c.pause(secs(1))?;
        gfx_is(c, "postlights", "1+")?;
        c.capture("laser-on")?;
        dev(c, "laser", "0")
    })
}

/// A listen server's script errors (its `script` state: `[map, VM text]` pairs and the frag suicides it counted):
/// how many there were, and how many of them are excused, being the stock `ch_kills` error a frag suicide raises
/// ([`FRAG_SUICIDE_ERROR`], #539) with a counted suicide behind each, as `client-match` excuses them. The text's first
/// line is the message, its first `at` the innermost site, as the listen report reads them.
fn frag_suicide_errors(script: &Value) -> (u64, u64) {
    let errors = script["errors"].as_array().map_or(&[][..], Vec::as_slice);
    let seen = errors
        .iter()
        .filter(|e| {
            let mut lines = e[1]
                .as_str()
                .unwrap_or("")
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with("*******"));
            lines.next() == Some(FRAG_SUICIDE_ERROR.0)
                && lines.find_map(|l| l.strip_prefix("at ")) == Some(FRAG_SUICIDE_ERROR.1)
        })
        .count() as u64;
    let suicides = script["frag_suicides"].as_u64().unwrap_or(0);
    (errors.len() as u64, seen.min(suicides))
}

/// The listen server's script errors are all the stock `ch_kills` error with a frag suicide behind each (#557): a
/// bot's frag can kill its thrower, and the stock scripts then raise that error as the original's VM does, so
/// `logcheck` lets its two lines through in this stage up to a bound, and this fails on any error not so explained.
fn frag_suicides_explain_errors(c: &mut Driven) -> Result<(), String> {
    let script = c.ask(json!({"cmd": "server", "op": "state", "what": "script"}))?;
    match frag_suicide_errors(&script) {
        (errors, excused) if errors <= excused => Ok(()),
        (errors, excused) => Err(format!(
            "{errors} script errors on the listen server, {} not the stock ch_kills error behind a counted frag \
             suicide",
            errors - excused
        )),
    }
}

/// The whole run, step by step. Each listen server's script errors are checked before the next match replaces it.
fn play(c: &mut Driven, notes: &mut Vec<String>) -> Result<(), String> {
    front_end(c)?;
    menu_match(c)?;
    frag_suicides_explain_errors(c)?;
    devmap_renderer(c)?;
    devmap_cgame(c, notes)?;
    devmap_window(c)?;
    frag_suicides_explain_errors(c)
}

/// The run's report in the shape the session verdict reads (the steps, `gpu_health`, `render`, the match's `net`
/// counters and the HUD facts), written as `control-run.json` with the state it came from.
fn assemble(c: &mut Driven) -> Result<Value, String> {
    let written = c.report(
        "control-run.json",
        &["client", "render", "report", "hud", "gfx", "cgame"],
    )?;
    Ok(json!({
        "steps": written["steps"],
        "gpu_health": written["client"]["gpu_health"],
        "render": written["render"],
        "net": written["report"],
        "hud": written["hud"]["report"],
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
    // `Driven` gives the run the harness profile under `<dir>/config`; the window starts where it places it.
    let profile = ctx.dir.join("config/players/profiles/harness");
    std::fs::create_dir_all(&profile)?;
    std::fs::write(
        profile.join("config_mp.cfg"),
        "seta vid_xpos 40\nseta vid_ypos 60\n",
    )?;
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(&client, install, &ctx.dir, &["--bots", "3"]) {
        Ok(c) => c,
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
            return Ok(out);
        }
    };
    out.notes.push(format!("channel {}", c.addr));
    let played = play(&mut c, &mut out.notes);
    // Read before the run ends, played out or not: a failed step still says what the renderer and the device did.
    let report = assemble(&mut c);
    let status = c.finish(Duration::from_secs(30));
    for f in [
        "control-run.json",
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
        if ctx.dir.join(f).is_file() {
            out.files.push(f.into());
        }
    }
    let mut laser_lit = None;
    let problems: Vec<String> = match (played, report) {
        (Err(e), _) => vec![e],
        (Ok(()), Err(e)) => vec![format!("the run's report could not be read: {e}")],
        (Ok(()), Ok(report)) => {
            let laser = laser_light_lit(&ctx.dir);
            laser_lit = laser.as_ref().ok().copied();
            verdict(&report)
                .into_iter()
                .chain(dynamic_light_lit(&ctx.dir).err())
                .chain(gamma_moves_brightness(&ctx.dir).err())
                .chain(distortion_samples_the_scene(&ctx.dir).err())
                .chain(device_recovers(&ctx.dir, &report).err())
                .chain(laser.err())
                .collect()
        }
    };
    if let Some(lit) = laser_lit {
        out.notes.push(format!(
            "a laser sight's post light brightened {lit} pixels where it struck"
        ));
    }
    if !problems.is_empty() {
        out.status = Status::Failed;
        out.reason = Some(problems.join("; "));
    } else if !status.is_some_and(|s| s.success()) {
        out.status = Status::Failed;
        out.reason = Some(match status {
            Some(s) => format!("the client exited {s}"),
            None => "the client did not quit and was killed".to_owned(),
        });
    } else {
        out.notes.push(
            "vsync, aspect, antialiasing, specular, depth of field, glow and shadows took the menu's values, in the menu and in a running match".into(),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        brightened_share, crop, frag_suicide_errors, mean_luma, value_matches, white_share,
    };
    use serde_json::json;

    /// Only the stock `ch_kills` frag-suicide error is excused, read from the VM text with or without its banner, and
    /// only up to the server's counted frag suicides; every other script error is unexplained (#557).
    #[test]
    fn only_stock_ch_kills_errors_beyond_the_frag_suicides_are_unexplained() {
        let stock = "cannot cast undefined to bool\n  at maps\\mp\\gametypes\\_missions::ch_kills line 545\n  \
                     at maps\\mp\\gametypes\\_missions::domissioncallback line 1153\n";
        let bannered = format!("******* script runtime error *******\n{stock}");
        let other_site = "cannot cast undefined to bool\n  at maps\\mp\\gametypes\\_missions::ch_kills line 546\n";
        let other_error = "cannot cast undefined to int\n  at maps\\mp\\gametypes\\_missions::ch_kills line 545\n";
        let stock_only = |suicides: u64| json!({"errors": [["mp_crash", stock], ["mp_crash", bannered]], "frag_suicides": suicides});
        assert_eq!(frag_suicide_errors(&stock_only(2)), (2, 2));
        assert_eq!(frag_suicide_errors(&stock_only(3)), (2, 2));
        assert_eq!(frag_suicide_errors(&stock_only(1)), (2, 1));
        assert_eq!(frag_suicide_errors(&json!({"errors": []})), (0, 0));
        assert_eq!(
            frag_suicide_errors(&json!({"errors": [["mp_backlot", stock]]})),
            (1, 0)
        );
        for other in [other_site, other_error] {
            let script =
                json!({"errors": [["mp_crash", stock], ["mp_crash", other]], "frag_suicides": 5});
            assert_eq!(frag_suicide_errors(&script), (2, 1));
        }
    }

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
    fn renderer_values_compare_numerically_but_materials_exactly() {
        assert!(value_matches("0.5 2000", "0.50 2000.0"));
        assert!(value_matches("0 1 0", "0.0000001 0.9999999 0"));
        assert!(value_matches("sun sun_flare", "sun sun_flare"));
        assert!(!value_matches("sun sun_flare", "sun other"));
        assert!(!value_matches("1 2", "1"));
        assert!(!value_matches("1", "1 2"));
        assert!(!value_matches("NaN", "NaN"));
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
