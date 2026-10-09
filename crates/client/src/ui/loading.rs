// SPDX-License-Identifier: GPL-3.0-only
// Window, fade, list box and connect-screen behaviour follows KisakCOD (GPL-3.0; `ui/ui_shared.cpp`, `ui/ui_atoms.cpp`,
// `ui_mp/ui_main_mp.cpp`; copyright holders of KisakCOD and the original Call of Duty 4 authors).
//! The loading screen: the map's `loadscreen_<map>` picture over the whole window, its name and a progress bar.
//! Painted while a map loads in the background, so the window keeps presenting.

use super::Ui;
use super::paint::{Painter, TextDraw};
use super::place::{horz, vert};

/// What the loading screen shows.
#[derive(Clone, Copy)]
pub struct LoadingView<'a> {
    /// The map's name (`mp_crash`).
    pub map: &'a str,
    /// What the loader is doing now: the message line.
    pub note: &'a str,
    /// 0 to 1.
    pub progress: f32,
    /// The game mode's display name, or empty when it is not known yet.
    pub gametype: &'a str,
    /// The localized string key of the connection state line (`EXE_AWAITINGHOST`), or empty for none.
    pub status: &'a str,
    /// The clock, for the animated dots.
    pub now_ms: i32,
}

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const YELLOW: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
/// `ui_connectScreenTextGlowColor`.
const GLOW: [f32; 4] = [0.3, 0.6, 0.3, 1.0];

/// The animated dots after a connection state (`Text_PaintCenterWithDots`): none, one, two, three, every half second.
pub(crate) fn dots(now_ms: i32) -> &'static str {
    match (now_ms / 500) & 3 {
        1 => ".  ",
        2 => ".. ",
        3 => "...",
        _ => "   ",
    }
}

/// Cuts a message into lines of at most 58 characters, breaking at the first space past 41 (`UI_DrawConnectScreen`).
pub(crate) fn message_lines(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut lines = Vec::new();
    let (mut line, mut need_break) = (String::new(), false);
    for (i, &c) in chars.iter().enumerate() {
        line.push(c);
        if line.chars().count() > 41 && i > 0 {
            need_break = true;
        }
        if line.chars().count() > 58 || i + 1 == chars.len() || (need_break && c == ' ') {
            lines.push(std::mem::take(&mut line));
            need_break = false;
        }
    }
    lines
}

impl Ui {
    /// The map's display name: the stock `MPUI_<MAP>` string, else the name itself.
    fn map_title(&self, map: &str) -> String {
        let key = format!(
            "MPUI_{}",
            map.trim_start_matches("mp_").to_ascii_uppercase()
        );
        self.assets
            .localize
            .get(&key)
            .map_or_else(|| map.to_ascii_uppercase(), |s| s.to_ascii_uppercase())
    }

    /// `UI_DrawConnectScreen`: the map's load screen, the mode and map names in glowing objective-font text, the
    /// message and connection state lines, and the `connect` menu's bar along the bottom.
    pub fn paint_loading(&self, p: &mut Painter, v: &LoadingView) {
        let pl = &self.place;
        let picture = p.named(&self.assets, &format!("loadscreen_{}", v.map));
        let full = pl.rect(0.0, 0.0, 640.0, 480.0, horz::FULLSCREEN, vert::FULLSCREEN);
        p.pic(&picture, full, WHITE);
        let line =
            |text: &str, y: f32, color: [f32; 4], glow: Option<[f32; 4]>, p: &mut Painter| {
                let width = self.text_width(text, 6, 0.5);
                self.draw_text_fx(
                    p,
                    &TextDraw {
                        text,
                        font_enum: 6,
                        scale: 0.5,
                        style: 6,
                        color,
                        x: 320.0 - (width * 0.5).trunc(),
                        y,
                        horz: horz::FULLSCREEN,
                        vert: vert::FULLSCREEN,
                    },
                    glow,
                    0,
                );
            };
        if !v.gametype.is_empty() {
            line(v.gametype, 89.0, WHITE, Some(GLOW), p);
            line(&self.map_title(v.map), 119.0, WHITE, Some(GLOW), p);
        }
        let mut y = 299.0;
        for l in message_lines(v.note) {
            line(&l, y, YELLOW, None, p);
            y += 22.0;
        }
        if !v.status.is_empty() {
            let text = self
                .assets
                .translate(v.status)
                .map_or_else(|| v.status.to_owned(), |s| s.to_string());
            // The dots are drawn after the text, which is centred without them.
            let width = self.text_width(&text, 6, 0.5);
            let shown = format!("{text}{}", dots(v.now_ms));
            self.draw_text_fx(
                p,
                &TextDraw {
                    text: &shown,
                    font_enum: 6,
                    scale: 0.5,
                    style: 6,
                    color: WHITE,
                    x: 320.0 - (width * 0.5).trunc(),
                    y: 145.0,
                    horz: horz::FULLSCREEN,
                    vert: vert::FULLSCREEN,
                },
                None,
                0,
            );
        }
        // The `connect` menu: a frame of 260 by 4 with a bar of 258 by 2 filling it as the level loads.
        let frame = pl.rect(-128.0, -40.0, 260.0, 4.0, horz::CENTER, vert::BOTTOM);
        p.fill(frame, [0.0, 0.0, 0.0, 0.8]);
        let mut bar = pl.rect(-127.0, -39.0, 258.0, 2.0, horz::CENTER, vert::BOTTOM);
        bar.w *= v.progress.clamp(0.0, 1.0);
        p.fill(bar, WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_wrap_at_a_space_after_41_characters() {
        let m =
            "Connection Interrupted. The server stopped answering, please wait while it recovers.";
        let lines = message_lines(m);
        assert_eq!(lines.concat(), m, "nothing is lost");
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| l.chars().count() <= 59));
        assert!(lines[0].ends_with(' ') && lines[0].chars().count() > 41);
        assert_eq!(message_lines("short"), ["short"]);
        assert!(message_lines("").is_empty());
    }

    #[test]
    fn the_dots_cycle_every_half_second() {
        let d: Vec<_> = (0..4).map(|k| dots(k * 500 + 10)).collect();
        assert_eq!(d, ["   ", ".  ", ".. ", "..."]);
        assert_eq!(dots(2000), "   ");
    }
}
