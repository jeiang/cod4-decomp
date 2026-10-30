// SPDX-License-Identifier: GPL-3.0-only
//! The frame rate counter of `cg_drawFPS` (`CG_DrawFPS`, `CG_CalculateFPS` and `CG_CornerDebugPrint` of the
//! original's `cg_draw_debug.cpp`): the last 32 frame times, printed at the top right of the window in the small
//! developer font while a match is on (`CL_DrawScreen` draws it under the console, above the HUD and the menus, so
//! `cg_draw2D` does not hide it).
//!
//! `cg_drawFPS` is the enum `Off`, `Simple`, `SimpleRanges`, `Verbose`. Every mode but `Off` prints the same two
//! lines (`CG_DrawFPS` does not look at the mode until its scene statistics): the rate with the slowest and fastest
//! frame as rates and the mean deviation of the frame times, `(min-max, var) fps`, then the shortest, longest and
//! average frame time, `(min-max) avg cg ms/frame`. From `SimpleRanges` (`cg_drawFPS >= 2`) on, a `-Scene-`
//! caption and the batches the last frame drew (` prim`) follow. The original's other lines (the server's frame
//! time of a listen server, free memory, triangle counts, texture memory) count what this engine does not keep. The
//! rate turns red below 55 and white above 65 frames per second (`Vec4Lerp(colorRed, colorWhite, (fps - 55) / 10)`).
//!
//! With `cg_drawFPSLabels` on, each value is right-aligned to the label column and its label follows; off, the value
//! alone sits at the right edge. `cg_debugInfoCornerOffset` moves the column from the top-right corner.

use crate::input::Cvars;
use crate::ui::Ui;
use crate::ui::paint::Painter;
use render::ui2d::{self, TextStyle};
use std::time::Instant;

/// Frames the original averages over.
const WINDOW: usize = 32;
/// The developer font and the scale `CG_DrawDevString` draws it at.
const FONT: &str = "fonts/smalldevfont";
const SCALE: f32 = 0.75;
/// The label whose width sets the label column (`R_TextWidth(" cg ms/frame")`).
const COLUMN_LABEL: &str = " cg ms/frame";
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
/// `colorWhiteFaded`, the labels' colour, and `colorGreenFaded`, the captions'.
const WHITE_FADED: [f32; 4] = [1.0, 1.0, 1.0, 0.75];
const GREEN_FADED: [f32; 4] = [0.0, 1.0, 0.0, 0.75];

/// `cg_drawFPS` as the enum the original registers it with (`cg_drawFpsNames`), by index or name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Simple,
    SimpleRanges,
    Verbose,
}

impl Mode {
    pub const NAMES: [&'static str; 4] = ["Off", "Simple", "SimpleRanges", "Verbose"];

    /// `Off`, `1`, `SimpleRanges`...; a value the enum has no entry for is `None`.
    pub fn parse(v: &str) -> Option<Mode> {
        let v = v.trim();
        let index = v
            .parse::<usize>()
            .ok()
            .or_else(|| Self::NAMES.iter().position(|n| n.eq_ignore_ascii_case(v)))?;
        [Mode::Off, Mode::Simple, Mode::SimpleRanges, Mode::Verbose]
            .get(index)
            .copied()
    }
}

/// What the dvars ask for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cfg {
    pub mode: Mode,
    pub labels: bool,
    /// `cg_debugInfoCornerOffset`: virtual units from the top-right corner (x to the right, y down).
    pub offset: [f32; 2],
}

impl Cfg {
    /// `cg_drawFPS` (an unparsable value draws nothing), `cg_drawFPSLabels` and `cg_debugInfoCornerOffset`.
    pub fn read(c: &Cvars) -> Cfg {
        let mut offset = [5.0, -5.0];
        if let Some(v) = c.get("cg_debugInfoCornerOffset") {
            let mut it = v.split_whitespace().map(|n| n.parse::<f32>());
            if let (Some(Ok(x)), Some(Ok(y))) = (it.next(), it.next())
                && x.is_finite()
                && y.is_finite()
            {
                offset = [x, y];
            }
        }
        Cfg {
            mode: c
                .get("cg_drawFPS")
                .and_then(Mode::parse)
                .unwrap_or(Mode::Off),
            labels: c.get("cg_drawFPSLabels").is_none_or(|v| v.trim() != "0"),
            offset,
        }
    }
}

/// The last 32 frame times in ms (`fps_previousTimes`), oldest overwritten first.
pub struct Meter {
    times: [u32; WINDOW],
    index: usize,
    last: Option<Instant>,
}

impl Default for Meter {
    fn default() -> Self {
        Meter {
            times: [0; WINDOW],
            index: 0,
            last: None,
        }
    }
}

