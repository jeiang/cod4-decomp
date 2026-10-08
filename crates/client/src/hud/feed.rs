// SPDX-License-Identifier: GPL-3.0-or-later
//! The scrolling message windows of the original's console (`Con_DrawGameMessageWindow`): `iprintln` lines and
//! obituaries in window 0, `iprintlnbold` and announcements in window 1, subtitles in window 2; plus chat, the
//! centre string and the kill-feed lines.
//!
//! A message lives from the server time it arrived until its window's message time is up. Its lines fade in while
//! the window scrolls up to make room, and fade out at the end. The stock `gamemessages`, `boldgamemessages` and
//! `subtitles` menus each hold one window item (item type 19); [`Feed::draw_window`] draws it where the menu puts it.

use super::{LiveUi, localize, team_color_escape};
use crate::ui::Ui;
use crate::ui::assets::UiAssets;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::{Px, horz, vert};
use ::assets::zone::menu::{ItemDef, Rect};
use net::ui::Obituary;

pub const WINDOWS: usize = 4;

/// Per-window settings (`con_gameMsgWindowN*` defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cfg {
    /// Most lines visible at once.
    lines: usize,
    /// How long a message stays up, ms.
    msg_ms: i32,
    /// How long the window takes to scroll a new line into place, ms.
    scroll_ms: i32,
    fade_in_ms: i32,
    fade_out_ms: i32,
    /// Where a line may wrap: the window's pixel width in 640x480 units.
    width: f32,
}

const CFG: [Cfg; WINDOWS] = [
    Cfg {
        lines: 4,
        msg_ms: 5000,
        scroll_ms: 250,
        fade_in_ms: 250,
        fade_out_ms: 500,
        width: 455.0,
    },
    Cfg {
        lines: 5,
        msg_ms: 8000,
        scroll_ms: 250,
        fade_in_ms: 250,
        fade_out_ms: 10,
        width: 390.0,
    },
    Cfg {
        lines: 7,
        msg_ms: 5000,
        scroll_ms: 250,
        fade_in_ms: 750,
        fade_out_ms: 500,
        width: 455.0,
    },
    Cfg {
        lines: 5,
        msg_ms: 5000,
        scroll_ms: 250,
        fade_in_ms: 250,
        fade_out_ms: 500,
        width: 455.0,
    },
];

/// The window an `iprintln` line goes to and the one `iprintlnbold` and announcements go to.
pub const NOTIFY: usize = 0;
pub const BOLD: usize = 1;

