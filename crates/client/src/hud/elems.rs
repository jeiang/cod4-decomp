// SPDX-License-Identifier: GPL-3.0-or-later
//! Script hud elements (`newHudElem` and friends): `CG_Draw2dHudElems` and `DrawSingleHudElem2d` of the original.
//!
//! Sizes and positions are worked out in pixels. An element is `width x height` big (text: label plus text width and
//! the font height; materials: the scripted size, or the font height when it is 0); `alignx`/`aligny` pick which part
//! of that box sits on the placed `x`/`y`; `moveovertime` slides between two placements; `fadeovertime` blends the
//! colour; `scaleovertime` the material size. Waypoints draw an icon at the screen position of a world point, clamped
//! to the screen edge with a pointer arrow when it is off screen.

use super::{LiveElem, LiveUi, elem_time_ms, localize, tenths_timer_text, timer_text};
use crate::shell::ShellState;
use crate::ui::Ui;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::{Place, Px, horz, vert};
use net::ui::{HudElem, he, hf};

/// Base text scale and menu font number of the script font numbers (`GetHudElemInfo`).
fn font_of(e: &HudElem) -> (i32, f32) {
    let (enum_, base) = match e.font {
        1 => (4, 0.5),
        2 => (5, 1.0 / 3.0),
        3 => (6, 0.25),
        4 => (2, 0.25),
        5 => (3, 0.25),
        _ => (0, 0.25),
    };
    (enum_, base * e.font_scale)
}

const ALIGN_SCALE: [f32; 4] = [0.0, 0.5, 1.0, 0.0];

/// Where an element's top-left corner is when placed at `(x, y)` with its alignments (`GetHudElemOrg`).
fn origin(
    place: &Place,
    align_org: u8,
    align_screen: u8,
    (x, y): (f32, f32),
    (w, h): (f32, f32),
) -> (f32, f32) {
    let ax = ALIGN_SCALE[usize::from((align_org >> 2) & 3)];
    let ay = ALIGN_SCALE[usize::from(align_org & 3)];
    (
        place.x(x, i32::from((align_screen >> 3) & 7)) - w * ax,
        place.y(y, i32::from(align_screen & 7)) - h * ay,
    )
}

