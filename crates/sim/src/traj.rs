// SPDX-License-Identifier: GPL-3.0-or-later
//! Entity trajectories (`trajectory_t`): how scripted movers, missiles and falling items move
//! between server ticks. Shared with client interpolation.

use crate::Vec3;

/// Gravity of a `Gravity` trajectory, units per second squared (the original halves nothing:
/// the term is `400 * t^2`, an effective gravity of 800).
const TRAJECTORY_GRAVITY: f32 = 400.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrType {
    #[default]
    Stationary,
    Interpolate,
    Linear,
    /// Linear for `duration` ms, then holds.
    LinearStop,
    Sine,
    Gravity,
    /// Speeds up from rest to `delta` over `duration` ms.
    Accelerate,
    /// Slows from `delta` to rest over `duration` ms.
    Decelerate,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Trajectory {
    pub kind: TrType,
    /// Level time in ms the trajectory started.
    pub time: i32,
    /// Duration in ms (`LinearStop`, `Sine`, `Accelerate`, `Decelerate`).
    pub duration: i32,
    pub base: Vec3,
    /// Units per second.
    pub delta: Vec3,
}

fn mad(base: Vec3, s: f32, dir: Vec3) -> Vec3 {
    [
        base[0] + s * dir[0],
        base[1] + s * dir[1],
        base[2] + s * dir[2],
    ]
}

fn length(v: Vec3) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// `v / |v|`, or zero for a zero vector.
fn normalized(v: Vec3) -> Vec3 {
    let l = length(v);
    if l == 0.0 {
        [0.0; 3]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

impl Trajectory {
    pub fn stationary(base: Vec3) -> Self {
        Self {
            base,
            ..Self::default()
        }
    }

    /// `BG_EvaluateTrajectory`: the position at level time `at` (ms).
    pub fn evaluate(&self, at: i32) -> Vec3 {
        let t = |at: i32| (at - self.time) as f32 * 0.001;
        let end = self.time + self.duration;
        match self.kind {
            TrType::Stationary | TrType::Interpolate => self.base,
            TrType::Linear => mad(self.base, t(at), self.delta),
            TrType::LinearStop => mad(self.base, t(at.min(end)).max(0.0), self.delta),
            TrType::Sine => {
                let phase = (at - self.time) as f32 / self.duration as f32;
                mad(
                    self.base,
                    (phase * std::f32::consts::PI * 2.0).sin(),
                    self.delta,
                )
            }
            TrType::Gravity => {
                let dt = t(at);
                let mut r = mad(self.base, dt, self.delta);
                r[2] -= dt * TRAJECTORY_GRAVITY * dt;
                r
            }
            TrType::Accelerate => {
                let dt = t(at.min(end));
                let accel = length(self.delta) / (self.duration as f32 * 0.001);
                mad(self.base, accel * 0.5 * dt * dt, normalized(self.delta))
            }
            TrType::Decelerate => {
                let dt = t(at.min(end));
                let accel = length(self.delta) / (self.duration as f32 * 0.001);
                let at_speed = mad(self.base, dt, self.delta);
                mad(at_speed, -accel * 0.5 * dt * dt, normalized(self.delta))
            }
        }
    }

    /// `BG_EvaluateTrajectoryDelta`: the velocity in units per second at level time `at` (ms).
    pub fn evaluate_delta(&self, at: i32) -> Vec3 {
        let t = |at: i32| (at - self.time) as f32 * 0.001;
        let end = self.time + self.duration;
        match self.kind {
            TrType::Stationary | TrType::Interpolate => [0.0; 3],
            TrType::Linear => self.delta,
            TrType::LinearStop => {
                if at >= end {
                    [0.0; 3]
                } else {
                    self.delta
                }
            }
            TrType::Sine => {
                let phase = (at - self.time) as f32 / self.duration as f32;
                let rate = std::f32::consts::PI * 2.0 / (self.duration as f32 * 0.001);
                let k = (phase * std::f32::consts::PI * 2.0).cos() * rate;
                [self.delta[0] * k, self.delta[1] * k, self.delta[2] * k]
            }
            TrType::Gravity => {
                let mut v = self.delta;
                v[2] -= t(at) * TRAJECTORY_GRAVITY * 2.0;
                v
            }
            TrType::Accelerate => {
                let dt = t(at.min(end));
                let accel = length(self.delta) / (self.duration as f32 * 0.001);
                let dir = normalized(self.delta);
                let s = accel * dt;
                [dir[0] * s, dir[1] * s, dir[2] * s]
            }
            TrType::Decelerate => {
                let dt = t(at.min(end));
                let accel = length(self.delta) / (self.duration as f32 * 0.001);
                let dir = normalized(self.delta);
                let s = -accel * dt;
                mad(self.delta, 1.0, [dir[0] * s, dir[1] * s, dir[2] * s])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gravity_velocity_is_the_derivative_of_the_position() {
        let tr = Trajectory {
            kind: TrType::Gravity,
            time: 1000,
            base: [0.0; 3],
            delta: [100.0, 0.0, 50.0],
            ..Trajectory::default()
        };
        let (a, b) = (tr.evaluate(1500), tr.evaluate(1501));
        let v = tr.evaluate_delta(1500);
        assert!((v[0] - 100.0).abs() < 1e-3);
        assert!((v[2] - (50.0 - 800.0 * 0.5)).abs() < 1e-3);
        let numeric = (b[2] - a[2]) * 1000.0;
        assert!((numeric - v[2]).abs() < 1.5, "{numeric} vs {}", v[2]);
    }

    #[test]
    fn a_stopped_move_has_no_velocity() {
        let tr = Trajectory {
            kind: TrType::LinearStop,
            time: 0,
            duration: 500,
            delta: [10.0, 0.0, 0.0],
            ..Trajectory::default()
        };
        assert_eq!(tr.evaluate_delta(499), [10.0, 0.0, 0.0]);
        assert_eq!(tr.evaluate_delta(500), [0.0; 3]);
    }
}
