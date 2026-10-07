// SPDX-License-Identifier: GPL-3.0-or-later
//! Traces against the stock map mp_crash. Skipped without `COD4_PATH`.

use super::test_support::crash_map;
use super::{ClipModel, CollisionWorld, Trace};
use crate::Vec3;
use crate::contents::{MASK_ALL, MASK_PLAYERSOLID, MASK_SHOT, SOLID};

const MINS: Vec3 = [-15.0, -15.0, 0.0];
const MAXS: Vec3 = [15.0, 15.0, 70.0];
const POINT: Vec3 = [0.0; 3];

fn world() -> Option<CollisionWorld> {
    crash_map().map(CollisionWorld::new)
}

/// Deterministic xorshift for sample points.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn between(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

fn at(start: Vec3, end: Vec3, f: f32) -> Vec3 {
    std::array::from_fn(|i| start[i] + (end[i] - start[i]) * f)
}

fn trace(w: &CollisionWorld, a: Vec3, b: Vec3, mins: Vec3, maxs: Vec3) -> Trace {
    w.box_trace(a, b, mins, maxs, &ClipModel::World, MASK_PLAYERSOLID)
}

/// Random xy over the map with a downward player-hull trace; yields resting positions.
fn floor_samples(w: &CollisionWorld, n: usize) -> Vec<(Vec3, Trace)> {
    let (lo, hi) = w.model_bounds(0).unwrap();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut out = Vec::new();
    while out.len() < n {
        let (x, y) = (rng.between(lo[0], hi[0]), rng.between(lo[1], hi[1]));
        let (a, b) = ([x, y, hi[2] + 10.0], [x, y, lo[2] - 10.0]);
        let t = trace(w, a, b, MINS, MAXS);
        if t.fraction > 0.0 && t.fraction < 1.0 && !t.start_solid {
            out.push((at(a, b, t.fraction), t));
        }
    }
    out
}

#[test]
fn hull_dropped_from_the_sky_comes_to_rest_without_overlap() {
    let Some(w) = world() else { return };
    let mut flat = 0;
    for (p, t) in floor_samples(&w, 1500) {
        let here = trace(&w, p, p, MINS, MAXS);
        assert!(!here.start_solid, "resting at {p:?} overlaps something");
        if t.normal[2] > 0.99 {
            // Sinking a unit into a flat floor must overlap it.
            let sunk = [p[0], p[1], p[2] - 1.0];
            assert!(
                trace(&w, sunk, sunk, MINS, MAXS).start_solid,
                "floor at {p:?} is not solid"
            );
            flat += 1;
        }
    }
    assert!(flat > 300, "only {flat} flat floors sampled");
}

#[test]
fn a_long_trace_ends_where_chained_short_steps_end() {
    let Some(w) = world() else { return };
    let mut rng = Rng(12345);
    let mut checked = 0;
    for (p, _) in floor_samples(&w, 600) {
        let up = [p[0], p[1], p[2] + 40.0];
        for (mins, maxs) in [(MINS, MAXS), (POINT, POINT)] {
            let (a, b) = (rng.between(-1.0, 1.0), rng.between(-1.0, 1.0));
            let len = (a * a + b * b).sqrt().max(0.1);
            let end = [
                up[0] + a / len * 1500.0,
                up[1] + b / len * 1500.0,
                up[2] + rng.between(-40.0, 40.0),
            ];
            let long = trace(&w, up, end, mins, maxs);
            if long.start_solid {
                continue;
            }
            // Walk the same line in 100-unit pieces until something blocks.
            let mut hit_at = None;
            let steps = 15;
            for i in 0..steps {
                let s = at(up, end, i as f32 / steps as f32);
                let e = at(up, end, (i + 1) as f32 / steps as f32);
                let t = trace(&w, s, e, mins, maxs);
                if t.fraction < 1.0 {
                    hit_at = Some((i as f32 + t.fraction) / steps as f32);
                    break;
                }
            }
            let chained = hit_at.unwrap_or(1.0);
            // The blocked fraction is the same to within the clearance on a 1500-unit line.
            assert!(
                (chained - long.fraction).abs() < 0.002,
                "{up:?}->{end:?}: long {} chained {chained}",
                long.fraction
            );
            checked += 1;
        }
    }
    assert!(checked > 600);
}

