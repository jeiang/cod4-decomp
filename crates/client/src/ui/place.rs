// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual 640x480 coordinates to pixels (the original's `ScreenPlacement`).
//!
//! Menus and the HUD are laid out in a 640x480 virtual space whose rects carry an alignment per axis. One virtual
//! unit is `height / 480` pixels on both axes, so the 4:3 area stays square at any window shape (Hor+: a wider window
//! shows more to the sides, not a stretched picture). The UI anchors to a 16:9 safe area: `LEFT`/`RIGHT` edges and
//! the `fullscreen` rects of a 16:9 layout sit on that area's edges rather than the window's, so an ultra-wide window
//! keeps its menus in the middle and a window narrower than 16:9 uses the whole width.

/// Horizontal alignment of a rect (`horzAlign`).
pub mod horz {
    /// Left edge of the 16:9 safe area.
    pub const LEFT: i32 = 1;
    pub const CENTER: i32 = 2;
    /// Right edge of the safe area.
    pub const RIGHT: i32 = 3;
    /// Stretched over the whole window.
    pub const FULLSCREEN: i32 = 4;
    /// Pixels, unscaled.
    pub const NOSCALE: i32 = 5;
    /// Virtual 640 wide units scaled by the real to virtual ratio (pixel to 640 space).
    pub const TO640: i32 = 6;
    pub const CENTER_SAFEAREA: i32 = 7;
}

/// Vertical alignment of a rect (`vertAlign`).
pub mod vert {
    pub const TOP: i32 = 1;
    pub const CENTER: i32 = 2;
    pub const BOTTOM: i32 = 3;
    pub const FULLSCREEN: i32 = 4;
    pub const NOSCALE: i32 = 5;
    pub const TO480: i32 = 6;
    pub const CENTER_SAFEAREA: i32 = 7;
}

/// A rect in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Px {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Place {
    /// Window size in pixels.
    pub size: (f32, f32),
    /// Pixels per virtual unit, both axes.
    pub scale: (f32, f32),
    /// Pixels per virtual unit when 640x480 is stretched over the whole window.
    pub full: (f32, f32),
    /// Left edge of the centred 4:3 area.
    pub sub_left: f32,
    /// Safe-area edges in pixels.
    pub view_min: (f32, f32),
    pub view_max: (f32, f32),
}

impl Place {
    pub fn new(width: u32, height: u32) -> Self {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        // 4:3 at full height, but never wider than the window.
        let adjusted = (h * 4.0 / 3.0).min(w);
        let safe_w = w.min(h * 16.0 / 9.0);
        let safe_x = (w - safe_w) * 0.5;
        Place {
            size: (w, h),
            scale: (adjusted / 640.0, h / 480.0),
            full: (w / 640.0, h / 480.0),
            sub_left: (w - adjusted) * 0.5,
            view_min: (safe_x, 0.0),
            view_max: (safe_x + safe_w, h),
        }
    }

    /// Pixel size of one virtual unit along y (also what scales text).
    pub fn unit(&self) -> f32 {
        self.scale.1
    }

    pub fn x(&self, x: f32, align: i32) -> f32 {
        match align {
            horz::LEFT => x * self.scale.0 + self.view_min.0,
            horz::CENTER => x * self.scale.0 + self.size.0 * 0.5,
            horz::RIGHT => x * self.scale.0 + self.view_max.0,
            horz::FULLSCREEN => x * self.full.0,
            horz::NOSCALE => x,
            horz::TO640 => x * 640.0 / self.size.0 * self.scale.0,
            horz::CENTER_SAFEAREA => x * self.scale.0 + (self.view_min.0 + self.view_max.0) * 0.5,
            _ => x * self.scale.0 + self.sub_left,
        }
    }

    pub fn y(&self, y: f32, align: i32) -> f32 {
        match align {
            vert::TOP => y * self.scale.1 + self.view_min.1,
            vert::CENTER => y * self.scale.1 + self.size.1 * 0.5,
            vert::BOTTOM => y * self.scale.1 + self.view_max.1,
            vert::FULLSCREEN => y * self.full.1,
            vert::NOSCALE => y,
            vert::TO480 => y * 480.0 / self.size.1 * self.scale.1,
            vert::CENTER_SAFEAREA => y * self.scale.1 + (self.view_min.1 + self.view_max.1) * 0.5,
            _ => y * self.scale.1,
        }
    }

    fn w(&self, w: f32, align: i32) -> f32 {
        match align {
            horz::FULLSCREEN => w * self.full.0,
            horz::NOSCALE => w,
            horz::TO640 => w * 640.0 / self.size.0 * self.scale.0,
            _ => w * self.scale.0,
        }
    }

    fn h(&self, h: f32, align: i32) -> f32 {
        match align {
            vert::FULLSCREEN => h * self.full.1,
            vert::NOSCALE => h,
            vert::TO480 => h * 480.0 / self.size.1 * self.scale.1,
            _ => h * self.scale.1,
        }
    }

    /// A virtual rect to pixels.
    pub fn rect(&self, x: f32, y: f32, w: f32, h: f32, horz: i32, vert: i32) -> Px {
        Px {
            x: self.x(x, horz),
            y: self.y(y, vert),
            w: self.w(w, horz),
            h: self.h(h, vert),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_16_9_layout_fills_the_window_at_16_9_and_stays_centred_when_wider() {
        let p = Place::new(1920, 1080);
        // The stock full-screen backdrop: -107..747 of the 4:3 area is exactly the 16:9 window.
        let bg = p.rect(-106.67, 0.0, 853.33, 480.0, 0, 0);
        assert!((bg.x).abs() < 0.5 && (bg.w - 1920.0).abs() < 0.5, "{bg:?}");
        let wide = Place::new(3440, 1440);
        let left = wide.x(0.0, horz::LEFT);
        // The safe area is 16:9 of the full height, centred.
        assert!((left - (3440.0 - 1440.0 * 16.0 / 9.0) * 0.5).abs() < 0.5);
        // The window edge is still reachable with fullscreen alignment.
        assert_eq!(wide.x(640.0, horz::FULLSCREEN), 3440.0);
    }

    #[test]
    fn a_narrow_window_scales_to_its_width() {
        let p = Place::new(800, 800);
        assert!(
            (p.scale.0 * 640.0 - 800.0).abs() < 1e-3,
            "4:3 never wider than the window"
        );
    }
}
