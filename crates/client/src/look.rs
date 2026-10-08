// SPDX-License-Identifier: GPL-3.0-only
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
#[derive(Clone, Debug, PartialEq)]
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
    sound: Option<SoundParams>,
    look: Option<LookControl>,
    /// `bg_shock_volume_<channel>` whether or not the shock has sound of its own.
    volumes: Vec<(String, f32)>,
}

/// `bg_shock_sound*`: the room, the ducking and the tinnitus of a shock.
#[derive(Clone, Debug, PartialEq)]
struct SoundParams {
    loop_alias: String,
    loop_silent: String,
    end: String,
    end_abort: String,
    fade_in: i32,
    fade_out: i32,
    loop_fade: i32,
    loop_end_delay: i32,
    mod_end_delay: i32,
    room: String,
    wet: f32,
    /// `bg_shock_volume_<channel>`, by channel name.
    volumes: Vec<(String, f32)>,
}

/// `bg_shock_lookControl*`: how much a shock slows the turning of the view.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LookControl {
    max_pitch_speed: f32,
    max_yaw_speed: f32,
    sensitivity: f32,
    fade: i32,
}

/// What the sound system is told, in the order a shock produces it (`UpdateShellShockSound`).
#[derive(Clone, Debug, PartialEq)]
pub enum ShockSound {
    /// The room effect and the channel volumes of the shock, fading in over `fade_ms`.
    Enter {
        room: String,
        wet: f32,
        volumes: Vec<(String, f32)>,
        fade_ms: i32,
    },
    /// They fade out over `fade_ms`.
    Leave {
        fade_ms: i32,
    },
    /// The tinnitus: `fade` is 0 for the loud loop and 1 for the quiet one.
    Loop {
        loud: String,
        quiet: String,
        fade: f32,
    },
    LoopStop,
    /// A one-shot: the end sting or the abort.
    Play(String),
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
            sound: None,
            look: None,
            volumes: Vec::new(),
        };
        let mut s = SoundParams {
            loop_alias: String::new(),
            loop_silent: String::new(),
            end: String::new(),
            end_abort: String::new(),
            fade_in: 1,
            fade_out: 1,
            loop_fade: 1,
            loop_end_delay: 0,
            mod_end_delay: 0,
            room: "default".into(),
            wet: 0.0,
            volumes: Vec::new(),
        };
        let mut l = LookControl {
            max_pitch_speed: 0.0,
            max_yaw_speed: 0.0,
            sensitivity: 1.0,
            fade: 1,
        };
        let (mut sound_on, mut look_on) = (false, false);
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(name), Some(first)) = (it.next(), it.next()) else {
                continue;
            };
            let value = line[line.find(first).unwrap_or(0)..]
                .trim()
                .trim_matches('"');
            let v = value.parse::<f32>().ok();
            let secs = v.unwrap_or(0.0);
            let ms = (secs * 1000.0).round() as i32;
            let name = name.to_ascii_lowercase();
            if let Some(channel) = name.strip_prefix("bg_shock_volume_") {
                if let Some(v) = v {
                    s.volumes.push((channel.to_owned(), v.clamp(0.0, 1.0)));
                }
                continue;
            }
            match name.as_str() {
                "bg_shock_screentype" => {
                    p.screen = match value.to_ascii_lowercase().as_str() {
                        "blurred" | "0" => Screen::Blurred,
                        "flashed" | "1" => Screen::Flashed,
                        _ => Screen::None,
                    }
                }
                "bg_shock_screenblurblendtime" => p.blur_time = secs * 1000.0,
                "bg_shock_screenblurblendfadetime" => p.blur_fade = (secs * 1000.0).max(1.0),
                "bg_shock_screenflashwhitefadetime" => {
                    p.flash_white_fade = (secs * 1000.0).max(1.0)
                }
                "bg_shock_screenflashshotfadetime" => p.flash_shot_fade = (secs * 1000.0).max(1.0),
                "bg_shock_viewkickperiod" => p.kick_period = secs,
                "bg_shock_viewkickradius" => p.kick_radius = secs,
                "bg_shock_viewkickfadetime" => p.kick_fade = (secs * 1000.0).max(1.0),
                "bg_shock_sound" => sound_on = v.is_some_and(|v| v != 0.0),
                "bg_shock_soundloop" => s.loop_alias = value.to_owned(),
                "bg_shock_soundloopsilent" => s.loop_silent = value.to_owned(),
                "bg_shock_soundend" => s.end = value.to_owned(),
                "bg_shock_soundendabort" => s.end_abort = value.to_owned(),
                "bg_shock_soundfadeintime" => s.fade_in = ms.max(1),
                "bg_shock_soundfadeouttime" => s.fade_out = ms.max(1),
                "bg_shock_soundloopfadetime" => s.loop_fade = ms.max(1),
                "bg_shock_soundloopenddelay" => s.loop_end_delay = ms,
                "bg_shock_soundmodenddelay" => s.mod_end_delay = ms,
                "bg_shock_soundroomtype" => s.room = value.to_owned(),
                "bg_shock_soundwetlevel" => s.wet = secs.clamp(0.0, 1.0),
                "bg_shock_lookcontrol" => look_on = v.is_some_and(|v| v != 0.0),
                "bg_shock_lookcontrol_maxpitchspeed" => l.max_pitch_speed = secs,
                "bg_shock_lookcontrol_maxyawspeed" => l.max_yaw_speed = secs,
                "bg_shock_lookcontrol_mousesensitivityscale" => l.sensitivity = secs,
                "bg_shock_lookcontrol_fadetime" => l.fade = ms.max(1),
                _ => {}
            }
        }
        p.volumes = s.volumes.clone();
        p.sound = sound_on.then_some(s);
        p.look = look_on.then_some(l);
        p
    }

    /// The channel volumes of the file, for `setchannelvolumes`.
    pub fn volumes(&self) -> &[(String, f32)] {
        &self.volumes
    }

    /// Milliseconds past the shock's own time that its sound still has to play out.
    fn sound_tail(&self) -> i32 {
        self.sound.as_ref().map_or(0, |s| {
            (s.fade_out + s.mod_end_delay)
                .max(s.loop_fade + s.loop_end_delay)
                .max(s.loop_end_delay + 1)
                .max(0)
        })
    }
}

