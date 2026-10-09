// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests on hand-built maps. Expected fractions are worked out by hand in the comments:
//! a contact stops `0.125` short of the surface, and the player hull is a capsule of radius 15
//! whose center sits 35 above the origin.

use super::test_support::{BrushSpec, MapSpec};
use super::{ClipModel, CollisionWorld};
use crate::contents::{CANSHOOTCLIP, MASK_PLAYERSOLID, MASK_SHOT, PLAYERCLIP, SOLID};

const MINS: [f32; 3] = [-15.0, -15.0, 0.0];
const MAXS: [f32; 3] = [15.0, 15.0, 70.0];
const POINT: [f32; 3] = [0.0; 3];

fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-4, "{a} != {b}");
}

/// A floor slab with its top at z = 0 and a wall slab whose face is at x = 100.
fn room() -> CollisionWorld {
    MapSpec {
        world: vec![
            BrushSpec::aabb([-1000.0, -1000.0, -100.0], [1000.0, 1000.0, 0.0], SOLID),
            BrushSpec::aabb([100.0, -1000.0, 0.0], [200.0, 1000.0, 500.0], SOLID),
        ],
        ..Default::default()
    }
    .world()
}

fn trace(
    w: &CollisionWorld,
    a: [f32; 3],
    b: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
) -> super::Trace {
    w.box_trace(a, b, mins, maxs, &ClipModel::World, MASK_PLAYERSOLID)
}

#[test]
fn hull_dropped_onto_floor_rests_an_epsilon_above_it() {
    let w = room();
    let t = trace(&w, [0.0, 0.0, 100.0], [0.0, 0.0, -50.0], MINS, MAXS);
    // 100 units to the floor out of 150, less the 0.125 clearance.
    near(t.fraction, (100.0 - 0.125) / 150.0);
    assert_eq!(t.normal, [0.0, 0.0, 1.0]);
    assert!(t.walkable);
    assert_eq!(t.contents, SOLID);
    assert_eq!(t.surface_flags, 0x1234);
    assert!(!t.start_solid && !t.all_solid);
}

#[test]
fn hull_walking_into_a_wall_stops_a_radius_short() {
    let w = room();
    let t = trace(&w, [0.0, 0.0, 10.0], [500.0, 0.0, 10.0], MINS, MAXS);
    // The hull's +x side (15 from its center) meets x = 100 after 85 units, less 0.125.
    near(t.fraction, (85.0 - 0.125) / 500.0);
    assert_eq!(t.normal, [-1.0, 0.0, 0.0]);
    assert!(!t.walkable);
}

#[test]
fn trace_that_misses_everything_is_clear() {
    let w = room();
    let t = trace(&w, [0.0, 0.0, 10.0], [50.0, 0.0, 400.0], MINS, MAXS);
    assert_eq!(t.fraction, 1.0);
    assert_eq!(t.contents, 0);
}

#[test]
fn contents_mask_selects_what_blocks() {
    let w = MapSpec {
        world: vec![BrushSpec::aabb(
            [100.0, -50.0, 0.0],
            [120.0, 50.0, 200.0],
            PLAYERCLIP,
        )],
        ..Default::default()
    }
    .world();
    let go = |mask| {
        w.box_trace(
            [0.0, 0.0, 10.0],
            [300.0, 0.0, 10.0],
            MINS,
            MAXS,
            &ClipModel::World,
            mask,
        )
    };
    assert!(go(MASK_PLAYERSOLID).fraction < 1.0);
    assert_eq!(go(MASK_SHOT).fraction, 1.0);
    assert_eq!(go(CANSHOOTCLIP).fraction, 1.0);
}

#[test]
fn starting_inside_a_brush_is_all_solid_at_fraction_zero() {
    let w = room();
    let t = trace(&w, [0.0, 0.0, -10.0], [0.0, 0.0, -30.0], MINS, MAXS);
    assert!(t.start_solid && t.all_solid);
    assert_eq!(t.fraction, 0.0);
    // The stationary form agrees.
    let p = trace(&w, [0.0, 0.0, -10.0], [0.0, 0.0, -10.0], MINS, MAXS);
    assert!(p.start_solid && p.all_solid);
}

#[test]
fn leaving_a_brush_the_hull_only_partly_overlaps_is_start_solid_not_all_solid() {
    let w = room();
    // Feet 5 below the floor, head above it, moving up and out.
    let t = trace(&w, [0.0, 0.0, -5.0], [0.0, 0.0, 40.0], MINS, MAXS);
    assert!(t.start_solid);
    assert!(!t.all_solid);
    assert_eq!(t.fraction, 1.0);
}

