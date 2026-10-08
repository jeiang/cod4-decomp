// SPDX-License-Identifier: GPL-3.0-or-later
//! The scoreboard's player rows (`CG_DrawScoreboard`): the stock `scoreboard` menu draws the bars and team scores at
//! the top of the screen, the lines of players under them are the client's own drawing.
//!
//! Everything is laid out in 640x480 units in a 500x435 panel centred in the safe area: column headers on the first
//! team's banner line, a banner per team (icon, name, player count), a coloured row per player, the viewer's own row
//! in its own colour, teams of the viewer first, spectators last. Rows that do not fit scroll off the bottom.

use super::{LiveUi, ScoreLine, localize};
use crate::input::Cvars;
use crate::shell::ShellState;
use crate::ui::Ui;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::{Px, horz};

pub const PANEL_W: f32 = 500.0;
pub const PANEL_H: f32 = 435.0;
const ITEM_H: f32 = 18.0;
const BANNER_H: f32 = 35.0;
const FONT_SCALE: f32 = 0.35;
const TEXT_OFFSET: f32 = 0.5;
/// Panel edge to list: border, shadow and a little air on the left; the list is this much narrower on both sides.
const LIST_X: f32 = 3.0 + 2.0 + 4.0;
const LIST_W: f32 = PANEL_W - 6.0 - 4.0 - 8.0;

const COLUMNS: [(Col, f32, &str, u8); 9] = [
    (Col::Rank, 0.05, "", 0),
    (Col::Status, 0.05, "", 2),
    (Col::Name, 0.35, "", 0),
    (Col::Talking, 0.05, "", 0),
    (Col::Score, 0.1, "CGAME_SB_SCORE", 2),
    (Col::Kills, 0.1, "CGAME_SB_KILLS", 2),
    (Col::Assists, 0.1, "CGAME_SB_ASSISTS", 2),
    (Col::Deaths, 0.1, "CGAME_SB_DEATHS", 2),
    (Col::Ping, 0.1, "CGAME_SB_PING", 2),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Rank,
    Status,
    Name,
    Talking,
    Score,
    Kills,
    Assists,
    Deaths,
    Ping,
}

/// What a line of the list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// A team banner with the team's player count.
    Banner(u8, usize),
    /// Index into the rows.
    Row(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Item {
    pub kind: Kind,
    pub y: f32,
}

#[derive(Debug, PartialEq)]
pub(super) struct Plan {
    pub items: Vec<Item>,
    /// Banner and row lines in all, the scroll range.
    pub total: usize,
    /// A line did not fit under the last drawn one.
    pub off_bottom: bool,
    /// Where the column headers go (their own line once the list is scrolled).
    pub header_y: f32,
}

const TOP: f32 = (480.0 - PANEL_H) / 2.0;
/// The first banner line, the header sitting on it when the list is not scrolled.
const FIRST_Y: f32 = TOP + 3.0 + 2.0 + 24.0 + 1.0 + ITEM_H + 4.0 + 15.0;
const BOTTOM: f32 = TOP + PANEL_H - 3.0 - 2.0 - 14.0 - 1.0;

/// Team order of the list: the viewer's team first (allies when the viewer has none), then the other, free players,
/// spectators; the two sides are listed when anyone is on one.
fn team_order(rows: &[ScoreLine], own_team: u8) -> Vec<u8> {
    let any = |t: u8| rows.iter().any(|r| r.team == t);
    let mut order = Vec::new();
    if any(1) || any(2) {
        let first = if own_team == 1 { 1 } else { 2 };
        order.extend([first, 3 - first]);
    }
    for t in [0, 3] {
        if any(t) {
            order.push(t);
        }
    }
    order
}

/// Lays the list out: which banners and rows are drawn and where, from line `top` (1 = not scrolled).
pub(super) fn plan(rows: &[ScoreLine], own_team: u8, top: usize) -> Plan {
    let top = top.max(1);
    let order = team_order(rows, own_team);
    let total = rows.len() + order.len();
    let header_y = FIRST_Y;
    let mut y = if top > 1 {
        header_y + BANNER_H + 4.0
    } else {
        header_y
    };
    let mut items = Vec::new();
    let mut line = 1;
    let mut off_bottom = false;
    // One line of the list: drawn when it is in view and fits, skipped above the view, the end below it.
    let mut place = |kind: Kind, h: f32, y: &mut f32, step: f32| {
        if off_bottom {
            return;
        }
        if line >= top {
            if BOTTOM >= *y + h {
                line += 1;
                items.push(Item { kind, y: *y });
                *y += step;
            } else {
                off_bottom = true;
            }
        } else {
            line += 1;
        }
    };
    for (i, &team) in order.iter().enumerate() {
        let count = rows.iter().filter(|r| r.team == team).count();
        place(Kind::Banner(team, count), BANNER_H, &mut y, BANNER_H + 4.0);
        for (k, r) in rows.iter().enumerate() {
            if r.team == team {
                place(Kind::Row(k), ITEM_H, &mut y, ITEM_H + 4.0);
            }
        }
        if i + 1 < order.len() {
            y += 4.0;
        }
    }
    Plan {
        items,
        total,
        off_bottom,
        header_y,
    }
}

/// How the scoreboard looks, from dvars: the stock values unless a script changed them.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreView {
    pub team_icon: [String; 4],
    pub team_name: [String; 4],
    pub team_color: [[f32; 3]; 4],
    pub my_color: [f32; 3],
}

