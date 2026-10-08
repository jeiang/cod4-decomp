// SPDX-License-Identifier: GPL-3.0-only
//! Visibility: the camera's cell from the BSP, a portal walk that narrows the frustum through each portal, and
//! AABB-tree traversal of the visible cells.

use assets::zone::gfxworld::{AabbTree, GfxWorld};
use glam::{Mat4, Vec3, Vec4};

/// Planes `n . p + d >= 0` inside; the first two are always near and far.
#[derive(Clone, Default)]
pub struct Frustum {
    pub planes: Vec<Vec4>,
}

/// Size of the original `GfxAabbTree` record; `children_offset` is in bytes.
const TREE_SIZE: i32 = 44;
const MAX_PORTAL_DEPTH: usize = 48;

impl Frustum {
    /// The six planes of a D3D-style (depth 0..1) clip matrix.
    pub fn from_clip(m: &Mat4) -> Frustum {
        let r = |i| m.row(i);
        let planes = [
            r(2),
            r(3) - r(2),
            r(3) + r(0),
            r(3) - r(0),
            r(3) + r(1),
            r(3) - r(1),
        ]
        .into_iter()
        .map(normalize)
        .collect();
        Frustum { planes }
    }

    /// The same frustum with the near plane moved to pass through `eye`, for the portal walk: a doorway beside the eye
    /// lies between the eye and the near plane, and clipping it there narrows the view through it to the rays that
    /// cross it at a grazing angle, hiding everything beyond. (The original moves the near plane to the nearest point
    /// of each portal.)
    pub fn with_near_at(&self, eye: Vec3) -> Frustum {
        let mut f = self.clone();
        if let Some(near) = f.planes.first_mut() {
            near.w = -near.truncate().dot(eye);
        }
        f
    }

    /// True when the box is entirely outside one plane.
    pub fn culls(&self, mins: Vec3, maxs: Vec3) -> bool {
        self.planes.iter().any(|p| {
            let n = p.truncate();
            let far = Vec3::new(
                if n.x >= 0.0 { maxs.x } else { mins.x },
                if n.y >= 0.0 { maxs.y } else { mins.y },
                if n.z >= 0.0 { maxs.z } else { mins.z },
            );
            n.dot(far) + p.w < 0.0
        })
    }

    pub fn contains_box(&self, mins: Vec3, maxs: Vec3) -> bool {
        self.planes.iter().all(|p| {
            let n = p.truncate();
            let near = Vec3::new(
                if n.x >= 0.0 { mins.x } else { maxs.x },
                if n.y >= 0.0 { mins.y } else { maxs.y },
                if n.z >= 0.0 { mins.z } else { maxs.z },
            );
            n.dot(near) + p.w >= 0.0
        })
    }
}

fn normalize(p: Vec4) -> Vec4 {
    p / p.truncate().length().max(1e-12)
}

fn v3(a: [f32; 3]) -> Vec3 {
    Vec3::from(a)
}

/// The cell containing `p`, by walking the visibility BSP. `nodes` holds `(cellIndex, rightChildOffset)` pairs: a
/// value of `cell_count + 1` or more selects a plane, anything less is a leaf (`0` = outside every cell).
pub fn cell_for_point(world: &GfxWorld, p: Vec3) -> Option<usize> {
    let cell_count = world.cells.len() + 1;
    let mut at = 0usize;
    loop {
        let value = usize::from(*world.nodes.get(at)?);
        let Some(plane) = value.checked_sub(cell_count) else {
            return value.checked_sub(1);
        };
        let pl = world.planes.get(plane)?;
        let d = v3(pl.normal).dot(p) - pl.dist;
        at = if d >= 0.0 {
            at + 2
        } else {
            at + usize::from(*world.nodes.get(at + 1)?)
        };
    }
}

/// Surfaces and static models that survive culling.
#[derive(Default)]
pub struct Visible {
    pub surfaces: Vec<u32>,
    pub smodels: Vec<u32>,
    pub cells: usize,
}

pub fn visible(world: &GfxWorld, eye: Vec3, frustum: &Frustum) -> Visible {
    let mut out = Visible::default();
    let frustum = &frustum.with_near_at(eye);
    match cell_for_point(world, eye) {
        Some(c) => {
            let mut path = vec![c];
            walk(world, eye, c, frustum, &mut path, &mut out);
        }
        None => {
            for c in 0..world.cells.len() {
                add_cell(world, c, frustum, &mut out);
            }
        }
    }
    out.surfaces.sort_unstable();
    out.surfaces.dedup();
    out.smodels.sort_unstable();
    out.smodels.dedup();
    out
}

