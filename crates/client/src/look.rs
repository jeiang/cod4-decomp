// SPDX-License-Identifier: GPL-3.0-or-later
//! How the picture looks beyond the map: the vision set the scripts pick (`visionsetnaked` blends the glow and film
//! settings to those of `vision/<name>.vision`) and the shell shock a player takes (`shock/<name>.shock`: a blurred or
//! flashed copy of the saved screen laid over the view, and a shaking of the camera).

use net::ui::ServerCmd;
use render::ShellShock;
use render::art::{Film, Glow, MapArt};

/// The shell shock screen blend of a `.shock` file (`bg_shock_screenType`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Screen {
    Blurred,
    Flashed,
    None,
}

/// What a `.shock` file asks for, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShockParams {
    screen: Screen,
    blur_time: f32,
    blur_fade: f32,
    flash_white_fade: f32,
    flash_shot_fade: f32,
    /// Seconds per camera kick and how far a kick turns the view, in degrees.
    kick_period: f32,
    kick_radius: f32,
    kick_fade: f32,
}

impl ShockParams {
    pub fn parse(text: &str) -> ShockParams {
        let mut p = ShockParams {
            screen: Screen::Blurred,
            blur_time: 4000.0,
            blur_fade: 1000.0,
            flash_white_fade: 1000.0,
            flash_shot_fade: 500.0,
            kick_period: 0.0,
            kick_radius: 0.0,
            kick_fade: 3000.0,
        };
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(name), Some(first)) = (it.next(), it.next()) else {
                continue;
            };
            let value = line[line.find(first).unwrap_or(0)..]
                .trim()
                .trim_matches('"');
            let Ok(v) = value.parse::<f32>() else {
                continue;
            };
            match name.to_ascii_lowercase().as_str() {
                "bg_shock_screentype" => {
                    p.screen = match v as i32 {
                        0 => Screen::Blurred,
                        1 => Screen::Flashed,
                        _ => Screen::None,
                    }
                }
                "bg_shock_screenblurblendtime" => p.blur_time = v * 1000.0,
                "bg_shock_screenblurblendfadetime" => p.blur_fade = (v * 1000.0).max(1.0),
                "bg_shock_screenflashwhitefadetime" => p.flash_white_fade = (v * 1000.0).max(1.0),
                "bg_shock_screenflashshotfadetime" => p.flash_shot_fade = (v * 1000.0).max(1.0),
                "bg_shock_viewkickperiod" => p.kick_period = v,
                "bg_shock_viewkickradius" => p.kick_radius = v,
                "bg_shock_viewkickfadetime" => p.kick_fade = (v * 1000.0).max(1.0),
                _ => {}
            }
        }
        p
    }
}

fn smooth(x: f32) -> f32 {
    (((x.clamp(0.0, 1.0) - 0.5) * std::f32::consts::PI).sin() + 1.0) * 0.5
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

fn blend_glow(a: &Glow, b: &Glow, t: f32) -> Glow {
    Glow {
        enabled: if t < 1.0 {
            a.enabled || b.enabled
        } else {
            b.enabled
        },
        radius: lerp(a.radius, b.radius, t),
        bloom_cutoff: lerp(a.bloom_cutoff, b.bloom_cutoff, t),
        bloom_desaturation: lerp(a.bloom_desaturation, b.bloom_desaturation, t),
        bloom_intensity: lerp(a.bloom_intensity, b.bloom_intensity, t),
        sky_bleed_intensity: lerp(a.sky_bleed_intensity, b.sky_bleed_intensity, t),
    }
}

fn blend_film(a: &Film, b: &Film, t: f32) -> Film {
    Film {
        enabled: if t < 1.0 {
            a.enabled || b.enabled
        } else {
            b.enabled
        },
        contrast: lerp(a.contrast, b.contrast, t),
        brightness: lerp(a.brightness, b.brightness, t),
        desaturation: lerp(a.desaturation, b.desaturation, t),
        invert: if t < 0.5 { a.invert } else { b.invert },
        tint_light: lerp3(a.tint_light, b.tint_light, t),
        tint_dark: lerp3(a.tint_dark, b.tint_dark, t),
    }
}

struct Shock {
    params: ShockParams,
    start_ms: i32,
    duration_ms: i32,
    /// A copy of the screen has been kept for the overlay.
    saved: bool,
}

/// What to put on the picture this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct LookOut {
    /// The glow and film once a script has picked a vision set; until then the map's own stay.
    pub vision: Option<(Glow, Film)>,
    pub shell_shock: Option<ShellShock>,
    pub save_screen: bool,
    /// Pitch and yaw to add to the view, degrees.
    pub kick: [f32; 2],
}

