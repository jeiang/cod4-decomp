// SPDX-License-Identifier: GPL-3.0-or-later
//! Gamepads through `gilrs`: button edges for the bind table, stick positions for move and look.
//!
//! Failure to initialise is logged and the pad stays inert; there is no game state that depends on one.

use super::keys::pad_name;
use gilrs::{Axis, EventType, Gilrs};

pub struct Pad {
    gilrs: Option<Gilrs>,
    pub left: (f32, f32),
    pub right: (f32, f32),
    down: Vec<&'static str>,
}

/// Radial deadzone: inside `dz` the stick reads zero; outside, the magnitude is rescaled so it still reaches 1 at the
/// rim, keeping the direction. Magnitudes above 1 (square gates) are clamped.
pub fn radial_deadzone(x: f32, y: f32, dz: f32) -> (f32, f32) {
    let mag = x.hypot(y);
    let dz = dz.clamp(0.0, 0.99);
    if mag <= dz {
        return (0.0, 0.0);
    }
    let scaled = ((mag - dz) / (1.0 - dz)).min(1.0);
    (x / mag * scaled, y / mag * scaled)
}

impl Pad {
    /// No device support at all (tests, headless).
    pub fn none() -> Self {
        Self {
            gilrs: None,
            left: (0.0, 0.0),
            right: (0.0, 0.0),
            down: Vec::new(),
        }
    }

    pub fn new() -> Self {
        let mut p = Self::none();
        match Gilrs::new() {
            Ok(g) => p.gilrs = Some(g),
            Err(e) => eprintln!("gamepad support unavailable: {e}"),
        }
        p
    }

    /// Drain pending events: update the sticks, return `(button name, pressed)` edges.
    pub fn poll(&mut self) -> Vec<(&'static str, bool)> {
        let mut out = Vec::new();
        let Some(g) = self.gilrs.as_mut() else {
            return out;
        };
        while let Some(ev) = g.next_event() {
            match ev.event {
                EventType::ButtonPressed(b, _) => {
                    if let Some(n) = pad_name(b) {
                        self.down.push(n);
                        out.push((n, true));
                    }
                }
                EventType::ButtonReleased(b, _) => {
                    if let Some(n) = pad_name(b) {
                        self.down.retain(|d| *d != n);
                        out.push((n, false));
                    }
                }
                EventType::AxisChanged(a, v, _) => match a {
                    Axis::LeftStickX => self.left.0 = v,
                    Axis::LeftStickY => self.left.1 = v,
                    Axis::RightStickX => self.right.0 = v,
                    Axis::RightStickY => self.right.1 = v,
                    _ => {}
                },
                EventType::Disconnected => {
                    self.left = (0.0, 0.0);
                    self.right = (0.0, 0.0);
                    out.extend(self.down.drain(..).map(|n| (n, false)));
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_zeroes_inside_rescales_outside() {
        assert_eq!(radial_deadzone(0.1, 0.1, 0.2), (0.0, 0.0));
        // Straight right at magnitude 0.6, deadzone 0.2 -> (0.6-0.2)/0.8 = 0.5.
        let (x, y) = radial_deadzone(0.6, 0.0, 0.2);
        assert!((x - 0.5).abs() < 1e-6 && y == 0.0);
        // Full deflection stays 1, diagonals (|v| > 1 on a square gate) are clamped, direction kept.
        let (x, y) = radial_deadzone(1.0, 1.0, 0.2);
        assert!((x.hypot(y) - 1.0).abs() < 1e-6 && (x - y).abs() < 1e-6);
        // The radial rule does not flatten the minor axis the way per-axis deadzones do.
        let (x, y) = radial_deadzone(0.9, 0.15, 0.2);
        assert!(x > 0.0 && y > 0.0);
    }
}
