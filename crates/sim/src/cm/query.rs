// SPDX-License-Identifier: GPL-3.0-or-later
//! Swept traces against the world or one model, and the point/area queries.

use assets::zone::clipmap::Leaf;

use super::capsule::Cap;
use super::map::{ClipModel, CollisionWorld, LbNode};
use super::tw::Tw;
use super::vec::dot;
use super::{Trace, Vec3};

/// Longest leaf list a position test gathers (the original's `leafs[1024]`).
const MAX_TEST_LEAFS: usize = 1024;

/// Minimum Z of a surface normal for the ground to count as walkable.
pub const WALKABLE_NORMAL_Z: f32 = 0.7;

impl CollisionWorld {
    /// A fresh trace against `model` (`CM_BoxTrace`).
    pub fn box_trace(
        &self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        model: &ClipModel,
        mask: i32,
    ) -> Trace {
        let mut trace = Trace::MISS;
        self.trace_model(&mut trace, start, end, mins, maxs, model, mask);
        trace
    }

    /// Sweeps `mins..maxs` from `start` to `end` through `model`, tightening `trace` for
    /// anything hit before `trace.fraction` (`CM_Trace`). `start == end` is a position test.
    #[allow(clippy::too_many_arguments)]
    pub fn trace_model(
        &self,
        trace: &mut Trace,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        model: &ClipModel,
        mask: i32,
    ) {
        let tw = Tw::new(start, end, mins, maxs, mask);
        if start == end {
            match *model {
                ClipModel::World => self.position_test(&tw, trace),
                ClipModel::Submodel(n) => {
                    if !trace.all_solid
                        && let Some(m) = self.cm.cmodels.get(usize::from(n))
                    {
                        self.test_leaf(&tw, &m.leaf, trace);
                    }
                }
                ClipModel::Box {
                    mins,
                    maxs,
                    contents,
                } => {
                    if mask & contents != 0 {
                        Cap::new(mins, maxs, contents).test(&tw, trace);
                    }
                }
            }
        } else {
            match *model {
                ClipModel::World => self.trace_tree(&tw, trace),
                ClipModel::Submodel(n) => {
                    if let Some(m) = self.cm.cmodels.get(usize::from(n)) {
                        self.trace_leaf(&tw, &m.leaf, trace);
                    }
                }
                ClipModel::Box {
                    mins,
                    maxs,
                    contents,
                } => {
                    if mask & contents != 0 {
                        Cap::new(mins, maxs, contents).trace(&tw, trace);
                    }
                }
            }
        }
        if !trace.walkable && !trace.start_solid {
            trace.walkable = trace.normal[2] >= WALKABLE_NORMAL_Z;
        }
    }

    fn position_test(&self, tw: &Tw, trace: &mut Trace) {
        if trace.all_solid {
            return;
        }
        let lo = [
            tw.start[0] - tw.size[0] - 1.0,
            tw.start[1] - tw.size[1] - 1.0,
            tw.start[2] - tw.size[2] - 1.0,
        ];
        let hi = [
            tw.start[0] + tw.size[0] + 1.0,
            tw.start[1] + tw.size[1] + 1.0,
            tw.start[2] + tw.size[2] + 1.0,
        ];
        let mut leafs = [0u16; MAX_TEST_LEAFS];
        let (count, _) = self.box_leafnums(lo, hi, &mut leafs);
        for &l in &leafs[..count] {
            if trace.all_solid {
                break;
            }
            self.test_leaf(tw, &self.cm.leafs[usize::from(l)], trace);
        }
    }

    /// The leaf containing `p` (`CM_PointLeafnum`).
    pub fn point_leafnum(&self, p: Vec3) -> u16 {
        let mut num = 0i32;
        if self.nodes.is_empty() {
            return 0;
        }
        while num >= 0 {
            let n = &self.nodes[num as usize];
            let d = if n.kind >= 3 {
                dot(n.normal, p) - n.dist
            } else {
                p[usize::from(n.kind)] - n.dist
            };
            num = i32::from(n.children[usize::from(d < 0.0)]);
        }
        (-1 - num) as u16
    }

