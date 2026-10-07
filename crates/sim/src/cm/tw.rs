// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-trace working set (`traceWork_t`): everything derived once from the swept bounds.
//!
//! The original treats every swept hull as a capsule: the radius is the smaller of the half
//! width and half height, and the rest of the height is a vertical segment of `offset_z` either
//! side of the center. Brushes see it as a box of `radius_offset`, planes as a sphere pushed
//! along the segment.

use crate::Vec3;

use super::vec::sub;

/// Extra clearance every contact keeps (the original's `0.125` trace epsilon).
pub(super) const EPS: f32 = 0.125;

/// Plane-distance span below which a segment counts as parallel to the plane (`2^-21`).
pub(super) const PARALLEL_EPS: f32 = 1.0 / 2_097_152.0;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Tw {
    pub contents: i32,
    /// Hull center at the start and end of the sweep.
    pub start: Vec3,
    pub end: Vec3,
    /// `1 / (start - end)` per axis, zero where the sweep does not move.
    pub inv_delta: Vec3,
    pub delta: Vec3,
    pub delta_len: f32,
    pub delta_len_sq: f32,
    pub midpoint: Vec3,
    pub half_delta: Vec3,
    pub half_delta_abs: Vec3,
    /// Half extent of the hull.
    pub size: Vec3,
    /// World-space box the sweep can touch, grown by the radius.
    pub bounds: [Vec3; 2],
    pub is_point: bool,
    pub axial_cull_only: bool,
    pub radius: f32,
    pub offset_z: f32,
    pub radius_offset: Vec3,
    pub bounding_radius: f32,
}

impl Tw {
    pub fn new(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, contents: i32) -> Self {
        let mut tw = Self {
            contents,
            start: [0.0; 3],
            end: [0.0; 3],
            inv_delta: [0.0; 3],
            delta: [0.0; 3],
            delta_len: 0.0,
            delta_len_sq: 0.0,
            midpoint: [0.0; 3],
            half_delta: [0.0; 3],
            half_delta_abs: [0.0; 3],
            size: [0.0; 3],
            bounds: [[0.0; 3]; 2],
            is_point: false,
            axial_cull_only: false,
            radius: 0.0,
            offset_z: 0.0,
            radius_offset: [0.0; 3],
            bounding_radius: 0.0,
        };
        for i in 0..3 {
            let offset = (mins[i] + maxs[i]) * 0.5;
            tw.size[i] = maxs[i] - offset;
            tw.start[i] = start[i] + offset;
            tw.end[i] = end[i] + offset;
            tw.midpoint[i] = (tw.start[i] + tw.end[i]) * 0.5;
            tw.delta[i] = tw.end[i] - tw.start[i];
            tw.half_delta[i] = tw.delta[i] * 0.5;
            tw.half_delta_abs[i] = tw.half_delta[i].abs();
            let diff = tw.start[i] - tw.end[i];
            tw.inv_delta[i] = if diff == 0.0 { 0.0 } else { 1.0 / diff };
        }
        tw.delta_len_sq = super::vec::len_sq(tw.delta);
        tw.delta_len = tw.delta_len_sq.sqrt();
        tw.radius = tw.size[0].min(tw.size[2]);
        tw.bounding_radius = super::vec::len_sq(tw.size).sqrt();
        tw.offset_z = tw.size[2] - tw.radius;
        for i in 0..2 {
            let (lo, hi) = (tw.start[i].min(tw.end[i]), tw.start[i].max(tw.end[i]));
            tw.bounds[0][i] = lo - tw.radius;
            tw.bounds[1][i] = hi + tw.radius;
        }
        let reach = tw.offset_z + tw.radius;
        tw.bounds[0][2] = tw.start[2].min(tw.end[2]) - reach;
        tw.bounds[1][2] = tw.start[2].max(tw.end[2]) + reach;
        let total = sub(tw.bounds[1], tw.bounds[0]);
        let octant = tw.size[0] * tw.size[1] * tw.size[2];
        tw.axial_cull_only = total[0] * total[1] * total[2] < octant * 16.0 * tw.delta_len;
        tw.is_point = tw.size[0] + tw.size[1] + tw.size[2] == 0.0;
        tw.radius_offset = [tw.radius, tw.radius, tw.radius + tw.offset_z];
        tw
    }

    /// True when the swept center cannot touch the box `mins..maxs` (grown by the hull) before
    /// `fraction` (`CM_TraceBox`).
    pub fn misses_box(&self, mins: Vec3, maxs: Vec3, mut fraction: f32) -> bool {
        let mut enter = 0.0f32;
        let mut sign = -1.0f32;
        let mut bounds = mins;
        loop {
            for t in 0..3 {
                let d1 = (self.start[t] - bounds[t]) * sign;
                let d2 = (self.end[t] - bounds[t]) * sign;
                if d1 <= 0.0 {
                    if d2 > 0.0 {
                        let f = d1 * self.inv_delta[t] * sign;
                        if f <= enter {
                            return true;
                        }
                        if f < fraction {
                            fraction = f;
                        }
                    }
                } else {
                    if d2 > 0.0 {
                        return true;
                    }
                    let f = d1 * self.inv_delta[t] * sign;
                    if fraction <= f {
                        return true;
                    }
                    if enter < f {
                        enter = f;
                    }
                }
            }
            if sign == 1.0 {
                return false;
            }
            sign = 1.0;
            bounds = maxs;
        }
    }
}