#[test]
fn standing_on_the_floor_is_not_stuck() {
    let w = room();
    let t = trace(&w, [0.0, 0.0, 0.2], [0.0, 0.0, 0.2], MINS, MAXS);
    assert!(!t.start_solid);
    assert_eq!(t.fraction, 1.0);
}

#[test]
fn sloped_brush_side_reports_its_normal_and_is_walkable() {
    // Everything below the plane 0.6 x + 0.8 z = 0 (a 53 degree ramp).
    let w = MapSpec {
        world: vec![BrushSpec {
            mins: [-200.0; 3],
            maxs: [200.0; 3],
            contents: SOLID,
            planes: vec![([0.6, 0.0, 0.8], 0.0)],
        }],
        ..Default::default()
    }
    .world();
    let t = w.box_trace(
        [0.0, 0.0, 100.0],
        [0.0, 0.0, -100.0],
        POINT,
        POINT,
        &ClipModel::World,
        MASK_PLAYERSOLID,
    );
    // Signed distance falls from 80 to -80; contact at 80 - 0.125 over a span of 160.
    near(t.fraction, 79.875 / 160.0);
    near(t.normal[0], 0.6);
    near(t.normal[2], 0.8);
    assert!(t.walkable);
    // A steeper plane (0.8, 0, 0.6) is a wall: normal z 0.6 < 0.7.
    let steep = MapSpec {
        world: vec![BrushSpec {
            mins: [-200.0; 3],
            maxs: [200.0; 3],
            contents: SOLID,
            planes: vec![([0.8, 0.0, 0.6], 0.0)],
        }],
        ..Default::default()
    }
    .world();
    let t = steep.box_trace(
        [0.0, 0.0, 100.0],
        [0.0, 0.0, -100.0],
        POINT,
        POINT,
        &ClipModel::World,
        MASK_PLAYERSOLID,
    );
    assert!(!t.walkable);
}

fn terrain() -> CollisionWorld {
    MapSpec {
        triangles: vec![[
            [-100.0, -100.0, 0.0],
            [0.0, 100.0, 0.0],
            [100.0, -100.0, 0.0],
        ]],
        terrain_contents: SOLID,
        ..Default::default()
    }
    .world()
}

#[test]
fn capsule_dropped_onto_a_terrain_triangle_stops_above_it() {
    let w = terrain();
    let t = trace(&w, [0.0, 0.0, 100.0], [0.0, 0.0, -100.0], MINS, MAXS);
    // Feet 100 above the triangle, 200 of travel, less the 0.125 clearance.
    near(t.fraction, 99.875 / 200.0);
    assert_eq!(t.normal, [0.0, 0.0, 1.0]);
    assert_eq!(t.contents, SOLID);
    assert_eq!(t.surface_flags, 0x1234);
}

#[test]
fn capsule_cannot_tunnel_through_terrain_in_one_long_step() {
    let w = terrain();
    let t = trace(&w, [30.0, 20.0, 5000.0], [30.0, 20.0, -5000.0], MINS, MAXS);
    near(t.fraction, 4999.875 / 10000.0);
    // From beneath, the single-sided triangle lets the hull through (its back is not solid).
    let up = trace(&w, [0.0, 0.0, -100.0], [0.0, 0.0, 100.0], MINS, MAXS);
    assert_eq!(up.fraction, 1.0);
}

#[test]
fn capsule_landing_beside_a_triangle_rests_on_its_edge() {
    let w = terrain();
    // 10 outside the base edge y = -100. The lower sphere (centre 15 above the feet) meets the
    // edge when its centre is 15.125 away from it: 10 across and sqrt(15.125^2 - 10^2) up.
    let up = (15.125f32 * 15.125 - 100.0).sqrt();
    let t = trace(&w, [0.0, -110.0, 100.0], [0.0, -110.0, -100.0], MINS, MAXS);
    near(t.fraction, (115.0 - up) / 200.0);
    near(t.normal[1], -10.0 / 15.125);
    near(t.normal[2], up / 15.125);
    assert!(t.walkable);
    // Moving along the plane well outside the triangle never touches it.
    let along = trace(
        &w,
        [-300.0, -200.0, 10.0],
        [300.0, -200.0, 10.0],
        MINS,
        MAXS,
    );
    assert_eq!(along.fraction, 1.0);
}

#[test]
fn position_test_finds_the_hull_in_terrain() {
    let w = terrain();
    // Feet 5 below the triangle: the lower sphere's centre is 10 from it, inside the radius.
    let inside = trace(&w, [0.0, 0.0, -5.0], [0.0, 0.0, -5.0], MINS, MAXS);
    assert!(inside.all_solid && inside.start_solid);
    let above = trace(&w, [0.0, 0.0, 20.0], [0.0, 0.0, 20.0], MINS, MAXS);
    assert!(!above.start_solid);
}

