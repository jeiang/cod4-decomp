// SPDX-License-Identifier: GPL-3.0-or-later
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
        base.aa_samples = self.aa_samples;
        base.aspect = self.aspect;
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
        assert!(!s.specular && !s.dof && !s.glow);
        assert_eq!(s.aa_samples, 4);
    }

    #[test]
    fn unset_dvars_keep_everything_on_and_picmip_is_automatic_unless_manual() {
        let g = Gfx::from_cvars(&cvars(&[("r_picmip", "3")]));
        assert_eq!(g.picmip, 0);
        let s = g.settings(render::Settings::default());
        assert!(s.specular && s.dof && s.glow && s.shadows != render::ShadowMode::Off);
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
}