pub struct Look {
    /// A script has picked a vision set.
    active: bool,
    from: (Glow, Film),
    to: (Glow, Film),
    start_ms: i32,
    blend_ms: i32,
    /// The names the scripts set, for the report.
    pub naked: Option<String>,
    pub night: Option<String>,
    pub missing: Vec<String>,
    shock: Option<Shock>,
    last_ms: i32,
    /// How many shell shocks started.
    pub shocks: u64,
}

impl Look {
    /// `base` is the map's own look, in force until a script picks another.
    pub fn new(base: (Glow, Film)) -> Self {
        Self {
            active: false,
            from: base,
            to: base,
            start_ms: 0,
            blend_ms: 0,
            naked: None,
            night: None,
            missing: Vec::new(),
            shock: None,
            last_ms: 0,
            shocks: 0,
        }
    }

    /// Takes a server command if it is one of the look's; `now_ms` is the server time and `file` finds a rawfile's
    /// text by name. `true` if it was the look's.
    pub fn command(
        &mut self,
        c: &ServerCmd,
        now_ms: i32,
        file: &dyn Fn(&str) -> Option<String>,
    ) -> bool {
        match c {
            ServerCmd::Vision {
                night: false,
                name,
                ms,
            } => {
                match file(&format!("vision/{name}.vision")) {
                    Some(text) => {
                        let art = MapArt::parse(None, Some(&text));
                        self.active = true;
                        self.from = self.current(now_ms);
                        self.to = (art.glow, art.film);
                        self.start_ms = now_ms;
                        self.blend_ms = *ms;
                    }
                    None => self.missing.push(format!("vision/{name}.vision")),
                }
                self.naked = Some(name.clone());
                true
            }
            ServerCmd::Vision {
                night: true, name, ..
            } => {
                // Night vision goggles are not drawn yet; the name is kept for the report.
                self.night = Some(name.clone());
                true
            }
            ServerCmd::ShellShock { name, ms } => {
                if name.is_empty() || *ms <= 0 {
                    self.shock = None;
                } else {
                    let text = file(&format!("shock/{name}.shock"))
                        .or_else(|| file("shock/default.shock"));
                    match text {
                        Some(t) => {
                            self.shock = Some(Shock {
                                params: ShockParams::parse(&t),
                                start_ms: now_ms,
                                duration_ms: *ms,
                                saved: false,
                            });
                            self.shocks += 1;
                        }
                        None => self.missing.push(format!("shock/{name}.shock")),
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn current(&self, now_ms: i32) -> (Glow, Film) {
        let t = if self.blend_ms <= 0 {
            1.0
        } else {
            ((now_ms - self.start_ms) as f32 / self.blend_ms as f32).clamp(0.0, 1.0)
        };
        (
            blend_glow(&self.from.0, &self.to.0, t),
            blend_film(&self.from.1, &self.to.1, t),
        )
    }

    /// The look at server time `now_ms`.
    pub fn frame(&mut self, now_ms: i32) -> LookOut {
        let (glow, film) = self.current(now_ms);
        let dt = (now_ms - self.last_ms).clamp(1, 200) as f32;
        self.last_ms = now_ms;
        let mut out = LookOut {
            vision: self.active.then_some((glow, film)),
            shell_shock: None,
            save_screen: false,
            kick: [0.0; 2],
        };
        let Some(s) = &mut self.shock else {
            return out;
        };
        let left = s.start_ms + s.duration_ms - now_ms;
        if left <= 0 {
            self.shock = None;
            return out;
        }
        let p = s.params;
        match p.screen {
            Screen::Blurred => {
                // Each frame the screen so far is laid over the next, which smears what moves; in the last
                // moments the effect time shrinks with what is left.
                let fade = if left as f32 >= p.blur_fade {
                    p.blur_time
                } else {
                    left as f32 / p.blur_fade * p.blur_time
                }
                .max(1.0);
                if s.saved {
                    out.shell_shock = Some(ShellShock {
                        blur_alpha: ShellShock::blurred_alpha(dt, fade),
                        ..ShellShock::default()
                    });
                }
                out.save_screen = true;
            }
            Screen::Flashed => {
                let white = smooth(left as f32 / p.flash_white_fade);
                let shot = smooth(left as f32 / p.flash_shot_fade);
                if s.saved {
                    out.shell_shock = Some(ShellShock {
                        blur_alpha: 0.0,
                        flash_screengrab: shot,
                        flash_whiteout: white,
                    });
                } else {
                    out.save_screen = true;
                }
            }
            Screen::None => {}
        }
        s.saved = true;
        if p.kick_radius > 0.0 && p.kick_period > 0.0 {
            let t = (now_ms - s.start_ms) as f32 / 1000.0 / p.kick_period;
            let fade = (left as f32 / p.kick_fade).min(1.0);
            // Two incommensurate waves read as shaking without a table of random numbers.
            out.kick = [
                (t * std::f32::consts::TAU).sin() * p.kick_radius * fade,
                (t * std::f32::consts::TAU * 1.618).cos() * p.kick_radius * fade,
            ];
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A look and the rawfiles it can read.
    struct Rig {
        look: Look,
        files: &'static [(&'static str, &'static str)],
    }

    impl Rig {
        fn command(&mut self, c: &ServerCmd, now: i32) -> bool {
            let files = self.files;
            self.look.command(c, now, &|n| {
                files
                    .iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, v)| (*v).to_owned())
            })
        }
    }

    impl std::ops::Deref for Rig {
        type Target = Look;
        fn deref(&self) -> &Look {
            &self.look
        }
    }

    impl std::ops::DerefMut for Rig {
        fn deref_mut(&mut self) -> &mut Look {
            &mut self.look
        }
    }

    fn look(files: &'static [(&'static str, &'static str)]) -> Rig {
        Rig {
            look: Look::new((Glow::default(), Film::default())),
            files,
        }
    }

    const NIGHT: &str = "r_filmEnable \"1\"\nr_filmContrast \"2\"\nr_glow \"1\"\nr_glowRadius0 \"10\"\nr_glowBloomIntensity0 \"1\"\n";

    fn set(name: &str, ms: i32) -> ServerCmd {
        ServerCmd::Vision {
            night: false,
            name: name.into(),
            ms,
        }
    }

    #[test]
    fn a_vision_set_blends_the_film_over_its_transition_and_then_holds() {
        let mut l = look(&[("vision/dark.vision", NIGHT)]);
        assert!(l.command(&set("dark", 1000), 5000));
        assert_eq!(l.frame(5000).vision.unwrap().1.contrast, 1.0);
        let mid = l.frame(5500).vision.unwrap();
        assert!((mid.1.contrast - 1.5).abs() < 1e-4, "{}", mid.1.contrast);
        let end = l.frame(7000).vision.unwrap();
        assert_eq!(
            (end.1.contrast, end.1.enabled, end.0.radius),
            (2.0, true, 10.0)
        );
    }

    #[test]
    fn a_missing_vision_file_is_reported_and_changes_nothing() {
        let mut l = look(&[]);
        l.command(&set("nope", 0), 0);
        assert_eq!(l.missing, ["vision/nope.vision"]);
        assert!(l.frame(10).vision.is_none());
    }

    const BLUR: &str = "bg_shock_screenType \"0\"\nbg_shock_screenBlurBlendTime \"2\"\nbg_shock_screenBlurBlendFadeTime \"0.5\"\nbg_shock_viewKickRadius \"4\"\nbg_shock_viewKickPeriod \"0.2\"\n";

    #[test]
    fn a_blurred_shock_saves_the_screen_then_overlays_it_until_the_time_is_up() {
        let mut l = look(&[("shock/boom.shock", BLUR)]);
        l.command(
            &ServerCmd::ShellShock {
                name: "boom".into(),
                ms: 3000,
            },
            1000,
        );
        let first = l.frame(1016);
        assert!(first.save_screen && first.shell_shock.is_none());
        let second = l.frame(1032);
        let a = second.shell_shock.expect("overlay").blur_alpha;
        assert!(second.save_screen && a > 0.9 && a < 1.0, "{a}");
        assert!(second.kick != [0.0; 2]);
        let over = l.frame(4100);
        assert!(!over.save_screen && over.shell_shock.is_none() && over.kick == [0.0; 2]);
    }

    const FLASH: &str = "bg_shock_screenType \"1\"\nbg_shock_screenFlashWhiteFadeTime \"1\"\nbg_shock_screenFlashShotFadeTime \"2\"\n";

    #[test]
    fn a_flash_fades_its_white_before_its_picture_and_stopshellshock_ends_it() {
        let mut l = look(&[("shock/flash.shock", FLASH)]);
        l.command(
            &ServerCmd::ShellShock {
                name: "flash".into(),
                ms: 4000,
            },
            0,
        );
        assert!(l.frame(10).save_screen);
        let f = l.frame(3500).shell_shock.expect("flash");
        // 500 ms left: the white fade (1 s) is half done, the picture's (2 s) a quarter.
        assert!(f.flash_whiteout > f.flash_screengrab, "{f:?}");
        l.command(
            &ServerCmd::ShellShock {
                name: String::new(),
                ms: 0,
            },
            3600,
        );
        assert!(l.frame(3700).shell_shock.is_none());
    }
}
