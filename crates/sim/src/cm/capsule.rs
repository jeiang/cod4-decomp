// SPDX-License-Identifier: GPL-3.0-or-later
//! Capsule against capsule: how entities without a brush model block a moving hull. The
//! stationary capsule is the entity's bounds read as a capsule (radius = the smaller of half
//! width and half height, the rest of the height a vertical segment).

use super::Trace;
use super::tw::{EPS, Tw};
use super::vec::{add, dot, len_sq, normalize, sub};
use crate::Vec3;

/// The stationary capsule.
#[derive(Clone, Copy)]
pub(super) struct Cap {
    pub contents: i32,
    pub mins: Vec3,
    pub maxs: Vec3,
    center: Vec3,
    radius: f32,
    /// Half length of the vertical segment.
    offs: f32,
}

impl Cap {
    pub fn new(mins: Vec3, maxs: Vec3, contents: i32) -> Self {
        let center = [
            (mins[0] + maxs[0]) * 0.5,
            (mins[1] + maxs[1]) * 0.5,
            (mins[2] + maxs[2]) * 0.5,
        ];
        let half_w = maxs[0] - center[0];
        let half_h = maxs[2] - center[2];
        let radius = half_w.min(half_h);
        Self {
            contents,
            mins,
            maxs,
            center,
            radius,
            offs: half_h - radius,
        }
    }

    fn top(&self) -> Vec3 {
        add(self.center, [0.0, 0.0, self.offs])
    }

    fn bottom(&self) -> Vec3 {
        sub(self.center, [0.0, 0.0, self.offs])
    }

    /// Whether the sweep's bounds reach this capsule's bounds (plus one unit).
    fn in_reach(&self, tw: &Tw) -> bool {
        (0..3)
            .all(|i| tw.bounds[0][i] <= self.maxs[i] + 1.0 && tw.bounds[1][i] >= self.mins[i] - 1.0)
    }

    /// Stationary hull overlap test (`CM_TestCapsuleInCapsule`).
    pub fn test(&self, tw: &Tw, trace: &mut Trace) {
        let top = add(tw.start, [0.0, 0.0, tw.offset_z]);
        let bottom = sub(tw.start, [0.0, 0.0, tw.offset_z]);
        let r = (tw.radius + self.radius) * (tw.radius + self.radius);
        let (p1, p2) = (self.top(), self.bottom());
        let solid = r > len_sq(sub(p1, top))
            || r > len_sq(sub(p1, bottom))
            || r > len_sq(sub(p2, top))
            || r > len_sq(sub(p2, bottom))
            || {
                let height_diff = tw.start[2] - self.center[2];
                let total_half = self.offs + tw.size[2] - tw.radius;
                total_half >= height_diff.abs() && {
                    let (mut a, mut b) = (top, p1);
                    a[2] = 0.0;
                    b[2] = 0.0;
                    r > len_sq(sub(a, b))
                }
            };
        if solid {
            trace.start_solid = true;
            trace.all_solid = true;
            trace.fraction = 0.0;
            trace.surface_flags = 0;
        }
    }

    /// Moving hull against this capsule (`CM_TraceCapsuleThroughCapsule`).
    pub fn trace(&self, tw: &Tw, trace: &mut Trace) {
        if !self.in_reach(tw) {
            return;
        }
        let z = [0.0, 0.0, tw.offset_z];
        let (start_top, start_bottom) = (add(tw.start, z), sub(tw.start, z));
        let (end_top, end_bottom) = (add(tw.end, z), sub(tw.end, z));
        let (top, bottom) = (self.top(), self.bottom());
        if top[2] >= start_bottom[2] {
            if bottom[2] > start_top[2]
                && (!self.sphere(tw, start_top, end_top, bottom, trace) || tw.delta[2] <= 0.0)
            {
                return;
            }
        } else if !self.sphere(tw, start_bottom, end_bottom, top, trace) || tw.delta[2] >= 0.0 {
            return;
        }
        if self.cylinder(tw, trace) {
            if top[2] >= end_bottom[2] {
                if bottom[2] > end_top[2] && bottom[2] <= start_top[2] {
                    self.sphere(tw, start_top, end_top, bottom, trace);
                }
            } else if top[2] >= start_bottom[2] {
                self.sphere(tw, start_bottom, end_bottom, top, trace);
            }
        }
    }