impl Default for ScoreView {
    fn default() -> Self {
        ScoreView {
            team_icon: Default::default(),
            team_name: Default::default(),
            // `g_ScoresColor_Free`, `_Axis`, `_Allies`, `_Spectator`.
            team_color: [
                [0.76, 0.78, 0.1],
                [0.69, 0.07, 0.05],
                [0.09, 0.46, 0.07],
                [0.25, 0.25, 0.25],
            ],
            my_color: [1.0, 0.8, 0.4],
        }
    }
}

impl ScoreView {
    /// Reads the dvars the scripts and menus set (`g_TeamIcon_*`, `g_TeamName_*`, `g_ScoresColor_*`).
    pub fn refresh(&mut self, cvars: &Cvars) {
        let get = |n: &str| cvars.get(n).unwrap_or("").to_owned();
        self.team_icon = [
            get("g_TeamIcon_Free"),
            get("g_TeamIcon_Axis"),
            get("g_TeamIcon_Allies"),
            get("g_TeamIcon_Spectator"),
        ];
        self.team_name = [
            String::new(),
            get("g_TeamName_Axis"),
            get("g_TeamName_Allies"),
            "CGAME_SPECTATORS".to_owned(),
        ];
        for (i, n) in ["Free", "Axis", "Allies", "Spectator"].iter().enumerate() {
            if let Some(c) = cvars
                .get(&format!("g_ScoresColor_{n}"))
                .and_then(parse_color)
            {
                self.team_color[i] = c;
            }
        }
        if let Some(c) = cvars.get("cg_scoreboardMyColor").and_then(parse_color) {
            self.my_color = c;
        }
    }
}

/// Three floats from a dvar value such as `0.6 0.64 0.69`.
pub fn parse_color(s: &str) -> Option<[f32; 3]> {
    let v: Vec<f32> = s
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    (v.len() >= 3).then(|| [v[0], v[1], v[2]])
}

/// The width of text shrunk until it fits `width` (`DrawListString`), and the scale it ended up at.
fn fit(ui: &Ui, text: &str, width: f32) -> f32 {
    let mut scale = FONT_SCALE;
    while scale > 0.05 && ui.text_width(text, 0, scale) > width {
        scale -= 0.025;
    }
    scale
}

/// Banner and row lines the list holds, the most it scrolls.
pub fn scoreboard_lines(rows: &[ScoreLine], own_team: u8) -> usize {
    plan(rows, own_team, 1).total
}

/// How many player rows the scoreboard shows for these rows scrolled to `top`.
pub fn rows_shown(rows: &[ScoreLine], own_team: u8, top: usize) -> usize {
    plan(rows, own_team, top)
        .items
        .iter()
        .filter(|i| matches!(i.kind, Kind::Row(_)))
        .count()
}