fn door_world(mins: [f32; 3], maxs: [f32; 3]) -> CollisionWorld {
    MapSpec {
        models: vec![vec![BrushSpec::aabb(mins, maxs, SOLID)]],
        ..Default::default()
    }
    .world()
}

#[test]
fn brush_model_trace_is_in_model_space() {
    let w = door_world([-50.0; 3], [50.0; 3]);
    let mut t = super::Trace::MISS;
    w.transformed_trace(
        &mut t,
        [0.0; 3],
        [400.0, 0.0, 0.0],
        POINT,
        POINT,
        &ClipModel::Submodel(1),
        MASK_PLAYERSOLID,
        [200.0, 0.0, 0.0],
        [0.0; 3],
    );
    near(t.fraction, (150.0 - 0.125) / 400.0);
    assert_eq!(t.normal, [-1.0, 0.0, 0.0]);
}

#[test]
fn rotated_brush_model_turns_its_geometry_and_normal() {
    // 100 long in x and 20 thick in y; yawed 90 degrees it is 20 long in x and 100 in y.
    let w = door_world([-50.0, -10.0, -50.0], [50.0, 10.0, 50.0]);
    let mut t = super::Trace::MISS;
    w.transformed_trace(
        &mut t,
        [0.0; 3],
        [400.0, 0.0, 0.0],
        POINT,
        POINT,
        &ClipModel::Submodel(1),
        MASK_PLAYERSOLID,
        [200.0, 0.0, 0.0],
        [0.0, 90.0, 0.0],
    );
    near(t.fraction, (190.0 - 0.125) / 400.0);
    near(t.normal[0], -1.0);
    near(t.normal[1], 0.0);
    // Unrotated, the same line hits 40 units further along.
    let mut u = super::Trace::MISS;
    w.transformed_trace(
        &mut u,
        [0.0; 3],
        [400.0, 0.0, 0.0],
        POINT,
        POINT,
        &ClipModel::Submodel(1),
        MASK_PLAYERSOLID,
        [200.0, 0.0, 0.0],
        [0.0; 3],
    );
    near(u.fraction, (150.0 - 0.125) / 400.0);
}

#[test]
fn box_model_is_a_capsule() {
    let w = room();
    let model = ClipModel::Box {
        mins: MINS,
        maxs: MAXS,
        contents: crate::contents::PLAYER,
    };
    let mut t = super::Trace::MISS;
    w.transformed_trace(
        &mut t,
        [0.0; 3],
        [200.0, 0.0, 0.0],
        MINS,
        MAXS,
        &model,
        MASK_PLAYERSOLID,
        [100.0, 0.0, 0.0],
        [0.0; 3],
    );
    // Two radius-15 cylinders 100 apart touch after 70 units: c = 100^2 - 30^2, b = -20000,
    // a = 40000, entry = (20000 - sqrt(b^2 - a c)) / a minus 0.125 / 200 of clearance.
    near(t.fraction, 0.35 - 12.5 / 20000.0);
    near(t.normal[0], -1.0);
    assert_eq!(t.contents, crate::contents::PLAYER);
    // A mask without the box's contents passes through it.
    let mut u = super::Trace::MISS;
    w.transformed_trace(
        &mut u,
        [0.0; 3],
        [200.0, 0.0, 0.0],
        MINS,
        MAXS,
        &model,
        SOLID,
        [100.0, 0.0, 0.0],
        [0.0; 3],
    );
    assert_eq!(u.fraction, 1.0);
}

