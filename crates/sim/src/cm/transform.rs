// SPDX-License-Identifier: GPL-3.0-only
//! Traces and point queries against a model placed in the world (an entity's `origin` and
//! `angles`).

use super::map::{ClipModel, CollisionWorld};
use super::vec::{angles_to_axis, rotate_in, rotate_out, sub};
use super::{Trace, Vec3};

/// The hull recentred on its midpoint: `(center, mins, maxs)`.
pub(super) fn symmetric(mins: Vec3, maxs: Vec3) -> (Vec3, Vec3, Vec3) {
    let c = [
        (mins[0] + maxs[0]) * 0.5,
        (mins[1] + maxs[1]) * 0.5,
        (mins[2] + maxs[2]) * 0.5,
    ];
    (c, sub(mins, c), sub(maxs, c))
}

impl CollisionWorld {
    /// `CM_TransformedBoxTrace`: sweeps the hull through `model` as if the model sat at
    /// `origin` rotated by `angles`. A trace that hits reports its normal in world space.
    #[allow(clippy::too_many_arguments)]
    pub fn transformed_trace(
        &self,
        trace: &mut Trace,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        model: &ClipModel,
        mask: i32,
        origin: Vec3,
        angles: Vec3,
    ) {
        let (center, lo, hi) = symmetric(mins, maxs);
        let mut start_l = sub(
            [
                start[0] + center[0],
                start[1] + center[1],
                start[2] + center[2],
            ],
            origin,
        );
        let mut end_l = sub(
            [end[0] + center[0], end[1] + center[1], end[2] + center[2]],
            origin,
        );
        if angles == [0.0; 3] {
            self.trace_model(trace, start_l, end_l, lo, hi, model, mask);
            return;
        }
        let axis = angles_to_axis(angles);
        start_l = rotate_in(&axis, start_l);
        end_l = rotate_in(&axis, end_l);
        let old = trace.fraction;
        self.trace_model(trace, start_l, end_l, lo, hi, model, mask);
        if old > trace.fraction {
            trace.normal = rotate_out(&axis, trace.normal);
        }
    }

    /// `CM_TransformedPointContents`.
    pub fn transformed_point_contents(
        &self,
        p: Vec3,
        model: &ClipModel,
        origin: Vec3,
        angles: Vec3,
    ) -> i32 {
        let mut local = sub(p, origin);
        if angles != [0.0; 3] {
            local = rotate_in(&angles_to_axis(angles), local);
        }
        self.point_contents(local, model)
    }
}
