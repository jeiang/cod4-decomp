// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD `cgame/cg_camerashake.cpp` (GPL-3.0; LWSS and the KisakCOD contributors).
//! Camera shake (`earthquake` in scripts): up to four shakes at a time, each strongest at its source and fading with
//! distance and with time. The strongest one the view feels sways the view angles.

use crate::Vec3;

/// Shakes that run at once; a new one beyond that replaces a weaker one or is dropped.
pub const MAX_SHAKES: usize = 4;
/// Peak sway (degrees) of pitch, yaw and roll at full strength, and its angular speed (radians per 600 ms).
const SWAY: [(f32, f32); 3] = [(18.0, 25.132_742), (16.0, 47.123_89), (10.0, 37.699_112)];
/// Full strength is capped here.
const MAX_SCALE: f32 = 1.0;

#[derive(Clone, Copy, Default)]
struct Shake {
    scale: f32,
    /// Milliseconds.
    length: f32,
    radius: f32,
    /// When it started, 0 length if the slot is free.
    time: i32,
    src: Vec3,
    /// How hard the view feels it now, and the time fade alone.
    size: f32,
    rumble: f32,
}

impl Shake {
    /// Works out how hard the view at `eye` feels the shake at `now`; false once it is over or not yet begun.
    fn update(&mut self, now: i32, eye: Vec3) -> bool {
        let dt = now.wrapping_sub(self.time);
        if dt < 0 || self.length <= dt as f32 {
            return false;
        }
        let dist = crate::pm::math::length(&crate::pm::math::sub(&eye, &self.src));
        let near = 1.0 - dist / self.radius;
        let x = (1.0 - dt as f32 / self.length) * self.scale;
        if x <= 0.0 {
            return false;
        }
        // Outside the radius the "strength" is negative, so it never wins.
        self.size = if near < 0.0 { near / x } else { near * x };
        self.rumble = x;
        true
    }

    fn live(&self, now: i32) -> bool {
        self.time <= now && (now as f32) < self.time as f32 + self.length
    }
}

#[derive(Default)]
pub struct CameraShakes {
    shakes: [Shake; MAX_SHAKES],
    phase: f32,
}

impl CameraShakes {
    /// An earthquake of `scale` lasting `duration_ms` from `src`, felt out to `radius`, for a view at `eye` at `now`.
    pub fn start(
        &mut self,
        now: i32,
        eye: Vec3,
        scale: f32,
        duration_ms: i32,
        src: Vec3,
        radius: f32,
    ) {
        if scale <= 0.0 || duration_ms <= 0 || radius <= 0.0 {
            return;
        }
        let mut new = Shake {
            scale,
            length: duration_ms as f32,
            radius,
            time: now,
            src,
            size: 0.0,
            rumble: 0.0,
        };
        new.update(now, eye);
        let slot = self.shakes.iter().position(|s| !s.live(now)).or_else(|| {
            // Every slot is busy: replace the weakest, if it is weaker than the new one.
            let (i, weakest) = self
                .shakes
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.size.total_cmp(&b.1.size))?;
            (new.size > weakest.size).then_some(i)
        });
        if let Some(i) = slot {
            if !self.shakes.iter().any(|s| s.live(now)) {
                // A new phase for each run of shaking, so two quakes do not sway the same way.
                self.phase = (now as f32 * 0.001).sin() * std::f32::consts::PI;
            }
            self.shakes[i] = new;
        }
    }

    /// How far the shaking at `now` turns a view at `eye`: pitch, yaw and roll in degrees, all 0 when nothing shakes
    /// it. Call once per frame.
    pub fn sway(&mut self, now: i32, eye: Vec3) -> Vec3 {
        let mut strength = 0.0f32;
        let mut rumble = 0.0;
        for s in &mut self.shakes {
            if s.update(now, eye) && strength < s.size {
                strength = s.size;
                rumble = s.rumble;
            }
        }
        if strength <= 0.0 {
            return [0.0; 3];
        }
        let strength = strength.min(MAX_SCALE);
        let t = now as f32 / 600.0;
        SWAY.map(|(deg, speed)| (self.phase + t * speed).sin() * rumble * deg * strength)
    }

    /// Ends every shake (a new map).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The strongest shake the view at `eye` feels at `now`, 0 to 1.
    pub fn strength(&mut self, now: i32, eye: Vec3) -> f32 {
        self.shakes
            .iter_mut()
            .filter_map(|s| s.update(now, eye).then_some(s.size))
            .fold(0.0, f32::max)
            .min(MAX_SCALE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(s: &mut CameraShakes, eye: Vec3, from: i32, to: i32) -> f32 {
        (from..to)
            .step_by(7)
            .map(|t| s.sway(t, eye).iter().fold(0.0f32, |m, a| m.max(a.abs())))
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_quake_shakes_the_near_view_and_not_the_far_one() {
        let (near, far) = ([100.0, 0.0, 0.0], [5000.0, 0.0, 0.0]);
        let mut a = CameraShakes::default();
        a.start(1000, near, 0.6, 2000, [0.0; 3], 1000.0);
        assert!(peak(&mut a, near, 1000, 3000) > 1.0);
        let mut b = CameraShakes::default();
        b.start(1000, far, 0.6, 2000, [0.0; 3], 1000.0);
        assert_eq!(peak(&mut b, far, 1000, 3000), 0.0);
    }

    #[test]
    fn the_shake_fades_with_distance_and_ends_with_its_duration() {
        let at = |d: f32| {
            let mut s = CameraShakes::default();
            s.start(0, [d, 0.0, 0.0], 1.0, 2000, [0.0; 3], 1000.0);
            s.strength(100, [d, 0.0, 0.0])
        };
        assert!(at(100.0) > at(500.0) && at(500.0) > at(900.0) && at(900.0) > 0.0);
        assert_eq!(at(1000.0), 0.0);

        let mut s = CameraShakes::default();
        s.start(0, [0.0; 3], 1.0, 2000, [0.0; 3], 1000.0);
        let early = s.strength(100, [0.0; 3]);
        assert!(early > s.strength(1500, [0.0; 3]));
        assert_eq!(s.sway(2000, [0.0; 3]), [0.0; 3]);
    }

    #[test]
    fn a_full_set_keeps_the_strongest_shakes() {
        let mut s = CameraShakes::default();
        for k in 0..MAX_SHAKES {
            s.start(
                0,
                [0.0; 3],
                0.2 + 0.1 * k as f32,
                5000,
                [800.0, 0.0, 0.0],
                1000.0,
            );
        }
        // A weaker quake than all four is dropped; a stronger one takes the weakest slot.
        let before = s.strength(10, [0.0; 3]);
        s.start(10, [0.0; 3], 0.05, 5000, [0.0; 3], 1000.0);
        assert_eq!(s.strength(10, [0.0; 3]), before.max(0.05 * 0.999));
        s.start(10, [0.0; 3], 1.0, 5000, [0.0; 3], 1000.0);
        assert!(s.strength(10, [0.0; 3]) > 0.9);
    }
}