#[test]
fn capsule_vs_capsule_overlap_and_height_gap() {
    let w = room();
    let model = ClipModel::Box {
        mins: MINS,
        maxs: MAXS,
        contents: crate::contents::PLAYER,
    };
    let overlap = |dz: f32| {
        let mut t = super::Trace::MISS;
        w.transformed_trace(
            &mut t,
            [10.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            MINS,
            MAXS,
            &model,
            MASK_PLAYERSOLID,
            [0.0, 0.0, dz],
            [0.0; 3],
        );
        t
    };
    assert!(overlap(0.0).all_solid);
    assert!(overlap(60.0).all_solid);
    // The capsules are 40 apart in height: the facing spheres' centres are 70 apart.
    assert!(!overlap(110.0).start_solid);
}

#[test]
fn point_contents_reports_brush_contents_only_inside() {
    let w = room();
    assert_eq!(w.point_contents([0.0, 0.0, -5.0], &ClipModel::World), SOLID);
    assert_eq!(w.point_contents([0.0, 0.0, 5.0], &ClipModel::World), 0);
    assert_eq!(
        w.point_contents([150.0, 0.0, 100.0], &ClipModel::World),
        SOLID
    );
    let d = door_world([-50.0; 3], [50.0; 3]);
    assert_eq!(
        d.transformed_point_contents(
            [210.0, 0.0, 0.0],
            &ClipModel::Submodel(1),
            [200.0, 0.0, 0.0],
            [0.0; 3]
        ),
        SOLID
    );
    assert_eq!(
        d.transformed_point_contents(
            [260.0, 0.0, 0.0],
            &ClipModel::Submodel(1),
            [200.0, 0.0, 0.0],
            [0.0; 3]
        ),
        0
    );
}

#[test]
fn sight_is_blocked_by_walls_and_clear_over_them() {
    let w = room();
    let sight = |a, b| w.sight_trace(0, a, b, POINT, POINT, &ClipModel::World, MASK_SHOT);
    assert_ne!(sight([0.0, 0.0, 50.0], [300.0, 0.0, 50.0]), 0);
    assert_ne!(sight([300.0, 0.0, 50.0], [0.0, 0.0, 50.0]), 0);
    assert_eq!(sight([0.0, 0.0, 600.0], [300.0, 0.0, 600.0]), 0);
    assert_eq!(sight([0.0, 0.0, 50.0], [90.0, 0.0, 50.0]), 0);
    // A blocked answer is the brush number plus one, and works as a hint for the next call.
    let hit = sight([0.0, 0.0, 50.0], [300.0, 0.0, 50.0]);
    assert_eq!(hit, 2);
    assert_eq!(
        w.sight_trace(
            hit,
            [0.0, 0.0, 50.0],
            [300.0, 0.0, 50.0],
            POINT,
            POINT,
            &ClipModel::World,
            MASK_SHOT
        ),
        hit
    );
}

#[test]
fn sight_trace_with_a_hull_is_blocked_by_a_gap_narrower_than_it() {
    let w = MapSpec {
        world: vec![
            BrushSpec::aabb([100.0, -500.0, 0.0], [120.0, -10.0, 200.0], SOLID),
            BrushSpec::aabb([100.0, 10.0, 0.0], [120.0, 500.0, 200.0], SOLID),
        ],
        ..Default::default()
    }
    .world();
    let through = |mins: [f32; 3], maxs: [f32; 3]| {
        w.sight_trace(
            0,
            [0.0, 0.0, 50.0],
            [300.0, 0.0, 50.0],
            mins,
            maxs,
            &ClipModel::World,
            MASK_SHOT,
        )
    };
    assert_eq!(through(POINT, POINT), 0);
    assert_ne!(through([-15.0, -15.0, -15.0], [15.0, 15.0, 15.0]), 0);
}

#[test]
fn box_leafnums_lists_the_leaf_and_its_cluster() {
    let w = room();
    let mut out = [0u16; 4];
    let (n, last) = w.box_leafnums([-10.0; 3], [10.0; 3], &mut out);
    assert_eq!((n, last), (1, 0));
    assert_eq!(w.leaf_cluster(0), 0);
    assert_eq!(w.leaf_cluster(9), -1);
}

#[test]
fn model_bounds_and_contents_are_looked_up_by_index() {
    let w = door_world([-50.0, -10.0, -50.0], [50.0, 10.0, 50.0]);
    assert_eq!(w.num_submodels(), 2);
    assert_eq!(
        w.model_bounds(1),
        Some(([-50.0, -10.0, -50.0], [50.0, 10.0, 50.0]))
    );
    assert_eq!(w.model_contents(1), SOLID);
    assert_eq!(w.model_bounds(7), None);
    assert_eq!(w.model_contents(7), 0);
}

#[test]
fn a_viewer_sees_its_own_cluster_and_a_box_across_the_split_is_in_both() {
    let w = MapSpec {
        split_x: Some(0.0),
        ..Default::default()
    }
    .world();
    let east = w.pvs_at([100.0, 0.0, 0.0]).expect("in a cluster");
    let west = w.pvs_at([-100.0, 0.0, 0.0]).expect("in a cluster");
    assert!(east.sees(0) && !east.sees(1));
    assert!(west.sees(1) && !west.sees(0));
    assert_eq!(w.box_clusters([50.0; 3], [60.0; 3]), Some(vec![0]));
    assert_eq!(w.box_clusters([-10.0; 3], [10.0; 3]), Some(vec![0, 1]));
}

#[test]
fn a_map_without_visibility_data_shows_everything() {
    let w = room();
    let eye = w.pvs_at([0.0, 0.0, 10.0]).expect("in a cluster");
    assert!(eye.sees(0) && eye.sees(7));
}
