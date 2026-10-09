// SPDX-License-Identifier: GPL-3.0-only
//! Display modes: what the monitors offer, what the user asked for, and the window that results.

#[cfg(not(target_arch = "wasm32"))]
use serde_json::{Value, json};
#[cfg(not(target_arch = "wasm32"))]
use winit::dpi::{PhysicalSize, Size};
use winit::event_loop::ActiveEventLoop;
#[cfg(not(target_arch = "wasm32"))]
use winit::monitor::{MonitorHandle, VideoModeHandle};
use winit::window::Window;
#[cfg(not(target_arch = "wasm32"))]
use winit::window::{Fullscreen, WindowAttributes};

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

/// Horizontal field of view for a display of `aspect` (width / height) when `fov_4_3` degrees is the horizontal
/// field of view at 4:3: the vertical field is held, so wider displays see more to the sides (Hor+).
pub fn hor_plus(fov_4_3: f32, aspect: f32) -> f32 {
    let tan_x = (fov_4_3.to_radians() * 0.5).tan();
    2.0 * (tan_x * 0.75 * aspect).atan()
}

/// The `fov_4_3`-degree field of view the world is drawn with (`CG_GetViewFov`). `fixed` is the intermission's or a
/// turret's own field; otherwise aiming a zoom weapon (`zoom` = its `adsZoomFov` and how far the zoom has come, 0 to 1)
/// moves from `cg_fov` to exactly the weapon's field, whatever `cg_fov` is. `scale` and `min` are `cg_fovScale` and
/// `cg_fovMin`.
pub fn view_fov(
    cg_fov: f32,
    fixed: Option<f32>,
    zoom: Option<(f32, f32)>,
    scale: f32,
    min: f32,
) -> f32 {
    let fov = fixed.unwrap_or(match zoom {
        Some((zoom_fov, k)) if zoom_fov > 0.0 => cg_fov - (cg_fov - zoom_fov) * k,
        _ => cg_fov,
    });
    (fov * scale).max(min)
}

/// `cgameGlob->zoomSensitivity`: how far the mouse turns the view per count relative to hip fire, the ratio of the
/// tangents of the half fields of view.
pub fn zoom_sensitivity(view_fov: f32, cg_fov: f32) -> f32 {
    (view_fov.to_radians() * 0.5).tan() / (cg_fov.to_radians() * 0.5).tan()
}