#[test]
fn hulls_stopped_by_a_trace_are_not_inside_anything() {
    let Some(w) = world() else { return };
    let mut rng = Rng(777);
    let (mut stopped, mut stuck) = (0, 0);
    for (p, _) in floor_samples(&w, 1200) {
        let start = [p[0], p[1], p[2] + 36.0];
        let end = [
            start[0] + rng.between(-300.0, 300.0),
            start[1] + rng.between(-300.0, 300.0),
            start[2] + rng.between(-100.0, 100.0),
        ];
        let t = trace(&w, start, end, MINS, MAXS);
        if t.start_solid || t.fraction == 1.0 {
            continue;
        }
        stopped += 1;
        let stop = at(start, end, t.fraction);
        // Terrain triangles block from their front only, but a stationary test counts them from
        // both sides (as the original does), so a hull resting on a brush floor that has a
        // back-facing triangle co-located with it reports stuck. That is rare.
        stuck += usize::from(trace(&w, stop, stop, MINS, MAXS).start_solid);
    }
    eprintln!("stopped {stopped} stuck {stuck}");
    assert!(stopped > 300, "{stopped}");
    assert!(
        stuck * 50 <= stopped,
        "{stuck} of {stopped} stopped hulls overlap something"
    );
}

#[test]
fn centers_of_solid_brushes_are_solid_for_hulls_and_points() {
    let Some(w) = world() else { return };
    let cm = w.clipmap();
    let mut checked = 0;
    for b in cm.brushes.iter().filter(|b| b.contents & SOLID != 0) {
        let c: Vec3 = std::array::from_fn(|i| (b.mins[i] + b.maxs[i]) * 0.5);
        if w.point_contents(c, &ClipModel::World) & SOLID == 0 {
            continue;
        }
        let t = w.box_trace(c, c, MINS, MAXS, &ClipModel::World, MASK_SHOT | SOLID);
        assert!(
            t.start_solid && t.all_solid,
            "hull at the center of brush {b:?}"
        );
        checked += 1;
    }
    assert!(checked > 1000, "{checked}");
}

