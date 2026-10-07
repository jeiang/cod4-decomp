// SPDX-License-Identifier: GPL-3.0-or-later
//! Synthetic skeleton, sampling and hit tests: hand-built models and animations whose answers
//! follow from the geometry.

use super::anim::{self, Accum, NO_BONE};
use super::hitloc::{self, BULLET_PRIORITY, HitLocation, RIFLE_PRIORITY, Stance};
use super::*;
use assets::zone::xanim::{Indices, XAnimParts};
use assets::zone::xmodel::{BaseMat, BoneInfo, LodInfo, XModel};
use std::sync::Arc;

const EPS: f32 = 1e-3;

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < EPS
}

fn near3(a: [f32; 3], b: [f32; 3]) -> bool {
    near(a[0], b[0]) && near(a[1], b[1]) && near(a[2], b[2])
}

struct B {
    name: &'static str,
    /// Global bone index of the parent; `None` for a root.
    parent: Option<usize>,
    rest_q: [i16; 4],
    rest_t: [f32; 3],
    class: u8,
    /// Box mins/maxs, bone-local; the sphere is sized to contain it.
    hull: Option<([f32; 3], [f32; 3])>,
}

fn bone(name: &'static str, parent: Option<usize>, t: [f32; 3]) -> B {
    B {
        name,
        parent,
        rest_q: [0, 0, 0, 32767],
        rest_t: t,
        class: 0,
        hull: None,
    }
}

fn model(name: &str, bones: &[B]) -> (Arc<XModel>, Vec<&'static str>) {
    let roots = bones.iter().take_while(|b| b.parent.is_none()).count();
    let n = bones.len();
    let lod = || LodInfo {
        dist: 0.0,
        surf_count: 0,
        surf_index: 0,
        part_bits: [0; 4],
        lod: 0,
        smc_index_plus_one: 0,
        smc_alloc_bits: 0,
    };
    let m = XModel {
        name: Some(name.into()),
        num_bones: n as u8,
        num_root_bones: roots as u8,
        lod_ramp_type: 0,
        bone_names: (0..n as u16).collect(),
        parent_list: bones[roots..]
            .iter()
            .enumerate()
            .map(|(i, b)| (roots + i - b.parent.unwrap()) as u8)
            .collect(),
        quats: bones[roots..].iter().map(|b| b.rest_q).collect(),
        trans: bones[roots..].iter().flat_map(|b| b.rest_t).collect(),
        part_classification: bones.iter().map(|b| b.class).collect(),
        base_mat: (0..n)
            .map(|_| BaseMat {
                quat: [0.0, 0.0, 0.0, 1.0],
                trans: [0.0; 3],
                trans_weight: 0.0,
            })
            .collect(),
        surfs: Arc::new([]),
        materials: Arc::new([]),
        lod_info: [lod(), lod(), lod(), lod()],
        coll_surfs: Arc::new([]),
        contents: 0,
        bone_info: bones
            .iter()
            .map(|b| match b.hull {
                Some((mins, maxs)) => {
                    let off = [0, 1, 2].map(|k| (mins[k] + maxs[k]) * 0.5);
                    let r2 = (0..3)
                        .map(|k| ((maxs[k] - mins[k]) * 0.5).powi(2))
                        .sum::<f32>();
                    BoneInfo {
                        bounds: [mins, maxs],
                        offset: off,
                        radius_squared: r2 + 1.0,
                    }
                }
                None => BoneInfo {
                    bounds: [[0.0; 3]; 2],
                    offset: [0.0; 3],
                    radius_squared: 0.0,
                },
            })
            .collect(),
        radius: 0.0,
        mins: [0.0; 3],
        maxs: [0.0; 3],
        num_lods: 1,
        coll_lod: 0,
        mem_usage: 0,
        flags: 0,
        bad: false,
        phys_preset: None,
        phys_geoms: None,
    };
    (Arc::new(m), bones.iter().map(|b| b.name).collect())
}