/// `CL_CapTurnRate` on one frame's turning: `delta` degrees limited to what `max_speed` degrees per second allows in
/// `dt` seconds; a speed of 0 is no limit.
pub fn cap_turn(delta: f32, max_speed: f32, dt: f32) -> f32 {
    if max_speed > 0.0 {
        let cap = max_speed * dt;
        delta.clamp(-cap, cap)
    } else {
        delta
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
    /// Where the shock's sound is: 0 not begun, 1 fading in, 2 steady, 3 fading out.
    phase: u8,
    /// The end sting has played (`shellshock.loopEndTime`).
    ended: bool,
    /// The tinnitus is on.
    looping: bool,
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
    /// What the shock asks of the sound system this frame.
    pub sound: Vec<ShockSound>,
    /// `CL_CapTurnRate`: the fastest the view may turn, degrees per second, pitch and yaw; 0 is no cap.
    pub max_turn: [f32; 2],
    /// The shock's scale on the mouse sensitivity (1 when none).
    pub sensitivity: f32,
}

/// How long the goggles take to change the picture, milliseconds.
const GOGGLES_BLEND_MS: i32 = 300;

pub struct Look {
    /// A script has picked a vision set.
    active: bool,
    from: (Glow, Film),
    to: (Glow, Film),
    /// What the picture returns to when the goggles come off: the map's own look or the last `visionsetnaked`.
    day: (Glow, Film),
    /// The look of `visionsetnight`, shown while the goggles are on.
    night_set: Option<(Glow, Film)>,
    goggles: bool,
    start_ms: i32,
    blend_ms: i32,
    /// The names the scripts set, for the report.
    pub naked: Option<String>,
    pub night: Option<String>,
    pub missing: Vec<String>,
    shock: Option<Shock>,
    /// Sound commands owed to the next frame (a shock that was replaced or cleared).
    pending: Vec<ShockSound>,
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
            day: base,
            night_set: None,
            goggles: false,
            start_ms: 0,
            blend_ms: 0,
            naked: None,
            night: None,
            missing: Vec::new(),
            shock: None,
            pending: Vec::new(),
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
                        self.day = (art.glow, art.film);
                        self.go(now_ms, *ms);
                    }
                    None => self.missing.push(format!("vision/{name}.vision")),
                }
                self.naked = Some(name.clone());
                true
            }
            ServerCmd::Vision {
                night: true, name, ..
            } => {
                match file(&format!("vision/{name}.vision")) {
                    Some(text) => {
                        let art = MapArt::parse(None, Some(&text));
                        self.night_set = Some((art.glow, art.film));
                    }
                    None => self.missing.push(format!("vision/{name}.vision")),
                }
                self.night = Some(name.clone());
                true
            }
            ServerCmd::ShellShock { name, ms } => {
                self.silence();
                if !name.is_empty() && *ms > 0 {
                    let text = file(&format!("shock/{name}.shock"))
                        .or_else(|| file("shock/default.shock"));
                    match text {
                        Some(t) => {
                            self.shock = Some(Shock {
                                params: ShockParams::parse(&t),
                                start_ms: now_ms,
                                duration_ms: *ms,
                                saved: false,
                                phase: 0,
                                ended: false,
                                looping: false,
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

    /// Starts the blend from what is on screen to the look that now applies.
    fn go(&mut self, now_ms: i32, ms: i32) {
        self.active = true;
        self.from = self.current(now_ms);
        self.to = match (self.goggles, self.night_set) {
            (true, Some(n)) => n,
            _ => self.day,
        };
        self.start_ms = now_ms;
        self.blend_ms = ms;
    }

    /// The night vision goggles went on or off (the player state's weapon flag); the look blends to the night set
    /// and back.
    pub fn goggles(&mut self, on: bool, now_ms: i32) {
        if on != self.goggles {
            self.goggles = on;
            if self.night_set.is_some() {
                self.go(now_ms, GOGGLES_BLEND_MS);
            }
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
            sound: std::mem::take(&mut self.pending),
            max_turn: [0.0; 2],
            sensitivity: 1.0,
        };
        let Some(s) = &mut self.shock else {
            return out;
        };
        let time = now_ms - s.start_ms;
        let left = s.duration_ms - time;
        let p = &s.params;
        if left < -p.sound_tail() {
            if s.looping {
                out.sound.push(ShockSound::LoopStop);
            }
            self.shock = None;
            return out;
        }
        if let Some(sp) = &p.sound {
            // `UpdateShellShockSound`.
            let to_go = sp.fade_out + sp.mod_end_delay + s.duration_ms - time;
            if time < sp.fade_in {
                if s.phase != 1 {
                    s.phase = 1;
                    out.sound.push(enter(sp, sp.fade_in - time));
                }
            } else if to_go <= sp.fade_out {
                if (0..sp.fade_out).contains(&to_go) && s.phase != 3 {
                    s.phase = 3;
                    out.sound.push(ShockSound::Leave { fade_ms: to_go });
                }
            } else if s.phase < 2 {
                s.phase = 2;
                out.sound.push(enter(sp, 0));
            }
            let loop_left = sp.loop_fade + sp.loop_end_delay + s.duration_ms - time;
            if loop_left > 0 {
                let fade = if loop_left <= sp.loop_fade {
                    1.0 - loop_left as f32 / sp.loop_fade as f32
                } else {
                    0.0
                };
                s.looping = true;
                out.sound.push(ShockSound::Loop {
                    loud: sp.loop_alias.clone(),
                    quiet: sp.loop_silent.clone(),
                    fade,
                });
            } else if std::mem::take(&mut s.looping) {
                out.sound.push(ShockSound::LoopStop);
            }
            if time >= sp.loop_end_delay + s.duration_ms {
                if !std::mem::replace(&mut s.ended, true) {
                    out.sound.push(ShockSound::Play(sp.end.clone()));
                }
            } else if std::mem::take(&mut s.ended) {
                out.sound.push(ShockSound::Play(sp.end_abort.clone()));
            }
        }
        if let Some(lc) = p.look
            && left > 0
        {
            // `UpdateShellShockLookControl`: the caps loosen as the shock ends.
            let fade = if left < lc.fade {
                left as f32 / lc.fade as f32
            } else {
                1.0
            };
            out.sensitivity = (lc.sensitivity - 1.0) * fade + 1.0;
            out.max_turn = [lc.max_pitch_speed / fade, lc.max_yaw_speed / fade];
        }
        if left <= 0 {
            return out;
        }
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

    /// `EndShellShockSound`: whatever the shock was doing to the sound stops, and a sting that has begun is
    /// aborted.
    fn silence(&mut self) {
        let Some(s) = self.shock.take() else { return };
        if s.phase != 0 {
            self.pending.push(ShockSound::Leave { fade_ms: 0 });
        }
        if s.looping {
            self.pending.push(ShockSound::LoopStop);
        }
        if let (true, Some(sp)) = (s.ended, &s.params.sound) {
            self.pending.push(ShockSound::Play(sp.end_abort.clone()));
        }
    }
}

fn enter(sp: &SoundParams, fade_ms: i32) -> ShockSound {
    ShockSound::Enter {
        room: sp.room.clone(),
        wet: sp.wet,
        volumes: sp.volumes.clone(),
        fade_ms,
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
    fn the_goggles_blend_to_the_night_set_and_back_to_the_day_look() {
        let mut l = look(&[("vision/dark.vision", NIGHT)]);
        let night = ServerCmd::Vision {
            night: true,
            name: "dark".into(),
            ms: 0,
        };
        l.command(&night, 0);
        assert!(
            l.frame(100).vision.is_none(),
            "the set alone changes nothing"
        );
        l.goggles(true, 1000);
        assert_eq!(l.frame(1000).vision.unwrap().1.contrast, 1.0);
        assert_eq!(l.frame(1300).vision.unwrap().1.contrast, 2.0);
        l.goggles(false, 2000);
        assert_eq!(l.frame(2300).vision.unwrap().1.contrast, 1.0);
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

    const STOCK_FLASH: &str = "bg_shock_screenType \"flashed\"\nbg_shock_sound \"1\"\nbg_shock_soundLoop \"loud\"\nbg_shock_soundLoopSilent \"quiet\"\nbg_shock_soundEnd \"end\"\nbg_shock_soundEndAbort \"abort\"\nbg_shock_soundFadeInTime \"0.25\"\nbg_shock_soundFadeOutTime \"2.0\"\nbg_shock_soundModEndDelay \"1.5\"\nbg_shock_soundLoopFadeTime \"2\"\nbg_shock_soundLoopEndDelay \"2.0\"\nbg_shock_soundRoomType \"underwater\"\nbg_shock_soundWetLevel \"0.8\"\nbg_shock_volume_weapon \"0.1\"\nbg_shock_lookControl \"1\"\nbg_shock_lookControl_maxpitchspeed \"90\"\nbg_shock_lookControl_maxyawspeed \"60\"\nbg_shock_lookControl_mousesensitivityscale \"0.5\"\nbg_shock_lookControl_fadeTime \"2\"\n";

    fn flash(ms: i32) -> Rig {
        let mut l = look(&[("shock/f.shock", STOCK_FLASH)]);
        l.command(
            &ServerCmd::ShellShock {
                name: "f".into(),
                ms,
            },
            0,
        );
        l
    }

    #[test]
    fn a_shock_ducks_the_sound_rings_and_ends_with_a_sting() {
        let mut l = flash(4000);
        let first = l.frame(10).sound;
        assert!(matches!(
            &first[..],
            [ShockSound::Enter { room, volumes, fade_ms: 240, .. }, ShockSound::Loop { fade, .. }]
                if room == "underwater" && volumes == &[("weapon".to_string(), 0.1)] && *fade == 0.0
        ));
        // Steady: the ducking is set once, the ring only goes on.
        let mid = l.frame(1000).sound;
        assert!(matches!(&mid[0], ShockSound::Enter { fade_ms: 0, .. }));
        assert!(matches!(
            &l.frame(1100).sound[..],
            [ShockSound::Loop { .. }]
        ));
        // The ducking lets go with the stock delays after the shock (4 s + 1.5 s, less its 2 s fade out).
        let leave = l.frame(5600).sound;
        assert!(leave.contains(&ShockSound::Leave { fade_ms: 1900 }));
        // The sting plays once, at the loop's end delay.
        let sting = l.frame(6100).sound;
        assert!(sting.contains(&ShockSound::Play("end".into())));
        assert!(
            l.frame(6200)
                .sound
                .iter()
                .all(|s| !matches!(s, ShockSound::Play(_)))
        );
        // The ring leans to the quiet loop near its end, then stops.
        let late = l.frame(7500).sound;
        assert!(
            late.iter()
                .any(|s| matches!(s, ShockSound::Loop { fade, .. } if *fade > 0.4))
        );
        assert!(l.frame(8100).sound.contains(&ShockSound::LoopStop));
        assert!(l.frame(8200).sound.is_empty());
        assert!(l.frame(20_000).sound.is_empty());
    }

    #[test]
    fn clearing_a_shock_early_releases_the_ducking_and_aborts_the_sting() {
        let mut l = flash(4000);
        l.frame(10);
        l.frame(1000);
        l.command(
            &ServerCmd::ShellShock {
                name: String::new(),
                ms: 0,
            },
            1500,
        );
        let s = l.frame(1510).sound;
        assert_eq!(s, [ShockSound::Leave { fade_ms: 0 }, ShockSound::LoopStop]);
    }

    #[test]
    fn look_control_caps_the_turn_rate_and_loosens_as_the_shock_ends() {
        let mut l = flash(4000);
        let held = l.frame(1000);
        assert_eq!(held.max_turn, [90.0, 60.0]);
        assert_eq!(held.sensitivity, 0.5);
        // With a second left of a two second fade the cap is twice as loose and the scale halfway back.
        let easing = l.frame(3000);
        assert_eq!(easing.max_turn, [180.0, 120.0]);
        assert!((easing.sensitivity - 0.75).abs() < 1e-5);
        let over = l.frame(4500);
        assert_eq!((over.max_turn, over.sensitivity), ([0.0; 2], 1.0));
    }

    #[test]
    fn a_turn_rate_cap_limits_each_frame_and_zero_means_none() {
        assert_eq!(cap_turn(10.0, 60.0, 0.1), 6.0);
        assert_eq!(cap_turn(-10.0, 60.0, 0.1), -6.0);
        assert_eq!(cap_turn(2.0, 60.0, 0.1), 2.0);
        assert_eq!(cap_turn(10.0, 0.0, 0.1), 10.0);
    }

    #[test]
    fn the_stock_flash_file_asks_for_the_flashed_screen() {
        assert_eq!(ShockParams::parse(STOCK_FLASH).screen, Screen::Flashed);
    }
}
