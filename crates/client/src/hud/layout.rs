// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame_mp/cg_main_mp.cpp`
// (`cg_hud*Position`, `cg_hudDamageIcon*`, `cg_chatTime`, `cg_chatHeight` registrations) and `client/cl_console.cpp`
// (`con_gameMsgWindowN*` registrations).
//! Where the HUD puts its chat, vote lines, message windows and damage wedges, from the archived dvars the options
//! and the config file set. Every value is clamped to the range the stock engine registers it with and falls back to
//! the stock default while the dvar is unset or not a number.

use crate::input::Cvars;

/// One game message window (`con_gameMsgWindowN*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowCfg {
    /// Most lines visible at once (`LineCount`, 1..=9).
    pub lines: usize,
    /// How long a message stays up, ms (`MsgTime`).
    pub msg_ms: i32,
    /// How long the window takes to scroll a new line into place, ms (`ScrollTime`).
    pub scroll_ms: i32,
    pub fade_in_ms: i32,
    pub fade_out_ms: i32,
}

/// The HUD placement dvars of one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// `cg_hudVotePosition`: the vote box's left and top.
    pub vote: [f32; 2],
    /// `cg_hudSayPosition`: the chat field sits 24 below its `y`.
    pub say: [f32; 2],
    /// `cg_hudChatPosition` and `cg_hudChatIntermissionPosition` (the scoreboard is up): where the chat lines end.
    pub chat: [f32; 2],
    pub chat_intermission: [f32; 2],
    /// `cg_chatTime`: how long a chat line stays, ms.
    pub chat_ms: i32,
    /// `cg_chatHeight`: how many chat lines show; 0 hides chat.
    pub chat_lines: usize,
    pub windows: [WindowCfg; 4],
    /// `cg_hudDamageIconWidth` and `Height`, `Offset` from the screen centre (640x480 units), and `Time` in ms.
    pub icon_size: [f32; 2],
    pub icon_offset: f32,
    pub icon_ms: i32,
}

const WINDOW_MS: [f32; 4] = [5.0, 8.0, 5.0, 5.0];
const WINDOW_LINES: [i32; 4] = [4, 5, 7, 5];
const FADE_IN: [f32; 4] = [0.25, 0.25, 0.75, 0.25];
const FADE_OUT: [f32; 4] = [0.5, 0.01, 0.5, 0.5];
const WINDOW_NAMES: [&str; 4] = [
    "con_gameMsgWindow0",
    "con_gameMsgWindow1",
    "con_gameMsgWindow2",
    "con_gameMsgWindow3",
];

fn number(cvars: &Cvars, name: &str) -> Option<f32> {
    cvars
        .get(name)
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
}

/// A float dvar clamped to `min..=max`.
pub fn float(cvars: &Cvars, name: &str, default: f32, min: f32, max: f32) -> f32 {
    number(cvars, name).unwrap_or(default).clamp(min, max)
}

/// An integer dvar clamped to `min..=max`.
pub fn int(cvars: &Cvars, name: &str, default: i32, min: i32, max: i32) -> i32 {
    number(cvars, name)
        .map_or(default, |v| v as i32)
        .clamp(min, max)
}

/// A two-float dvar (`5 220`) with both parts clamped to `min..=max`.
fn vec2(cvars: &Cvars, name: &str, default: [f32; 2], min: f32, max: f32) -> [f32; 2] {
    let parsed = cvars.get(name).and_then(|v| {
        let mut it = v.split_whitespace().map(|t| t.parse::<f32>());
        let (x, y) = (it.next()?.ok()?, it.next()?.ok()?);
        (x.is_finite() && y.is_finite()).then_some([x, y])
    });
    parsed.unwrap_or(default).map(|v| v.clamp(min, max))
}

/// Seconds to whole milliseconds.
fn ms(seconds: f32) -> i32 {
    (seconds * 1000.0).round().min(i32::MAX as f32) as i32
}

impl WindowCfg {
    fn read(cvars: &Cvars, i: usize) -> Self {
        let n = WINDOW_NAMES[i];
        let secs = |what: &str, default: f32, min: f32| {
            ms(float(cvars, &format!("{n}{what}"), default, min, f32::MAX))
        };
        Self {
            lines: int(cvars, &format!("{n}LineCount"), WINDOW_LINES[i], 1, 9) as usize,
            msg_ms: secs("MsgTime", WINDOW_MS[i], 0.0),
            scroll_ms: secs("ScrollTime", 0.25, 0.0),
            fade_in_ms: secs("FadeInTime", FADE_IN[i], 0.0),
            fade_out_ms: secs("FadeOutTime", FADE_OUT[i], 0.01),
        }
    }
}

impl Layout {
    pub fn read(cvars: &Cvars) -> Self {
        let pos = |name, default| vec2(cvars, name, default, 0.0, 640.0);
        Self {
            vote: pos("cg_hudVotePosition", [5.0, 220.0]),
            say: pos("cg_hudSayPosition", [5.0, 180.0]),
            chat: pos("cg_hudChatPosition", [5.0, 204.0]),
            chat_intermission: pos("cg_hudChatIntermissionPosition", [5.0, 110.0]),
            chat_ms: int(cvars, "cg_chatTime", 12_000, 0, 60_000),
            chat_lines: int(cvars, "cg_chatHeight", 8, 0, 8) as usize,
            windows: [0, 1, 2, 3].map(|i| WindowCfg::read(cvars, i)),
            icon_size: [
                float(cvars, "cg_hudDamageIconWidth", 128.0, 0.0, 512.0),
                float(cvars, "cg_hudDamageIconHeight", 64.0, 0.0, 512.0),
            ],
            icon_offset: float(cvars, "cg_hudDamageIconOffset", 128.0, 0.0, 512.0),
            icon_ms: int(cvars, "cg_hudDamageIconTime", 2000, 0, i32::MAX),
        }
    }

