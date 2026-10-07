// SPDX-License-Identifier: GPL-3.0-or-later
//! Static models (props baked into the map) as bullets see them: thin lines traced against each
//! model's collision triangles. Hulls ignore props; only point traces and sight checks that ask
//! for them use this.

use std::sync::Arc;

use assets::zone::clipmap::Clipmap;
use assets::zone::xmodel::XModel;

use crate::Vec3;
use crate::cm::{ENTITYNUM_WORLD, Trace, Tw};

/// Grid cell edge. Props are filed under every cell their bounds touch.
const CELL: f32 = 512.0;
/// Barycentric slack of the original's triangle test.
const BARY_SLACK: f32 = 0.001;
const EPS: f32 = 0.125;

struct Prop {
    model: Arc<XModel>,
    origin: Vec3,
    /// Rows of the world-to-model matrix: `local = delta * rows`.
    rows: [Vec3; 3],
    abs_min: Vec3,
    abs_max: Vec3,
}

pub(crate) struct Props {
    props: Box<[Prop]>,
    origin: [f32; 2],
    dims: [usize; 2],
    /// `(first, len)` into `items` per cell.
    cells: Box<[(u32, u32)]>,
    items: Box<[u32]>,
}

impl Props {
    pub fn new(cm: &Clipmap) -> Self {
        let props: Vec<Prop> = cm
            .static_models
            .iter()
            .filter_map(|s| {
                let m = s.model.clone()?;
                (m.contents != 0 && m.coll_lod >= 0).then(|| Prop {
                    model: m,
                    origin: s.origin,
                    rows: s.inv_scaled_axis,
                    abs_min: s.abs_min,
                    abs_max: s.abs_max,
                })
            })
            .collect();
        let (lo, hi) = cm
            .cmodels
            .first()
            .map_or(([0.0; 3], [0.0; 3]), |m| (m.mins, m.maxs));
        let origin = [lo[0], lo[1]];
        let cell_of =
            |v: f32, o: f32, n: usize| (((v - o) / CELL).floor().max(0.0) as usize).min(n - 1);
        let dims = [
            (((hi[0] - lo[0]) / CELL).ceil() as usize).max(1),
            (((hi[1] - lo[1]) / CELL).ceil() as usize).max(1),
        ];
        let mut lists = vec![Vec::<u32>::new(); dims[0] * dims[1]];
        for (i, p) in props.iter().enumerate() {
            let (x0, x1) = (
                cell_of(p.abs_min[0], origin[0], dims[0]),
                cell_of(p.abs_max[0], origin[0], dims[0]),
            );
            let (y0, y1) = (
                cell_of(p.abs_min[1], origin[1], dims[1]),
                cell_of(p.abs_max[1], origin[1], dims[1]),
            );
            for y in y0..=y1 {
                for x in x0..=x1 {
                    lists[y * dims[0] + x].push(i as u32);
                }
            }
        }
        let mut items = Vec::new();
        let cells = lists
            .iter()
            .map(|l| {
                let first = items.len() as u32;
                items.extend_from_slice(l);
                (first, l.len() as u32)
            })
            .collect();
        Self {
            props: props.into_boxed_slice(),
            origin,
            dims,
            cells,
            items: items.into_boxed_slice(),
        }
    }

    /// Calls `f` with every prop whose grid cells the segment's bounds touch (a prop can be
    /// offered more than once).
    fn candidates(&self, a: Vec3, b: Vec3, mut f: impl FnMut(&Prop) -> bool) {
        let cell =
            |v: f32, o: f32, n: usize| (((v - o) / CELL).floor().max(0.0) as usize).min(n - 1);
        let (x0, x1) = (
            cell(a[0].min(b[0]), self.origin[0], self.dims[0]),
            cell(a[0].max(b[0]), self.origin[0], self.dims[0]),
        );
        let (y0, y1) = (
            cell(a[1].min(b[1]), self.origin[1], self.dims[1]),
            cell(a[1].max(b[1]), self.origin[1], self.dims[1]),
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (first, len) = self.cells[y * self.dims[0] + x];
                for &i in &self.items[first as usize..(first + len) as usize] {
                    if !f(&self.props[i as usize]) {
                        return;
                    }
                }
            }
        }
    }

    /// Tightens `trace` with the nearest prop hit by the line, if it is nearer than
    /// `trace.fraction`.
    pub fn trace(&self, trace: &mut Trace, start: Vec3, end: Vec3, mask: i32) {
        let tw = Tw::new(start, end, [0.0; 3], [0.0; 3], mask);
        self.candidates(start, end, |p| {
            if p.model.contents & mask != 0 && !tw.misses_box(p.abs_min, p.abs_max, trace.fraction)
            {
                trace_prop(p, start, end, mask, trace);
            }
            true
        });
    }

    /// Whether any prop blocks the line at all.
    pub fn blocks(&self, start: Vec3, end: Vec3, mask: i32) -> bool {
        let tw = Tw::new(start, end, [0.0; 3], [0.0; 3], mask);
        let mut blocked = false;
        self.candidates(start, end, |p| {
            if p.model.contents & mask != 0 && !tw.misses_box(p.abs_min, p.abs_max, 1.0) {
                let mut t = Trace::MISS;
                trace_prop(p, start, end, mask, &mut t);
                blocked = t.fraction < 1.0;
            }
            !blocked
        });
        blocked
    }
}