/// The top-left corner at `now`, sliding from the `from_*` placement to the final one while `moveovertime` runs.
pub(super) fn position(place: &Place, e: &HudElem, size: (f32, f32), now: i32) -> (f32, f32) {
    let t = e.move_progress(now);
    let to = origin(place, e.align_org, e.align_screen, (e.x, e.y), size);
    if t >= 1.0 {
        return (to.0.round(), to.1.round());
    }
    let from = origin(
        place,
        e.from_align_org,
        e.from_align_screen,
        (e.from_x, e.from_y),
        size,
    );
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

/// A material size in pixels: 0 means "as tall as the font", full-screen alignments scale to the whole window.
fn material_extent(place: &Place, e: &HudElem, now: i32, font_h: f32) -> (f32, f32) {
    let w = |align: u8, v: u16| match v {
        0 => font_h,
        v if (align >> 3) & 7 == horz::FULLSCREEN as u8 => place.full.0 * f32::from(v),
        v => place.scale.0 * f32::from(v),
    };
    let h = |align: u8, v: u16| match v {
        0 => font_h,
        v if align & 7 == vert::FULLSCREEN as u8 => place.full.1 * f32::from(v),
        v => place.scale.1 * f32::from(v),
    };
    let to = (w(e.align_screen, e.width), h(e.align_screen, e.height));
    if e.scale_time <= 0 || now.wrapping_sub(e.scale_start) >= e.scale_time {
        return to;
    }
    let from = (
        w(e.from_align_screen, e.from_width),
        h(e.from_align_screen, e.from_height),
    );
    let t = e.scale_progress(now);
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

/// The label with `&&1` replaced by the text (`ConsolidateHudElemText`); label and text on their own otherwise.
pub(super) fn consolidate(label: &str, text: &str) -> String {
    match label.split_once("&&1") {
        Some((a, b)) => format!("{a}{text}{b}"),
        None => format!("{label}{text}"),
    }
}

fn rgba(c: [u8; 4]) -> [f32; 4] {
    c.map(|v| f32::from(v) / 255.0)
}

/// What a text-like element says right now.
fn shown_text(ui: &Ui, le: &LiveElem, now: i32) -> String {
    match le.e.kind {
        he::TEXT => localize(&ui.assets, &le.text),
        he::VALUE => format!("{}", le.e.value),
        he::TIMER_DOWN | he::TIMER_UP => timer_text(elem_time_ms(&le.e, now)),
        he::TENTHS_TIMER_DOWN | he::TENTHS_TIMER_UP => tenths_timer_text(elem_time_ms(&le.e, now)),
        _ => le.text.clone(),
    }
}

/// Elements under the menus, then the chat above them all (the order of `CG_Draw2D`).
pub fn draw_under(ui: &Ui, p: &mut Painter, st: &ShellState) {
    if !st.live.active {
        return;
    }
    st.feed.draw_chat(ui, p, &st.live, st.game.scoreboard);
    draw_elems(ui, p, &st.live, false);
}

/// Foreground elements, the spectated player's name and the scoreboard rows, over the menus.
pub fn draw_over(ui: &Ui, p: &mut Painter, st: &ShellState) {
    let live = &st.live;
    if !live.active {
        return;
    }
    draw_elems(ui, p, live, true);
    if let Some(name) = live
        .following
        .as_deref()
        .filter(|_| !st.scoreboard_shown(ui))
    {
        let header = localize(&ui.assets, "CGAME_FOLLOWING");
        for (text, y) in [(header.as_str(), 20.0), (name, 36.0)] {
            let w = ui.text_width(text, 6, 1.0 / 3.0);
            ui.draw_text(
                p,
                &TextDraw {
                    text,
                    font_enum: 6,
                    scale: 1.0 / 3.0,
                    style: 3,
                    color: [1.0; 4],
                    x: -w * 0.5,
                    y,
                    horz: horz::CENTER_SAFEAREA,
                    vert: vert::TOP,
                },
            );
        }
    }
    if st.scoreboard_shown(ui) {
        super::draw_scoreboard(ui, p, st);
    }
}

fn draw_elems(ui: &Ui, p: &mut Painter, live: &LiveUi, foreground: bool) {
    let menu_open = !ui.open_menus().is_empty();
    for le in &live.elems {
        let e = &le.e;
        let fg = e.flags & hf::FOREGROUND != 0;
        if fg != foreground
            || (live.dead && e.flags & hf::HIDE_WHEN_DEAD != 0)
            || (menu_open && e.flags & hf::HIDE_WHEN_IN_MENU != 0)
            || (live.killcam && !e.archived())
        {
            continue;
        }
        let now = live.hud_time;
        let color = rgba(e.color_at(now));
        if color[3] <= 0.0 {
            continue;
        }
        if e.kind == he::WAYPOINT {
            draw_waypoint(ui, p, live, le, color);
        } else {
            draw_elem(ui, p, le, now, color);
        }
    }
}

fn draw_elem(ui: &Ui, p: &mut Painter, le: &LiveElem, now: i32, color: [f32; 4]) {
    let e = &le.e;
    let place = &ui.place;
    let (font_enum, font_scale) = font_of(e);
    let font_h = ui.text_height(font_enum, font_scale) * place.scale.1;
    let width_of = |s: &str| ui.text_width(s, font_enum, font_scale) * place.scale.0;
    let mut label = if le.label.is_empty() {
        String::new()
    } else {
        localize(&ui.assets, &le.label)
    };
    let mut text = match e.kind {
        he::TEXT
        | he::VALUE
        | he::PLAYERNAME
        | he::MAPNAME
        | he::GAMETYPE
        | he::TIMER_DOWN
        | he::TIMER_UP
        | he::TENTHS_TIMER_DOWN
        | he::TENTHS_TIMER_UP => shown_text(ui, le, now),
        _ => String::new(),
    };
    if !label.is_empty() && !text.is_empty() {
        text = consolidate(&label, &text);
        label.clear();
    }
    let label_w = if label.is_empty() {
        0.0
    } else {
        width_of(&label)
    };
    let text_w = if text.is_empty() {
        0.0
    } else {
        width_of(&text)
    };
    let is_material = matches!(e.kind, he::MATERIAL | he::CLOCK_DOWN | he::CLOCK_UP);
    let mat = is_material.then(|| material_extent(place, e, now, font_h));
    let w = match mat {
        Some((mw, _)) => mw + label_w,
        None => label_w + text_w,
    };
    let h = mat.map_or(font_h, |(_, mh)| mh.max(font_h));
    let (mut x, y) = position(place, e, (w, h), now);
    let align_y = ALIGN_SCALE[usize::from(e.align_y())];
    let glow = (e.glow_color[3] > 0).then(|| rgba(e.glow_color));
    let draw = |p: &mut Painter, s: &str, x: f32, glow: Option<[f32; 4]>, chars: usize| {
        ui.draw_text_fx(
            p,
            &TextDraw {
                text: s,
                font_enum,
                scale: font_scale,
                style: 3,
                color,
                x,
                y: (y + (h - font_h) * align_y + font_h).round(),
                horz: horz::NOSCALE,
                vert: vert::NOSCALE,
            },
            glow,
            chars,
        );
    };
    if !label.is_empty() {
        draw(p, &label, x, glow, 0);
        x += label_w;
    }
    match (e.kind, mat) {
        (_, None) => {
            if !text.is_empty() {
                let (chars, glow) = pulse(e, now, glow, text.chars().count());
                if chars != Some(0) {
                    draw(p, &text, x, glow, chars.unwrap_or(0));
                }
            }
        }
        (he::MATERIAL, Some((mw, mh))) => {
            if let Some(img) = named(ui, p, &le.material) {
                let top = y + (h - mh) * align_y;
                p.pic(
                    &img,
                    Px {
                        x,
                        y: top,
                        w: mw,
                        h: mh,
                    },
                    color,
                );
            }
        }
        (_, Some((mw, mh))) => {
            // A clock: the face, and a hand turning once per `duration` (a minute when it has none).
            let top = y + (h - mh) * align_y;
            let r = Px {
                x,
                y: top,
                w: mw,
                h: mh,
            };
            if let Some(face) = named(ui, p, &le.material) {
                p.pic(&face, r, color);
            }
            if let Some(hand) = named(ui, p, &format!("{}needle", le.material)) {
                let ms = elem_time_ms(e, now) as f32;
                let turn = if e.duration > 0 {
                    ms / e.duration as f32
                } else {
                    ms * 0.006 / 360.0
                };
                let angle = (turn.fract()) * std::f32::consts::TAU;
                p.g.quad_rot(
                    &hand,
                    [r.x, r.y, r.w, r.h],
                    [0.0, 0.0, 1.0, 1.0],
                    color,
                    angle,
                    [r.x + r.w * 0.5, r.y + r.h * 0.5],
                );
            }
        }
    }
}

/// The first `fx_letter` ms per letter reveal of `setpulsefx` text, and its glow while it decays: `(letters shown,
/// glow)`; `None` letters means all of them.
fn pulse(
    e: &HudElem,
    now: i32,
    glow: Option<[f32; 4]>,
    total: usize,
) -> (Option<usize>, Option<[f32; 4]>) {
    if e.fx_birth == 0 || e.fx_letter <= 0 {
        return (None, glow);
    }
    let age = now.wrapping_sub(e.fx_birth).max(0);
    let shown = (age / e.fx_letter) as usize + 1;
    let faded = if e.fx_decay_duration > 0 {
        let t = (age - e.fx_decay_start) as f32 / e.fx_decay_duration as f32;
        (1.0 - t).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let glow = glow.map(|g| [g[0], g[1], g[2], g[3] * faded]);
    (Some(shown.min(total)), glow)
}

fn named(ui: &Ui, p: &mut Painter, name: &str) -> Option<render::ui2d::UiImage> {
    (!name.is_empty()).then(|| p.named(&ui.assets, name))
}

// ---- waypoints ------------------------------------------------------------------------------------------

/// Where a world point is on a `size` pixel screen: in front of the camera at `(x, y)`, or behind it with the
/// screen direction to put its icon in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Screen {
    Front(f32, f32),
    Behind(f32, f32),
}

pub(super) fn project(clip: &glam::Mat4, pos: [f32; 3], size: (f32, f32)) -> Screen {
    let c = *clip * glam::Vec4::new(pos[0], pos[1], pos[2], 1.0);
    if c.w > 0.001 {
        Screen::Front(
            (c.x / c.w * 0.5 + 0.5) * size.0,
            (0.5 - c.y / c.w * 0.5) * size.1,
        )
    } else if c.x.abs() < 1e-4 && c.y.abs() < 1e-4 {
        // Straight behind the viewer: the bottom edge.
        Screen::Behind(0.0, 1.0)
    } else {
        let n = c.x.hypot(c.y);
        Screen::Behind(c.x / n, -c.y / n)
    }
}

/// A clamped point, the outward unit direction it was moved along and how far it was moved.
pub(super) type Clamped = ((f32, f32), (f32, f32), f32);

/// Pulls `pt` inside the screen shrunk by `pad` (left, right, top, bottom); `None` when it is already inside, else
/// the clamped point, the outward unit direction and how far outside it was.
pub(super) fn clamp_to_edges(
    pt: (f32, f32),
    size: (f32, f32),
    pad: [f32; 4],
) -> Option<Clamped> {
    let c = (
        pt.0.clamp(pad[0], (size.0 - pad[1]).max(pad[0])),
        pt.1.clamp(pad[2], (size.1 - pad[3]).max(pad[2])),
    );
    let d = (pt.0 - c.0, pt.1 - c.1);
    let dist = d.0.hypot(d.1);
    (dist > 0.0).then(|| (c, (d.0 / dist, d.1 / dist), dist))
}

/// Icons shrink with distance between these ranges (`waypointDistScale*`).
pub(super) fn distance_scale(dist: f32) -> f32 {
    const MIN: f32 = 1000.0;
    const MAX: f32 = 3000.0;
    const SMALLEST: f32 = 0.8;
    if dist <= MIN {
        1.0
    } else if dist >= MAX {
        SMALLEST
    } else {
        let t = (dist - MIN) / (MAX - MIN);
        t * SMALLEST + (1.0 - t)
    }
}

fn draw_waypoint(ui: &Ui, p: &mut Painter, live: &LiveUi, le: &LiveElem, color: [f32; 4]) {
    let (Some(clip), Some(pos)) = (live.clip.as_ref(), le.world) else {
        return;
    };
    let place = &ui.place;
    let size = place.size;
    let eye = glam::Vec3::from(live.eye);
    let dist = (glam::Vec3::from(pos) - eye).length();
    let screen = project(clip, pos, size);
    if le.offscreen.is_empty() {
        // A sprite in the world: scales with the view, never clamped.
        let Screen::Front(x, y) = screen else { return };
        let Some(img) = named(ui, p, &le.material) else {
            return;
        };
        let side = f32::from(le.e.height).max(1.0) * 0.0043 * size.1 * distance_scale(dist);
        p.pic(
            &img,
            Px {
                x: x - side * 0.5,
                y: y - side * 0.5,
                w: side,
                h: side,
            },
            color,
        );
        return;
    }
    let avg = (place.scale.0 + place.scale.1) * 0.5;
    let (icon_w, icon_h) = (36.0 * place.scale.0, 36.0 * place.scale.1);
    let (ptr_w, ptr_h) = (25.0 * place.scale.0, 12.0 * place.scale.1);
    let ptr_dist = 30.0 * avg;
    let padding = ptr_h * 0.5 + ptr_dist;
    let pad = [
        103.0 * place.scale.0 + padding,
        padding,
        padding,
        30.0 * place.scale.1 + padding,
    ];
    let tweak_y = -17.0 * place.scale.1;
    let pt = match screen {
        Screen::Front(x, y) => (x, y + tweak_y),
        Screen::Behind(dx, dy) => (
            size.0 * 0.5 + dx * (size.0 + size.1),
            size.1 * 0.5 + dy * (size.0 + size.1) + tweak_y,
        ),
    };
    let (mut at, mut scale) = (pt, distance_scale(dist));
    if let Some((c, n, out)) = clamp_to_edges(pt, size, pad) {
        at = c;
        let mut arrow = color;
        let fade_at = (30.0 * avg).max(0.1);
        if out < fade_at {
            arrow[3] *= out / fade_at;
        }
        // Icons shrink to nothing special near the edge in the stock settings: the smallest scale is 1.
        scale = 1.0;
        let ax = c.0 + n.0 * ptr_dist;
        let ay = c.1 + n.1 * ptr_dist;
        let angle = n.0.atan2(-n.1);
        let img = p.named(&ui.assets, "hud_offscreenobjectivepointer");
        p.g.quad_rot(
            &img,
            [ax - ptr_w * 0.5, ay - ptr_h * 0.5, ptr_w, ptr_h],
            [0.0, 0.0, 1.0, 1.0],
            arrow,
            angle,
            [ax, ay],
        );
    }
    let Some(img) = named(ui, p, &le.offscreen) else {
        return;
    };
    let (w, h) = (icon_w * scale, icon_h * scale);
    p.pic(
        &img,
        Px {
            x: at.0 - w * 0.5,
            y: at.1 - h * 0.5,
            w,
            h,
        },
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use render::View;

    fn view() -> glam::Mat4 {
        View {
            origin: glam::Vec3::new(100.0, 0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_x: 90f32.to_radians(),
            time: 0.0,
        }
        .clip_from_world(16.0 / 9.0)
    }

    #[test]
    fn a_point_ahead_is_in_the_middle_and_one_to_the_left_is_left() {
        let size = (1920.0, 1080.0);
        match project(&view(), [600.0, 0.0, 0.0], size) {
            Screen::Front(x, y) => {
                assert!(
                    (x - 960.0).abs() < 1.0 && (y - 540.0).abs() < 1.0,
                    "{x} {y}"
                );
            }
            s => panic!("{s:?}"),
        }
        // +y is to the left of a viewer facing +x: the point is left of centre, 45 degrees off at distance 500.
        match project(&view(), [600.0, 500.0, 0.0], size) {
            Screen::Front(x, _) => assert!(x < 1.0, "{x}"),
            s => panic!("{s:?}"),
        }
        match project(&view(), [600.0, -250.0, 0.0], size) {
            Screen::Front(x, _) => assert!(x > 960.0 && x < 1920.0, "{x}"),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn a_point_behind_the_viewer_goes_to_the_side_it_is_on() {
        let s = project(&view(), [-400.0, 0.0, 0.0], (1920.0, 1080.0));
        assert_eq!(s, Screen::Behind(0.0, 1.0), "straight behind: bottom edge");
        // Behind and to the right of the viewer (-y is right).
        match project(&view(), [-400.0, -300.0, 0.0], (1920.0, 1080.0)) {
            Screen::Behind(dx, _) => assert!(dx > 0.5, "{dx}"),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn clamping_keeps_icons_inside_the_padded_screen() {
        let size = (1000.0, 500.0);
        let pad = [100.0, 0.0, 0.0, 50.0];
        assert_eq!(clamp_to_edges((500.0, 200.0), size, pad), None);
        let (c, n, d) = clamp_to_edges((-300.0, 200.0), size, pad).unwrap();
        assert_eq!(c, (100.0, 200.0));
        assert_eq!(n, (-1.0, 0.0));
        assert_eq!(d, 400.0);
        let (c, n, _) = clamp_to_edges((500.0, 900.0), size, pad).unwrap();
        assert_eq!(c, (500.0, 450.0));
        assert_eq!(n, (0.0, 1.0));
    }

    #[test]
    fn distant_icons_shrink_between_the_ranges() {
        assert_eq!(distance_scale(500.0), 1.0);
        assert_eq!(distance_scale(5000.0), 0.8);
        assert!((distance_scale(2000.0) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn elements_are_placed_by_alignment_and_slide_while_moving() {
        let place = Place::new(1920, 1080);
        let mut e = HudElem::new(1);
        e.x = 100.0;
        e.y = 50.0;
        e.set_horz_align(horz::LEFT as u8);
        e.set_vert_align(vert::TOP as u8);
        let at = |e: &HudElem, now| position(&place, e, (40.0, 20.0), now);
        assert_eq!(
            at(&e, 0),
            (
                (100.0 * place.scale.0 + place.view_min.0).round(),
                (50.0 * place.scale.1).round()
            )
        );
        // Centred on x, bottom on y.
        e.set_align_x(1);
        e.set_align_y(2);
        let (x, y) = at(&e, 0);
        assert_eq!(x, (100.0 * place.scale.0 + place.view_min.0 - 20.0).round());
        assert_eq!(y, (50.0 * place.scale.1 - 20.0).round());
        // Moving from (0,50) to (100,50) over a second: halfway at 500 ms.
        e.set_align_x(0);
        e.set_align_y(0);
        e.from_x = 0.0;
        e.from_y = 50.0;
        e.from_align_org = e.align_org;
        e.from_align_screen = e.align_screen;
        e.move_start = 1000;
        e.move_time = 1000;
        let start = at(&e, 1000).0;
        let mid = at(&e, 1500).0;
        let end = at(&e, 2000).0;
        assert!((mid - (start + end) * 0.5).abs() < 1.0);
        assert!(start < mid && mid < end);
    }

    #[test]
    fn a_label_swallows_the_text_at_its_value_marker() {
        assert_eq!(consolidate("Time: &&1 left", "0:30"), "Time: 0:30 left");
        assert_eq!(consolidate("Score ", "5"), "Score 5");
    }

    #[test]
    fn pulse_text_types_in_and_its_glow_dies_down() {
        let mut e = HudElem::new(1);
        e.fx_birth = 1000;
        e.fx_letter = 50;
        e.fx_decay_start = 500;
        e.fx_decay_duration = 1000;
        let glow = Some([1.0, 1.0, 1.0, 1.0]);
        assert_eq!(pulse(&e, 1000, glow, 10).0, Some(1));
        assert_eq!(pulse(&e, 1200, glow, 10).0, Some(5));
        assert_eq!(pulse(&e, 5000, glow, 10).0, Some(10));
        assert_eq!(pulse(&e, 1400, glow, 10).1.unwrap()[3], 1.0);
        assert!((pulse(&e, 2000, glow, 10).1.unwrap()[3] - 0.5).abs() < 1e-6);
        assert_eq!(pulse(&e, 4000, glow, 10).1.unwrap()[3], 0.0);
        e.fx_birth = 0;
        assert_eq!(
            pulse(&e, 1200, glow, 10).0,
            None,
            "no pulse fx: all letters, glow as is"
        );
    }
}
