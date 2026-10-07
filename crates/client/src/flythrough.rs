// SPDX-License-Identifier: GPL-3.0-or-later
//! The scripted camera path of the flythrough scenario: a loop over the map at street-to-roof height, a pure
//! function of time so every machine and display mode sees the same tour.

use glam::Vec3;
use std::f32::consts::TAU;

/// Seconds for one lap.
const LAP: f32 = 48.0;

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub origin: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

/// Pose at `t` seconds for a world with the given bounds.
pub fn pose(t: f32, mins: Vec3, maxs: Vec3) -> Pose {
    let centre = (mins + maxs) * 0.5;
    let half = (maxs - mins) * 0.5;
    let a = TAU * t / LAP;
    let (rx, ry) = (half.x * 0.45, half.y * 0.45);
    let height = mins.z + (maxs.z - mins.z) * 0.22 + 40.0 * (a * 3.0).sin();
    let origin = Vec3::new(centre.x + rx * a.cos(), centre.y + ry * a.sin(), height);
    // Tangent of the ellipse, so the camera looks where it is going, with a slow sway.
    let (tx, ty) = (-rx * a.sin(), ry * a.cos());
    Pose {
        origin,
        yaw: ty.atan2(tx) + 0.35 * (a * 2.0).sin(),
        pitch: (-6.0f32).to_radians() + 0.1 * (a * 4.0).sin(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_is_deterministic_closed_and_inside_the_bounds() {
        let (mins, maxs) = (
            Vec3::new(-100.0, -200.0, 0.0),
            Vec3::new(300.0, 400.0, 500.0),
        );
        let p0 = pose(0.0, mins, maxs);
        let p1 = pose(LAP, mins, maxs);
        assert!((p0.origin - p1.origin).length() < 1e-2);
        for i in 0..200 {
            let p = pose(i as f32 * 0.5, mins, maxs);
            assert!(p.origin.cmpge(mins).all() && p.origin.cmple(maxs).all());
        }
        assert_eq!(pose(7.0, mins, maxs).origin, pose(7.0, mins, maxs).origin);
    }
}