#[cfg(not(target_arch = "wasm32"))]
fn mode_json(m: &VideoModeHandle) -> Value {
    json!({
        "width": m.size().width,
        "height": m.size().height,
        "refresh_mhz": m.refresh_rate_millihertz(),
        "bit_depth": m.bit_depth(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
/// The sizes and refresh rates (Hz) the window's monitor offers, for the graphics menus.
pub fn video_modes(window: &Window) -> (Vec<(u32, u32)>, Vec<u32>) {
    let Some(m) = window
        .current_monitor()
        .or_else(|| window.primary_monitor())
    else {
        return (Vec::new(), Vec::new());
    };
    let modes: Vec<VideoModeHandle> = m.video_modes().collect();
    let sizes = modes.iter().map(|v| (v.size().width, v.size().height));
    let rates = modes
        .iter()
        .map(|v| (v.refresh_rate_millihertz() + 500) / 1000);
    (sizes.collect(), rates.collect())
}

/// A page has no video modes to offer.
#[cfg(target_arch = "wasm32")]
pub fn video_modes(_: &Window) -> (Vec<(u32, u32)>, Vec<u32>) {
    (Vec::new(), Vec::new())
}

#[cfg(not(target_arch = "wasm32"))]
fn monitor(el: &ActiveEventLoop) -> Option<MonitorHandle> {
    el.primary_monitor()
        .or_else(|| el.available_monitors().next())
}

/// The video mode closest to the request on `m`: the size asked for (else native), then the refresh nearest to the
/// one asked for (else the highest).
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
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

/// Moves a running window to what `req` asks: windowed at its size, or fullscreen on the monitor it is on. Returns a
/// note about any fallback taken. The window answers with a resize, which the surface follows.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply(window: &Window, req: &Request) -> Option<String> {
    let mon = window
        .current_monitor()
        .or_else(|| window.primary_monitor());
    match req.kind {
        FullscreenKind::Windowed => {
            window.set_fullscreen(None);
            let (w, h) = req.size.unwrap_or((1280, 720));
            let _ = window.request_inner_size(Size::Physical(PhysicalSize::new(w, h)));
            None
        }
        FullscreenKind::Borderless => {
            window.set_fullscreen(Some(Fullscreen::Borderless(mon)));
            None
        }
        FullscreenKind::Exclusive => match mon.as_ref().and_then(|m| pick_mode(m, req)) {
            Some(m) => {
                window.set_fullscreen(Some(Fullscreen::Exclusive(m)));
                None
            }
            None => {
                window.set_fullscreen(Some(Fullscreen::Borderless(mon)));
                Some("no matching video mode; fell back to borderless".to_owned())
            }
        },
    }
}

/// The page lays the canvas out; there is nothing to move.
#[cfg(target_arch = "wasm32")]
pub fn apply(_: &Window, _: &Request) -> Option<String> {
    None
}

/// Create the window: the page's canvas, which the page lays out, so the window is whatever size the canvas has.
#[cfg(target_arch = "wasm32")]
pub fn create_window(
    el: &ActiveEventLoop,
    _req: &Request,
) -> Result<(Window, Option<String>), String> {
    use winit::platform::web::WindowAttributesExtWebSys;
    let attrs = Window::default_attributes()
        .with_title("cod4e")
        .with_canvas(Some(page_canvas()?))
        .with_prevent_default(true);
    let window = el.create_window(attrs).map_err(|e| e.to_string())?;
    Ok((window, None))
}

/// The page's `<canvas id="cod4e-canvas">`.
#[cfg(target_arch = "wasm32")]
fn page_canvas() -> Result<web_sys::HtmlCanvasElement, String> {
    use wasm_bindgen::JsCast;
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("cod4e-canvas"))
        .and_then(|e| e.dyn_into().ok())
        .ok_or_else(|| "the page has no canvas".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aimed_view_fov_is_the_weapons_whatever_cg_fov_is() {
        for cg in [65.0, 80.0, 110.0] {
            assert_eq!(view_fov(cg, None, Some((20.0, 1.0)), 1.0, 10.0), 20.0);
            assert_eq!(view_fov(cg, None, Some((20.0, 0.0)), 1.0, 10.0), cg);
            assert_eq!(view_fov(cg, None, None, 1.0, 10.0), cg);
        }
        assert_eq!(view_fov(80.0, None, Some((20.0, 0.5)), 1.0, 10.0), 50.0);
        // A weapon with no zoom field leaves the field alone.
        assert_eq!(view_fov(80.0, None, Some((0.0, 1.0)), 1.0, 10.0), 80.0);
        // Turret and intermission fields win over aiming; the scale and the floor apply to all.
        assert_eq!(
            view_fov(80.0, Some(55.0), Some((20.0, 1.0)), 1.0, 10.0),
            55.0
        );
        assert_eq!(view_fov(80.0, None, None, 2.0, 10.0), 160.0);
        assert_eq!(view_fov(80.0, None, Some((4.0, 1.0)), 1.0, 10.0), 10.0);
    }

    #[test]
    fn zoom_sensitivity_is_the_tangent_ratio() {
        assert!((zoom_sensitivity(80.0, 80.0) - 1.0).abs() < 1e-6);
        let s = zoom_sensitivity(20.0, 80.0);
        assert!((s - 10f32.to_radians().tan() / 40f32.to_radians().tan()).abs() < 1e-6);
        assert!(s < 0.25);
    }

    #[test]
    fn hor_plus_keeps_the_four_three_field_and_widens_for_wide_displays() {
        let f = |a| hor_plus(80.0, a).to_degrees();
        assert!((f(4.0 / 3.0) - 80.0).abs() < 1e-3);
        assert!(f(16.0 / 9.0) > 95.0 && f(16.0 / 9.0) < 105.0);
        assert!(f(21.0 / 9.0) > f(16.0 / 9.0));
    }

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
