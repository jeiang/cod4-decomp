// SPDX-License-Identifier: GPL-3.0-or-later
//! Display modes: what the monitors offer, what the user asked for, and the window that results.

use serde_json::{Value, json};
use winit::dpi::{PhysicalSize, Size};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::{MonitorHandle, VideoModeHandle};
use winit::window::{Fullscreen, Window, WindowAttributes};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenKind {
    Windowed,
    Borderless,
    /// Falls back to borderless where the OS has no exclusive mode switch (Wayland, macOS Spaces).
    Exclusive,
}

impl FullscreenKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "windowed" => Self::Windowed,
            "borderless" => Self::Borderless,
            "exclusive" => Self::Exclusive,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless",
            Self::Exclusive => "exclusive",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub kind: FullscreenKind,
    /// `None`: the window default (windowed) or the monitor's native size.
    pub size: Option<(u32, u32)>,
    /// Hertz; `None` picks the highest the size offers.
    pub refresh: Option<f64>,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            kind: FullscreenKind::Windowed,
            size: Some((1280, 720)),
            refresh: None,
        }
    }
}

fn mode_json(m: &VideoModeHandle) -> Value {
    json!({
        "width": m.size().width,
        "height": m.size().height,
        "refresh_mhz": m.refresh_rate_millihertz(),
        "bit_depth": m.bit_depth(),
    })
}

pub fn monitors_json(el: &ActiveEventLoop) -> Value {
    let primary = el.primary_monitor();
    el.available_monitors()
        .map(|m| {
            let mut modes: Vec<VideoModeHandle> = m.video_modes().collect();
            modes.sort_by_key(|v| (v.size().width, v.size().height, v.refresh_rate_millihertz()));
            json!({
                "name": m.name(),
                "primary": primary.as_ref() == Some(&m),
                "native": {
                    "width": m.size().width,
                    "height": m.size().height,
                    "refresh_mhz": m.refresh_rate_millihertz(),
                },
                "scale_factor": m.scale_factor(),
                "modes": modes.iter().map(mode_json).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn monitor(el: &ActiveEventLoop) -> Option<MonitorHandle> {
    el.primary_monitor()
        .or_else(|| el.available_monitors().next())
}

/// The video mode closest to the request on `m`: the size asked for (else native), then the refresh nearest to the
/// one asked for (else the highest).
fn pick_mode(m: &MonitorHandle, r: &Request) -> Option<VideoModeHandle> {
    let want = r.size.unwrap_or((m.size().width, m.size().height));
    let mut modes: Vec<VideoModeHandle> = m
        .video_modes()
        .filter(|v| (v.size().width, v.size().height) == want)
        .collect();
    modes.sort_by(|a, b| {
        let key = |v: &VideoModeHandle| {
            let hz = f64::from(v.refresh_rate_millihertz()) / 1000.0;
            r.refresh.map_or(-hz, |want| (hz - want).abs())
        };
        key(a).total_cmp(&key(b))
    });
    modes.into_iter().next()
}

/// Create the window for `req`. Returns it with a note about any fallback taken.
pub fn create_window(
    el: &ActiveEventLoop,
    req: &Request,
) -> Result<(Window, Option<String>), String> {
    let mut attrs: WindowAttributes = Window::default_attributes().with_title("cod4e");
    let mon = monitor(el);
    let mut note = None;
    match req.kind {
        FullscreenKind::Windowed => {
            let (w, h) = req.size.unwrap_or((1280, 720));
            attrs = attrs.with_inner_size(Size::Physical(PhysicalSize::new(w, h)));
        }
        FullscreenKind::Borderless => {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(mon)))
        }
        FullscreenKind::Exclusive => {
            let mode = mon.as_ref().and_then(|m| pick_mode(m, req));
            attrs = match mode {
                Some(m) => attrs.with_fullscreen(Some(Fullscreen::Exclusive(m))),
                None => {
                    note = Some("no matching video mode; fell back to borderless".to_owned());
                    attrs.with_fullscreen(Some(Fullscreen::Borderless(mon)))
                }
            };
        }
    }
    let window = el.create_window(attrs).map_err(|e| e.to_string())?;
    Ok((window, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_names_round_trip() {
        for k in [
            FullscreenKind::Windowed,
            FullscreenKind::Borderless,
            FullscreenKind::Exclusive,
        ] {
            assert_eq!(FullscreenKind::parse(k.name()), Some(k));
        }
        assert_eq!(FullscreenKind::parse("nope"), None);
    }
}
