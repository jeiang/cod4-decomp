// SPDX-License-Identifier: GPL-3.0-or-later
//! Terrain (and patch) collision: collision AABB trees over triangle partitions plus the
//! partition borders that stop a capsule from sliding off a mesh edge.

use assets::zone::clipmap::{CollisionBorder, Leaf};

use super::Trace;
use super::map::CollisionWorld;
use super::tw::{EPS, Tw};
use super::vec::{add, cross, dot, len_sq, mad, normalize, scale, sub};
use crate::Vec3;

/// Where a swept sphere stands against one triangle edge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Hits,
    Misses,
    /// The sphere passes the end of the edge: only its first vertex can be hit.
    MayHitV0,
    /// Only the second vertex can be hit.
    MayHitV1,
}

impl CollisionWorld {
    fn tri_verts(&self, tri: usize) -> [Vec3; 3] {
        let i = &self.cm.tri_indices[3 * tri..3 * tri + 3];
        [
            self.cm.verts[usize::from(i[0])],
            self.cm.verts[usize::from(i[1])],
            self.cm.verts[usize::from(i[2])],
        ]
    }

    /// Bit `n` of the triangle-edge walkable set.
    fn edge_walkable(&self, n: usize) -> bool {
        self.cm
            .tri_edge_is_walkable
            .get(n >> 3)
            .is_some_and(|b| b & (1 << (n & 7)) != 0)
    }

    /// Sweeps `tw` through one collision AABB tree if its material matches the trace mask.
    pub(super) fn trace_aabb_tree(&self, tw: &Tw, tree: usize, trace: &mut Trace) {
        let t = &self.cm.aabb_trees[tree];
        let Some(material) = self.cm.materials.get(usize::from(t.material_index)) else {
            return;
        };
        if material.content_flags & tw.contents == 0 {
            return;
        }
        let old = trace.fraction;
        self.trace_aabb_r(tw, tree, trace);
        if old > trace.fraction {
            trace.surface_flags = material.surface_flags;
            trace.contents = material.content_flags;
            trace.material = u32::from(t.material_index);
        }
    }

    fn trace_aabb_r(&self, tw: &Tw, tree: usize, trace: &mut Trace) {
        let t = &self.cm.aabb_trees[tree];
        if cull_box(tw, t.origin, t.half_size) {
            return;
        }
        if t.child_count > 0 {
            let first = t.index as usize;
            for c in 0..usize::from(t.child_count) {
                self.trace_aabb_r(tw, first + c, trace);
            }
            return;
        }
        let part = &self.cm.partitions[t.index as usize];
        let first = part.first_tri as usize;
        if tw.is_point {
            for tri in first..first + usize::from(part.tri_count) {
                self.trace_point_triangle(tw, tri, trace);
            }
            return;
        }
        for tri in first..first + usize::from(part.tri_count) {
            self.trace_capsule_triangle(tw, tri, trace);
        }
        if (tw.delta[0] != 0.0 || tw.delta[1] != 0.0) && tw.offset_z != 0.0 {
            let b0 = part.borders.map_or(0, |b| b as usize);
            for b in 0..usize::from(part.border_count) {
                trace_capsule_border(tw, &self.cm.borders[b0 + b], trace);
            }
        }
    }

    fn trace_point_triangle(&self, tw: &Tw, tri: usize, trace: &mut Trace) {
        let [v0, v1, v2] = self.tri_verts(tri);
        let v0_v1 = sub(v0, v1);
        let v0_v2 = sub(v0, v2);
        let n = cross(v0_v2, v0_v1);
        let proj = dot(tw.delta, n);
        if proj >= 0.0 {
            return;
        }
        let v0_start = sub(v0, tw.start);
        let t = dot(v0_start, n);
        if t > 0.0 || t <= trace.fraction * proj {
            return;
        }
        let plane = cross(tw.delta, v0_start);
        let v = dot(plane, v0_v1);
        if v > 0.0 || proj > v {
            return;
        }
        let neg_u = dot(plane, v0_v2);
        if neg_u < 0.0 || proj > v - neg_u {
            return;
        }
        trace.walkable = false;
        trace.normal = normalize(n).0;
        trace.fraction = t / proj;
    }

