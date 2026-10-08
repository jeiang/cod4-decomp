// SPDX-License-Identifier: GPL-3.0-or-later
//! The loading screen: the map's `loadscreen_<map>` picture over the whole window, its name and a progress bar.
//! Painted while a map loads in the background, so the window keeps presenting.

use super::Ui;
use super::paint::{Painter, TextDraw};
use super::place::{horz, vert};

/// What the loading screen shows.
pub struct LoadingView<'a> {
    /// The map's name (`mp_crash`).
    pub map: &'a str,
    /// What the loader is doing now.
    pub note: &'a str,
    /// 0 to 1.
    pub progress: f32,
}

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

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

    pub fn paint_loading(&self, p: &mut Painter, v: &LoadingView) {
        let pl = &self.place;
        let picture = p.named(&self.assets, &format!("loadscreen_{}", v.map));
        let full = pl.rect(0.0, 0.0, 640.0, 480.0, horz::FULLSCREEN, vert::FULLSCREEN);
        p.pic(&picture, full, WHITE);
        // Under the bar and the text, so both stay readable on a bright picture.
        let band = pl.rect(0.0, 392.0, 640.0, 88.0, horz::FULLSCREEN, vert::FULLSCREEN);
        p.fill(band, [0.0, 0.0, 0.0, 0.55]);
        let (w, h) = (360.0, 5.0);
        let track = pl.rect(-w * 0.5, 452.0, w, h, horz::CENTER, vert::TOP);
        p.fill(track, [1.0, 1.0, 1.0, 0.2]);
        let mut done = track;
        done.w *= v.progress.clamp(0.0, 1.0);
        p.fill(done, WHITE);
        let scale = 0.4;
        let line = |text: &str, y: f32, p: &mut Painter| {
            let width = self.text_width(text, 4, scale);
            self.draw_text(
                p,
                &TextDraw {
                    text,
                    font_enum: 4,
                    scale,
                    style: 3,
                    color: WHITE,
                    x: -width * 0.5,
                    y,
                    horz: horz::CENTER,
                    vert: vert::TOP,
                },
            );
        };
        line(&self.map_title(v.map), 424.0, p);
        line(v.note, 444.0, p);
    }
}