fn rig_of(models: &[(&Arc<XModel>, &[&str], Option<&str>)]) -> Rig {
    let specs: Vec<RigModel> = models
        .iter()
        .map(|(m, names, attach)| RigModel {
            model: (*m).clone(),
            bone_names: names,
            attach: *attach,
        })
        .collect();
    Rig::new(&specs).unwrap()
}

fn anim(num_frames: u16, looping: bool) -> XAnimParts {
    XAnimParts {
        name: None,
        num_frames,
        looping,
        has_delta: false,
        bone_counts: [0; 10],
        asset_type: 0,
        is_default: false,
        frame_rate: 30.0,
        frequency: 0.0,
        names: Vec::new(),
        notify: Vec::new(),
        delta: None,
        data_byte: Vec::new(),
        data_short: Vec::new(),
        data_int: Vec::new(),
        random_data_short: Vec::new(),
        random_data_byte: Vec::new(),
        random_data_int: Vec::new(),
        indices: Indices::None,
    }
}

fn q16(q: f32) -> i16 {
    (q * 32767.0).round() as i16
}

fn yaw_frame(deg: f32) -> [i16; 2] {
    let h = deg.to_radians() * 0.5;
    [q16(h.sin()), q16(h.cos())]
}

fn f32_bits(v: f32) -> i32 {
    v.to_bits() as i32
}

fn yaw_of(q: &[f32; 4]) -> f32 {
    2.0 * q[2].atan2(q[3]).to_degrees()
}

// --- sampling --------------------------------------------------------------------------

