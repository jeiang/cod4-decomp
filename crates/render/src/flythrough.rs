// SPDX-License-Identifier: GPL-3.0-or-later

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

/// Horizontal field of view for a display of `aspect` (width / height) when `fov_4_3` degrees is the horizontal
/// field of view at 4:3: the vertical field is held, so wider displays see more to the sides (Hor+).
pub fn hor_plus(fov_4_3: f32, aspect: f32) -> f32 {
    let tan_x = (fov_4_3.to_radians() * 0.5).tan();
    2.0 * (tan_x * 0.75 * aspect).atan()
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

    #[test]
    fn hor_plus_keeps_the_four_three_field_and_widens_for_wide_displays() {
        let f = |a| hor_plus(80.0, a).to_degrees();
        assert!((f(4.0 / 3.0) - 80.0).abs() < 1e-3);
        assert!(f(16.0 / 9.0) > 95.0 && f(16.0 / 9.0) < 105.0);
        assert!(f(21.0 / 9.0) > f(16.0 / 9.0));
    }
}