/// The numbers one window of frame times yields (`CG_DrawFPS`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// Frames per second over the window, and the rates of the slowest and fastest frame.
    pub fps: i32,
    pub fps_min: i32,
    pub fps_max: i32,
    /// Mean absolute deviation of the frame times, ms.
    pub variance: i32,
    /// Shortest, longest and average frame time, ms.
    pub ms_min: u32,
    pub ms_max: u32,
    pub ms_avg: f32,
}

impl Meter {
    /// Marks a frame (`CG_CalculateFPS`, once per rendered frame).
    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last {
            self.times[self.index % WINDOW] = now.duration_since(last).as_millis() as u32;
            self.index += 1;
        }
        self.last = Some(now);
    }

    /// A frame time, as a test feeds it.
    #[cfg(test)]
    pub fn push(&mut self, ms: u32) {
        self.times[self.index % WINDOW] = ms;
        self.index += 1;
    }

    /// Nothing until the window is full, as the original waits for 32 frames.
    pub fn reading(&self) -> Option<Reading> {
        if self.index < WINDOW {
            return None;
        }
        Some(reading(&self.times))
    }
}

fn reading(times: &[u32; WINDOW]) -> Reading {
    let total: u32 = times.iter().sum();
    let ms_min = *times.iter().min().unwrap_or(&0);
    let ms_max = *times.iter().max().unwrap_or(&0);
    let average = total as f32 / WINDOW as f32;
    let variance = times
        .iter()
        .map(|&t| (t as f32 - average).abs())
        .sum::<f32>()
        / WINDOW as f32;
    let total = total.max(1);
    let min = ms_min.max(1);
    Reading {
        fps: (WINDOW as f32 * 1000.0 / total as f32).round() as i32,
        fps_min: (1000.0 / ms_max.max(1) as f32).round() as i32,
        fps_max: (1000.0 / min as f32).round() as i32,
        variance: variance.round() as i32,
        ms_min,
        ms_max,
        ms_avg: total as f32 / WINDOW as f32,
    }
}

/// The colour of the rate: red up to 55, white from 65, blended between.
fn rate_color(fps: i32) -> [f32; 4] {
    let t = ((fps - 55) as f32 / 10.0).clamp(0.0, 1.0);
    std::array::from_fn(|i| RED[i] + (WHITE[i] - RED[i]) * t)
}

/// What the last frame drew, for the `-Scene-` lines (`R_TrackStatistics`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scene {
    /// Batches drawn (`c_batches`, ` prim`): the scene's, the shadow maps' and the dynamic lights'.
    pub prims: usize,
}

/// One printed line: a value with its label (`CG_CornerDebugPrint`) or a caption (`CG_CornerDebugPrintCaption`).
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Value {
        value: String,
        label: &'static str,
        color: [f32; 4],
    },
    Caption(&'static str),
}

impl Line {
    fn value(value: String, label: &'static str, color: [f32; 4]) -> Line {
        Line::Value {
            value,
            label,
            color,
        }
    }

    /// The line as the run report shows it (value and label, or the caption).
    pub fn text(&self) -> String {
        match self {
            Line::Value { value, label, .. } => format!("{value}{label}"),
            Line::Caption(c) => (*c).to_owned(),
        }
    }
}

/// The lines a mode prints, in order (`CG_DrawFPS`): nothing for `Off`; the rate and frame-time lines for every
/// other mode; from `SimpleRanges` on (`cg_drawFPS >= 2`) the scene statistics too.
pub fn lines(mode: Mode, r: &Reading, scene: Scene) -> Vec<Line> {
    if mode == Mode::Off {
        return Vec::new();
    }
    let mut v = vec![
        Line::value(
            format!("({}-{}, {}) {}", r.fps_min, r.fps_max, r.variance, r.fps),
            " FPS",
            rate_color(r.fps),
        ),
        Line::value(
            format!("({}-{}) {:.2}", r.ms_min, r.ms_max, r.ms_avg),
            " cg ms/frame",
            WHITE,
        ),
    ];
    if mode != Mode::Simple {
        v.push(Line::Caption("-Scene-"));
        v.push(Line::value(scene.prims.to_string(), " prim", WHITE));
    }
    v
}