#[test]
fn yaw_track_interpolates_between_keys() {
    // One named part with a 2-key yaw track over 10 frames: key 0 at 0 degrees, key 1 at 90.
    let mut a = anim(10, false);
    a.bone_counts = [0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
    a.names = vec![0];
    a.data_short = vec![1]; // tableSize
    a.data_byte = vec![0, 10]; // frame numbers of the keys
    let (f0, f1) = (yaw_frame(0.0), yaw_frame(90.0));
    a.random_data_short = vec![f0[0], f0[1], f1[0], f1[1]];
    let mut acc = [Accum::ZERO];
    anim::accumulate(&a, &[0], 0.5, 1.0, &mut acc);
    let (q, t) = acc[0].finish();
    assert!(near(yaw_of(&q.unwrap()), 45.0), "{:?}", q);
    assert!(t.is_none(), "a rotation track gives no translation");

    let mut acc = [Accum::ZERO];
    anim::accumulate(&a, &[0], 1.0, 1.0, &mut acc);
    assert!(
        near(yaw_of(&acc[0].finish().0.unwrap()), 90.0),
        "time 1.0 takes the last key"
    );
}

#[test]
fn translation_tracks_scale_into_their_box() {
    // Bone 0: byte track, mins (10, 0, 0), extent (0.1, 0, 0) per step. Bone 1: constant.
    // Rotation groups: both bones get "identity".
    let mut a = anim(4, true);
    a.bone_counts = [2, 0, 0, 0, 0, 1, 0, 1, 0, 2];
    a.names = vec![0, 1];
    // group 5: part index byte then tableSize word and key table in data_byte.
    a.data_byte = vec![
        0, /* part */ 0, 4, /* keys 0 and 4 */
        /* group 7 part */ 1,
    ];
    a.data_short = vec![1];
    a.random_data_byte = vec![0, 0, 0, 200, 0, 0];
    a.data_int = vec![
        f32_bits(10.0),
        f32_bits(0.0),
        f32_bits(0.0),
        f32_bits(0.1),
        f32_bits(0.0),
        f32_bits(0.0),
        f32_bits(1.0),
        f32_bits(2.0),
        f32_bits(3.0),
    ];
    let mut acc = [Accum::ZERO; 2];
    anim::accumulate(&a, &[0, 1], 0.5, 1.0, &mut acc);
    let (_, t0) = acc[0].finish();
    assert!(near3(t0.unwrap(), [10.0 + 0.1 * 100.0, 0.0, 0.0]), "{t0:?}");
    let (q1, t1) = acc[1].finish();
    assert!(near3(t1.unwrap(), [1.0, 2.0, 3.0]));
    assert_eq!(q1.unwrap(), [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn long_tracks_skip_their_coarse_table() {
    // 300 frames (wide indices). Part 0: a 65-key yaw track (>= 64 keys carries a coarse table in
    // data_short). Part 1 follows it with a constant 90 degree yaw, so a cursor error shows up.
    let mut a = anim(300, true);
    a.bone_counts = [0, 1, 0, 1, 0, 0, 0, 0, 0, 2];
    a.names = vec![0, 1];
    let keys: Vec<u16> = (0..=64).map(|i| (i * 300 / 64) as u16).collect();
    assert_eq!(*keys.last().unwrap(), 300);
    // Part 0 yaws 0..64 degrees across its keys (1 degree per key).
    let mut frames = Vec::new();
    for i in 0..=64 {
        frames.extend(yaw_frame(i as f32));
    }
    a.random_data_short = frames;
    a.indices = Indices::Short(keys.clone());
    // tableSize, coarse table (finalTableSize + 1 = 2 entries), then part 1's constant.
    let c = yaw_frame(90.0);
    a.data_short = vec![64, 0, 300, c[0], c[1]];
    let mut acc = [Accum::ZERO; 2];
    // Frame 150 lies in key 32 (150 * 64 / 300 = 32.0 exactly: keys[32] = 150).
    anim::accumulate(&a, &[0, 1], 150.0 / 300.0, 1.0, &mut acc);
    assert!(
        near(yaw_of(&acc[0].finish().0.unwrap()), 32.0),
        "{:?}",
        acc[0]
    );
    assert!(
        near(yaw_of(&acc[1].finish().0.unwrap()), 90.0),
        "{:?}",
        acc[1]
    );
    // Between keys 10 (frame 46) and 11 (frame 51): 3/5 of the way is frame 49.
    let mut acc = [Accum::ZERO; 2];
    anim::accumulate(&a, &[0, 1], 49.0 / 300.0, 1.0, &mut acc);
    // (The 16-bit quantisation of the key quaternions allows a few hundredths of a degree.)
    assert!(
        (yaw_of(&acc[0].finish().0.unwrap()) - 10.6).abs() < 0.05,
        "{:?}",
        acc[0]
    );
}

#[test]
fn layers_blend_by_weight_and_unmapped_parts_are_skipped() {
    let mut a = anim(10, true);
    a.bone_counts = [0, 0, 0, 2, 0, 0, 0, 0, 0, 2];
    a.names = vec![0, 1];
    let (p, q) = (yaw_frame(0.0), yaw_frame(80.0));
    a.data_short = vec![p[0], p[1], q[0], q[1]];
    let mut acc = [Accum::ZERO; 2];
    anim::accumulate(&a, &[0, NO_BONE], 0.0, 1.0, &mut acc);
    assert_eq!(acc[1], Accum::ZERO, "NO_BONE parts are dropped");
    // Part 1 mapped to bone 1, weights 0.25 and 0.75 of the same animation: part 0 stays 0,
    // part 1 stays 80 whatever the weights; two animations disagreeing blend.
    let mut b = anim(10, true);
    b.bone_counts = [0, 0, 0, 1, 0, 0, 0, 0, 0, 1];
    b.names = vec![0];
    let r = yaw_frame(40.0);
    b.data_short = vec![r[0], r[1]];
    let mut acc = [Accum::ZERO; 2];
    anim::accumulate(&a, &[0, 1], 0.0, 0.5, &mut acc);
    anim::accumulate(&b, &[0, NO_BONE], 0.0, 0.5, &mut acc);
    assert!(near(yaw_of(&acc[0].finish().0.unwrap()), 20.0));
    assert!(
        near(yaw_of(&acc[1].finish().0.unwrap()), 80.0),
        "bone 1 only had one layer"
    );
}

// --- skeleton --------------------------------------------------------------------------

fn pose_of(rig: &Rig, layers: &[AnimLayer], ctl: &Controllers) -> Pose {
    let mut p = Pose::default();
    rig.pose(layers, ctl, &mut p);
    p
}

fn chain() -> (Arc<XModel>, Vec<&'static str>) {
    model(
        "chain",
        &[
            bone("tag_origin", None, [0.0; 3]),
            bone("a", Some(0), [0.0, 0.0, 10.0]),
            bone("b", Some(1), [5.0, 0.0, 0.0]),
        ],
    )
}

#[test]
fn rest_pose_stacks_offsets() {
    let (m, names) = chain();
    let rig = rig_of(&[(&m, &names, None)]);
    let p = pose_of(&rig, &[], &Controllers::NONE);
    assert!(near3(p.bones[2].trans, [5.0, 0.0, 10.0]));
}

#[test]
fn animated_rotation_swings_children_about_the_parent() {
    let (m, names) = chain();
    let rig = rig_of(&[(&m, &names, None)]);
    // Bone "a" yawed 90 degrees: its child's +x offset becomes +y.
    let mut a = anim(10, true);
    a.bone_counts = [0, 0, 0, 1, 0, 0, 0, 0, 0, 1];
    a.names = vec![0];
    let y = yaw_frame(90.0);
    a.data_short = vec![y[0], y[1]];
    let bind = rig.bind(&["A"]);
    assert_eq!(bind.matched(), 1, "bone names compare case-insensitively");
    let layer = AnimLayer {
        anim: &a,
        bind: &bind,
        time: 0.0,
        weight: 1.0,
    };
    let p = pose_of(&rig, &[layer], &Controllers::NONE);
    assert!(
        near3(p.bones[2].trans, [0.0, 5.0, 10.0]),
        "{:?}",
        p.bones[2].trans
    );
    assert!(near(yaw_of(&p.bones[1].quat), 90.0));
}

#[test]
fn root_controller_moves_and_turns_the_whole_body() {
    let (m, names) = chain();
    let rig = rig_of(&[(&m, &names, None)]);
    let ctl = Controllers {
        tag_origin_angles: [0.0, 90.0, 0.0],
        tag_origin_offset: [1.0, 2.0, 0.0],
        ..Controllers::NONE
    };
    let p = pose_of(&rig, &[], &ctl);
    // Rest offset (5, 0, 10) turned by 90 degrees of yaw, then shifted.
    assert!(
        near3(p.bones[2].trans, [1.0, 7.0, 10.0]),
        "{:?}",
        p.bones[2].trans
    );
}

#[test]
fn controller_bones_rotate_in_the_root_frame() {
    let (m, names) = model(
        "ctl",
        &[
            bone("tag_origin", None, [0.0; 3]),
            bone("j_spine", Some(0), [0.0, 0.0, 10.0]),
            bone("back_low", Some(1), [0.0, 0.0, 10.0]),
            bone("tip", Some(2), [4.0, 0.0, 0.0]),
        ],
    );
    let rig = rig_of(&[(&m, &names, None)]);
    assert_eq!(rig.controller_bone(0), Some(2));
    // Identity controller angles with a turned root: the control bone follows its parent only.
    let turned = Controllers {
        tag_origin_angles: [0.0, 90.0, 0.0],
        ..Controllers::NONE
    };
    let p = pose_of(&rig, &[], &turned);
    assert!(
        near(yaw_of(&p.bones[2].quat), 90.0),
        "parent's 90 degrees, not 180"
    );
    // Pitching the control bone by 90 degrees (nose down) swings the tip from +x to -z.
    let mut ctl = Controllers::NONE;
    ctl.angles[0] = [90.0, 0.0, 0.0];
    let p = pose_of(&rig, &[], &ctl);
    assert!(
        near3(p.bones[3].trans, [0.0, 0.0, 16.0]),
        "{:?}",
        p.bones[3].trans
    );
}

#[test]
fn melded_model_bones_copy_the_matching_body_bone() {
    let (body, bn) = model(
        "body",
        &[
            bone("tag_origin", None, [0.0; 3]),
            bone("spine", Some(0), [0.0, 0.0, 10.0]),
        ],
    );
    let (head, hn) = model(
        "head",
        &[
            bone("spine", None, [0.0; 3]),
            bone("skull", Some(0), [0.0, 0.0, 7.0]),
        ],
    );
    let rig = rig_of(&[(&body, &bn, None), (&head, &hn, None)]);
    assert_eq!(rig.len(), 4);
    // Move the body's spine with an animation: the head's copy and its child follow.
    let mut a = anim(10, true);
    a.bone_counts = [0, 0, 0, 1, 0, 0, 0, 0, 0, 1];
    a.names = vec![0];
    let y = yaw_frame(0.0);
    a.data_short = vec![y[0], y[1]];
    let bind = rig.bind(&["spine"]);
    assert_eq!(
        bind.as_slice(),
        &[1],
        "binds to the first bone of that name"
    );
    let layer = AnimLayer {
        anim: &a,
        bind: &bind,
        time: 0.0,
        weight: 1.0,
    };
    let p = pose_of(&rig, &[layer], &Controllers::NONE);
    assert_eq!(p.bones[2], p.bones[1]);
    assert!(near3(p.bones[3].trans, [0.0, 0.0, 17.0]));
}

#[test]
fn attached_model_roots_hang_off_the_tag() {
    let (body, bn) = model(
        "body",
        &[
            bone("tag_origin", None, [0.0; 3]),
            bone("hand", Some(0), [3.0, 0.0, 4.0]),
        ],
    );
    let (gun, gn) = model("gun", &[bone("gun_root", None, [0.0, 1.0, 0.0])]);
    let rig = rig_of(&[(&body, &bn, None), (&gun, &gn, Some("hand"))]);
    let p = pose_of(&rig, &[], &Controllers::NONE);
    // A root has no rest translation of its own; it sits on the tag.
    assert!(
        near3(p.bones[2].trans, [3.0, 0.0, 4.0]),
        "{:?}",
        p.bones[2].trans
    );
}

// --- hit volumes -----------------------------------------------------------------------

fn boxed(name: &'static str, parent: usize, t: [f32; 3], class: u8, half: f32) -> B {
    B {
        hull: Some(([-half; 3], [half; 3])),
        class,
        ..bone(name, Some(parent), t)
    }
}

fn target() -> (Rig, Pose) {
    // A torso box with a helmet box above it; the helmet is 'helmet', the torso 'torso_lower'.
    let (m, names) = model(
        "t",
        &[
            bone("tag_origin", None, [0.0; 3]),
            boxed(
                "torso",
                0,
                [0.0, 0.0, 10.0],
                HitLocation::TorsoLower as u8,
                6.0,
            ),
            boxed(
                "helmet",
                1,
                [0.0, 0.0, 10.0],
                HitLocation::Helmet as u8,
                4.0,
            ),
        ],
    );
    let rig = rig_of(&[(&m, &names, None)]);
    let pose = pose_of(&rig, &[], &Controllers::NONE);
    (rig, pose)
}

#[test]
fn horizontal_segment_enters_the_box_face() {
    let (rig, pose) = target();
    let h = locational_trace(
        &rig,
        &pose,
        &[-30.0, 0.0, 10.0],
        &[30.0, 0.0, 10.0],
        &BULLET_PRIORITY,
        1.0,
    )
    .unwrap();
    assert_eq!(h.location(), HitLocation::TorsoLower);
    assert!(near(h.fraction, 24.0 / 60.0), "{}", h.fraction);
    assert!(near3(h.normal, [-1.0, 0.0, 0.0]), "{:?}", h.normal);
    assert_eq!(rig.bone_name(h.bone), "torso");
    // Beside the box and past the end of the segment: no hit.
    assert!(
        locational_trace(
            &rig,
            &pose,
            &[-30.0, 9.0, 10.0],
            &[30.0, 9.0, 10.0],
            &BULLET_PRIORITY,
            1.0
        )
        .is_none()
    );
    assert!(
        locational_trace(
            &rig,
            &pose,
            &[-30.0, 0.0, 10.0],
            &[-10.0, 0.0, 10.0],
            &BULLET_PRIORITY,
            1.0
        )
        .is_none()
    );
    // A nearer obstruction (max_fraction) hides it.
    assert!(
        locational_trace(
            &rig,
            &pose,
            &[-30.0, 0.0, 10.0],
            &[30.0, 0.0, 10.0],
            &BULLET_PRIORITY,
            0.3
        )
        .is_none()
    );
}

#[test]
fn start_inside_a_box_is_not_a_hit_when_leaving() {
    let (rig, pose) = target();
    assert!(
        locational_trace(
            &rig,
            &pose,
            &[0.0, 0.0, 10.0],
            &[30.0, 0.0, 10.0],
            &BULLET_PRIORITY,
            1.0
        )
        .is_none()
    );
}

#[test]
fn rifle_priority_credits_the_helmet_through_the_torso() {
    let (rig, pose) = target();
    // Rising from +x: enters the torso box first, then crosses into the helmet box above it.
    let (start, end) = ([10.0, 0.0, 8.0], [-10.0, 0.0, 22.0]);
    let bullet = locational_trace(&rig, &pose, &start, &end, &BULLET_PRIORITY, 1.0).unwrap();
    let rifle = locational_trace(&rig, &pose, &start, &end, &RIFLE_PRIORITY, 1.0).unwrap();
    assert_eq!(
        bullet.location(),
        HitLocation::TorsoLower,
        "nearest part wins at equal priority"
    );
    assert_eq!(
        rifle.location(),
        HitLocation::Helmet,
        "rifle rounds prefer the head"
    );
    assert!(rifle.fraction > bullet.fraction);
}

#[test]
fn unclassified_bone_takes_the_parents_location() {
    let (m, names) = model(
        "inherit",
        &[
            bone("tag_origin", None, [0.0; 3]),
            boxed(
                "arm",
                0,
                [0.0, 0.0, 10.0],
                HitLocation::LeftArmUpper as u8,
                3.0,
            ),
            boxed("wrist", 1, [0.0, 10.0, 0.0], HitLocation::None as u8, 3.0),
        ],
    );
    let rig = rig_of(&[(&m, &names, None)]);
    let pose = pose_of(&rig, &[], &Controllers::NONE);
    let h = locational_trace(
        &rig,
        &pose,
        &[-20.0, 10.0, 10.0],
        &[20.0, 10.0, 10.0],
        &BULLET_PRIORITY,
        1.0,
    )
    .unwrap();
    assert_eq!(rig.bone_name(h.bone), "wrist");
    assert_eq!(h.location(), HitLocation::LeftArmUpper);
}

#[test]
fn gun_and_volumeless_bones_are_never_hit() {
    let (m, names) = model(
        "gun",
        &[
            bone("tag_origin", None, [0.0; 3]),
            boxed("gun", 0, [0.0, 0.0, 10.0], HitLocation::Gun as u8, 5.0),
            bone("empty", Some(0), [0.0, 0.0, 30.0]),
        ],
    );
    let rig = rig_of(&[(&m, &names, None)]);
    let pose = pose_of(&rig, &[], &Controllers::NONE);
    assert!(
        locational_trace(
            &rig,
            &pose,
            &[-20.0, 0.0, 10.0],
            &[20.0, 0.0, 10.0],
            &BULLET_PRIORITY,
            1.0
        )
        .is_none()
    );
}

#[test]
fn trace_player_rotates_into_the_entity_frame() {
    let (rig, pose) = target();
    // The same shot at the torso as seen from an entity at (100, 200) facing +y: the shooter is
    // 30 units in front of the entity's left (its local -x is world -y here).
    let origin = [100.0, 200.0, 0.0];
    let angles = [0.0, 90.0, 0.0];
    let start = Pose::to_world(&[-30.0, 0.0, 10.0], &origin, 90.0);
    let end = Pose::to_world(&[30.0, 0.0, 10.0], &origin, 90.0);
    assert!(near3(start, [100.0, 170.0, 10.0]), "{start:?}");
    let h = trace_player(
        &rig,
        &pose,
        &Placement { origin, angles },
        &start,
        &end,
        &BULLET_PRIORITY,
        1.0,
    )
    .unwrap();
    assert_eq!(h.location(), HitLocation::TorsoLower);
    assert!(near(h.fraction, 0.4));
    // Local -x face, world normal = local -x turned by 90 degrees = -y.
    assert!(near3(h.normal, [0.0, -1.0, 0.0]), "{:?}", h.normal);
}

// --- locations -------------------------------------------------------------------------

#[test]
fn hit_location_names_round_trip() {
    for i in 0..hitloc::COUNT as u8 {
        let l = HitLocation::from_index(i).unwrap();
        assert_eq!(l as u8, i);
        assert_eq!(HitLocation::from_name(l.name()), Some(l));
        assert_eq!(hitloc::from_name(&l.name().to_uppercase()), Some(i));
        assert_eq!(hitloc::name(i), l.name());
    }
    assert_eq!(HitLocation::from_index(19), None);
    assert_eq!(HitLocation::from_name("shin"), None);
    assert_eq!(
        hitloc::NAMES[HitLocation::TorsoUpper as usize],
        "torso_upper"
    );
    assert!(
        HitLocation::Helmet.is_head()
            && HitLocation::Head.is_head()
            && !HitLocation::Neck.is_head()
    );
}

#[test]
fn box_fallback_classifies_by_height_and_side() {
    let origin = [0.0, 0.0, 0.0];
    let shot = |z: f32, y: f32| {
        box_hit_location(&[100.0, y, z], &[-100.0, y, z], &origin, 0.0, Stance::Stand)
    };
    assert_eq!(shot(66.0, 0.0).unwrap().1, HitLocation::Head);
    assert_eq!(shot(50.0, 0.0).unwrap().1, HitLocation::TorsoUpper);
    assert_eq!(shot(35.0, 0.0).unwrap().1, HitLocation::TorsoLower);
    assert_eq!(shot(25.0, 5.0).unwrap().1, HitLocation::LeftLegUpper);
    assert_eq!(shot(25.0, -5.0).unwrap().1, HitLocation::RightLegUpper);
    assert_eq!(shot(10.0, -5.0).unwrap().1, HitLocation::RightLegLower);
    assert_eq!(shot(2.0, 5.0).unwrap().1, HitLocation::LeftFoot);
    let (f, _) = shot(66.0, 0.0).unwrap();
    assert!(
        near(f, 0.425),
        "enters the hull 15 units from the centre: {f}"
    );
    assert!(shot(80.0, 0.0).is_none(), "above the hull");
    assert!(shot(30.0, 20.0).is_none(), "beside the hull");
    // A crouching hull is 50 high: 45 is its head, not its chest.
    let crouch = box_hit_location(
        &[100.0, 0.0, 45.0],
        &[-100.0, 0.0, 45.0],
        &origin,
        0.0,
        Stance::Crouch,
    );
    assert_eq!(crouch.unwrap().1, HitLocation::Head);
}

#[test]
fn box_fallback_left_is_relative_to_facing() {
    // Facing +y (yaw 90) the player's left is world -x.
    let side = |x: f32| {
        box_hit_location(
            &[x, 100.0, 25.0],
            &[x, -100.0, 25.0],
            &[0.0; 3],
            90.0,
            Stance::Stand,
        )
        .unwrap()
        .1
    };
    assert_eq!(side(-5.0), HitLocation::LeftLegUpper);
    assert_eq!(side(5.0), HitLocation::RightLegUpper);
}
