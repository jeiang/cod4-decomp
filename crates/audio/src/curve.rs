// SPDX-License-Identifier: GPL-3.0-only
//! The volume falloff curve of an alias (`.vfcurve`): up to eight (distance fraction, volume) knots,
//! piecewise linear. Curves with fewer than two knots (80 aliases have none) fall back to a straight line
//! from full volume to silence.

use assets::zone::sound::SndCurve;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    knots: [[f32; 2]; 8],
    count: u8,
}

impl Curve {
    pub const LINEAR: Curve = Curve {
        knots: [
            [0.0, 1.0],
            [1.0, 0.0],
            [0.0; 2],
            [0.0; 2],
            [0.0; 2],
            [0.0; 2],
            [0.0; 2],
            [0.0; 2],
        ],
        count: 2,
    };

    pub fn from_knots(points: &[[f32; 2]]) -> Self {
        if points.len() < 2 || points.len() > 8 {
            return Self::LINEAR;
        }
        let mut knots = [[0.0; 2]; 8];
        knots[..points.len()].copy_from_slice(points);
        Self {
            knots,
            count: points.len() as u8,
        }
    }

    pub fn from_asset(c: &SndCurve) -> Self {
        let n = usize::try_from(c.knot_count).unwrap_or(0).min(8);
        Self::from_knots(&c.knots[..n])
    }

    /// The volume at `fraction` of the way from the minimum to the maximum distance.
    pub fn eval(&self, fraction: f32) -> f32 {
        let k = &self.knots[..usize::from(self.count)];
        let f = fraction.clamp(0.0, 1.0);
        for w in k.windows(2) {
            if w[1][0] >= f {
                let span = w[1][0] - w[0][0];
                let t = if span > 0.0 {
                    (f - w[0][0]) / span
                } else {
                    1.0
                };
                return w[0][1] + (w[1][1] - w[0][1]) * t.clamp(0.0, 1.0);
            }
        }
        k[k.len() - 1][1]
    }

    /// The gain at `distance` for an alias audible between `min` and `max` (units): full inside `min`, silent
    /// from `max` out, the curve between.
    pub fn attenuate(&self, distance: f32, min: f32, max: f32) -> f32 {
        let past = distance - min;
        if past <= 0.0 {
            1.0
        } else if max <= min || past >= max - min {
            0.0
        } else {
            self.eval(past / (max - min))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_between_knots_and_holds_the_ends() {
        let c = Curve::from_knots(&[[0.0, 1.0], [0.5, 0.5], [1.0, 0.0]]);
        assert_eq!(c.eval(0.0), 1.0);
        assert!((c.eval(0.25) - 0.75).abs() < 1e-6);
        assert_eq!(c.eval(0.5), 0.5);
        assert!((c.eval(0.75) - 0.25).abs() < 1e-6);
        assert_eq!(c.eval(1.0), 0.0);
        assert_eq!(c.eval(2.0), 0.0);
    }

    #[test]
    fn a_curve_without_knots_is_a_straight_fall() {
        let c = Curve::from_knots(&[]);
        assert!((c.eval(0.25) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn attenuation_is_full_inside_min_and_silent_at_max() {
        let c = Curve::LINEAR;
        assert_eq!(c.attenuate(10.0, 50.0, 250.0), 1.0);
        assert_eq!(c.attenuate(50.0, 50.0, 250.0), 1.0);
        assert!((c.attenuate(150.0, 50.0, 250.0) - 0.5).abs() < 1e-6);
        assert_eq!(c.attenuate(250.0, 50.0, 250.0), 0.0);
        assert_eq!(c.attenuate(1e6, 50.0, 250.0), 0.0);
    }
}
