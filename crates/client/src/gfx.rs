// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): gfx_d3d/r_scene.cpp (`R_SetFilmInfo`).
//! The graphics settings of the options menus (`r_*`, `sm_enable`), read as the renderer and the window take them.
//!
//! The menus only write dvars; [`Gfx::from_cvars`] turns them into the video request, the present mode, the aspect
//! ratio and the renderer's [`render::Settings`]. A match's renderer takes them when its map loads; `vid_restart`
//! applies the ones that can change under a running window and renderer.

use crate::display::{FullscreenKind, Request};
use crate::input::Cvars;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Gfx {
    /// `r_fullscreen`.
    pub fullscreen: bool,
    /// `r_mode`: `WxH`.
    pub mode: Option<(u32, u32)>,
    /// `r_displayRefresh`: `N Hz`.
    pub refresh: Option<f64>,
    /// `r_monitor`: the 1-based monitor fullscreen goes to, 0 for automatic.
    pub monitor: Option<usize>,
    /// `vid_xpos`/`vid_ypos`, physical coordinates of a windowed window.
    pub position: Option<(i32, i32)>,
    /// `r_aspectRatio`: the shape of the screen, `None` for the window's own.
    pub aspect: Option<f32>,
    /// `r_vsync`.
    pub vsync: bool,
    /// `r_aaSamples`.
    pub aa_samples: u32,
    /// `r_picmip` unless `r_picmip_manual` is off, when the engine picks full size; and the same for `r_picmip_bump`
    /// (normal maps) and `r_picmip_spec` (specular maps).
    pub picmip: usize,
    pub picmip_bump: usize,
    pub picmip_spec: usize,
    /// `r_texFilterAnisoMax` and `r_texFilterAnisoMin` (the least anisotropy any mipped sampler gets, capped by the
    /// most).
    pub aniso_max: u16,
    pub aniso_min: u16,
    /// `r_texFilterMipMode`: `Unchanged`, `Force Trilinear`, `Force Bilinear`, `Force MipMaps Off` (0 to 3).
    pub mip_mode: render::MipMode,
    /// `r_fullbright`: the scene without lighting. A cheat dvar: only with `sv_cheats` on.
    pub fullbright: bool,
    /// `sm_enable`.
    pub shadows: bool,
    /// `r_specular`.
    pub specular: bool,
    /// `r_dof_enable`.
    pub dof: bool,
    /// `r_dof_bias`, within the 0.1 to 3 the engine registers it with.
    pub dof_bias: f32,
    /// `r_dof_tweak`: the blur the `r_dof_*` dvars describe, replacing the game's.
    pub dof_tweak: Option<render::Dof>,
    /// `r_glow_allowed`.
    pub glow: bool,
    /// `r_dlightLimit`, at most four.
    pub dlight_limit: usize,
    /// `sc_enable` and `sc_count`.
    pub cookies: bool,
    pub cookie_count: usize,
    /// `sm_spotEnable` and `r_spotLightShadows`.
    pub spot_shadows: bool,
    pub dynamic_spot_shadows: bool,
    /// `sm_maxLights`, at most four, and `sm_spotShadowFadeTime` in seconds.
    pub max_shadow_lights: usize,
    pub spot_fade_time: f32,
    /// `r_lodScaleRigid` and `r_lodScaleSkinned`, `r_lodBiasRigid` and `r_lodBiasSkinned`.
    pub lod_scale: [f32; 2],
    pub lod_bias: [f32; 2],
    /// `r_spotLightStartRadius`, `r_spotLightEndRadius`, `r_spotLightFovInnerFraction` and `r_spotLightBrightness`.
    pub spot: render::SpotParams,
    /// `r_drawSun`.
    pub draw_sun: bool,
    /// `r_distortion`.
    pub distortion: bool,
    /// `r_zFeather`.
    pub z_feather: bool,
    /// `r_drawWater`.
    pub draw_water: bool,
    /// `r_clear`, `r_clearColor`, `r_clearColor2` and `developer`: what the pixels nothing draws show.
    pub clear: render::Clear,
}