fn walk(
    world: &GfxWorld,
    eye: Vec3,
    cell: usize,
    frustum: &Frustum,
    path: &mut Vec<usize>,
    out: &mut Visible,
) {
    add_cell(world, cell, frustum, out);
    if path.len() >= MAX_PORTAL_DEPTH {
        return;
    }
    for portal in &world.cells[cell].portals {
        let target = portal.cell as usize;
        if path.contains(&target) || target >= world.cells.len() {
            continue;
        }
        let poly: Vec<Vec3> = portal.vertices.iter().map(|&v| v3(v)).collect();
        let Some(clipped) = clip_polygon(poly, frustum) else {
            continue;
        };
        path.push(target);
        walk(
            world,
            eye,
            target,
            &narrow(eye, &clipped, frustum),
            path,
            out,
        );
        path.pop();
    }
}

/// Sutherland-Hodgman clip of `poly` against every frustum plane; `None` when nothing is left.
fn clip_polygon(mut poly: Vec<Vec3>, frustum: &Frustum) -> Option<Vec<Vec3>> {
    for pl in &frustum.planes {
        if poly.len() < 3 {
            return None;
        }
        let (n, d) = (pl.truncate(), pl.w);
        let mut next = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (da, db) = (n.dot(a) + d, n.dot(b) + d);
            if da >= 0.0 {
                next.push(a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                next.push(a + (b - a) * (da / (da - db)));
            }
        }
        poly = next;
    }
    (poly.len() >= 3).then_some(poly)
}

/// The frustum seen through `poly` from `eye`: its edge planes, plus the parent's near and far planes.
fn narrow(eye: Vec3, poly: &[Vec3], parent: &Frustum) -> Frustum {
    let centroid = poly.iter().copied().sum::<Vec3>() / poly.len() as f32;
    let mut planes = vec![parent.planes[0], parent.planes[1]];
    for i in 0..poly.len() {
        let (a, b) = (poly[i] - eye, poly[(i + 1) % poly.len()] - eye);
        let mut n = a.cross(b);
        if n.length_squared() < 1e-6 {
            continue;
        }
        n = n.normalize();
        if n.dot(centroid - eye) < 0.0 {
            n = -n;
        }
        planes.push(n.extend(-n.dot(eye)));
    }
    if planes.len() < 5 {
        return parent.clone();
    }
    Frustum { planes }
}

fn add_cell(world: &GfxWorld, cell: usize, frustum: &Frustum, out: &mut Visible) {
    out.cells += 1;
    let trees = &world.cells[cell].aabb_trees;
    if !trees.is_empty() {
        tree(world, trees, 0, frustum, false, out);
    }
}

fn tree(
    world: &GfxWorld,
    trees: &[AabbTree],
    at: usize,
    frustum: &Frustum,
    inside: bool,
    out: &mut Visible,
) {
    let Some(t) = trees.get(at) else { return };
    let (mins, maxs) = (v3(t.mins), v3(t.maxs));
    let inside = inside || {
        if frustum.culls(mins, maxs) {
            return;
        }
        frustum.contains_box(mins, maxs)
    };
    if t.child_count == 0 {
        let first = usize::from(t.start_surf_index);
        let sorted = &world.dpvs.sorted_surf_index;
        let end = (first + usize::from(t.surface_count)).min(sorted.len());
        out.surfaces
            .extend(sorted[first.min(end)..end].iter().map(|&s| u32::from(s)));
        out.smodels
            .extend(t.smodel_indexes.iter().map(|&m| u32::from(m)));
        return;
    }
    let first = at as i32 + t.children_offset / TREE_SIZE;
    for k in 0..i32::from(t.child_count) {
        let c = first + k;
        if c >= 0 {
            tree(world, trees, c as usize, frustum, inside, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A doorway 2 units ahead of the eye (nearer than the near plane) still shows what is behind it: the view
    /// through it is not narrowed to the rays that graze it.
    #[test]
    fn portal_nearer_than_the_near_plane_keeps_the_view_through_it() {
        let eye = Vec3::ZERO;
        let view = crate::View {
            origin: eye,
            yaw: 0.0,
            pitch: 0.0,
            fov_x: 90f32.to_radians(),
            time: 0.0,
        };
        let frustum = Frustum::from_clip(&view.clip_from_world(16.0 / 9.0)).with_near_at(eye);
        let portal = vec![
            Vec3::new(2.0, 30.0, 30.0),
            Vec3::new(2.0, -30.0, 30.0),
            Vec3::new(2.0, -30.0, -30.0),
            Vec3::new(2.0, 30.0, -30.0),
        ];
        let clipped = clip_polygon(portal, &frustum).expect("the portal is in view");
        let through = narrow(eye, &clipped, &frustum);
        let ahead = Vec3::new(500.0, 20.0, -10.0);
        assert!(!through.culls(ahead, ahead));
    }
}
