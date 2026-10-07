// SPDX-License-Identifier: GPL-3.0-or-later
//! Visibility traces (`CM_BoxSightTrace`): a yes/no "is anything solid in the way" with the
//! original's own rules. Unlike a normal trace there is no contact epsilon (touching a brush
//! does not block) and the answer is a hit number the caller can pass back as a cache hint.
//!
//! Hit numbers: brush `i` is `i + 1`; terrain collision tree `t` is `brushes + t + 1`;
//! anything that is not a map brush (a capsule entity) is `-1`; `0` means the line is clear.

use assets::zone::clipmap::{Brush, Leaf};

use super::Trace;
use super::capsule::Cap;
use super::map::{ClipModel, CollisionWorld};
use super::transform::symmetric;
use super::tw::{EPS, PARALLEL_EPS, Tw};
use super::vec::{angles_to_axis, dot, lerp3, rotate_in, sub};
use crate::Vec3;

impl CollisionWorld {
    /// Sight trace through `model`. `old_hit` is a previous result for the same viewer: the
    /// brush it names is tried first, which makes repeated checks cheap.
    #[allow(clippy::too_many_arguments)]
    pub fn sight_trace(
        &self,
        old_hit: i32,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        model: &ClipModel,
        mask: i32,
    ) -> i32 {
        let tw = Tw::new(start, end, mins, maxs, mask);
        let mut trace = Trace::MISS;
        match *model {
            ClipModel::World => {
                if old_hit > 0
                    && let Some(b) = self.cm.brushes.get(old_hit as usize - 1)
                    && self.sight_brush(&tw, b)
                {
                    return old_hit;
                }
                if self.nodes.is_empty() {
                    return 0;
                }
                self.sight_tree(&tw, 0, tw.start, tw.end, &mut trace)
            }
            ClipModel::Submodel(n) => match self.cm.cmodels.get(usize::from(n)) {
                Some(m) => self.sight_leaf(&tw, &m.leaf, &mut trace),
                None => 0,
            },
            ClipModel::Box {
                mins,
                maxs,
                contents,
            } => {
                if mask & contents != 0 && Cap::new(mins, maxs, contents).blocks(&tw) {
                    -1
                } else {
                    0
                }
            }
        }
    }

    /// `CM_TransformedBoxSightTrace`.
    #[allow(clippy::too_many_arguments)]
    pub fn transformed_sight_trace(
        &self,
        old_hit: i32,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        model: &ClipModel,
        mask: i32,
        origin: Vec3,
        angles: Vec3,
    ) -> i32 {
        let (center, lo, hi) = symmetric(mins, maxs);
        let mut s = sub(
            [
                start[0] + center[0],
                start[1] + center[1],
                start[2] + center[2],
            ],
            origin,
        );
        let mut e = sub(
            [end[0] + center[0], end[1] + center[1], end[2] + center[2]],
            origin,
        );
        if angles != [0.0; 3] {
            let axis = angles_to_axis(angles);
            s = rotate_in(&axis, s);
            e = rotate_in(&axis, e);
        }
        self.sight_trace(old_hit, s, e, lo, hi, model, mask)
    }

    fn sight_tree(&self, tw: &Tw, mut num: i32, mut p1: Vec3, p2: Vec3, trace: &mut Trace) -> i32 {
        loop {
            if num < 0 {
                return self.sight_leaf(tw, &self.cm.leafs[(-1 - num) as usize], trace);
            }
            let node = &self.nodes[num as usize];
            let (t1, t2, offset) = if node.kind >= 3 {
                let off = if tw.is_point {
                    EPS
                } else {
                    tw.bounding_radius + EPS
                };
                (
                    dot(node.normal, p1) - node.dist,
                    dot(node.normal, p2) - node.dist,
                    off,
                )
            } else {
                let a = usize::from(node.kind);
                (p1[a] - node.dist, p2[a] - node.dist, tw.size[a] + EPS)
            };
            let (tmin, tmax) = (t1.min(t2), t1.max(t2));
            if offset <= tmin {
                num = i32::from(node.children[0]);
            } else if tmax <= -offset {
                num = i32::from(node.children[1]);
            } else {
                let s = split3(t1, t2, offset);
                let hit = self.sight_tree(
                    tw,
                    i32::from(node.children[s.0]),
                    p1,
                    lerp3(p1, p2, s.1),
                    trace,
                );
                if hit != 0 {
                    return hit;
                }
                p1 = lerp3(p1, p2, s.2);
                num = i32::from(node.children[1 - s.0]);
            }
        }
    }

    fn sight_leaf(&self, tw: &Tw, leaf: &Leaf, trace: &mut Trace) -> i32 {
        if tw.contents & leaf.brush_contents != 0 {
            let hit = self.sight_leaf_brushes(tw, leaf);
            if hit != 0 {
                return hit;
            }
        }
        if tw.contents & leaf.terrain_contents != 0 {
            let first = usize::from(leaf.first_coll_aabb_index);
            for k in 0..usize::from(leaf.coll_aabb_count) {
                let tree = first + k;
                let Some(m) = self
                    .cm
                    .materials
                    .get(usize::from(self.cm.aabb_trees[tree].material_index))
                else {
                    continue;
                };
                if m.content_flags & tw.contents == 0 {
                    continue;
                }
                self.trace_aabb_tree(tw, tree, trace);
                if trace.fraction != 1.0 {
                    return (tree + self.cm.brushes.len() + 1) as i32;
                }
            }
        }
        0
    }