    fn trace_capsule_triangle(&self, tw: &Tw, tri: usize, trace: &mut Trace) {
        let [v0, v1, v2] = self.tri_verts(tri);
        let v0_v1 = sub(v0, v1);
        let v0_v2 = sub(v0, v2);
        let scaled_normal = cross(v0_v2, v0_v1);
        let proj = dot(tw.delta, scaled_normal);
        if proj >= 0.0 {
            return;
        }
        let (normal, area2) = normalize(scaled_normal);
        let mut sphere_start = tw.start;
        sphere_start[2] -= if normal[2] < 0.0 {
            -tw.offset_z
        } else {
            tw.offset_z
        };
        let shifted = mad(sphere_start, -tw.radius, normal);
        let hit_dist = dot(sub(shifted, v0), normal);
        let (mut start_v0, hit_frac, start_solid);
        if hit_dist >= 0.0 {
            hit_frac = -((hit_dist - EPS) / proj) * area2;
            if trace.fraction <= hit_frac {
                return;
            }
            start_v0 = sub(shifted, v0);
            start_solid = false;
        } else {
            start_v0 = sub(sphere_start, v0);
            let start_dist = dot(start_v0, normal);
            if tw.radius * tw.radius <= start_dist * start_dist {
                return;
            }
            start_v0 = mad(start_v0, -start_dist, normal);
            hit_frac = 0.0;
            start_solid = true;
        }
        let plane = cross(tw.delta, start_v0);
        let mut missed_edge = false;
        let mut walkable = true;
        let mut vertex: Option<Vec3> = None;

        let neg_v = dot(plane, v0_v1);
        if neg_v < 0.0 {
            missed_edge = true;
            match self.trace_sphere_edge(tw, sphere_start, v0, v0_v1, trace) {
                Edge::Hits => {
                    trace.walkable = self.edge_walkable(3 * tri + 2);
                    return;
                }
                Edge::Misses => {}
                e => {
                    vertex = Some(if e == Edge::MayHitV0 { v0 } else { v1 });
                    walkable &= self.edge_walkable(3 * tri + 2);
                }
            }
        }
        let u = dot(plane, v0_v2);
        if u > 0.0 {
            missed_edge = true;
            match self.trace_sphere_edge(tw, sphere_start, v0, v0_v2, trace) {
                Edge::Hits => {
                    trace.walkable = self.edge_walkable(3 * tri + 1);
                    return;
                }
                Edge::Misses => {}
                e => {
                    vertex = Some(if e == Edge::MayHitV0 { v0 } else { v2 });
                    walkable &= self.edge_walkable(3 * tri + 1);
                }
            }
        }
        if proj > u - neg_v {
            missed_edge = true;
            let v1_v2 = sub(v1, v2);
            match self.trace_sphere_edge(tw, sphere_start, v1, v1_v2, trace) {
                Edge::Hits => {
                    trace.walkable = self.edge_walkable(3 * tri);
                    return;
                }
                Edge::Misses => {}
                e => {
                    vertex = Some(if e == Edge::MayHitV0 { v1 } else { v2 });
                    walkable &= self.edge_walkable(3 * tri);
                }
            }
        }
        if missed_edge {
            if let Some(v) = vertex {
                trace_sphere_vertex(tw, walkable, sphere_start, v, trace);
            }
        } else {
            trace.normal = normal;
            trace.walkable = false;
            trace.fraction = hit_frac.max(0.0);
            trace.start_solid = start_solid;
        }
    }

    fn trace_sphere_edge(
        &self,
        tw: &Tw,
        sphere_start: Vec3,
        v0: Vec3,
        v0_v1: Vec3,
        trace: &mut Trace,
    ) -> Edge {
        trace_sphere_edge(tw, sphere_start, v0, v0_v1, trace)
    }

    pub(super) fn mesh_test_leaf(&self, tw: &Tw, leaf: &Leaf, trace: &mut Trace) {
        let first = usize::from(leaf.first_coll_aabb_index);
        for k in 0..usize::from(leaf.coll_aabb_count) {
            let tree = &self.cm.aabb_trees[first + k];
            let Some(material) = self.cm.materials.get(usize::from(tree.material_index)) else {
                continue;
            };
            if material.content_flags & tw.contents == 0 {
                continue;
            }
            self.test_aabb_r(tw, first + k, trace);
            if trace.all_solid {
                trace.surface_flags = material.surface_flags;
                trace.contents = material.content_flags;
                trace.material = u32::from(tree.material_index);
                return;
            }
        }
    }

