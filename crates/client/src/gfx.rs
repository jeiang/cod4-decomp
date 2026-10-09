// SPDX-License-Identifier: GPL-3.0-only
//! The graphics settings of the options menus (`r_*`, `sm_enable`), read as the renderer and the window take them.
//!
//! The menus only write dvars; [`Gfx::from_cvars`] turns them into the video request, the present mode, the aspect
//! ratio and the renderer's [`render::Settings`]. A match's renderer takes them when its map loads; `vid_restart`
//! applies the ones that can change under a running window and renderer.

use crate::display::{FullscreenKind, Request};
use crate::input::Cvars;

#[derive(Clone, Debug, PartialEq)]
pub struct Gfx {
    /// `r_fullscreen`.
    pub fullscreen: bool,
    /// `r_mode`: `WxH`.
    pub mode: Option<(u32, u32)>,
    /// `r_displayRefresh`: `N Hz`.
    pub refresh: Option<f64>,
    /// `r_aspectRatio`: the shape of the screen, `None` for the window's own.
    pub aspect: Option<f32>,
    /// `r_vsync`.
    pub vsync: bool,
    /// `r_aaSamples`.
    pub aa_samples: u32,
    /// `r_picmip` unless `r_picmip_manual` is off, when the engine picks full size.
    pub picmip: usize,
    /// `r_texFilterAnisoMax`.
    pub aniso_max: u16,
    /// `sm_enable`.
    pub shadows: bool,
    /// `r_specular`.
    pub specular: bool,
    /// `r_dof_enable`.
    pub dof: bool,
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

impl Gfx {
    pub fn from_cvars(c: &Cvars) -> Gfx {
        let text = |n: &str| c.get(n).unwrap_or("");
        // Unset means the engine's default: everything on.
        let on = |n: &str| c.get(n).is_none_or(|v| v.trim() != "0");
        Gfx {
            fullscreen: c.bool("r_fullscreen"),
            mode: parse_mode(text("r_mode")),
            refresh: parse_hz(text("r_displayRefresh")),
            aspect: parse_aspect(text("r_aspectRatio")),
            vsync: c.bool("r_vsync"),
            aa_samples: text("r_aaSamples").trim().parse().unwrap_or(1).max(1),
            picmip: if c.bool("r_picmip_manual") {
                (c.f32("r_picmip").max(0.0) as usize).min(3)
            } else {
                0
            },
            aniso_max: (c.f32("r_texFilterAnisoMax").max(1.0) as u16).min(16),
            shadows: on("sm_enable"),
            specular: on("r_specular"),
            dof: on("r_dof_enable"),
            glow: on("r_glow_allowed"),
            dlight_limit: c
                .get("r_dlightLimit")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map_or(render::dlight::MAX_VISIBLE, |n| {
                    n.min(render::dlight::MAX_VISIBLE)
                }),
            spot: spot_params(c),
            cookies: on("sc_enable"),
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
        }
    }

    /// The renderer's settings: `base` (the command line's) with these on top.
    pub fn settings(&self, mut base: render::Settings) -> render::Settings {
        if !self.shadows {
            base.shadows = render::ShadowMode::Off;
        }
        base.specular = self.specular;
        base.dof = self.dof;
        base.glow = self.glow;
        base.draw_sun = self.draw_sun;
        base.distortion = self.distortion;
        base.aa_samples = self.aa_samples;
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
}