/// Draws the counter; the text of the first line drawn, for the run report, or `None` when nothing was.
pub fn draw(ui: &Ui, p: &mut Painter, meter: &Meter, cfg: &Cfg, scene: Scene) -> Option<String> {
    if cfg.mode == Mode::Off {
        return None;
    }
    let r = meter.reading()?;
    let rows = lines(cfg.mode, &r, scene);
    let font = ui.assets.fonts.get(FONT)?;
    let fimg = p.material_of_font(&ui.assets, font, false);
    let unit = ui.place.unit();
    // `CG_DrawDevString` scales the font by 0.75 of its own pixel height (not the menus' 48-pixel normalization
    // that `draw_text` takes), and `CL_DrawText` by the placement's virtual-to-real ratio on top; the metrics then
    // come out in pixels.
    let scale = SCALE * unit * font.pixel_height.max(1) as f32 / 48.0;
    let far_right = ui.place.size.0 + cfg.offset[0] * unit;
    let column = ui2d::text_width(font, COLUMN_LABEL, 0, scale);
    let step = ui2d::text_height(font, scale);
    // `CG_DrawDevString` with a bottom alignment: the baseline sits a line below `y`.
    let mut y = cfg.offset[1] * unit + step;
    let style = TextStyle::default();
    let x = if cfg.labels {
        far_right - column
    } else {
        far_right
    };
    let text = |p: &mut Painter, s: &str, x: f32, y: f32, color: [f32; 4]| {
        p.g.draw_text(
            font,
            &fimg,
            None,
            s,
            x.round(),
            y.round(),
            scale,
            color,
            style,
            0,
        );
    };
    for row in &rows {
        match row {
            Line::Value {
                value,
                label,
                color,
            } => {
                let w = ui2d::text_width(font, value, 0, scale);
                text(p, value, x - w, y, *color);
                if cfg.labels {
                    text(p, label, x, y, WHITE_FADED);
                }
            }
            // A caption is centred on the label column with labels on, right-aligned without.
            Line::Caption(c) => {
                let w = ui2d::text_width(font, c, 0, scale);
                let at = if cfg.labels { x - w / 2.0 } else { x - w };
                text(p, c, at, y, GREEN_FADED);
            }
        }
        y += step;
    }
    rows.first().map(Line::text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_parses_by_index_and_by_name() {
        assert_eq!(Mode::parse("0"), Some(Mode::Off));
        assert_eq!(Mode::parse("simpleranges"), Some(Mode::SimpleRanges));
        assert_eq!(Mode::parse(" Verbose "), Some(Mode::Verbose));
        assert_eq!(Mode::parse("3"), Some(Mode::Verbose));
        assert_eq!(Mode::parse("4"), None);
        assert_eq!(Mode::parse("fast"), None);
    }

    #[test]
    fn the_window_must_fill_before_a_reading_and_the_numbers_follow_the_original() {
        let mut m = Meter::default();
        for _ in 0..31 {
            m.push(10);
        }
        assert!(m.reading().is_none());
        m.push(20);
        let r = m.reading().unwrap();
        // 31 frames of 10 ms and one of 20: 330 ms for 32 frames.
        assert_eq!((r.fps, r.fps_min, r.fps_max), (97, 50, 100));
        assert_eq!((r.ms_min, r.ms_max), (10, 20));
        assert!((r.ms_avg - 10.3125).abs() < 1e-5);
        // Mean deviation: 31 * 0.3125 + 9.6875, over 32.
        assert_eq!(r.variance, 1);
    }

    /// `CG_DrawFPS`: every mode but `Off` prints the rate line and the frame-time line; `cg_drawFPS >= 2` adds the
    /// scene caption and its statistics.
    #[test]
    fn every_mode_prints_the_rate_and_frame_time_lines_and_2_up_add_the_scene() {
        let r = Reading {
            fps: 40,
            fps_min: 30,
            fps_max: 50,
            variance: 2,
            ms_min: 20,
            ms_max: 33,
            ms_avg: 25.0,
        };
        let scene = Scene { prims: 321 };
        assert!(lines(Mode::Off, &r, scene).is_empty());
        let simple = lines(Mode::Simple, &r, scene);
        assert_eq!(
            simple,
            [
                Line::value("(30-50, 2) 40".into(), " FPS", RED),
                Line::value("(20-33) 25.00".into(), " cg ms/frame", WHITE),
            ]
        );
        let texts = |v: &[Line]| v.iter().map(Line::text).collect::<Vec<_>>();
        let ranges = lines(Mode::SimpleRanges, &r, scene);
        assert_eq!(
            texts(&ranges),
            [
                "(30-50, 2) 40 FPS",
                "(20-33) 25.00 cg ms/frame",
                "-Scene-",
                "321 prim"
            ]
        );
        assert_eq!(ranges[2], Line::Caption("-Scene-"));
        assert_eq!(lines(Mode::Verbose, &r, scene), ranges);
        assert_eq!(rate_color(65), WHITE);
        assert_eq!(rate_color(60), [1.0, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn the_cfg_reads_the_dvars_with_the_stock_defaults() {
        let mut c = Cvars::default();
        assert_eq!(
            Cfg::read(&c),
            Cfg {
                mode: Mode::Off,
                labels: true,
                offset: [5.0, -5.0]
            }
        );
        c.set("cg_drawFPS", "Simple", false);
        c.set("cg_drawFPSLabels", "0", false);
        c.set("cg_debugInfoCornerOffset", "-20 10", false);
        assert_eq!(
            Cfg::read(&c),
            Cfg {
                mode: Mode::Simple,
                labels: false,
                offset: [-20.0, 10.0]
            }
        );
    }
}