    fn test_aabb_r(&self, tw: &Tw, tree: usize, trace: &mut Trace) {
        let t = &self.cm.aabb_trees[tree];
        if cull_box(tw, t.origin, t.half_size) {
            return;
        }
        if t.child_count > 0 {
            for c in 0..usize::from(t.child_count) {
                self.test_aabb_r(tw, t.index as usize + c, trace);
                if trace.start_solid {
                    break;
                }
            }
            return;
        }
        let part = &self.cm.partitions[t.index as usize];
        let first = part.first_tri as usize;
        for tri in first..first + usize::from(part.tri_count) {
            let [v0, v1, v2] = self.tri_verts(tri);
            let hit = if tw.offset_z == 0.0 {
                dist_sq_point_triangle(tw.start, v0, v1, v2) < tw.radius * tw.radius
            } else {
                let top = add(tw.start, [0.0, 0.0, tw.offset_z]);
                let bottom = sub(tw.start, [0.0, 0.0, tw.offset_z]);
                dist_sq_segment_triangle(top, bottom, v0, v1, v2) < tw.radius * tw.radius
            };
            if hit {
                trace.fraction = 0.0;
                trace.start_solid = true;
                trace.all_solid = true;
            }
            if trace.start_solid {
                break;
            }
        }
    }
}

/// True when the sweep cannot touch the AABB (`CM_CullBox`): separating-axis test of the swept
/// center-box against the box grown by the hull; the three edge-cross axes are skipped when the
/// sweep is long relative to the hull.
fn cull_box(tw: &Tw, origin: Vec3, half: Vec3) -> bool {
    let c = sub(tw.midpoint, origin);
    let h = add(half, tw.size);
    let (hd, ha) = (tw.half_delta, tw.half_delta_abs);
    if c[0].abs() > h[0] + ha[0] || c[1].abs() > h[1] + ha[1] || c[2].abs() > h[2] + ha[2] {
        return true;
    }
    if tw.axial_cull_only {
        return false;
    }
    (c[2] * hd[1] - c[1] * hd[2]).abs() > h[1] * ha[2] + h[2] * ha[1]
        || (c[0] * hd[2] - c[2] * hd[0]).abs() > h[2] * ha[0] + h[0] * ha[2]
        || (c[1] * hd[0] - c[0] * hd[1]).abs() > h[0] * ha[1] + h[1] * ha[0]
}

fn trace_sphere_edge(
    tw: &Tw,
    sphere_start: Vec3,
    v0: Vec3,
    v0_v1: Vec3,
    trace: &mut Trace,
) -> Edge {
    let start_delta = sub(sphere_start, v0);
    let perp = cross(v0_v1, tw.delta);
    let scaled_dist = dot(start_delta, perp);
    let perp_len_sq = len_sq(perp);
    let radius = tw.radius + EPS;
    let disc = radius * radius * perp_len_sq - scaled_dist * scaled_dist;
    if disc <= 0.0 {
        return Edge::Misses;
    }
    let edge_len_sq = len_sq(v0_v1);
    let f = (disc * edge_len_sq).sqrt() / perp_len_sq;
    let t_scaled = dot(cross(start_delta, v0_v1), perp);
    let t = t_scaled / perp_len_sq;
    if t + f < 0.0 {
        return Edge::Misses;
    }
    let enter = t - f;
    if trace.fraction <= enter {
        return Edge::Misses;
    }
    if enter >= 0.0 {
        let hit = mad(start_delta, enter, tw.delta);
        let proj = -dot(hit, v0_v1);
        if proj <= 0.0 {
            return Edge::MayHitV0;
        }
        if edge_len_sq <= proj {
            return Edge::MayHitV1;
        }
        let n = mad(hit, proj / edge_len_sq, v0_v1);
        trace.normal = scale(n, 1.0 / radius);
        trace.fraction = enter;
        return Edge::Hits;
    }
    let proj = -dot(start_delta, v0_v1);
    if proj <= 0.0 {
        return Edge::MayHitV0;
    }
    if edge_len_sq <= proj {
        return Edge::MayHitV1;
    }
    let n = mad(start_delta, proj / edge_len_sq, v0_v1);
    if dot(n, tw.delta) >= 0.0 {
        return Edge::Misses;
    }
    trace.normal = normalize(n).0;
    trace.fraction = 0.0;
    let inner = tw.radius * tw.radius * perp_len_sq - scaled_dist * scaled_dist;
    trace.start_solid = edge_len_sq * inner > t_scaled * t_scaled;
    Edge::Hits
}