    /// Leaves whose volume meets `mins..maxs`, written to `out` (further leaves are counted
    /// out). Returns how many were stored and the last stored leaf that belongs to a cluster
    /// (`CM_BoxLeafnums`).
    pub fn box_leafnums(&self, mins: Vec3, maxs: Vec3, out: &mut [u16]) -> (usize, u16) {
        let mut ll = LeafList {
            mins,
            maxs,
            out,
            count: 0,
            last: 0,
        };
        if !self.nodes.is_empty() {
            self.box_leafnums_r(&mut ll, 0);
        }
        (ll.count, ll.last)
    }

    fn box_leafnums_r(&self, ll: &mut LeafList, mut num: i32) {
        while num >= 0 {
            let n = &self.nodes[num as usize];
            match box_on_plane_side(ll.mins, ll.maxs, n.normal, n.dist, n.kind) {
                Side::Front => num = i32::from(n.children[0]),
                Side::Back => num = i32::from(n.children[1]),
                Side::Both => {
                    self.box_leafnums_r(ll, i32::from(n.children[0]));
                    num = i32::from(n.children[1]);
                }
            }
        }
        let leaf = (-1 - num) as u16;
        if self.leaf_cluster(leaf) != -1 {
            ll.last = leaf;
        }
        if ll.count < ll.out.len() {
            ll.out[ll.count] = leaf;
            ll.count += 1;
        }
    }

    /// Union of the contents of the brushes containing `p` in `model` (`CM_PointContents`).
    pub fn point_contents(&self, p: Vec3, model: &ClipModel) -> i32 {
        let leaf: &Leaf = match *model {
            ClipModel::World => {
                if self.nodes.is_empty() {
                    return 0;
                }
                &self.cm.leafs[usize::from(self.point_leafnum(p))]
            }
            ClipModel::Submodel(n) => match self.cm.cmodels.get(usize::from(n)) {
                Some(m) => &m.leaf,
                None => return 0,
            },
            ClipModel::Box {
                mins,
                maxs,
                contents,
            } => {
                return if (0..3).all(|i| mins[i] <= p[i] && p[i] <= maxs[i]) {
                    contents
                } else {
                    0
                };
            }
        };
        if leaf.leaf_brush_node <= 0 {
            return 0;
        }
        for i in 0..3 {
            if leaf.mins[i] >= p[i] || leaf.maxs[i] <= p[i] {
                return 0;
            }
        }
        self.point_contents_lb(p, leaf.leaf_brush_node as usize)
    }

    fn point_contents_lb(&self, p: Vec3, mut node: usize) -> i32 {
        let mut contents = 0;
        loop {
            let n: &LbNode = &self.lb_nodes[node];
            if n.count > 0 {
                let first = n.first_brush as usize;
                for &b in &self.lb_list[first..first + n.count as usize] {
                    let brush = &self.cm.brushes[usize::from(b)];
                    if (0..3).any(|i| brush.mins[i] > p[i] || brush.maxs[i] < p[i]) {
                        continue;
                    }
                    let first = brush.sides.map_or(0, |s| s as usize);
                    let inside = (0..brush.num_sides as usize).all(|i| {
                        let s = &self.sides[first + i];
                        s.dist >= dot(p, s.normal)
                    });
                    if inside {
                        contents |= brush.contents;
                    }
                }
                return contents;
            }
            if n.count < 0 {
                contents |= self.point_contents_lb(p, node + 1);
            }
            node += usize::from(n.child_offset[usize::from(n.dist >= p[usize::from(n.axis)])]);
        }
    }
}

struct LeafList<'a> {
    mins: Vec3,
    maxs: Vec3,
    out: &'a mut [u16],
    count: usize,
    last: u16,
}

enum Side {
    Front,
    Back,
    Both,
}

fn box_on_plane_side(mins: Vec3, maxs: Vec3, normal: Vec3, dist: f32, kind: u8) -> Side {
    let (lo, hi) = if kind < 3 {
        let a = usize::from(kind);
        (mins[a], maxs[a])
    } else {
        let (mut lo, mut hi) = (0.0, 0.0);
        for i in 0..3 {
            if normal[i] >= 0.0 {
                lo += normal[i] * mins[i];
                hi += normal[i] * maxs[i];
            } else {
                lo += normal[i] * maxs[i];
                hi += normal[i] * mins[i];
            }
        }
        (lo, hi)
    };
    if lo >= dist {
        Side::Front
    } else if hi < dist {
        Side::Back
    } else {
        Side::Both
    }
}