    /// Where the typed chat line is drawn (`CG_DrawSay`: 24 under the say position, whole units).
    pub fn typing_at(&self) -> (f32, f32) {
        (self.say[0].trunc(), self.say[1].trunc() + 24.0)
    }

    /// The vote's first line, given the font height (`CG_DrawVote`).
    pub fn vote_at(&self, font_h: f32) -> (f32, f32) {
        (self.vote[0], self.vote[1] + font_h)
    }

    /// Where the chat lines end: the intermission spot while the scoreboard is up, whole units.
    pub fn chat_at(&self, scoreboard: bool) -> (f32, f32) {
        let p = if scoreboard {
            self.chat_intermission
        } else {
            self.chat
        };
        (p[0].trunc(), p[1].trunc())
    }

    /// The wedge's box about the screen centre for a screen `h` high, as `(width, height, radius)` in pixels.
    pub fn icon_px(&self, h: f32) -> (f32, f32, f32) {
        let unit = h / 480.0;
        (
            self.icon_size[0] * unit,
            self.icon_size[1] * unit,
            self.icon_offset * unit,
        )
    }

    /// The opacity of a wedge `age_ms` after its hit: fully there for the first half of `cg_hudDamageIconTime`,
    /// then fading out; `None` once it is over.
    pub fn wedge_alpha(&self, age_ms: i32) -> Option<f32> {
        (age_ms > 0 && age_ms < self.icon_ms)
            .then(|| (2.0 - 2.0 * age_ms as f32 / self.icon_ms as f32).min(1.0))
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::read(&Cvars::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(c: &mut Cvars, name: &str, v: &str) {
        c.set(name, v, true);
    }

    #[test]
    fn unset_dvars_read_as_the_stock_defaults() {
        let l = Layout::default();
        assert_eq!(l.vote_at(16.0), (5.0, 236.0));
        assert_eq!(l.typing_at(), (5.0, 204.0));
        assert_eq!(l.chat_at(false), (5.0, 204.0));
        assert_eq!(l.chat_at(true), (5.0, 110.0));
        assert_eq!((l.chat_ms, l.chat_lines), (12_000, 8));
        assert_eq!(l.windows[0].msg_ms, 5000);
        assert_eq!(l.windows[1].fade_out_ms, 10);
        assert_eq!(l.windows[2].fade_in_ms, 750);
        assert_eq!(l.windows[2].lines, 7);
        assert_eq!(l.icon_px(480.0), (128.0, 64.0, 128.0));
        assert_eq!(l.icon_ms, 2000);
    }

    #[test]
    fn the_position_dvars_move_the_boxes_and_clamp_to_the_screen() {
        let mut c = Cvars::default();
        set(&mut c, "cg_hudVotePosition", "40 300");
        set(&mut c, "cg_hudSayPosition", "10 100");
        set(&mut c, "cg_hudChatPosition", "700 -5");
        set(&mut c, "cg_hudChatIntermissionPosition", "20 50");
        let l = Layout::read(&c);
        assert_eq!(l.vote_at(16.0), (40.0, 316.0));
        assert_eq!(l.typing_at(), (10.0, 124.0));
        assert_eq!(l.chat_at(false), (640.0, 0.0));
        assert_eq!(l.chat_at(true), (20.0, 50.0));
        set(&mut c, "cg_hudVotePosition", "garbage");
        assert_eq!(Layout::read(&c).vote, [5.0, 220.0]);
    }

    #[test]
    fn chat_dvars_set_how_long_and_how_many() {
        let mut c = Cvars::default();
        set(&mut c, "cg_chatTime", "3000");
        set(&mut c, "cg_chatHeight", "99");
        let l = Layout::read(&c);
        assert_eq!((l.chat_ms, l.chat_lines), (3000, 8));
        set(&mut c, "cg_chatHeight", "0");
        assert_eq!(Layout::read(&c).chat_lines, 0);
    }

    #[test]
    fn a_message_window_reads_its_own_dvars_in_seconds() {
        let mut c = Cvars::default();
        set(&mut c, "con_gameMsgWindow1MsgTime", "2.5");
        set(&mut c, "con_gameMsgWindow1LineCount", "12");
        set(&mut c, "con_gameMsgWindow1FadeOutTime", "0");
        set(&mut c, "con_gameMsgWindow0ScrollTime", "1");
        let l = Layout::read(&c);
        assert_eq!(l.windows[1].msg_ms, 2500);
        assert_eq!(l.windows[1].lines, 9, "at most nine lines");
        assert_eq!(l.windows[1].fade_out_ms, 10, "never below 0.01 s");
        assert_eq!(l.windows[0].scroll_ms, 1000);
        assert_eq!(l.windows[2], Layout::default().windows[2]);
    }

    #[test]
    fn the_damage_icon_dvars_size_place_and_time_the_wedge() {
        let mut c = Cvars::default();
        set(&mut c, "cg_hudDamageIconWidth", "64");
        set(&mut c, "cg_hudDamageIconHeight", "1000");
        set(&mut c, "cg_hudDamageIconOffset", "200");
        set(&mut c, "cg_hudDamageIconTime", "1000");
        let l = Layout::read(&c);
        assert_eq!(l.icon_px(960.0), (128.0, 1024.0, 400.0));
        assert_eq!(l.wedge_alpha(400), Some(1.0));
        assert_eq!(l.wedge_alpha(750), Some(0.5));
        assert_eq!(l.wedge_alpha(1000), None);
        assert_eq!(Layout::default().wedge_alpha(1000), Some(1.0));
    }
}