/// A piece of a message line: text, or an icon whose size is in multiples of the line height.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Text(String),
    Icon {
        material: String,
        w: f32,
        h: f32,
        flip: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct Msg {
    segs: Vec<Seg>,
    start: i32,
    end: i32,
}

struct ChatLine {
    text: String,
    time: i32,
}

struct Center {
    text: String,
    start: i32,
    priority: i32,
}

/// Everything the feed keeps between frames.
#[derive(Default)]
pub struct Feed {
    windows: [Vec<Msg>; WINDOWS],
    chat: Vec<ChatLine>,
    center: Option<Center>,
    /// Console-only prints (`clientprint`), newest last.
    pub console: Vec<String>,
}

/// How long chat stays up and how many lines it keeps (`cg_chatTime`, `cg_chatHeight`).
const CHAT_MS: i32 = 12_000;
const CHAT_LINES: usize = 8;
/// How long the centre string stays (`cg_centertime`).
const CENTER_MS: i32 = 5000;

/// The opacity of a message at `now`: fading out over the last `fade_out_ms`, and fading in over the last
/// `fade_in_ms` of the scroll when it scrolls into place (`Con_GetMessageAlpha`).
fn message_alpha(cfg: &Cfg, start: i32, end: i32, now: i32) -> f32 {
    let mut a = 1.0;
    if end - now < cfg.fade_out_ms {
        a *= (end - now) as f32 / cfg.fade_out_ms as f32;
    }
    let age = now - start;
    if cfg.fade_in_ms < cfg.scroll_ms {
        if age < cfg.scroll_ms {
            if age <= cfg.scroll_ms - cfg.fade_in_ms {
                return 0.0;
            }
            a *= (age - (cfg.scroll_ms - cfg.fade_in_ms)) as f32 / cfg.fade_in_ms as f32;
        }
    } else if cfg.fade_in_ms > 0 && age < cfg.fade_in_ms {
        a *= age as f32 / cfg.fade_in_ms as f32;
    }
    a.max(0.0)
}

/// How much of a line height the whole window is still scrolled down by: each line newer than the scroll time
/// pushes the older ones by what is left of its own scroll.
fn scroll_lines(cfg: &Cfg, starts: impl Iterator<Item = i32>, now: i32, line_h: f32) -> f32 {
    starts
        .filter(|s| now - s < cfg.scroll_ms)
        .map(|s| (line_h * (1.0 - (now - s) as f32 / cfg.scroll_ms as f32).clamp(0.0, 1.0)).round())
        .sum()
}

/// Breaks `text` into lines no wider than `max_w` at word boundaries (a word wider than a line is cut), carrying the
/// last `^N` colour over to the next line. `measure` is the width of a string.
fn wrap(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut color = String::new();
    let push_line = |line: &mut String, lines: &mut Vec<String>, color: &mut String| {
        lines.push(std::mem::take(line));
        line.push_str(color);
    };
    let track = |word: &str, color: &mut String| {
        let b = word.as_bytes();
        for i in 0..b.len().saturating_sub(1) {
            if b[i] == b'^' && b[i + 1].is_ascii_digit() {
                *color = word[i..i + 2].to_owned();
            }
        }
    };
    for word in text.split(' ') {
        let sep = if line.len() > color.len() { " " } else { "" };
        let cand = format!("{line}{sep}{word}");
        if measure(&cand) <= max_w || line.len() <= color.len() && measure(word) <= max_w {
            line = cand;
            track(word, &mut color);
            continue;
        }
        if line.len() > color.len() {
            push_line(&mut line, &mut lines, &mut color);
        }
        // Cut a word wider than the whole line.
        let mut piece = String::new();
        for ch in word.chars() {
            piece.push(ch);
            if measure(&format!("{line}{piece}")) > max_w && piece.chars().count() > 1 {
                piece.pop();
                line.push_str(&piece);
                track(&piece, &mut color);
                push_line(&mut line, &mut lines, &mut color);
                piece = ch.to_string();
            }
        }
        line.push_str(&piece);
        track(&piece, &mut color);
    }
    if line.len() > color.len() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// `CG_FadeAlpha`: opaque, then fading over the last `fade` ms of `total`; `None` when over.
fn fade_alpha(now: i32, start: i32, total: i32, fade: i32) -> Option<f32> {
    let age = now - start;
    if start == 0 || age >= total {
        return None;
    }
    let left = total - age;
    Some(if left < fade {
        left as f32 / fade as f32
    } else {
        1.0
    })
}

impl Feed {
    pub fn clear(&mut self) {
        *self = Feed::default();
    }

    /// Adds a message to `window` at server time `now`.
    pub fn message(&mut self, window: usize, segs: Vec<Seg>, now: i32) {
        let cfg = CFG[window.min(WINDOWS - 1)];
        let w = &mut self.windows[window.min(WINDOWS - 1)];
        w.push(Msg {
            segs,
            start: now,
            end: now + cfg.msg_ms,
        });
        if w.len() > cfg.lines {
            w.remove(0);
        }
    }

    pub fn text(&mut self, window: usize, text: &str, now: i32) {
        self.message(window, vec![Seg::Text(text.to_owned())], now);
    }

    /// `CG_PriorityCenterPrint`: replaces the centre string unless a higher priority one is still up.
    pub fn center_print(&mut self, text: String, priority: i32, now: i32) {
        let busy = self
            .center
            .as_ref()
            .is_some_and(|c| now - c.start < CENTER_MS && priority < c.priority);
        if !busy {
            self.center = Some(Center {
                text,
                start: now,
                priority,
            });
        }
    }

    pub fn chat(&mut self, text: String, now: i32) {
        self.chat.push(ChatLine { text, time: now });
        if self.chat.len() > CHAT_LINES {
            self.chat.remove(0);
        }
    }

    /// Whether the window has a line on screen (`gamemsgwndactive`).
    pub fn active(&self, window: usize, now: i32) -> bool {
        self.windows
            .get(window)
            .is_some_and(|w| w.iter().any(|m| m.end > now))
    }

    /// Adds a kill: the line in window 0, and "you killed ..." in the centre when the viewer is involved.
    /// A killcam replays an old kill, so it shows neither.
    pub fn obituary(&mut self, o: &Obituary, live: &LiveUi, assets: &UiAssets, now: i32) {
        if live.killcam {
            return;
        }
        let (victim, attacker) = (live.name(o.victim), live.name(o.killer));
        let named = !o.suicide() && !attacker.is_empty();
        let mut segs = Vec::new();
        if named {
            segs.push(Seg::Text(format!(
                "{}{attacker}^7 ",
                team_color_escape(live.own_team, live.team(o.killer))
            )));
        }
        segs.extend(kill_icon_segs(o, live));
        segs.push(Seg::Text(format!(
            " {}{victim}",
            team_color_escape(live.own_team, live.team(o.victim))
        )));
        self.message(NOTIFY, segs, now);
        let me = live.own;
        if named && o.killer == me {
            let key = if live.team(o.killer) != 0 && live.team(o.killer) == live.team(o.victim) {
                "CGAME_YOUKILLED\x15{}\x14CGAME_TEAMMATE"
            } else {
                "CGAME_YOUKILLED\x15{}"
            };
            self.center_print(youkilled(assets, key, victim), 0, now);
        } else if o.victim == me && named {
            let key = if live.team(o.killer) != 0 && live.team(o.killer) == live.team(o.victim) {
                "CGAME_YOUWEREKILLED\x15{}\x14CGAME_TEAMMATE"
            } else {
                "CGAME_YOUWEREKILLED\x15{}"
            };
            self.center_print(youkilled(assets, key, attacker), 0, now);
        }
    }

    /// Drops what has timed out.
    fn cull(&mut self, now: i32) {
        for w in &mut self.windows {
            w.retain(|m| m.end > now);
        }
        self.chat.retain(|c| now - c.time < CHAT_MS);
    }

    /// Draws message window `window` where the item `d` of rect `r` puts it, text laid out as the item asks.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_window(
        &mut self,
        ui: &Ui,
        p: &mut Painter,
        window: usize,
        r: &Rect,
        d: &ItemDef,
        color: [f32; 4],
        now: i32,
    ) -> usize {
        if window >= WINDOWS {
            return 0;
        }
        self.cull(now);
        let cfg = CFG[window];
        let (font, scale) = (d.font_enum, d.text_scale);
        let line_h = (scale * 48.0).round();
        let width = |s: &str| ui.text_width(s, font, scale);
        // Visual lines, oldest first, each remembering its message's times.
        let mut lines: Vec<(Vec<Seg>, i32, i32)> = Vec::new();
        for m in &self.windows[window] {
            match m.segs.as_slice() {
                [Seg::Text(t)] => {
                    for l in wrap(t, cfg.width, width) {
                        lines.push((vec![Seg::Text(l)], m.start, m.end));
                    }
                }
                _ => lines.push((m.segs.clone(), m.start, m.end)),
            }
        }
        let skip = lines.len().saturating_sub(cfg.lines);
        let lines = &lines[skip..];
        let (newest_first, up) = match d.game_msg_window_mode {
            1 => (true, true),
            2 => (true, false),
            3 => (false, true),
            _ => (false, false),
        };
        let drawn = std::cell::Cell::new(0);
        let at = |p: &mut Painter, line: &(Vec<Seg>, i32, i32), y: f32| {
            let alpha = message_alpha(&cfg, line.1, line.2, now);
            if alpha > 0.0 {
                drawn.set(drawn.get() + 1);
                let mut c = color;
                c[3] *= alpha;
                let pos = (r.x, y);
                draw_line(
                    ui,
                    p,
                    &line.0,
                    pos,
                    (r.horz_align, r.vert_align),
                    d,
                    line_h,
                    c,
                );
            }
        };
        if newest_first {
            // The window slides by what is left of every fresh line's scroll; each older line sits a line away.
            let shift = scroll_lines(&cfg, lines.iter().map(|l| l.1), now, line_h);
            let mut y = if up {
                r.y + shift
            } else {
                r.y - line_h - shift
            };
            for line in lines.iter().rev() {
                y += if up { -line_h } else { line_h };
                at(p, line, y);
            }
        } else {
            let n = lines.len();
            for (i, line) in lines.iter().enumerate() {
                let y = if up {
                    r.y - (n - 1 - i) as f32 * line_h
                } else {
                    r.y + (i + 1) as f32 * line_h
                };
                at(p, line, y);
            }
        }
        drawn.get()
    }

    /// The centre string (owner draw 90): centred on `rect`'s x, fading over its last 100 ms.
    pub fn draw_center(
        &mut self,
        ui: &Ui,
        p: &mut Painter,
        d: &ItemDef,
        rect: Px,
        color: [f32; 4],
        now: i32,
    ) {
        let Some(c) = &self.center else { return };
        let Some(a) = fade_alpha(now, c.start, CENTER_MS, 100) else {
            self.center = None;
            return;
        };
        let w = ui.text_width(&c.text, d.font_enum, d.text_scale) * ui.place.scale.0;
        let mut col = color;
        col[3] *= a;
        ui.draw_text(
            p,
            &TextDraw {
                text: &c.text,
                font_enum: d.font_enum,
                scale: d.text_scale,
                style: d.text_style,
                color: col,
                x: rect.x - (w * 0.5).round(),
                y: rect.y,
                horz: horz::NOSCALE,
                vert: vert::NOSCALE,
            },
        );
    }

    /// Recent chat above the HUD (`CG_DrawChatMessages`): a dark bar the colour of the line, then the text.
    pub fn draw_chat(&self, ui: &Ui, p: &mut Painter, live: &LiveUi, scoreboard: bool) {
        let (x, y0) = if scoreboard {
            (5.0, 110.0)
        } else {
            (5.0, 204.0)
        };
        let tall = ui.place.size.1 <= 768.0;
        let (font_h, font_w) = if tall { (16.0, 12.0) } else { (10.0, 8.0) };
        let scale = font_h / 48.0;
        let count = self.chat.len() as f32;
        let white = p.g.white();
        for (i, line) in self.chat.iter().enumerate() {
            let left = CHAT_MS - (live.time - line.time);
            let a = if left <= 200 {
                left as f32 / 200.0
            } else {
                1.0
            };
            if a <= 0.0 {
                continue;
            }
            let y = y0 - (count - i as f32) * font_h;
            let w = ui.text_width(&line.text, 0, scale) + font_w * 3.0;
            let tint = escape_rgb(&line.text).map(|c| c * 0.25);
            let bar = ui.place.rect(0.0, y, w, font_h, horz::LEFT, vert::TOP);
            p.pic(&white, bar, [tint[0], tint[1], tint[2], a * 0.6]);
            ui.draw_text(
                p,
                &TextDraw {
                    text: &line.text,
                    font_enum: 0,
                    scale,
                    style: 3,
                    color: [1.0, 1.0, 1.0, a],
                    x,
                    y: y + font_h - 1.0,
                    horz: horz::LEFT,
                    vert: vert::TOP,
                },
            );
        }
    }
}

/// The colour a line starts with (`^1`..`^7`), white without one.
fn escape_rgb(text: &str) -> [f32; 3] {
    let b = text.as_bytes();
    if b.len() >= 2 && b[0] == b'^' {
        match b[1] {
            b'0' => return [0.0; 3],
            b'1' => return [1.0, 0.36, 0.36],
            b'2' => return [0.0, 1.0, 0.0],
            b'3' => return [1.0, 1.0, 0.0],
            b'4' => return [0.0, 0.0, 1.0],
            b'5' => return [0.0, 1.0, 1.0],
            b'6' => return [1.0, 0.36, 1.0],
            _ => {}
        }
    }
    [1.0; 3]
}

/// "You killed <name>" with the translated key; the `{}` marks where the name goes.
fn youkilled(assets: &UiAssets, key: &str, name: &str) -> String {
    let (head, tail) = key.split_once('\x14').map_or((key, ""), |(a, b)| (a, b));
    let base = head.replace("{}", name);
    let mut out = localize(assets, &base);
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(&localize(assets, tail));
    }
    out
}