/// `1920x1080` as a size.
fn parse_mode(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// `60 Hz` as hertz.
fn parse_hz(s: &str) -> Option<f64> {
    s.trim()
        .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == ' ')
        .parse()
        .ok()
        .filter(|&hz: &f64| hz > 0.0)
}

/// What the aspect menu stores: `auto`, `standard` (4:3), `wide 16:10`, `wide 16:9`.
fn parse_aspect(s: &str) -> Option<f32> {
    let s = s.to_ascii_lowercase();
    if s.contains("16:10") {
        Some(16.0 / 10.0)
    } else if s.contains("16:9") {
        Some(16.0 / 9.0)
    } else if s.contains("4:3") || s.contains("standard") {
        Some(4.0 / 3.0)
    } else {
        None
    }
}

/// The spot light dvars, each within the range the engine registers it with.
fn spot_params(c: &Cvars) -> render::SpotParams {
    let d = render::SpotParams::default();
    let value = |n: &str, min: f32, max: f32, default: f32| {
        c.get(n)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .map_or(default, |v| v.clamp(min, max))
    };
    render::SpotParams {
        start_radius: value("r_spotLightStartRadius", 0.0, 1200.0, d.start_radius),
        end_radius: value("r_spotLightEndRadius", 1.0, 1200.0, d.end_radius),
        fov_inner_fraction: value(
            "r_spotLightFovInnerFraction",
            0.0,
            0.99,
            d.fov_inner_fraction,
        ),
        brightness: value("r_spotLightBrightness", 0.0, 16.0, d.brightness),
    }
}

/// The blur `r_dof_tweak` asks for (`R_SetDepthOfField`), each dvar within the range the engine registers it with.
fn dof_tweak(c: &Cvars) -> render::Dof {
    let value = |n: &str, min: f32, max: f32, default: f32| {
        c.get(n)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map_or(default, |v| v.clamp(min, max))
    };
    render::Dof {
        view_model_start: value("r_dof_viewModelStart", 0.0, 128.0, 2.0),
        view_model_end: value("r_dof_viewModelEnd", 0.0, 128.0, 8.0),
        near_start: value("r_dof_nearStart", 0.0, 1000.0, 10.0),
        near_end: value("r_dof_nearEnd", 0.0, 1000.0, 60.0),
        far_start: value("r_dof_farStart", 0.0, 20000.0, 1000.0),
        far_end: value("r_dof_farEnd", 0.0, 20000.0, 7000.0),
        near_blur: value("r_dof_nearBlur", 4.0, 10.0, 6.0),
        far_blur: value("r_dof_farBlur", 0.0, 10.0, 1.8),
    }
}

/// The clear dvars (`R_GetClearColor`): `r_clear` by stock name or index (unset or unknown is the stock default,
/// `dev-only blink`), the colour dvars as `r g b [a]` in 0..1 (a bad or missing channel is 0, as `Dvar_RegisterColor`'s
/// default is black).
fn clear(c: &Cvars) -> render::Clear {
    let color = |n: &str| {
        let mut rgb = [0.0_f32; 3];
        let mut parts = c.get(n).unwrap_or("").split_whitespace();
        for ch in &mut rgb {
            *ch = parts
                .next()
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map_or(0.0, |v| v.clamp(0.0, 1.0));
        }
        rgb
    };
    render::Clear {
        mode: c
            .get("r_clear")
            .and_then(render::ClearMode::parse)
            .unwrap_or(render::ClearMode::DevOnlyBlink),
        developer: c.bool("developer"),
        colors: [color("r_clearColor"), color("r_clearColor2")],
    }
}

/// KisakCOD gfx_d3d/r_scene.cpp:1833-1861 (`R_SetFilmInfo`): overlay the live adjustments on the vision film.
/// Preserve the double application of desaturation; applying the multiplier directly is not stock behavior.
pub fn film(c: &Cvars, mut film: render::art::Film) -> render::art::Film {
    if c.bool("sv_cheats") {
        let f = |n: &str, d: f32, lo: f32, hi: f32| {
            c.get(n)
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .unwrap_or(d)
                .clamp(lo, hi)
        };
        let scale =
            (1.0 - film.desaturation) * f("r_desaturation", 1.0, 0.0, 4.0) + film.desaturation;
        film.desaturation *= scale;
        film.contrast *= f("r_contrast", 1.0, 0.0, 4.0);
        film.brightness += f("r_brightness", 0.0, -1.0, 1.0);
    }
    film
}