fn trace_sphere_vertex(tw: &Tw, walkable: bool, sphere_start: Vec3, v: Vec3, trace: &mut Trace) {
    let delta = sub(sphere_start, v);
    let b = dot(tw.delta, delta);
    if b >= 0.0 {
        return;
    }
    let delta_len_sq = dot(delta, delta);
    let reach = tw.radius + EPS;
    let c = delta_len_sq - reach * reach;
    if c <= 0.0 {
        trace.normal = scale(delta, 1.0 / delta_len_sq.sqrt());
        trace.walkable = walkable;
        trace.fraction = 0.0;
        if delta_len_sq < tw.radius * tw.radius {
            trace.start_solid = true;
        }
        return;
    }
    let a = tw.delta_len_sq;
    let b_sq = b * b;
    let disc = b_sq - a * c;
    if disc < b_sq * 0.001 {
        return;
    }
    let frac = (-disc.sqrt() - b) / a;
    if trace.fraction <= frac {
        return;
    }
    let n = scale(mad(delta, frac, tw.delta), 1.0 / reach);
    // One Newton step towards unit length, as the original does.
    trace.normal = scale(n, (3.0 - len_sq(n)) * 0.5);
    trace.walkable = walkable;
    trace.fraction = frac;
}

/// Border: a vertical wall segment along the rim of a partition that a capsule's cylindrical
/// part would otherwise slip past between triangles.
fn trace_capsule_border(tw: &Tw, border: &CollisionBorder, trace: &mut Trace) {
    let eq = border.dist_eq;
    let delta_dot = eq[1] * tw.delta[1] + eq[0] * tw.delta[0];
    if delta_dot >= 0.0 {
        return;
    }
    let radius = tw.radius + EPS;
    let start_dist = eq[1] * tw.start[1] + eq[0] * tw.start[0] - eq[2];
    let mut t = (radius - start_dist) / delta_dot;
    if trace.fraction <= t || -radius > t * tw.delta_len {
        return;
    }
    let mut end = mad(tw.start, t, tw.delta);
    let mut s = eq[1] * end[0] - eq[0] * end[1] - border.start;

    if s < 0.0 || border.length < s {
        // The wall plane is struck beyond an end of the segment: the end post is what counts.
        let along = if s < 0.0 {
            border.start
        } else {
            border.start + border.length
        };
        let post_x = eq[1] * along + eq[0] * eq[2];
        let post_y = eq[1] * eq[2] - eq[0] * along;
        let off = [tw.start[0] - post_x, tw.start[1] - post_y];
        let delta_dot_off = off[1] * tw.delta[1] + off[0] * tw.delta[0];
        if delta_dot_off >= 0.0 {
            return;
        }
        let off_len_sq = off[1] * off[1] + off[0] * off[0];
        let c = off_len_sq - radius * radius;
        if c < 0.0 {
            let post_z = if s < 0.0 {
                border.z_base
            } else {
                border.z_slope * border.length + border.z_base
            };
            if (post_z - tw.start[2]).abs() <= tw.offset_z {
                trace.normal = [eq[0], eq[1], 0.0];
                trace.walkable = false;
                trace.fraction = 0.0;
                if off_len_sq < tw.radius * tw.radius {
                    trace.start_solid = true;
                }
            }
            return;
        }
        let disc = delta_dot_off * delta_dot_off - tw.delta_len_sq * c;
        if disc < 0.0 {
            return;
        }
        t = (-delta_dot_off - disc.sqrt()) / tw.delta_len_sq;
        if trace.fraction <= t || t <= 0.0 {
            return;
        }
        end = mad(tw.start, t, tw.delta);
        s = if s < 0.0 { 0.0 } else { border.length };
    } else if t < 0.0 {
        t = 0.0;
    }

    let edge_z = s * border.z_slope + border.z_base - end[2];
    if edge_z <= tw.offset_z {
        if edge_z >= -tw.offset_z {
            trace.fraction = t;
            trace.walkable = false;
            trace.normal = [eq[0], eq[1], 0.0];
        } else if edge_z > -tw.offset_z - tw.radius {
            trace_sphere_border(tw, border, -tw.offset_z, trace);
        }
    } else if edge_z < tw.offset_z + tw.radius {
        trace_sphere_border(tw, border, tw.offset_z, trace);
    }
}