    fn sight_leaf_brushes(&self, tw: &Tw, leaf: &Leaf) -> i32 {
        if leaf.leaf_brush_node <= 0 {
            return 0;
        }
        let mut lo = leaf.mins;
        let mut hi = leaf.maxs;
        for i in 0..3 {
            lo[i] -= tw.size[i];
            hi[i] += tw.size[i];
        }
        if tw.misses_box(lo, hi, 1.0) {
            return 0;
        }
        self.sight_lb_r(tw, leaf.leaf_brush_node as usize, tw.start, tw.end)
    }

    fn sight_lb_r(&self, tw: &Tw, mut node: usize, mut p1: Vec3, p2: Vec3) -> i32 {
        loop {
            let n = &self.lb_nodes[node];
            if tw.contents & n.contents == 0 {
                return 0;
            }
            if n.count > 0 {
                let first = n.first_brush as usize;
                for &b in &self.lb_list[first..first + n.count as usize] {
                    let brush = &self.cm.brushes[usize::from(b)];
                    if tw.contents & brush.contents != 0 && self.sight_brush(tw, brush) {
                        return i32::from(b) + 1;
                    }
                }
                return 0;
            }
            if n.count < 0 {
                let hit = self.sight_lb_r(tw, node + 1, p1, p2);
                if hit != 0 {
                    return hit;
                }
            }
            let axis = usize::from(n.axis);
            let t1 = p1[axis] - n.dist;
            let t2 = p2[axis] - n.dist;
            let offset = tw.size[axis] + EPS - n.range;
            let (tmin, tmax) = (t1.min(t2), t1.max(t2));
            if offset <= tmin {
                if tmax <= -offset {
                    return 0;
                }
                node += usize::from(n.child_offset[0]);
            } else if tmax > -offset {
                let s = split3(t1, t2, offset);
                let hit = self.sight_lb_r(
                    tw,
                    node + usize::from(n.child_offset[s.0]),
                    p1,
                    lerp3(p1, p2, s.1),
                );
                if hit != 0 {
                    return hit;
                }
                p1 = lerp3(p1, p2, s.2);
                node += usize::from(n.child_offset[1 - s.0]);
            } else {
                node += usize::from(n.child_offset[1]);
            }
        }
    }

    /// Whether the (hull-grown) brush intersects the sweep at all.
    fn sight_brush(&self, tw: &Tw, brush: &Brush) -> bool {
        let mut enter = 0.0f32;
        let mut leave = 1.0f32;
        for (bounds, sign) in [(brush.mins, -1.0f32), (brush.maxs, 1.0)] {
            for j in 0..3 {
                let d1 = (tw.start[j] - bounds[j]) * sign - tw.radius_offset[j];
                let d2 = (tw.end[j] - bounds[j]) * sign - tw.radius_offset[j];
                if d1 <= 0.0 {
                    if d2 > 0.0 {
                        let f = d1 * tw.inv_delta[j] * sign;
                        if enter >= f {
                            return false;
                        }
                        leave = leave.min(f);
                    }
                } else {
                    if d2 > 0.0 {
                        return false;
                    }
                    let f = d1 * tw.inv_delta[j] * sign;
                    if leave <= f {
                        return false;
                    }
                    enter = enter.max(f);
                }
            }
        }
        let first = brush.sides.map_or(0, |s| s as usize);
        for i in 0..brush.num_sides as usize {
            let side = &self.sides[first + i];
            let dist = side.dist + tw.radius + (tw.offset_z * side.normal[2]).abs();
            let d1 = dot(tw.start, side.normal) - dist;
            let d2 = dot(tw.end, side.normal) - dist;
            if d1 <= 0.0 {
                if d2 > 0.0 {
                    let delta = d1 - d2;
                    if d1 > leave * delta {
                        leave = d1 / delta;
                        if leave <= enter {
                            return false;
                        }
                    }
                }
            } else {
                if d2 > 0.0 {
                    return false;
                }
                let delta = d1 - d2;
                if d1 > enter * delta {
                    enter = d1 / delta;
                    if leave <= enter {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// `(first side, end of first part, start of second part)` for a segment `t1 -> t2` across a
/// slab of half-width `offset`.
fn split3(t1: f32, t2: f32, offset: f32) -> (usize, f32, f32) {
    let diff = t2 - t1;
    let abs = diff.abs();
    if abs <= PARALLEL_EPS {
        return (0, 1.0, 0.0);
    }
    let v = if diff < 0.0 { t1 } else { -t1 };
    let inv = 1.0 / abs;
    (
        usize::from(diff >= 0.0),
        ((v + offset) * inv).min(1.0),
        ((v - offset) * inv).max(0.0),
    )
}