impl Gfx {
    pub fn from_cvars(c: &Cvars) -> Gfx {
        let text = |n: &str| c.get(n).unwrap_or("");
        // Unset means the engine's default: everything on.
        let on = |n: &str| c.get(n).is_none_or(|v| v.trim() != "0");
        let window_coord = |n: &str| {
            text(n)
                .trim()
                .parse::<i32>()
                .ok()
                .map(|v| v.clamp(-4096, 4096))
        };
        let picmip = |n: &str| (c.f32(n).max(0.0) as usize).min(3);
        Gfx {
            fullscreen: c.bool("r_fullscreen"),
            mode: parse_mode(text("r_mode")),
            refresh: parse_hz(text("r_displayRefresh")),
            monitor: text("r_monitor").trim().parse().ok().filter(|&n| n > 0),
            position: window_coord("vid_xpos").zip(window_coord("vid_ypos")),
            aspect: parse_aspect(text("r_aspectRatio")),
            vsync: c.bool("r_vsync"),
            aa_samples: text("r_aaSamples").trim().parse().unwrap_or(1).clamp(1, 16),
            picmip: if c.bool("r_picmip_manual") {
                picmip("r_picmip")
            } else {
                0
            },
            picmip_bump: if c.bool("r_picmip_manual") {
                picmip("r_picmip_bump")
            } else {
                0
            },
            picmip_spec: if c.bool("r_picmip_manual") {
                picmip("r_picmip_spec")
            } else {
                0
            },
            aniso_max: (c.f32("r_texFilterAnisoMax").max(1.0) as u16).min(16),
            aniso_min: (c.f32("r_texFilterAnisoMin").max(1.0) as u16).min(16),
            mip_mode: c
                .get("r_texFilterMipMode")
                .and_then(render::MipMode::parse)
                .unwrap_or_default(),
            fullbright: c.bool("sv_cheats") && c.bool("r_fullbright"),
            shadows: on("sm_enable"),
            specular: on("r_specular"),
            dof: on("r_dof_enable"),
            // `r_dof_bias` and `r_dof_tweak` are CHEAT dvars: they only act with `sv_cheats` on.
            dof_bias: c
                .get("r_dof_bias")
                .filter(|_| c.bool("sv_cheats"))
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map_or(render::DOF_BIAS, |v| v.clamp(0.1, 3.0)),
            dof_tweak: (c.bool("sv_cheats") && c.bool("r_dof_tweak")).then(|| dof_tweak(c)),
            glow: on("r_glow_allowed"),
            dlight_limit: c
                .get("r_dlightLimit")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map_or(render::dlight::MAX_VISIBLE, |n| {
                    n.min(render::dlight::MAX_VISIBLE)
                }),
            spot: spot_params(c),
            cookies: c.bool("sc_enable"),
            cookie_count: c
                .get("sc_count")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map_or(24, |n| n.min(24)),
            spot_shadows: on("sm_spotEnable"),
            dynamic_spot_shadows: on("r_spotLightShadows"),
            max_shadow_lights: c
                .get("sm_maxLights")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map_or(render::spotshadow::TILES as usize, |n| {
                    n.min(render::spotshadow::TILES as usize)
                }),
            spot_fade_time: c
                .get("sm_spotShadowFadeTime")
                .and_then(|v| v.trim().parse::<f32>().ok())
                .map_or(1.0, |v| v.clamp(0.0, 5.0)),
            lod_scale: ["r_lodScaleRigid", "r_lodScaleSkinned"].map(|n| {
                c.get(n)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                    .map_or(1.0, |v| v.max(0.0))
            }),
            lod_bias: ["r_lodBiasRigid", "r_lodBiasSkinned"].map(|n| {
                c.get(n)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0)
            }),
            draw_sun: on("r_drawSun"),
            distortion: on("r_distortion"),
            z_feather: on("r_zFeather"),
            draw_water: on("r_drawWater"),
            clear: clear(c),
        }
    }

    /// The window the settings ask for, where `base` (the command line's) leaves the choice open.
    pub fn request(&self, base: &Request) -> Request {
        let kind = match (self.fullscreen, base.kind) {
            (false, _) => FullscreenKind::Windowed,
            (true, FullscreenKind::Windowed) => FullscreenKind::Exclusive,
            (true, k) => k,
        };
        Request {
            kind,
            size: self.mode.or(base.size),
            refresh: self.refresh.or(base.refresh),
            monitor: self.monitor.or(base.monitor),
            position: self.position.or(base.position),
        }
    }

    /// The renderer's settings: `base` (the command line's) with these on top.
    pub fn settings(&self, mut base: render::Settings) -> render::Settings {
        if !self.shadows {
            base.shadows = render::ShadowMode::Off;
        }
        base.specular = self.specular;
        base.dof = self.dof;
        base.dof_bias = self.dof_bias;
        base.dof_tweak = self.dof_tweak;
        base.glow = self.glow;
        base.draw_sun = self.draw_sun;
        base.distortion = self.distortion;
        base.aa_samples = self.aa_samples;
        base.z_feather = self.z_feather;
        base.draw_water = self.draw_water;
        base.aspect = self.aspect;
        base.dlight_limit = self.dlight_limit;
        base.spot = self.spot;
        base.cookies = self.cookies;
        base.cookie_count = self.cookie_count;
        base.spot_shadows = self.spot_shadows;
        base.dynamic_spot_shadows = self.dynamic_spot_shadows;
        base.max_shadow_lights = self.max_shadow_lights;
        base.spot_fade_time = self.spot_fade_time;
        base.lod_scale = self.lod_scale;
        base.lod_bias = self.lod_bias;
        base.fullbright = self.fullbright;
        base.clear = self.clear;
        base
    }

    /// The present mode to ask for: `--present` when it named one, else the queue for vsync and the first mode
    /// that does not wait for the display without.
    pub fn present<'a>(&self, cli: &'a str) -> &'a str {
        match (cli, self.vsync) {
            ("auto", true) => "fifo",
            ("auto", false) => "uncapped",
            _ => cli,
        }
    }
}