fn to_local(p: &Prop, v: Vec3) -> Vec3 {
    let d = [v[0] - p.origin[0], v[1] - p.origin[1], v[2] - p.origin[2]];
    std::array::from_fn(|i| d[0] * p.rows[0][i] + d[1] * p.rows[1][i] + d[2] * p.rows[2][i])
}

fn dot4(v: Vec3, p: [f32; 4]) -> f32 {
    v[0] * p[0] + v[1] * p[1] + v[2] * p[2]
}

/// `XModelTraceLine`: the nearest front-facing collision triangle crossed by the line.
fn trace_prop(p: &Prop, world_start: Vec3, world_end: Vec3, mask: i32, trace: &mut Trace) {
    let start = to_local(p, world_start);
    let end = to_local(p, world_end);
    let tw = Tw::new(start, end, [0.0; 3], [0.0; 3], mask);
    let delta = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let mut hit = None;
    for surf in p.model.coll_surfs.iter() {
        if mask & surf.contents == 0 || tw.misses_box(surf.mins, surf.maxs, trace.fraction) {
            continue;
        }
        for tri in surf.tris.iter() {
            let end_dist = dot4(end, tri.plane) - tri.plane[3];
            if end_dist >= 0.0 {
                continue;
            }
            let start_dist = dot4(start, tri.plane) - tri.plane[3];
            if start_dist <= 0.0 {
                continue;
            }
            let frac = ((start_dist - EPS) / (start_dist - end_dist)).max(0.0);
            if trace.fraction <= frac {
                continue;
            }
            let at = start_dist / (start_dist - end_dist);
            let point = [
                start[0] + delta[0] * at,
                start[1] + delta[1] * at,
                start[2] + delta[2] * at,
            ];
            let s = dot4(point, tri.svec) - tri.svec[3];
            if !(-BARY_SLACK..=1.0 + BARY_SLACK).contains(&s) {
                continue;
            }
            let t = dot4(point, tri.tvec) - tri.tvec[3];
            if t >= -BARY_SLACK && s + t <= 1.0 + BARY_SLACK {
                trace.fraction = frac;
                trace.surface_flags = surf.surface_flags;
                trace.contents = surf.contents;
                hit = Some([tri.plane[0], tri.plane[1], tri.plane[2]]);
            }
        }
    }
    if let Some(n) = hit {
        trace.start_solid = false;
        trace.all_solid = false;
        trace.hit_id = ENTITYNUM_WORLD;
        trace.material = u32::MAX;
        trace.walkable = false;
        let world: Vec3 = std::array::from_fn(|i| {
            n[0] * p.rows[i][0] + n[1] * p.rows[i][1] + n[2] * p.rows[i][2]
        });
        let len = (world[0] * world[0] + world[1] * world[1] + world[2] * world[2]).sqrt();
        trace.normal = if len > 0.0 {
            world.map(|c| c / len)
        } else {
            world
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cm::ENTITYNUM_NONE;
    use crate::cm::test_support::crash_map;
    use crate::contents::MASK_SHOT;
    use crate::world::World;

    #[test]
    fn bullets_stop_in_props_that_are_in_their_way() {
        let Some(cm) = crash_map() else { return };
        let w = World::new(cm.clone());
        let (mut shot, mut hit_inside) = (0, 0);
        for s in cm.static_models.iter() {
            let Some(m) = &s.model else { continue };
            if m.contents & MASK_SHOT == 0 || m.coll_lod < 0 {
                continue;
            }
            let c: Vec3 = std::array::from_fn(|i| (s.abs_min[i] + s.abs_max[i]) * 0.5);
            let top = s.abs_max[2] + 40.0;
            let (a, b) = ([c[0], c[1], top], [c[0], c[1], s.abs_min[2] - 40.0]);
            let t = w.bullet_trace(a, b, ENTITYNUM_NONE, MASK_SHOT);
            shot += 1;
            if t.fraction < 1.0 {
                let p = [a[0], a[1], a[2] + (b[2] - a[2]) * t.fraction];
                // The shot may end on the world instead, but never beyond the prop it started over.
                assert!(p[2] >= s.abs_min[2] - 41.0);
                hit_inside += usize::from(p[2] <= s.abs_max[2] + 1.0 && p[2] >= s.abs_min[2] - 1.0);
            }
        }
        assert!(shot > 500, "{shot}");
        assert!(
            hit_inside * 2 > shot,
            "{hit_inside} of {shot} shots into props hit them"
        );
    }

    #[test]
    fn props_block_matches_the_trace() {
        let Some(cm) = crash_map() else { return };
        let w = World::new(cm.clone());
        let mut blocked = 0;
        for s in cm.static_models.iter().take(600) {
            let Some(m) = &s.model else { continue };
            if m.contents & MASK_SHOT == 0 || m.coll_lod < 0 {
                continue;
            }
            let c: Vec3 = std::array::from_fn(|i| (s.abs_min[i] + s.abs_max[i]) * 0.5);
            let (a, b) = (
                [c[0], c[1], s.abs_max[2] + 40.0],
                [c[0], c[1], s.abs_min[2] - 40.0],
            );
            let mut t = Trace::MISS;
            w.props_trace_for_test(&mut t, a, b);
            let b_block = w.props_block(a, b, MASK_SHOT);
            assert_eq!(b_block, t.fraction < 1.0);
            blocked += usize::from(b_block);
        }
        assert!(blocked > 50, "{blocked}");
    }

    /// `cargo test -p sim --release bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_bullet_and_player_traces_through_the_world() {
        use crate::cm::Collide;
        use std::hint::black_box;
        use std::time::Instant;
        let Some(cm) = crash_map() else { return };
        let w = World::new(cm.clone());
        let mut ents = 0;
        let lines: Vec<(Vec3, Vec3)> = cm
            .static_models
            .iter()
            .step_by(3)
            .map(|s| {
                let c: Vec3 = std::array::from_fn(|i| (s.abs_min[i] + s.abs_max[i]) * 0.5);
                (
                    [c[0], c[1], c[2] + 60.0],
                    [c[0] + 1500.0, c[1] - 900.0, c[2] + 20.0],
                )
            })
            .collect();
        let mut w = w;
        for n in 0..64u16 {
            let l = lines[usize::from(n) % lines.len()].0;
            w.link(
                n,
                &crate::world::ClipEnt {
                    contents: crate::contents::PLAYER,
                    origin: l,
                    mins: [-15.0, -15.0, 0.0],
                    maxs: [15.0, 15.0, 70.0],
                    ..crate::world::ClipEnt::EMPTY
                },
            );
            ents += 1;
        }
        let reps = 50;
        let t = Instant::now();
        for _ in 0..reps {
            for (a, b) in &lines {
                black_box(w.bullet_trace(*a, *b, ENTITYNUM_NONE, MASK_SHOT));
            }
        }
        eprintln!(
            "bullet line through world+props+{ents} entities: {:.0} ns/call",
            t.elapsed().as_nanos() as f64 / (reps * lines.len()) as f64
        );
        let t = Instant::now();
        for _ in 0..reps {
            for (a, _) in &lines {
                let b = [a[0] + 20.0, a[1] - 12.0, a[2]];
                black_box(w.trace(
                    *a,
                    b,
                    [-15.0, -15.0, 0.0],
                    [15.0, 15.0, 70.0],
                    ENTITYNUM_NONE,
                    crate::contents::MASK_PLAYERSOLID,
                ));
            }
        }
        eprintln!(
            "player step through world+{ents} entities: {:.0} ns/call",
            t.elapsed().as_nanos() as f64 / (reps * lines.len()) as f64
        );
    }
}
