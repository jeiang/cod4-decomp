// SPDX-License-Identifier: GPL-3.0-only
//! What the player types and what the server asks of them: the chat field (`Con_DrawSay`), the console
//! (`Con_DrawSolidConsole`) and the vote lines (`CG_DrawVote`).

use super::{LiveUi, localize};
use crate::console::{Console, Mode};
use crate::ui::Ui;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::{horz, vert};

/// The vote in progress, read from the vote configstrings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveVote {
    /// When the vote ends, on the server clock.
    pub end_ms: i32,
    pub text: String,
    pub yes: i32,
    pub no: i32,
}

impl LiveVote {
    /// `time` is `<end ms> <server id>`, empty when no vote runs.
    pub fn parse(time: &str, text: &str, yes: &str, no: &str) -> Option<Self> {
        let end_ms = time.split_whitespace().next()?.parse().ok()?;
        let count = |s: &str| s.trim().parse().unwrap_or(0);
        Some(Self {
            end_ms,
            text: text.to_owned(),
            yes: count(yes),
            no: count(no),
        })
    }

    /// Whole seconds left at server time `now`.
    pub fn seconds_left(&self, now: i32) -> i32 {
        ((self.end_ms - now) / 1000).max(0)
    }
}

/// The font height the HUD text is drawn at: 16 in a window up to 768 high, else 10 (as `CG_DrawChatMessages`).
fn font_h(ui: &Ui) -> f32 {
    if ui.place.size.1 <= 768.0 { 16.0 } else { 10.0 }
}

fn line(ui: &Ui, p: &mut Painter, text: &str, x: f32, y: f32, h: f32, color: [f32; 4]) {
    ui.draw_text(
        p,
        &TextDraw {
            text,
            font_enum: 0,
            scale: h / 48.0,
            style: 3,
            color,
            x,
            y: y + h - 1.0,
            horz: horz::LEFT,
            vert: vert::TOP,
        },
    );
}

const YELLOW: [f32; 4] = [1.0, 1.0, 0.0, 1.0];

/// `CG_DrawVote`: the vote's text with the seconds left, then the tallies and the keys that answer it.
pub fn draw_vote(ui: &Ui, p: &mut Painter, live: &LiveUi) {
    let Some(v) = &live.vote else { return };
    let h = font_h(ui);
    let word = |key: &str| localize(&ui.assets, &format!("&{key}"));
    let key = |cmd: &'static str| live.keys.get(cmd).map_or(cmd, String::as_str);
    let head = format!(
        "{}({}):{}",
        word("CGAME_VOTE"),
        v.seconds_left(live.time),
        localize(&ui.assets, &v.text)
    );
    let tally = format!(
        "{}({}):{}, {}({}):{}",
        word("CGAME_YES"),
        key("vote yes"),
        v.yes,
        word("CGAME_NO"),
        key("vote no"),
        v.no
    );
    line(ui, p, &head, 5.0, 220.0 + h, h, YELLOW);
    line(ui, p, &tally, 5.0, 220.0 + 2.0 * h, h, YELLOW);
}

/// The chat field under the chat lines, or the console over the top of the screen.
pub fn draw_typing(ui: &Ui, p: &mut Painter, c: &Console, log: &[String]) {
    let with_cursor = || {
        let (a, b) = c.text.split_at(c.cursor.min(c.text.len()));
        format!(
            "{}|{}",
            a.iter().collect::<String>(),
            b.iter().collect::<String>()
        )
    };
    let h = font_h(ui);
    match c.mode {
        Mode::Closed => {}
        Mode::Chat { team } => {
            let key = if team { "&EXE_SAYTEAM" } else { "&EXE_SAY" };
            let text = format!("{}: {}", localize(&ui.assets, key), with_cursor());
            line(ui, p, &text, 5.0, 204.0, h, [1.0; 4]);
        }
        Mode::Console => {
            let rows = 240.0;
            let white = p.g.white();
            let panel = ui
                .place
                .rect(0.0, 0.0, 640.0, rows, horz::FULLSCREEN, vert::TOP);
            p.pic(&white, panel, [0.0, 0.0, 0.0, 0.85]);
            let shown = ((rows - 2.0 * h) / h) as usize;
            let first = log.len().saturating_sub(shown);
            for (i, l) in log[first..].iter().enumerate() {
                line(ui, p, l, 5.0, h * i as f32, h, [1.0; 4]);
            }
            line(
                ui,
                p,
                &format!("] {}", with_cursor()),
                5.0,
                rows - h,
                h,
                YELLOW,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vote_reads_from_its_configstrings_and_ends_with_an_empty_time() {
        let v = LiveVote::parse("45000 3", "Kick Bo", "2", "x").unwrap();
        assert_eq!((v.end_ms, v.yes, v.no), (45000, 2, 0));
        assert_eq!(v.seconds_left(40_500), 4);
        assert_eq!(v.seconds_left(50_000), 0, "never negative");
        assert_eq!(LiveVote::parse("", "Kick Bo", "2", "1"), None);
    }
}