/// The shortest time a frame may take (`com_maxfps`: 85 unless set, within the original's 0 to 1000), `None` for no
/// cap. Whole milliseconds, as the original counts them (`1000 / com_maxfps`, at least one), so 85 paces at 11 ms.
/// Deliberately unlike the original, no cap applies while the display paces the frames (`display_paced`: the present
/// mode waits for the vertical blank). A cap below the refresh rate (the stock 85 on a 120 or 144 Hz screen) would make
/// frames land on every second vblank now and then, an uneven cadence; vsync alone paces them.
pub fn frame_gap(c: &Cvars, display_paced: bool) -> Option<Duration> {
    if display_paced {
        return None;
    }
    let fps = c
        .get("com_maxfps")
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map_or(85, |v| v.clamp(0.0, 1000.0) as u32);
    (fps > 0).then(|| Duration::from_millis(u64::from((1000 / fps).max(1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cvars(pairs: &[(&str, &str)]) -> Cvars {
        let mut c = Cvars::default();
        for (k, v) in pairs {
            c.set(k, v, false);
        }
        c
    }

    #[test]
    fn window_coordinates_follow_the_stock_domain() {
        let g = Gfx::from_cvars(&cvars(&[("vid_xpos", "10000"), ("vid_ypos", "-10000")]));
        assert_eq!(g.position, Some((4096, -4096)));
    }

    #[test]
    fn menu_values_become_settings() {
        let g = Gfx::from_cvars(&cvars(&[
            ("r_mode", "1920x1080"),
            ("r_displayRefresh", "144 Hz"),
            ("r_aspectRatio", "wide 16:10"),
            ("r_aaSamples", "4"),
            ("r_picmip", "2"),
            ("r_picmip_manual", "1"),
            ("r_texFilterAnisoMax", "8"),
            ("sm_enable", "0"),
            ("r_specular", "0"),
            ("r_dof_enable", "0"),
            ("r_glow_allowed", "0"),
            ("r_drawSun", "0"),
            ("r_distortion", "0"),
            ("r_fullscreen", "1"),
            ("r_vsync", "1"),
        ]));
        assert_eq!(g.mode, Some((1920, 1080)));
        assert_eq!(g.refresh, Some(144.0));
        assert_eq!(g.aspect, Some(1.6));
        assert_eq!((g.aa_samples, g.picmip, g.aniso_max), (4, 2, 8));
        assert!(g.fullscreen && g.vsync);
        let s = g.settings(render::Settings::default());
        assert_eq!(s.shadows, render::ShadowMode::Off);
        assert!(!s.specular && !s.dof && !s.glow && !s.draw_sun && !s.distortion);
        assert_eq!(s.aa_samples, 4);
    }

    #[test]
    fn the_clear_dvars_follow_r_getclearcolor() {
        // Unset: the stock default, `dev-only blink` with `developer` off, clears nothing (black here).
        let s = Gfx::from_cvars(&cvars(&[])).settings(render::Settings::default());
        assert_eq!(s.clear.mode, render::ClearMode::DevOnlyBlink);
        assert!(!s.clear.developer);
        assert_eq!(s.clear.color(Some([0.6, 0.7, 0.7]), 0), [0.0; 3]);
        // By index or by name, with the two colours.
        let g = Gfx::from_cvars(&cvars(&[
            ("r_clear", "2"),
            ("r_clearColor", "1 0.5 0 1"),
            ("r_clearColor2", "0 bad 2"),
        ]));
        assert_eq!(g.clear.mode, render::ClearMode::Blink);
        assert_eq!(g.clear.colors, [[1.0, 0.5, 0.0], [0.0, 0.0, 1.0]]);
        let g = Gfx::from_cvars(&cvars(&[("r_clear", "fog color"), ("developer", "1")]));
        assert_eq!(g.clear.mode, render::ClearMode::FogColor);
        assert!(g.clear.developer);
        assert_eq!(
            Gfx::from_cvars(&cvars(&[("r_clear", "nonsense")]))
                .clear
                .mode,
            render::ClearMode::DevOnlyBlink
        );
    }

    #[test]
    fn the_dof_dvars_set_the_bias_and_the_tweaked_blur() {
        let s = Gfx::from_cvars(&cvars(&[])).settings(render::Settings::default());
        assert_eq!((s.dof_bias, s.dof_tweak), (0.5, None));
        // Cheat dvars do nothing while the server has `sv_cheats` off.
        let s = Gfx::from_cvars(&cvars(&[
            ("sv_cheats", "0"),
            ("r_dof_bias", "2"),
            ("r_dof_tweak", "1"),
        ]))
        .settings(render::Settings::default());
        assert_eq!((s.dof_bias, s.dof_tweak), (0.5, None));
        let g = Gfx::from_cvars(&cvars(&[
            ("sv_cheats", "1"),
            ("r_dof_bias", "9"),
            ("r_dof_tweak", "1"),
            ("r_dof_nearEnd", "90"),
            ("r_dof_nearBlur", "1"),
        ]));
        let s = g.settings(render::Settings::default());
        assert_eq!(s.dof_bias, 3.0);
        let d = s.dof_tweak.unwrap();
        assert_eq!((d.near_end, d.near_blur, d.far_end), (90.0, 4.0, 7000.0));
    }

    #[test]
    fn the_shadow_cookies_are_off_unless_asked_for() {
        assert!(!Gfx::from_cvars(&cvars(&[])).cookies);
        assert!(Gfx::from_cvars(&cvars(&[("sc_enable", "1")])).cookies);
    }

    #[test]
    fn unset_dvars_keep_everything_on_and_picmip_is_automatic_unless_manual() {
        let g = Gfx::from_cvars(&cvars(&[("r_picmip", "3")]));
        assert_eq!(g.picmip, 0);
        let s = g.settings(render::Settings::default());
        assert!(
            s.specular
                && s.dof
                && s.glow
                && s.draw_sun
                && s.distortion
                && s.shadows != render::ShadowMode::Off
        );
        assert_eq!(s.aa_samples, 1);
    }

    #[test]
    fn the_window_request_follows_the_menu_over_the_command_line() {
        let base = Request::default();
        let g = Gfx::from_cvars(&cvars(&[("r_mode", "800x600"), ("r_fullscreen", "1")]));
        let r = g.request(&base);
        assert_eq!(
            (r.kind, r.size),
            (FullscreenKind::Exclusive, Some((800, 600)))
        );
        let g = Gfx::from_cvars(&cvars(&[("r_monitor", "2")]));
        assert_eq!(g.request(&base).monitor, Some(2));
        let g = Gfx::from_cvars(&cvars(&[("r_monitor", "0")]));
        assert_eq!(g.request(&base).monitor, None);
        let g = Gfx::from_cvars(&cvars(&[("r_fullscreen", "0")]));
        let borderless = Request {
            kind: FullscreenKind::Borderless,
            ..Request::default()
        };
        assert_eq!(g.request(&borderless).kind, FullscreenKind::Windowed);
        let g = Gfx::from_cvars(&cvars(&[("r_fullscreen", "1")]));
        assert_eq!(g.request(&borderless).kind, FullscreenKind::Borderless);
    }

    #[test]
    fn vsync_picks_the_mode_unless_the_command_line_named_one() {
        let on = Gfx::from_cvars(&cvars(&[("r_vsync", "1")]));
        assert_eq!(on.present("auto"), "fifo");
        assert_eq!(on.present("mailbox"), "mailbox");
        let off = Gfx::from_cvars(&cvars(&[("r_vsync", "0")]));
        assert_eq!(off.present("auto"), "uncapped");
        assert_eq!(off.present("fifo"), "fifo");
    }

    #[test]
    fn the_lod_dvars_scale_and_bias_the_distance_models_are_judged_at() {
        let g = Gfx::from_cvars(&cvars(&[
            ("r_lodScaleRigid", "2.5"),
            ("r_lodBiasSkinned", "-40"),
            ("r_lodScaleSkinned", "-3"),
        ]));
        let s = g.settings(render::Settings::default());
        assert_eq!(s.lod_scale, [2.5, 0.0], "a scale is not negative");
        assert_eq!(s.lod_bias, [0.0, -40.0]);
        let s = Gfx::from_cvars(&cvars(&[])).settings(render::Settings::default());
        assert_eq!((s.lod_scale, s.lod_bias), ([1.0; 2], [0.0; 2]));
    }

    #[test]
    fn texture_quality_soft_smoke_and_water_follow_their_dvars() {
        let g = Gfx::from_cvars(&cvars(&[
            ("r_picmip_manual", "1"),
            ("r_picmip", "1"),
            ("r_picmip_bump", "2"),
            ("r_picmip_spec", "9"),
            ("r_zFeather", "0"),
            ("r_drawWater", "0"),
            ("r_aaSamples", "16"),
        ]));
        assert_eq!((g.picmip, g.picmip_bump, g.picmip_spec), (1, 2, 3));
        assert_eq!(g.aa_samples, 16);
        let s = g.settings(render::Settings::default());
        assert!(!s.z_feather && !s.draw_water);
        let auto = Gfx::from_cvars(&cvars(&[("r_picmip_bump", "2")]));
        assert_eq!(auto.picmip_bump, 0, "automatic detail unless set manually");
        assert_eq!(
            Gfx::from_cvars(&cvars(&[("r_aaSamples", "64")])).aa_samples,
            16
        );
    }

    #[test]
    fn the_frame_cap_is_whole_milliseconds_and_zero_means_none() {
        let ms = |v: &str| frame_gap(&cvars(&[("com_maxfps", v)]), false).map(|d| d.as_millis());
        assert_eq!(ms("85"), Some(11), "the stock default");
        assert_eq!(ms("250"), Some(4));
        assert_eq!(ms("1000"), Some(1));
        assert_eq!(ms("5000"), Some(1), "no more than 1000");
        assert_eq!(ms("0"), None);
        assert_eq!(
            frame_gap(&cvars(&[]), false).map(|d| d.as_millis()),
            Some(11)
        );
    }

    #[test]
    fn no_cap_applies_while_the_display_paces_the_frames() {
        let c = cvars(&[("com_maxfps", "85")]);
        assert_eq!(frame_gap(&c, true), None, "vsync alone paces");
        assert!(frame_gap(&c, false).is_some());
    }
}
