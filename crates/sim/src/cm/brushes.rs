// SPDX-License-Identifier: GPL-3.0-only
//! Swept tests against the BSP tree, the leaf-brush k-d trees and single brushes.

use assets::zone::clipmap::{Brush, Leaf};

use super::Trace;
use super::map::{CollisionWorld, LbNode};
use super::tw::{EPS, PARALLEL_EPS, Tw};
use super::vec::{dot, lerp4, xyz};

/// The plane that bounded the earliest entry into a brush.
#[derive(Clone, Copy)]
enum Lead {
    None,
    /// An axial side of the brush bounds: axis, sign of its outward normal, material.
    Axial(usize, f32, i16),
    /// A side listed in `brush_sides`.
    Side(usize),
}

/// How a segment `t1 -> t2` (distances to a splitting plane) is divided around a slab of
/// half-width `offset`: which child it enters first, where the first part ends and where the
/// second begins (as fractions of the segment).
#[derive(Clone, Copy)]
struct Split {
    side: usize,
    near_end: f32,
    far_start: f32,
}

#[inline]
fn split(t1: f32, t2: f32, offset: f32) -> Split {
    let diff = t2 - t1;
    let abs = diff.abs();
    let (side, near_end, far_start) = if abs <= PARALLEL_EPS {
        (0, 1.0, 0.0)
    } else {
        let v = if diff < 0.0 { t1 } else { -t1 };
        let inv = 1.0 / abs;
        (
            usize::from(diff >= 0.0),
            (v + offset) * inv,
            (v - offset) * inv,
        )
    };
    Split {
        side,
        near_end: near_end.min(1.0),
        far_start: far_start.max(0.0),
    }
}

impl CollisionWorld {
    /// Sweeps `tw` through the whole BSP tree (the world model).
    pub(super) fn trace_tree(&self, tw: &Tw, trace: &mut Trace) {
        let start = [tw.start[0], tw.start[1], tw.start[2], 0.0];
        let end = [tw.end[0], tw.end[1], tw.end[2], trace.fraction];
        self.trace_tree_r(tw, 0, start, end, trace);
    }

