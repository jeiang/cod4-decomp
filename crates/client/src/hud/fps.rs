// SPDX-License-Identifier: GPL-3.0-only
//! The frame rate counter of `cg_drawFPS` (`CG_DrawFPS`, `CG_CalculateFPS` and `CG_CornerDebugPrint` of the
//! original's `cg_draw_debug.cpp`): the last 32 frame times, printed at the top right of the window in the small
//! developer font while a match is on (`CL_DrawScreen` draws it under the console, above the HUD and the menus, so
//! `cg_draw2D` does not hide it).
//!
//! `cg_drawFPS` is the enum `Off`, `Simple`, `SimpleRanges`, `Verbose`. The original computes the same numbers for
//! every mode (the average frame rate over the window, the slowest and fastest frame as rates, the mean deviation of
//! the frame times, the average and extreme frame times in ms); which lines it prints per mode follows the enum's
//! names: `Simple` is the rate alone, `SimpleRanges` adds the ranges, `Verbose` adds the frame-time line. The
//! original's `Verbose` also lists memory and scene statistics this engine does not keep. The rate turns red below
//! 55 and white above 65 frames per second (`Vec4Lerp(colorRed, colorWhite, (fps - 55) / 10)`).
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
/// `colorWhiteFaded`, the labels' colour.
const WHITE_FADED: [f32; 4] = [1.0, 1.0, 1.0, 0.75];

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

/// The lines a mode prints: value, label, the value's colour.
pub fn lines(mode: Mode, r: &Reading) -> Vec<(String, &'static str, [f32; 4])> {
    let color = rate_color(r.fps);
    match mode {
        Mode::Off => Vec::new(),
        Mode::Simple => vec![(r.fps.to_string(), " FPS", color)],
        Mode::SimpleRanges | Mode::Verbose => {
            let mut v = vec![(
                format!("({}-{}, {}) {}", r.fps_min, r.fps_max, r.variance, r.fps),
                " FPS",
                color,
            )];
            if mode == Mode::Verbose {
                v.push((
                    format!("({}-{}) {:.2}", r.ms_min, r.ms_max, r.ms_avg),
                    " cg ms/frame",
                    WHITE,
                ));
            }
            v
        }
    }
}

/// Draws the counter; the text of the first line drawn, for the run report, or `None` when nothing was.
pub fn draw(ui: &Ui, p: &mut Painter, meter: &Meter, cfg: &Cfg) -> Option<String> {
    if cfg.mode == Mode::Off {
        return None;
    }
    let r = meter.reading()?;
    let rows = lines(cfg.mode, &r);
    let font = ui.assets.fonts.get(FONT)?;
    let fimg = p.material_of_font(&ui.assets, font, false);
    let unit = ui.place.unit();
    // `CG_DrawDevString` scales the font by 0.75 of its own pixel height (not the menus' 48-pixel normalization
    // that `draw_text` takes), and one virtual unit per pixel on top; the metrics then come out in pixels.
    let scale = SCALE * unit * font.pixel_height.max(1) as f32 / 48.0;
    let far_right = ui.place.size.0 + cfg.offset[0] * unit;
    let column = ui2d::text_width(font, COLUMN_LABEL, 0, scale);
    let step = ui2d::text_height(font, scale);
    // `CG_DrawDevString` with a bottom alignment: the baseline sits a line below `y`.
    let mut y = cfg.offset[1] * unit + step;
    let style = TextStyle::default();
    let mut first = None;
    for (value, label, color) in &rows {
        let x = if cfg.labels {
            far_right - column
        } else {
            far_right
        };
        let w = ui2d::text_width(font, value, 0, scale);
        p.g.draw_text(
            font,
            &fimg,
            None,
            value,
            (x - w).round(),
            y.round(),
            scale,
            *color,
            style,
            0,
        );
        if cfg.labels {
            p.g.draw_text(
                font,
                &fimg,
                None,
                label,
                x.round(),
                y.round(),
                scale,
                WHITE_FADED,
                style,
                0,
            );
        }
        first.get_or_insert_with(|| format!("{value}{label}"));
        y += step;
    }
    first
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

    #[test]
    fn each_mode_prints_its_lines_and_a_slow_rate_is_red() {
        let r = Reading {
            fps: 40,
            fps_min: 30,
            fps_max: 50,
            variance: 2,
            ms_min: 20,
            ms_max: 33,
            ms_avg: 25.0,
        };
        assert!(lines(Mode::Off, &r).is_empty());
        let simple = lines(Mode::Simple, &r);
        assert_eq!(simple.len(), 1);
        assert_eq!((simple[0].0.as_str(), simple[0].1), ("40", " FPS"));
        assert_eq!(simple[0].2, RED);
        let ranges = lines(Mode::SimpleRanges, &r);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].0, "(30-50, 2) 40");
        let verbose = lines(Mode::Verbose, &r);
        assert_eq!(verbose.len(), 2);
        assert_eq!(
            (verbose[1].0.as_str(), verbose[1].1),
            ("(20-33) 25.00", " cg ms/frame")
        );
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
