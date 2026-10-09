// SPDX-License-Identifier: GPL-3.0-only
//! Player animation and locational hits on the stock player models. Skips without `COD4_PATH`.

use server::content::{Content, Install};
use server::playeranim::{PlayerAnims, PlayerPoseInput, PlayerPoseState, StanceInput};
use sim::skel::Pose;
use sim::skel::hitloc::HitLocation;
use std::path::PathBuf;

fn content() -> Option<Content> {
    content_for(false)
}

/// `client`: keep what a client keeps, the upper-body `pt_*` clips among it.
fn content_for(client: bool) -> Option<Content> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    let install = Install::open(&root).expect("install");
    let mut c = Content::default();
    c.client = client;
    c.load_boot(&install).expect("boot zones");
    c.load_map(&install, "mp_crash").expect("mp_crash");
    Some(c)
}

fn head_top(anims: &PlayerAnims, s: &PlayerPoseState) -> f32 {
    let mut top = 0.0f32;
    for z in (0..90).rev() {
        for y in [-6.0, -3.0, 0.0, 3.0, 6.0] {
            let z = z as f32;
            if s.trace(
                anims,
                &[0.0; 3],
                &[200.0, y, z],
                &[-200.0, y, z],
                false,
                1.0,
            )
            .is_some()
            {
                top = top.max(z);
            }
        }
        if top > 0.0 {
            break;
        }
    }
    top
}

#[test]
fn every_selectable_animation_exists_and_binds() {
    let Some(c) = content() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let n = c.player_anims().count();
    let bytes: usize = c.player_anims().map(|a| a.data_bytes()).sum();
    println!(
        "{n} player anims retained, {:.2} MiB keyframe data",
        bytes as f64 / 1048576.0
    );
    assert!(n > 250);
    for (body, head) in [
        ("body_mp_usmc_assault", "head_mp_usmc_tactical_mich"),
        ("body_mp_usmc_sniper", "head_mp_usmc_shaved_head"),
        ("body_mp_arab_regular_cqb", "head_mp_arab_regular_headwrap"),
        (
            "body_mp_arab_regular_engineer",
            "head_mp_arab_regular_ski_mask",
        ),
    ] {
        let a = PlayerAnims::new(&c, body, Some(head)).unwrap_or_else(|e| panic!("{body}: {e}"));
        assert!(
            a.missing().is_empty(),
            "{body}: stock zones lack {:?}",
            a.missing()
        );
        assert!(a.rig().len() <= sim::skel::MAX_BONES);
    }
}