    fn trace_tree_r(
        &self,
        tw: &Tw,
        mut num: i32,
        mut p1: [f32; 4],
        p2: [f32; 4],
        trace: &mut Trace,
    ) {
        while num >= 0 {
            let node = &self.nodes[num as usize];
            let (t1, t2, offset) = if node.kind >= 3 {
                let off = if tw.is_point {
                    EPS
                } else {
                    tw.bounding_radius + EPS
                };
                (
                    dot(node.normal, xyz(p1)) - node.dist,
                    dot(node.normal, xyz(p2)) - node.dist,
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
                if p1[3] >= trace.fraction {
                    return;
                }
                let s = split(t1, t2, offset);
                let mid = lerp4(p1, p2, s.near_end);
                self.trace_tree_r(tw, i32::from(node.children[s.side]), p1, mid, trace);
                p1 = lerp4(p1, p2, s.far_start);
                num = i32::from(node.children[1 - s.side]);
            }
        }
        self.trace_leaf(tw, &self.cm.leafs[(-1 - num) as usize], trace);
    }

    /// Sweeps `tw` through everything a leaf (or brush model) holds.
    pub(super) fn trace_leaf(&self, tw: &Tw, leaf: &Leaf, trace: &mut Trace) {
        if trace.fraction == 0.0 {
            return;
        }
        if tw.contents & leaf.brush_contents != 0 && self.trace_leaf_brush_nodes(tw, leaf, trace) {
            return;
        }
        if tw.contents & leaf.terrain_contents == 0 {
            return;
        }
        let first = usize::from(leaf.first_coll_aabb_index);
        for k in 0..usize::from(leaf.coll_aabb_count) {
            if trace.fraction == 0.0 {
                break;
            }
            self.trace_aabb_tree(tw, first + k, trace);
        }
    }

    /// True when the trace ended at fraction zero.
    fn trace_leaf_brush_nodes(&self, tw: &Tw, leaf: &Leaf, trace: &mut Trace) -> bool {
        if leaf.leaf_brush_node <= 0 {
            return false;
        }
        let mut lo = leaf.mins;
        let mut hi = leaf.maxs;
        for i in 0..3 {
            lo[i] -= tw.size[i];
            hi[i] += tw.size[i];
        }
        if tw.misses_box(lo, hi, trace.fraction) {
            return false;
        }
        let start = [tw.start[0], tw.start[1], tw.start[2], 0.0];
        let end = [tw.end[0], tw.end[1], tw.end[2], trace.fraction];
        self.trace_lb_r(tw, leaf.leaf_brush_node as usize, start, end, trace);
        trace.fraction == 0.0
    }

    fn trace_lb_r(
        &self,
        tw: &Tw,
        mut node: usize,
        mut p1: [f32; 4],
        p2: [f32; 4],
        trace: &mut Trace,
    ) {
        loop {
            let n: &LbNode = &self.lb_nodes[node];
            if tw.contents & n.contents == 0 {
                return;
            }
            if n.count > 0 {
                let first = n.first_brush as usize;
                for &b in &self.lb_list[first..first + n.count as usize] {
                    let brush = &self.cm.brushes[usize::from(b)];
                    if tw.contents & brush.contents != 0 {
                        self.trace_brush(tw, brush, trace);
                    }
                }
                return;
            }
            if n.count < 0 {
                self.trace_lb_r(tw, node + 1, p1, p2, trace);
            }
            let axis = usize::from(n.axis);
            let t1 = p1[axis] - n.dist;
            let t2 = p2[axis] - n.dist;
            let offset = tw.size[axis] + EPS - n.range;
            let (tmin, tmax) = (t1.min(t2), t1.max(t2));
            if offset <= tmin {
                if tmax <= -offset {
                    return;
                }
                node += usize::from(n.child_offset[0]);
            } else if tmax > -offset {
                if p1[3] >= trace.fraction {
                    return;
                }
                let s = split(t1, t2, offset);
                let mid = lerp4(p1, p2, s.near_end);
                self.trace_lb_r(
                    tw,
                    node + usize::from(n.child_offset[s.side]),
                    p1,
                    mid,
                    trace,
                );
                p1 = lerp4(p1, p2, s.far_start);
                node += usize::from(n.child_offset[1 - s.side]);
            } else {
                node += usize::from(n.child_offset[1]);
            }
        }
    }

    /// Sweeps `tw` through one brush, tightening `trace` if it is entered before
    /// `trace.fraction`.
    pub(super) fn trace_brush(&self, tw: &Tw, brush: &Brush, trace: &mut Trace) {
        let mut enter = 0.0f32;
        let mut leave = trace.fraction;
        let mut all_solid = true;
        let mut lead = Lead::None;

        for (index, sign) in [(0usize, -1.0f32), (1, 1.0)] {
            let bounds = if index == 0 { brush.mins } else { brush.maxs };
            for j in 0..3 {
                let d1 = (tw.start[j] - bounds[j]) * sign - tw.radius_offset[j];
                let d2 = (tw.end[j] - bounds[j]) * sign - tw.radius_offset[j];
                if d1 <= 0.0 {
                    if d2 > 0.0 {
                        let f = d1 * tw.inv_delta[j] * sign;
                        if enter >= f {
                            return;
                        }
                        all_solid = false;
                        if f < leave {
                            leave = f;
                        }
                    }
                } else {
                    if d1.min(EPS) <= d2 {
                        return;
                    }
                    let f = (d1 - EPS) * tw.inv_delta[j] * sign;
                    if leave <= f {
                        return;
                    }
                    if d2 > 0.0 {
                        all_solid = false;
                    }
                    if enter >= f {
                        if !matches!(lead, Lead::None) {
                            continue;
                        }
                    } else {
                        enter = f;
                    }
                    lead = Lead::Axial(j, sign, brush.axial_material_num[index][j]);
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
                            return;
                        }
                    }
                    all_solid = false;
                }
            } else {
                if d1.min(EPS) <= d2 {
                    return;
                }
                if d2 > 0.0 {
                    all_solid = false;
                }
                let delta = d1 - d2;
                let f = d1 - EPS;
                if f <= enter * delta {
                    if matches!(lead, Lead::None) {
                        lead = Lead::Side(first + i);
                    }
                } else {
                    enter = f / delta;
                    if leave <= enter {
                        return;
                    }
                    lead = Lead::Side(first + i);
                }
            }
        }

