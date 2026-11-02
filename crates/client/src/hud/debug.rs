// SPDX-License-Identifier: GPL-3.0-only
//! A listen server's developer-only `line` and `print3d` drawings, over the world view of the hosting player
//! (`RB_DrawDebugLines`, `RB_DrawDebugStrings`).
//!
//! The original draws the lines in the 3D scene, depth tested when the script asked, and the strings as text laid
//! out in the world: each font pixel is `scale` units along the view's right and down axes, from the string's
//! origin. Here both are drawn flat over the frame from the projected points, so a line is never hidden behind the
//! world even with `depthtest` set (issue #515, warned once), and the text uses the default UI font with the
//! original's size per world unit.
//!
//! Translated from KisakCOD `gfx_d3d/rb_debug.cpp:158-226`.

use super::LiveUi;
use crate::ui::Ui;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::{horz, vert};
use server::debugdraw::DebugDraw;

/// The near plane in clip `w` a line is cut at before it is projected.
const NEAR_W: f32 = 0.5;

/// Projects a clip-space point that is in front of the viewer.
fn to_screen(c: glam::Vec4, size: (f32, f32)) -> [f32; 2] {
    [
        (c.x / c.w * 0.5 + 0.5) * size.0,
        (0.5 - c.y / c.w * 0.5) * size.1,
    ]
}

/// The segment between two world points on the screen, cut where it passes behind the viewer.
fn segment(
    clip: &glam::Mat4,
    a: [f32; 3],
    b: [f32; 3],
    size: (f32, f32),
) -> Option<([f32; 2], [f32; 2])> {
    let mut ca = *clip * glam::Vec4::new(a[0], a[1], a[2], 1.0);
    let mut cb = *clip * glam::Vec4::new(b[0], b[1], b[2], 1.0);
    if ca.w < NEAR_W && cb.w < NEAR_W {
        return None;
    }
    if ca.w < NEAR_W {
        ca = ca.lerp(cb, (NEAR_W - ca.w) / (cb.w - ca.w));
    } else if cb.w < NEAR_W {
        cb = cb.lerp(ca, (NEAR_W - cb.w) / (ca.w - cb.w));
    }
    Some((to_screen(ca, size), to_screen(cb, size)))
}

/// Draws the shown lines and strings; returns how many of each reached the screen (in front of the viewer, a line
/// at least half a pixel long), for the harness's `hud.stats.debug_lines`/`debug_strings`.
pub fn draw(ui: &Ui, p: &mut Painter, live: &LiveUi, d: &DebugDraw) -> (usize, usize) {
    let Some(clip) = live.clip.as_ref().filter(|_| live.active) else {
        return (0, 0);
    };
    if d.shown_lines.is_empty() && d.shown_strings.is_empty() {
        return (0, 0);
    }
    let (mut lines, mut strings) = (0, 0);
    let size = ui.place.size;
    let white = p.g.white();
    if d.shown_lines.iter().any(|l| l.depth_test) {
        logging::dedup!(
            WARN,
            "depthtest",
            issue = 515,
            "a script line() asked for a depth test; debug lines are drawn over the world"
        );
    }
    for l in &d.shown_lines {
        let Some((a, b)) = segment(clip, l.start, l.end, size) else {
            continue;
        };
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        if len < 0.5 {
            continue;
        }
        p.g.quad_rot(
            &white,
            [a[0], a[1] - 0.5, len, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            l.color,
            dy.atan2(dx),
            a,
        );
        lines += 1;
    }
    // Screen pixels per world unit at clip depth w: half the screen height times the vertical focal scale over w.
    let focal = glam::Vec3::new(clip.x_axis.y, clip.y_axis.y, clip.z_axis.y).length();
    let unit = ui.place.scale.0;
    for s in &d.shown_strings {
        let c = *clip * glam::Vec4::new(s.origin[0], s.origin[1], s.origin[2], 1.0);
        if c.w < NEAR_W {
            continue;
        }
        let [x, y] = to_screen(c, size);
        let px_per_unit = size.1 * 0.5 * focal / c.w;
        // A font pixel is `scale` world units: the text scale that makes one glyph pixel that many screen pixels.
        let scale = s.scale * px_per_unit / unit;
        let h = ui.text_height(0, scale) * unit;
        ui.draw_text(
            p,
            &TextDraw {
                text: &s.text,
                font_enum: 0,
                scale,
                style: 0,
                color: s.color,
                x: x.round(),
                y: (y + h).round(),
                horz: horz::NOSCALE,
                vert: vert::NOSCALE,
            },
        );
        strings += 1;
    }
    (lines, strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_through_the_viewer_is_cut_at_the_near_plane() {
        // Looking down +w: clip w is the world x here.
        let clip = glam::Mat4::from_cols(
            glam::Vec4::new(0.0, 0.0, 0.0, 1.0),
            glam::Vec4::new(-1.0, 0.0, 0.0, 0.0),
            glam::Vec4::new(0.0, 1.0, 0.0, 0.0),
            glam::Vec4::new(0.0, 0.0, 0.0, 0.0),
        );
        let size = (100.0, 100.0);
        assert!(segment(&clip, [-10.0, 0.0, 0.0], [-5.0, 1.0, 0.0], size).is_none());
        let (a, b) = segment(&clip, [-10.0, 0.0, 0.0], [10.0, 0.0, 0.0], size).unwrap();
        assert_eq!(a, [50.0, 50.0]);
        assert_eq!(b, [50.0, 50.0]);
        let (a, _) = segment(&clip, [10.0, -5.0, 0.0], [10.0, 5.0, 0.0], size).unwrap();
        assert!((a[0] - 75.0).abs() < 1e-3, "{a:?}");
    }
}