pub fn draw_scoreboard(ui: &Ui, p: &mut Painter, st: &ShellState) {
    let live = &st.live;
    if live.scores.is_empty() {
        return;
    }
    let view = &st.score_view;
    let plan = plan(&live.scores, live.own_team, st.scores_top);
    let view_w = (ui.place.view_max.0 - ui.place.view_min.0) / ui.place.scale.0;
    let left = ((view_w - PANEL_W) / 2.0).max(0.0);
    let x0 = left + LIST_X;
    let white = p.g.white();
    let text =
        |p: &mut Painter, s: &str, scale: f32, x: f32, y: f32, color: [f32; 4], style: i32| {
            ui.draw_text(
                p,
                &TextDraw {
                    text: s,
                    font_enum: 0,
                    scale,
                    style,
                    color,
                    x,
                    y,
                    horz: horz::LEFT,
                    vert: 0,
                },
            );
        };
    let rect = |x: f32, y: f32, w: f32, h: f32| -> Px { ui.place.rect(x, y, w, h, horz::LEFT, 0) };
    // Column headers, on the first banner line.
    let hscale = FONT_SCALE * 0.85;
    let mut x = x0;
    for (_, frac, key, _) in COLUMNS {
        let w = frac * LIST_W;
        if !key.is_empty() {
            let t = localize(&ui.assets, key);
            let tw = ui.text_width(&t, 0, hscale);
            text(
                p,
                &t,
                hscale,
                x + (w - tw) * 0.5,
                plan.header_y + BANNER_H,
                [1.0; 4],
                3,
            );
        }
        x += w;
    }
    for item in &plan.items {
        match item.kind {
            Kind::Banner(team, count) => {
                let icon = &view.team_icon[usize::from(team)];
                let mut x = x0;
                if !icon.is_empty() {
                    let img = p.named(&ui.assets, icon);
                    p.pic(&img, rect(x, item.y, BANNER_H, BANNER_H), [1.0; 4]);
                    x += BANNER_H + 8.0;
                }
                let name = localize(&ui.assets, &view.team_name[usize::from(team)]);
                text(p, &name, FONT_SCALE, x, item.y + BANNER_H, [1.0; 4], 3);
                let nx = x + ui.text_width(&name, 0, FONT_SCALE).trunc() + 8.0;
                text(
                    p,
                    &format!("( {count} )"),
                    FONT_SCALE,
                    nx,
                    item.y + BANNER_H,
                    [1.0; 4],
                    3,
                );
            }
            Kind::Row(k) => draw_row(
                ui,
                p,
                &live.scores[k],
                live,
                view,
                item.y,
                x0,
                &white,
                &text,
                &rect,
            ),
        }
    }
    // The server address at the bottom right.
    if !live.server_addr.is_empty() {
        let tw = ui.text_width(&live.server_addr, 0, 0.35);
        let x = left + PANEL_W - 3.0 - 2.0 - 4.0 - (tw + 4.0);
        text(
            p,
            &live.server_addr,
            0.35,
            x,
            TOP + PANEL_H - 3.0 - 2.0 - 3.0,
            [1.0; 4],
            3,
        );
    }
    if plan.off_bottom || st.scores_top > 1 {
        // A scroll thumb at the right of the list.
        let track = rect(
            left + PANEL_W - 8.0,
            FIRST_Y + BANNER_H,
            4.0,
            BOTTOM - FIRST_Y - BANNER_H,
        );
        p.pic(&white, track, [1.0, 1.0, 1.0, 0.15]);
        let shown = plan.items.len().max(1) as f32;
        let frac = (shown / plan.total.max(1) as f32).min(1.0);
        let at = (st.scores_top.saturating_sub(1)) as f32 / plan.total.max(1) as f32;
        let thumb = Px {
            x: track.x,
            y: track.y + track.h * at,
            w: track.w,
            h: track.h * frac,
        };
        p.pic(&white, thumb, [1.0, 1.0, 1.0, 0.5]);
    }
}