#[test]
fn a_played_through_round_of_stances_and_a_death() {
    let Some(c) = content() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let anims = PlayerAnims::new(
        &c,
        "body_mp_usmc_assault",
        Some("head_mp_usmc_tactical_mich"),
    )
    .unwrap();
    let mut s = PlayerPoseState::default();
    let mut input = PlayerPoseInput {
        yaw: 0.0,
        ..Default::default()
    };
    let step = |s: &mut PlayerPoseState, input: &PlayerPoseInput, frames: u32| {
        for _ in 0..frames {
            s.update(1.0 / 30.0, input);
        }
    };
    step(&mut s, &input, 30);
    let stand = head_top(&anims, &s);
    input.stance = StanceInput::Crouch;
    step(&mut s, &input, 30);
    let crouch = head_top(&anims, &s);
    input.stance = StanceInput::Prone;
    step(&mut s, &input, 30);
    let prone = head_top(&anims, &s);
    println!("head top stand {stand} crouch {crouch} prone {prone}");
    assert!((60.0..=70.0).contains(&stand), "{stand}");
    assert!(crouch < stand - 15.0 && crouch > 38.0, "{crouch}");
    assert!((11.0..=24.0).contains(&prone), "{prone}");

    // A shot at the standing head from the front, in a rotated frame: facing +y at (500, 700).
    input.stance = StanceInput::Stand;
    input.yaw = 90.0;
    step(&mut s, &input, 30);
    let mut pose = Pose::default();
    s.pose(&anims, &mut pose);
    let head = pose.bones[anims.rig().bone_index("j_head").unwrap()].trans;
    let origin = [500.0, 700.0, 0.0];
    let target = Pose::to_world(&head, &origin, 90.0);
    let (muzzle, far) = (
        [target[0], target[1] + 150.0, target[2]],
        [target[0], target[1] - 150.0, target[2]],
    );
    let hit = s
        .trace(&anims, &origin, &muzzle, &far, true, 1.0)
        .expect("hit");
    assert!(hit.location().is_head(), "{:?}", hit.location());
    assert!(hit.normal[1] > 0.0, "{:?}", hit.normal);
    // Same shot at the left shin, halfway between knee and ankle.
    let at = |name: &str| pose.bones[anims.rig().bone_index(name).unwrap()].trans;
    let (knee, ankle) = (at("j_knee_le"), at("j_ankle_le"));
    let mid = [0, 1, 2].map(|k| (knee[k] + ankle[k]) * 0.5);
    let shin = Pose::to_world(&mid, &origin, 90.0);
    let hit = s
        .trace(
            &anims,
            &origin,
            &[shin[0], shin[1] + 150.0, shin[2]],
            &[shin[0], shin[1] - 150.0, shin[2]],
            false,
            1.0,
        )
        .expect("shin hit");
    assert_eq!(
        hit.location(),
        HitLocation::LeftLegLower,
        "{:?}",
        rig_name(&anims, hit.bone)
    );

    // Death while running: the run death plays out and the corpse pose is still a body.
    input.speed = 190.0;
    input.trying_to_move = true;
    step(&mut s, &input, 10);
    input.dead = true;
    step(&mut s, &input, 1);
    assert_eq!(s.current(), Some("pb_death_run_forward_crumple"));
    step(&mut s, &input, 60);
    s.pose(&anims, &mut pose);
    assert!(
        pose.bones()
            .iter()
            .all(|b| b.trans.iter().all(|v| v.is_finite()))
    );
    let low = pose.bones()[anims.rig().bone_index("j_head").unwrap()].trans[2];
    println!("dead head z {low}");
    assert!(low < 30.0, "corpse head at {low}");
    let _ = HitLocation::Head;
}

fn rig_name(anims: &PlayerAnims, bone: usize) -> &str {
    anims.rig().bone_name(bone)
}

#[test]
fn a_torso_clip_moves_the_arms_and_leaves_the_legs() {
    let Some(c) = content_for(true) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let anims = PlayerAnims::new(
        &c,
        "body_mp_usmc_assault",
        Some("head_mp_usmc_tactical_mich"),
    )
    .unwrap();
    let bone = |p: &Pose, n: &str| p.bones[anims.rig().bone_index(n).unwrap()].trans;
    let play = |ws: u8, frames: u32| {
        let mut s = PlayerPoseState::default();
        let rest = PlayerPoseInput::default();
        s.update(1.0 / 30.0, &rest);
        s.update(
            1.0 / 30.0,
            &PlayerPoseInput {
                weapon_state: ws,
                ..rest
            },
        );
        for _ in 0..frames {
            s.update(
                1.0 / 30.0,
                &PlayerPoseInput {
                    weapon_state: ws,
                    ..rest
                },
            );
        }
        let mut p = Pose::default();
        s.pose(&anims, &mut p);
        (s.torso(&anims), p)
    };
    let (none, base) = play(0, 5);
    assert_eq!(none, None);
    for (ws, clip) in [
        (5u8, "pt_stand_shoot"),
        (7, "pt_reload_stand_rifle"),
        (12, "pt_melee_right2right_1"),
    ] {
        let (name, p) = play(ws, 5);
        assert!(name.is_some(), "{clip}: no torso clip playing");
        let d = |n: &str| {
            let (a, b) = (bone(&p, n), bone(&base, n));
            (0..3).map(|k| (a[k] - b[k]).abs()).sum::<f32>()
        };
        assert!(
            d("j_wrist_ri") > 0.5 || d("j_wrist_le") > 0.5,
            "{clip}: hands did not move"
        );
        assert!(d("j_ankle_le") < 0.01, "{clip}: the legs moved");
    }
}

#[test]
fn the_server_keeps_no_torso_clips() {
    let Some(c) = content() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    assert!(c.player_anim("pt_stand_shoot").is_none());
    assert!(c.player_anim("pb_stand_alert").is_some());
}