/// The icon of a kill: the weapon's kill icon (a replacement when the death had no weapon), then the headshot mark.
fn kill_icon_segs(o: &Obituary, live: &LiveUi) -> Vec<Seg> {
    const BASE: f32 = 1.4;
    let icon = |material: &str, w: f32, h: f32, flip: bool| Seg::Icon {
        material: material.to_owned(),
        w,
        h,
        flip,
    };
    let mut segs = Vec::new();
    match live.kill_icons.get(&o.weapon) {
        Some(k) if !o.weapon.is_empty() && !k.material.is_empty() => {
            let (w, h) = match k.ratio {
                1 => (BASE * 2.0, BASE),
                2 => (BASE * 2.0, BASE * 0.5),
                _ => (BASE, BASE),
            };
            segs.push(icon(&k.material, w, h, k.flip));
        }
        _ => {
            let material = match o.mean.as_str() {
                "MOD_MELEE" => "killiconmelee",
                "MOD_HEAD_SHOT" => "killiconheadshot",
                "MOD_CRUSH" => "killiconcrush",
                "MOD_FALLING" => "killiconfalling",
                "MOD_SUICIDE" => "killiconsuicide",
                "MOD_IMPACT" => "killiconimpact",
                _ => "killicondied",
            };
            segs.push(icon(material, BASE, BASE, false));
        }
    }
    if o.headshot && o.mean != "MOD_HEAD_SHOT" {
        segs.push(icon("killiconheadshot", BASE, BASE, false));
    }
    segs
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    ui: &Ui,
    p: &mut Painter,
    segs: &[Seg],
    (x, baseline): (f32, f32),
    (horz_align, vert_align): (i32, i32),
    d: &ItemDef,
    line_h: f32,
    color: [f32; 4],
) {
    let (font, scale) = (d.font_enum, d.text_scale);
    let seg_w = |s: &Seg| match s {
        Seg::Text(t) => ui.text_width(t, font, scale),
        Seg::Icon { w, .. } => (w * line_h).round(),
    };
    let total: f32 = segs.iter().map(seg_w).sum();
    let mut x = match d.text_align_mode & 3 {
        1 => x - (total * 0.5).trunc(),
        2 => x - total,
        _ => x,
    };
    for s in segs {
        match s {
            Seg::Text(t) => ui.draw_text(
                p,
                &TextDraw {
                    text: t,
                    font_enum: font,
                    scale,
                    style: d.text_style,
                    color,
                    x,
                    y: baseline,
                    horz: horz_align,
                    vert: vert_align,
                },
            ),
            Seg::Icon {
                material,
                w,
                h,
                flip,
            } => {
                let (iw, ih) = ((w * line_h).round(), (h * line_h).round());
                let img = p.named(&ui.assets, material);
                let r = ui.place.rect(
                    x,
                    baseline - (ih * 0.5 + line_h * 0.35),
                    iw,
                    ih,
                    horz_align,
                    vert_align,
                );
                if *flip {
                    p.g.quad(&img, [r.x, r.y, r.w, r.h], [1.0, 0.0, 0.0, 1.0], color);
                } else {
                    p.pic(&img, r, color);
                }
            }
        }
        x += seg_w(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> f32 {
        // Ten units per visible letter; colour escapes take no room.
        let mut n = 0;
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '^' && it.peek().is_some_and(char::is_ascii_digit) {
                it.next();
            } else {
                n += 1;
            }
        }
        n as f32 * 10.0
    }

    #[test]
    fn wrapping_breaks_between_words_and_keeps_the_colour() {
        let lines = wrap("^2aaa bbb ccc", 70.0, chars);
        assert_eq!(lines, ["^2aaa bbb", "^2ccc"]);
        assert_eq!(wrap("short", 100.0, chars), ["short"]);
        assert_eq!(wrap("", 100.0, chars), [""]);
        let cut = wrap("abcdefghij", 40.0, chars);
        assert_eq!(cut, ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn a_new_line_fades_in_while_the_window_scrolls_and_out_at_the_end() {
        let cfg = CFG[0];
        let (start, end) = (1000, 6000);
        assert_eq!(
            message_alpha(&cfg, start, end, 1000),
            0.0,
            "invisible at birth"
        );
        let mid = message_alpha(&cfg, start, end, 3000);
        assert_eq!(mid, 1.0);
        let fading = message_alpha(&cfg, start, end, 5750);
        assert!((fading - 0.5).abs() < 1e-6, "{fading}");
        assert_eq!(message_alpha(&cfg, start, end, 6000), 0.0);
        // The subtitle window has a slower fade in than the scroll: plain fade in from the start.
        let sub = CFG[2];
        let q = message_alpha(&sub, 0, 5000, 375);
        assert!((q - 0.5).abs() < 1e-6, "{q}");
    }

    #[test]
    fn the_window_is_pushed_down_by_what_is_left_of_each_scroll() {
        let cfg = CFG[0];
        // One line just born pushes a full line; one half scrolled half; an old one nothing.
        let s = scroll_lines(&cfg, [1000, 1000 - 125, 0].into_iter(), 1000, 14.0);
        assert_eq!(s, 14.0 + 7.0);
        assert_eq!(scroll_lines(&cfg, [0].into_iter(), 1000, 14.0), 0.0);
    }

    #[test]
    fn old_messages_leave_and_the_window_keeps_at_most_its_lines() {
        let mut f = Feed::default();
        for i in 0..6 {
            f.text(NOTIFY, &format!("line {i}"), 1000 + i);
        }
        assert_eq!(f.windows[NOTIFY].len(), CFG[0].lines);
        assert_eq!(
            f.windows[NOTIFY][0].segs,
            vec![Seg::Text("line 2".into())],
            "the oldest went"
        );
        assert!(f.active(NOTIFY, 2000));
        assert!(!f.active(NOTIFY, 1000 + 5000 + 10));
        assert!(!f.active(BOLD, 2000));
        f.cull(1000 + 5003);
        assert_eq!(
            f.windows[NOTIFY].len(),
            2,
            "only the two newest are still up"
        );
    }

    #[test]
    fn the_centre_string_keeps_its_priority_until_it_fades() {
        let mut f = Feed::default();
        f.center_print("first".into(), 2, 1000);
        f.center_print("lower".into(), 1, 2000);
        assert_eq!(f.center.as_ref().unwrap().text, "first");
        f.center_print("equal".into(), 2, 2500);
        assert_eq!(f.center.as_ref().unwrap().text, "equal");
        f.center_print("later".into(), 0, 2500 + CENTER_MS + 1);
        assert_eq!(f.center.as_ref().unwrap().text, "later");
        assert_eq!(fade_alpha(2500, 1000, 5000, 100), Some(1.0));
        assert_eq!(fade_alpha(5950, 1000, 5000, 100), Some(0.5));
        assert_eq!(fade_alpha(6000, 1000, 5000, 100), None);
    }

    #[test]
    fn a_kill_line_names_both_sides_with_the_weapon_icon_between() {
        let mut live = LiveUi {
            own: 3,
            own_team: 2,
            names: vec![String::new(); 8],
            teams: vec![0; 8],
            ..LiveUi::default()
        };
        live.names[3] = "Me".into();
        live.names[5] = "Foe".into();
        live.teams[3] = 2;
        live.teams[5] = 1;
        live.kill_icons.insert(
            "ak47_mp".into(),
            super::super::KillIcon {
                material: "hud_icon_ak47".into(),
                ratio: 1,
                flip: false,
            },
        );
        let o = Obituary {
            killer: 5,
            victim: 3,
            weapon: "ak47_mp".into(),
            mean: "MOD_RIFLE_BULLET".into(),
            headshot: true,
        };
        let mut f = Feed::default();
        f.obituary(&o, &live, &UiAssets::default(), 100);
        let segs = &f.windows[NOTIFY][0].segs;
        assert_eq!(segs[0], Seg::Text("^9Foe^7 ".into()));
        assert!(
            matches!(&segs[1], Seg::Icon { material, w, .. } if material == "hud_icon_ak47" && *w > 2.0)
        );
        assert!(matches!(&segs[2], Seg::Icon { material, .. } if material == "killiconheadshot"));
        assert_eq!(segs[3], Seg::Text(" ^8Me".into()));
        // A suicide has no attacker and a world kill uses the cause as the icon.
        let s = Obituary {
            killer: 3,
            victim: 3,
            weapon: String::new(),
            mean: "MOD_SUICIDE".into(),
            headshot: false,
        };
        f.obituary(&s, &live, &UiAssets::default(), 200);
        let segs = &f.windows[NOTIFY][1].segs;
        assert!(matches!(&segs[0], Seg::Icon { material, .. } if material == "killiconsuicide"));
        // A killcam replay adds nothing.
        live.killcam = true;
        f.obituary(&o, &live, &UiAssets::default(), 300);
        assert_eq!(f.windows[NOTIFY].len(), 2);
    }
}