/// The sloped top or bottom of a border, hit by the capsule's end cap.
fn trace_sphere_border(tw: &Tw, border: &CollisionBorder, offset_z: f32, trace: &mut Trace) {
    let eq = border.dist_eq;
    let v0 = [
        eq[1] * border.start + eq[0] * eq[2],
        eq[1] * eq[2] - eq[0] * border.start,
        border.z_base,
    ];
    let end = border.start + border.length;
    let v1 = [
        end * eq[1] + eq[0] * eq[2],
        eq[1] * eq[2] - end * eq[0],
        border.z_slope * border.length + border.z_base,
    ];
    let mut sphere_start = tw.start;
    sphere_start[2] += offset_z;
    match trace_sphere_edge(tw, sphere_start, v0, sub(v0, v1), trace) {
        Edge::MayHitV0 => trace_sphere_vertex(tw, false, sphere_start, v0, trace),
        Edge::MayHitV1 => trace_sphere_vertex(tw, false, sphere_start, v1, trace),
        _ => {}
    }
}

/// Squared distance from `p` to the triangle (closest-point-on-triangle).
pub(super) fn dist_sq_point_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
    len_sq(sub(closest_point_on_triangle(p, a, b, c), p))
}

fn closest_point_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return mad(a, d1 / (d1 - d3), ab);
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return mad(a, d2 / (d2 - d6), ac);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return mad(b, w, sub(c, b));
    }
    let denom = 1.0 / (va + vb + vc);
    mad(mad(a, vb * denom, ab), vc * denom, ac)
}

/// Squared distance between the segment `p0..p1` and the triangle.
fn dist_sq_segment_triangle(p0: Vec3, p1: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let n = cross(sub(b, a), sub(c, a));
    let d0 = dot(sub(p0, a), n);
    let d1 = dot(sub(p1, a), n);
    if d0 * d1 < 0.0 {
        // Crosses the plane: zero when it pierces the triangle.
        let hit = mad(p0, d0 / (d0 - d1), sub(p1, p0));
        if len_sq(sub(closest_point_on_triangle(hit, a, b, c), hit)) == 0.0
            || point_in_triangle(hit, a, b, c, n)
        {
            return 0.0;
        }
    }
    let mut best = dist_sq_point_triangle(p0, a, b, c).min(dist_sq_point_triangle(p1, a, b, c));
    for (e0, e1) in [(a, b), (a, c), (b, c)] {
        best = best.min(dist_sq_segments(p0, sub(p1, p0), e0, sub(e1, e0)));
    }
    best
}

fn point_in_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3, n: Vec3) -> bool {
    dot(cross(sub(b, a), sub(p, a)), n) >= 0.0
        && dot(cross(sub(c, b), sub(p, b)), n) >= 0.0
        && dot(cross(sub(a, c), sub(p, c)), n) >= 0.0
}

/// Squared distance between segments `s0 + t d0` and `s1 + u d1`, `t, u` in `0..=1`.
fn dist_sq_segments(s0: Vec3, d0: Vec3, s1: Vec3, d1: Vec3) -> f32 {
    let r = sub(s0, s1);
    let a = dot(d0, d0);
    let e = dot(d1, d1);
    let f = dot(d1, r);
    let (t, u);
    if a <= 1e-12 && e <= 1e-12 {
        return dot(r, r);
    }
    if a <= 1e-12 {
        t = 0.0;
        u = (f / e).clamp(0.0, 1.0);
    } else {
        let c = dot(d0, r);
        if e <= 1e-12 {
            u = 0.0;
            t = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = dot(d0, d1);
            let denom = a * e - b * b;
            let mut tt = if denom > 1e-12 {
                ((b * f - c * e) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut uu = (b * tt + f) / e;
            if uu < 0.0 {
                uu = 0.0;
                tt = (-c / a).clamp(0.0, 1.0);
            } else if uu > 1.0 {
                uu = 1.0;
                tt = ((b - c) / a).clamp(0.0, 1.0);
            }
            t = tt;
            u = uu;
        }
    }
    len_sq(sub(mad(s0, t, d0), mad(s1, u, d1)))
}