/// Draws a string at a scale, position, colour and style.
type TextFn<'a> = dyn Fn(&mut Painter, &str, f32, f32, f32, [f32; 4], i32) + 'a;

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &Ui,
    p: &mut Painter,
    r: &ScoreLine,
    live: &LiveUi,
    view: &ScoreView,
    y: f32,
    x0: f32,
    white: &render::ui2d::UiImage,
    text: &TextFn<'_>,
    rect: &dyn Fn(f32, f32, f32, f32) -> Px,
) {
    let tc = view.team_color[usize::from(r.team.min(3))];
    p.pic(
        white,
        rect(x0, y, LIST_W, ITEM_H),
        [tc[0], tc[1], tc[2], 0.5],
    );
    let me = r.client == live.own;
    let c = if me { view.my_color } else { [1.0; 3] };
    let color = [c[0], c[1], c[2], 1.0];
    let spectator = r.team == 3;
    let mut x = x0;
    for (col, frac, _, align) in COLUMNS {
        let w = frac * LIST_W;
        let value = match col {
            Col::Name => Some(r.name.clone()),
            Col::Score if !spectator => Some(r.score.to_string()),
            Col::Kills if !spectator => Some(r.kills.to_string()),
            Col::Assists if !spectator => Some(r.assists.to_string()),
            Col::Deaths if !spectator => Some(r.deaths.to_string()),
            Col::Ping => Some(r.ping.max(0).to_string()),
            _ => None,
        };
        if let Some(s) = value {
            let scale = fit(ui, &s, w);
            let tw = ui.text_width(&s, 0, scale);
            let adj = match align {
                1 => (w - tw) * 0.5,
                2 => w - tw - 4.0,
                _ => 0.0,
            };
            let th = ui.text_height(0, scale);
            let style = if scale < 0.2 { 0 } else { 3 };
            text(
                p,
                &s,
                scale,
                x + adj,
                (th + ITEM_H) * TEXT_OFFSET + y,
                color,
                style,
            );
        } else if col == Col::Status && !r.status_icon.is_empty() {
            let img = p.named(&ui.assets, &r.status_icon);
            p.pic(&img, rect(x + (w - ITEM_H), y, ITEM_H, ITEM_H), [1.0; 4]);
        }
        x += w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(client: u16, team: u8) -> ScoreLine {
        ScoreLine {
            client,
            team,
            ..ScoreLine::default()
        }
    }

    fn lines(plan: &Plan) -> Vec<Kind> {
        plan.items.iter().map(|i| i.kind).collect()
    }

    #[test]
    fn the_viewers_team_is_listed_first_and_spectators_last() {
        let rows = [row(0, 1), row(1, 2), row(2, 3), row(3, 2)];
        let p = plan(&rows, 1, 1);
        assert_eq!(
            lines(&p),
            [
                Kind::Banner(1, 1),
                Kind::Row(0),
                Kind::Banner(2, 2),
                Kind::Row(1),
                Kind::Row(3),
                Kind::Banner(3, 1),
                Kind::Row(2)
            ]
        );
        // A viewer with no side sees allies first.
        let p = plan(&rows, 3, 1);
        assert_eq!(p.items[0].kind, Kind::Banner(2, 2));
        // Free-for-all: one banner, then the players in the order the server sent them.
        let ffa = [row(0, 0), row(1, 0)];
        assert_eq!(
            lines(&plan(&ffa, 0, 1)),
            [Kind::Banner(0, 2), Kind::Row(0), Kind::Row(1)]
        );
    }

    #[test]
    fn both_sides_get_a_banner_when_only_one_has_players() {
        let p = plan(&[row(0, 2)], 2, 1);
        assert_eq!(
            lines(&p),
            [Kind::Banner(2, 1), Kind::Row(0), Kind::Banner(1, 0)]
        );
    }

    #[test]
    fn rows_flow_down_and_stop_at_the_panel_bottom() {
        let rows: Vec<_> = (0..30).map(|i| row(i, 2)).collect();
        let p = plan(&rows, 2, 1);
        assert!(p.off_bottom);
        assert!(p.items.len() < rows.len());
        let ys: Vec<f32> = p.items.iter().map(|i| i.y).collect();
        assert_eq!(ys[0], FIRST_Y);
        assert_eq!(ys[1], FIRST_Y + BANNER_H + 4.0);
        assert_eq!(ys[2] - ys[1], ITEM_H + 4.0);
        assert!(ys.iter().all(|y| *y + ITEM_H <= BOTTOM));
    }

    #[test]
    fn scrolling_skips_lines_and_gives_the_header_its_own_line() {
        let rows: Vec<_> = (0..30).map(|i| row(i, 2)).collect();
        let top = plan(&rows, 2, 1);
        let scrolled = plan(&rows, 2, 4);
        assert_eq!(top.total, 32);
        // Lines 1..3 (banner, two rows) are above the view: the first drawn is the third player.
        assert_eq!(scrolled.items[0].kind, Kind::Row(2));
        assert_eq!(scrolled.items[0].y, FIRST_Y + BANNER_H + 4.0);
        let last = |p: &Plan| match p.items.last().unwrap().kind {
            Kind::Row(k) => k,
            k => panic!("{k:?}"),
        };
        assert!(
            last(&scrolled) > last(&top),
            "scrolling brings later players into view"
        );
        // Scrolled far enough, everything fits and nothing is off the bottom.
        assert!(!plan(&rows, 2, 25).off_bottom);
    }

    #[test]
    fn dvar_colours_parse() {
        assert_eq!(parse_color("0.6 0.64 0.69"), Some([0.6, 0.64, 0.69]));
        assert_eq!(parse_color("1 2"), None);
        assert_eq!(parse_color("1 0.5 0 1"), Some([1.0, 0.5, 0.0]));
    }
}