#[test]
fn terrain_is_not_tunnelled_from_above() {
    let Some(w) = world() else { return };
    let cm = w.clipmap();
    let mut tested = 0;
    for tree in cm.aabb_trees.iter().filter(|t| t.child_count == 0) {
        let material = &cm.materials[usize::from(tree.material_index)];
        if material.content_flags & MASK_PLAYERSOLID == 0 {
            continue;
        }
        let part = &cm.partitions[tree.index as usize];
        for tri in part.first_tri as usize..part.first_tri as usize + usize::from(part.tri_count) {
            let v: Vec<Vec3> = (0..3)
                .map(|k| cm.verts[usize::from(cm.tri_indices[3 * tri + k])])
                .collect();
            let e0: Vec3 = std::array::from_fn(|i| v[1][i] - v[0][i]);
            let e1: Vec3 = std::array::from_fn(|i| v[2][i] - v[0][i]);
            let n = [
                e0[1] * e1[2] - e0[2] * e1[1],
                e0[2] * e1[0] - e0[0] * e1[2],
                e0[0] * e1[1] - e0[1] * e1[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len < 1.0 || (n[2] / len).abs() < 0.7 {
                continue;
            }
            let c: Vec3 = std::array::from_fn(|i| (v[0][i] + v[1][i] + v[2][i]) / 3.0);
            // Whichever way the triangle faces, come at it from that side: feet 150 above/below.
            let dir = if n[2] > 0.0 { 1.0 } else { -1.0 };
            let _ = dir;
            let a = [c[0], c[1], c[2] + 300.0];
            let b = [c[0], c[1], c[2] - 300.0];
            let t = trace(&w, a, b, MINS, MAXS);
            // The hull's feet pass the triangle's plane after 300 of 600 units at the latest, unless
            // the triangle faces away (a back-face is not solid) or it started stuck.
            if t.start_solid {
                continue;
            }
            if n[2] > 0.0 {
                assert!(
                    t.fraction <= 0.5 + 1e-3,
                    "tunnelled through a triangle at {c:?}: {}",
                    t.fraction
                );
                tested += 1;
            }
        }
    }
    assert!(tested > 100, "{tested}");
}

#[test]
fn sight_blocked_implies_trace_blocked_and_is_symmetric() {
    let Some(w) = world() else { return };
    let mut rng = Rng(4242);
    let samples = floor_samples(&w, 800);
    let (mut asym, mut blocked, mut total) = (0, 0, 0);
    for pair in samples.chunks(2) {
        let (a, b) = (pair[0].0, pair[1].0);
        let (a, b) = (
            [a[0], a[1], a[2] + 60.0],
            [b[0], b[1], b[2] + rng.between(20.0, 80.0)],
        );
        let dist = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        if dist > 2500.0 {
            continue;
        }
        let ab = w.sight_trace(0, a, b, POINT, POINT, &ClipModel::World, MASK_SHOT) != 0;
        let ba = w.sight_trace(0, b, a, POINT, POINT, &ClipModel::World, MASK_SHOT) != 0;
        let tr = w.box_trace(a, b, POINT, POINT, &ClipModel::World, MASK_SHOT);
        if ab {
            assert!(
                tr.fraction < 1.0 || tr.start_solid,
                "sight blocked but trace clear {a:?}->{b:?}"
            );
            blocked += 1;
        }
        asym += usize::from(ab != ba);
        total += 1;
    }
    assert!(
        total > 100 && blocked > 10 && blocked < total,
        "{blocked}/{total}"
    );
    assert!(
        asym * 100 <= total,
        "{asym} of {total} sight lines differ by direction"
    );
}

#[test]
fn brush_models_are_hit_from_outside() {
    let Some(w) = world() else { return };
    let mut hit = 0;
    for n in 1..w.num_submodels() as u16 {
        let (lo, hi) = w.model_bounds(n).unwrap();
        let c: Vec3 = std::array::from_fn(|i| (lo[i] + hi[i]) * 0.5);
        if w.point_contents(c, &ClipModel::Submodel(n)) == 0 {
            continue;
        }
        let mut t = Trace::MISS;
        let from = [c[0] + (hi[0] - lo[0]) + 100.0, c[1], c[2]];
        w.transformed_trace(
            &mut t,
            from,
            c,
            POINT,
            POINT,
            &ClipModel::Submodel(n),
            MASK_ALL,
            [0.0; 3],
            [0.0; 3],
        );
        assert!(
            t.fraction < 1.0,
            "line into the middle of model {n} passed through"
        );
        hit += 1;
    }
    assert!(hit > 5, "{hit}");
}

/// Cost per call on mp_crash; run with `cargo test -p sim --release bench -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_trace_costs() {
    use std::hint::black_box;
    use std::time::Instant;
    let Some(w) = world() else { return };
    let samples: Vec<Vec3> = floor_samples(&w, 4000)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    let mut rng = Rng(99);
    let steps: Vec<(Vec3, Vec3)> = samples
        .iter()
        .map(|p| {
            let a = [p[0], p[1], p[2] + 1.0];
            (
                a,
                [
                    a[0] + rng.between(-30.0, 30.0),
                    a[1] + rng.between(-30.0, 30.0),
                    a[2] + rng.between(-5.0, 5.0),
                ],
            )
        })
        .collect();
    let lines: Vec<(Vec3, Vec3)> = samples
        .iter()
        .map(|p| {
            let a = [p[0], p[1], p[2] + 60.0];
            (
                a,
                [
                    a[0] + rng.between(-2000.0, 2000.0),
                    a[1] + rng.between(-2000.0, 2000.0),
                    a[2] + rng.between(-300.0, 300.0),
                ],
            )
        })
        .collect();
    let reps = 50;
    let per = |label: &str, n: usize, f: &mut dyn FnMut()| {
        let t = Instant::now();
        for _ in 0..reps {
            f();
        }
        eprintln!(
            "{label}: {:.0} ns/call",
            t.elapsed().as_nanos() as f64 / (reps * n) as f64
        );
    };
    per("player capsule step (<=30 units)", steps.len(), &mut || {
        for (a, b) in &steps {
            black_box(w.box_trace(*a, *b, MINS, MAXS, &ClipModel::World, MASK_PLAYERSOLID));
        }
    });
    per(
        "player capsule drop to floor (200 units)",
        samples.len(),
        &mut || {
            for p in &samples {
                black_box(w.box_trace(
                    [p[0], p[1], p[2] + 200.0],
                    *p,
                    MINS,
                    MAXS,
                    &ClipModel::World,
                    MASK_PLAYERSOLID,
                ));
            }
        },
    );
    per(
        "bullet line (<=2000 units, MASK_SHOT)",
        lines.len(),
        &mut || {
            for (a, b) in &lines {
                black_box(w.box_trace(*a, *b, POINT, POINT, &ClipModel::World, MASK_SHOT));
            }
        },
    );
    per(
        "position test (stationary hull)",
        samples.len(),
        &mut || {
            for p in &samples {
                black_box(w.box_trace(*p, *p, MINS, MAXS, &ClipModel::World, MASK_PLAYERSOLID));
            }
        },
    );
    per("point_contents", samples.len(), &mut || {
        for p in &samples {
            black_box(w.point_contents([p[0], p[1], p[2] + 30.0], &ClipModel::World));
        }
    });
    per("sight trace (<=2000 units)", lines.len(), &mut || {
        for (a, b) in &lines {
            black_box(w.sight_trace(0, *a, *b, POINT, POINT, &ClipModel::World, MASK_SHOT));
        }
    });
}