        trace.contents = brush.contents;
        let (normal, material) = match lead {
            Lead::None => {
                trace.start_solid = true;
                if all_solid {
                    trace.all_solid = true;
                    trace.fraction = 0.0;
                    trace.surface_flags = 0;
                }
                return;
            }
            Lead::Axial(j, sign, mat) => {
                let mut n = [0.0; 3];
                n[j] = sign;
                (n, mat as u16 as usize)
            }
            Lead::Side(s) => (self.sides[s].normal, self.sides[s].material as usize),
        };
        trace.fraction = enter;
        trace.normal = normal;
        if let Some(m) = self.cm.materials.get(material) {
            trace.surface_flags = m.surface_flags;
            trace.material = material as u32;
        }
        trace.walkable = false;
    }

    /// Whether the (stationary) hull overlaps brush contents of the leaf; sets `trace` to a
    /// zero-fraction all-solid hit if so.
    pub(super) fn test_leaf(&self, tw: &Tw, leaf: &Leaf, trace: &mut Trace) {
        if tw.contents & leaf.brush_contents != 0 && self.test_leaf_brush_nodes(tw, leaf, trace) {
            return;
        }
        if tw.contents & leaf.terrain_contents != 0 {
            self.mesh_test_leaf(tw, leaf, trace);
        }
    }

    fn test_leaf_brush_nodes(&self, tw: &Tw, leaf: &Leaf, trace: &mut Trace) -> bool {
        if leaf.leaf_brush_node <= 0 {
            return false;
        }
        for i in 0..3 {
            if leaf.mins[i] >= tw.bounds[1][i] || leaf.maxs[i] <= tw.bounds[0][i] {
                return false;
            }
        }
        self.test_lb_r(tw, leaf.leaf_brush_node as usize, trace);
        trace.all_solid
    }

    fn test_lb_r(&self, tw: &Tw, mut node: usize, trace: &mut Trace) {
        loop {
            let n = &self.lb_nodes[node];
            if tw.contents & n.contents == 0 {
                return;
            }
            if n.count > 0 {
                let first = n.first_brush as usize;
                for &b in &self.lb_list[first..first + n.count as usize] {
                    let brush = &self.cm.brushes[usize::from(b)];
                    if tw.contents & brush.contents != 0 {
                        self.test_box_in_brush(tw, brush, trace);
                        if trace.all_solid {
                            break;
                        }
                    }
                }
                return;
            }
            if n.count < 0 {
                self.test_lb_r(tw, node + 1, trace);
                if trace.all_solid {
                    return;
                }
            }
            let axis = usize::from(n.axis);
            if n.dist >= tw.bounds[0][axis] {
                if n.dist <= tw.bounds[1][axis] {
                    self.test_lb_r(tw, node + usize::from(n.child_offset[0]), trace);
                    if trace.all_solid {
                        return;
                    }
                }
                node += usize::from(n.child_offset[1]);
            } else {
                node += usize::from(n.child_offset[0]);
            }
        }
    }

    fn test_box_in_brush(&self, tw: &Tw, brush: &Brush, trace: &mut Trace) {
        let (lo, hi) = (tw.bounds[0], tw.bounds[1]);
        if !(brush.maxs[0] > lo[0]
            && brush.maxs[1] > lo[1]
            && brush.maxs[2] > lo[2]
            && brush.mins[0] < hi[0]
            && brush.mins[1] < hi[1]
            && brush.mins[2] < hi[2])
        {
            return;
        }
        let first = brush.sides.map_or(0, |s| s as usize);
        for i in 0..brush.num_sides as usize {
            let side = &self.sides[first + i];
            let dist = side.dist + tw.radius + (tw.offset_z * side.normal[2]).abs();
            if dot(tw.start, side.normal) - dist > 0.0 {
                return;
            }
        }
        trace.start_solid = true;
        trace.all_solid = true;
        trace.fraction = 0.0;
        trace.contents = brush.contents;
        trace.surface_flags = 0;
    }
}