    /// Sight version of [`trace`](Self::trace): true when the capsule blocks the line.
    pub fn blocks(&self, tw: &Tw) -> bool {
        let mut t = Trace::MISS;
        self.trace(tw, &mut t);
        t.fraction < 1.0 || t.start_solid
    }

    /// Sphere (the swept hull's end cap) against a stationary sphere. Returns true for a miss;
    /// a hit (or start inside) is written to `trace`.
    fn sphere(&self, tw: &Tw, from: Vec3, to: Vec3, fixed: Vec3, trace: &mut Trace) -> bool {
        let delta = sub(from, fixed);
        let reach = (self.radius + tw.radius) * (self.radius + tw.radius);
        let c = dot(delta, delta) - reach;
        if c <= 0.0 {
            trace.fraction = 0.0;
            trace.start_solid = true;
            trace.walkable = false;
            trace.normal = normalize(delta).0;
            trace.contents = self.contents;
            trace.surface_flags = 0;
            if reach >= len_sq(sub(to, fixed)) {
                trace.all_solid = true;
            }
            return false;
        }
        let b = dot(tw.delta, delta);
        if b >= 0.0 {
            return true;
        }
        let a = tw.delta_len_sq;
        let disc = b * b - a * c;
        if disc < 0.0 {
            return true;
        }
        let (normal, len) = normalize(delta);
        let root = disc.sqrt();
        let entry = (-b - root) / a + len * EPS / b;
        if trace.fraction <= entry {
            return true;
        }
        trace.fraction = entry.max(0.0);
        trace.normal = normal;
        trace.contents = self.contents;
        trace.walkable = false;
        trace.surface_flags = 0;
        false
    }

    /// The swept hull's side wall against the stationary capsule's cylinder. Returns true for a
    /// miss.
    fn cylinder(&self, tw: &Tw, trace: &mut Trace) -> bool {
        let mut delta = sub(tw.start, self.center);
        let reach = (self.radius + tw.radius) * (self.radius + tw.radius);
        let total_height = tw.size[2] - tw.radius + self.offs;
        let c = delta[1] * delta[1] + delta[0] * delta[0] - reach;
        if c <= 0.0 {
            if total_height < delta[2].abs() {
                return true;
            }
            trace.fraction = 0.0;
            trace.start_solid = true;
            trace.walkable = false;
            delta[2] = 0.0;
            trace.normal = normalize(delta).0;
            trace.contents = self.contents;
            trace.surface_flags = 0;
            if total_height >= (tw.end[2] - self.center[2]).abs() {
                trace.all_solid = true;
            }
            return false;
        }
        let b = delta[1] * tw.delta[1] + delta[0] * tw.delta[0];
        if b >= 0.0 {
            return true;
        }
        let a = tw.delta[1] * tw.delta[1] + tw.delta[0] * tw.delta[0];
        let disc = b * b - a * c;
        if disc < 0.0 {
            return true;
        }
        delta[2] = 0.0;
        let (normal, len) = normalize(delta);
        let eps = len * EPS / b;
        let entry = (-b - disc.sqrt()) / a + eps;
        if trace.fraction <= entry {
            return true;
        }
        let hit_height = (entry - eps) * tw.delta[2] + tw.start[2] - self.center[2];
        if total_height < hit_height.abs() {
            return true;
        }
        trace.fraction = entry.max(0.0);
        trace.normal = normal;
        trace.contents = self.contents;
        trace.surface_flags = 0;
        trace.walkable = false;
        false
    }
}
