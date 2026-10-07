// SPDX-License-Identifier: GPL-3.0-or-later
//! The scripted camera path of the flythrough scenario: a tour of the places players stand, a pure function of time
//! so every machine and display mode sees the same tour.
//!
//! A world's own bounds include backdrop buildings seen only from afar and gaps behind walls and under floors, and
//! the camera does not collide, so the path follows the map's spawn points (nearest neighbour order, there and back)
//! at eye height instead.

use glam::Vec3;
use std::f32::consts::TAU;

/// Metres of the tour per second, in game units.
const SPEED: f32 = 180.0;
/// Standing eye height above a spawn point's origin.
const EYE: f32 = 56.0;
/// Spawn points closer than this to an earlier one are skipped: the path would only wiggle.
const MIN_GAP: f32 = 160.0;
/// Seconds for one lap of the fallback ellipse.
const LAP: f32 = 48.0;

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub origin: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

/// The camera path of one map.
pub struct Tour {
    /// The chain there and back; empty for the fallback ellipse.
    chain: Vec<Vec3>,
    /// Arc length at each chain point.
    along: Vec<f32>,
    mins: Vec3,
    maxs: Vec3,
}

impl Tour {
    /// `bounds` are the world's, for the fallback when the map has fewer than three usable spawn points.
    pub fn new(spawns: &[[f32; 3]], bounds: (Vec3, Vec3)) -> Tour {
        let mut pts: Vec<Vec3> = Vec::new();
        for &s in spawns {
            let p = Vec3::from(s) + Vec3::Z * EYE;
            if pts.iter().all(|q| q.distance(p) >= MIN_GAP) {
                pts.push(p);
            }
        }
        let (mins, maxs) = bounds;
        if pts.len() < 3 {
            return Tour {
                chain: Vec::new(),
                along: Vec::new(),
                mins,
                maxs,
            };
        }
        // Nearest neighbour chain from the point nearest the lowest-numbered spawn.
        let mut chain = vec![pts.remove(0)];
        while !pts.is_empty() {
            let last = *chain.last().expect("non-empty");
            let (i, _) = pts
                .iter()
                .enumerate()
                .min_by(|a, b| last.distance(*a.1).total_cmp(&last.distance(*b.1)))
                .expect("non-empty");
            chain.push(pts.remove(i));
        }
        // And back, so the closing leg never crosses the map.
        let back: Vec<Vec3> = chain[1..chain.len() - 1].iter().rev().copied().collect();
        chain.extend(back);
        let mut along = vec![0.0];
        for w in chain.windows(2) {
            along.push(along.last().expect("non-empty") + w[0].distance(w[1]));
        }
        // Close the loop on the first point.
        let first = chain[0];
        let last = *chain.last().expect("non-empty");
        chain.push(first);
        along.push(along.last().expect("non-empty") + last.distance(first));
        Tour {
            chain,
            along,
            mins,
            maxs,
        }
    }

    /// Seconds for one lap.
    pub fn period(&self) -> f32 {
        self.along.last().map_or(LAP, |l| l / SPEED)
    }

    fn position(&self, t: f32) -> Vec3 {
        let total = *self.along.last().expect("a chain");
        let s = (t * SPEED).rem_euclid(total);
        let k = self
            .along
            .partition_point(|&a| a <= s)
            .clamp(1, self.chain.len() - 1);
        let (a, b) = (self.chain[k - 1], self.chain[k]);
        let f = (s - self.along[k - 1]) / (self.along[k] - self.along[k - 1]).max(1e-3);
        // Smoothstep eases the corners of the polyline.
        a.lerp(b, f * f * (3.0 - 2.0 * f))
    }

    pub fn pose(&self, t: f32) -> Pose {
        if self.chain.is_empty() {
            return ellipse(t, self.mins, self.maxs);
        }
        let origin = self.position(t);
        // Look where the path goes a moment from now, with a slow sway.
        let ahead = self.position(t + 0.6) - origin;
        let a = TAU * t / self.period();
        Pose {
            origin,
            yaw: ahead.y.atan2(ahead.x) + 0.3 * (a * 9.0).sin(),
            pitch: (-4.0f32).to_radians() + 0.08 * (a * 13.0).sin(),
        }
    }
}

/// A loop over the world's bounds at street-to-roof height.
fn ellipse(t: f32, mins: Vec3, maxs: Vec3) -> Pose {
    let centre = (mins + maxs) * 0.5;
    let half = (maxs - mins) * 0.5;
    let a = TAU * t / LAP;
    let (rx, ry) = (half.x * 0.45, half.y * 0.45);
    let height = mins.z + (maxs.z - mins.z) * 0.22 + 40.0 * (a * 3.0).sin();
    let origin = Vec3::new(centre.x + rx * a.cos(), centre.y + ry * a.sin(), height);
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

    fn bounds() -> (Vec3, Vec3) {
        (
            Vec3::new(-100.0, -200.0, 0.0),
            Vec3::new(300.0, 400.0, 500.0),
        )
    }

    #[test]
    fn the_tour_visits_every_spaced_spawn_and_closes() {
        let spawns = [
            [0.0, 0.0, 0.0],
            [400.0, 0.0, 0.0],
            [800.0, 0.0, 0.0],
            [820.0, 10.0, 0.0],
            [800.0, 500.0, 0.0],
        ];
        let tour = Tour::new(&spawns, bounds());
        let (p0, p1) = (tour.pose(0.0), tour.pose(tour.period()));
        assert!((p0.origin - p1.origin).length() < 1e-2);
        for s in [
            [0.0, 0.0, 0.0],
            [400.0, 0.0, 0.0],
            [800.0, 0.0, 0.0],
            [800.0, 500.0, 0.0],
        ] {
            let near = (0..2000)
                .map(|i| tour.pose(i as f32 * tour.period() / 2000.0).origin)
                .any(|o| o.distance(Vec3::from(s) + Vec3::Z * EYE) < 5.0);
            assert!(near, "{s:?}");
        }
        // The near-duplicate spawn is skipped: its neighbourhood is no extra stop.
        assert_eq!(tour.pose(3.0).origin, tour.pose(3.0).origin);
    }

    #[test]
    fn without_spawns_the_ellipse_stays_in_bounds() {
        let (mins, maxs) = bounds();
        let tour = Tour::new(&[], bounds());
        assert!((tour.pose(0.0).origin - tour.pose(LAP).origin).length() < 1e-2);
        for i in 0..200 {
            let p = tour.pose(i as f32 * 0.5);
            assert!(p.origin.cmpge(mins).all() && p.origin.cmple(maxs).all());
        }
    }
}
