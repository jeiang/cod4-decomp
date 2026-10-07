// SPDX-License-Identifier: GPL-3.0-or-later
//! Player skeleton against the stock player models and animations. Skipped without `COD4_PATH`.

use super::controllers::{self, ControllerInput};
use super::hitloc::BULLET_PRIORITY;
use super::*;
use assets::zone::xanim::XAnimParts;
use assets::zone::xmodel::XModel;
use assets::zone::{Asset, Consumer, Zone};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

pub(super) struct Data {
    pub models: HashMap<String, (Arc<XModel>, Vec<Arc<str>>)>,
    pub anims: HashMap<String, (Arc<XAnimParts>, Vec<Arc<str>>)>,
}

fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

pub(super) fn data() -> Option<&'static Data> {
    static DATA: LazyLock<Option<Data>> = LazyLock::new(|| {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return None;
        };
        let dir = find_ci(&find_ci(&PathBuf::from(root), "zone")?, "english")?;
        let mut d = Data {
            models: HashMap::new(),
            anims: HashMap::new(),
        };
        for zone in ["common_mp", "mp_crash"] {
            let file = std::fs::File::open(find_ci(&dir, &format!("{zone}.ff"))?).ok()?;
            let z = Zone::open(std::io::BufReader::new(file)).ok()?;
            let strings = z.script_strings().to_vec();
            let text = |i: u16| -> Arc<str> {
                strings
                    .get(usize::from(i))
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| Arc::from(""))
            };
            z.decode(&Consumer::Server, |a| match a {
                Asset::XModel(m) => {
                    if let Some(n) = m.name.clone() {
                        let names = m.bone_names.iter().map(|b| text(*b)).collect();
                        d.models.insert(n.to_string(), (m, names));
                    }
                }
                Asset::XAnimParts(x) => {
                    if let Some(n) = x.name.clone()
                        && n.starts_with("pb_")
                    {
                        let names = x.names.iter().map(|b| text(*b)).collect();
                        d.anims.insert(n.to_string(), (x, names));
                    }
                }
                _ => {}
            })
            .ok()?;
        }
        Some(d)
    });
    DATA.as_ref()
}

pub(super) fn player_rig(d: &Data) -> Rig {
    let (body, bn) = &d.models["body_mp_usmc_assault"];
    let (head, hn) = &d.models["head_mp_usmc_tactical_mich"];
    let bn: Vec<&str> = bn.iter().map(|s| &**s).collect();
    let hn: Vec<&str> = hn.iter().map(|s| &**s).collect();
    Rig::new(&[
        RigModel {
            model: body.clone(),
            bone_names: &bn,
            attach: None,
        },
        RigModel {
            model: head.clone(),
            bone_names: &hn,
            attach: None,
        },
    ])
    .unwrap()
}

fn pose_anim(d: &Data, rig: &Rig, name: &str, t: f32, ctl: &Controllers) -> Pose {
    let (a, names) = &d.anims[name];
    let bind = rig.bind(names);
    let mut pose = Pose::default();
    rig.pose(
        &[AnimLayer {
            anim: a,
            bind: &bind,
            time: t,
            weight: 1.0,
        }],
        ctl,
        &mut pose,
    );
    pose
}

#[test]
fn rest_pose_matches_base_mat() {
    let Some(d) = data() else { return };
    let rig = player_rig(d);
    let mut pose = Pose::default();
    rig.pose(&[], &Controllers::NONE, &mut pose);
    let body = rig.model(0);
    let mut worst = 0.0f32;
    for i in 0..usize::from(body.num_bones) {
        let b = &body.base_mat[i];
        let m = &pose.bones[i];
        for k in 0..3 {
            worst = worst.max((b.trans[k] - m.trans[k]).abs());
        }
        // Quaternions double-cover: compare the rotation of a probe vector.
        let probe = [1.0, 2.0, 3.0];
        let want = quat::rotate(&quat::normalize(&b.quat), &probe);
        let got = quat::rotate(&m.quat, &probe);
        for k in 0..3 {
            worst = worst.max((want[k] - got[k]).abs());
        }
    }
    println!("rest pose worst deviation from base_mat: {worst}");
    assert!(worst < 0.01, "worst {worst}");
}

#[test]
fn every_player_anim_samples_cleanly() {
    let Some(d) = data() else { return };
    let rig = player_rig(d);
    assert!(d.anims.len() > 250, "{} pb_ anims", d.anims.len());
    let mut worst_jump = 0.0f32;
    let mut worst_loop = 0.0f32;
    let mut pose = Pose::default();
    let mut next = Pose::default();
    let mut t_total = std::time::Duration::ZERO;
    let mut samples = 0u32;
    for (name, (a, names)) in &d.anims {
        let bind = rig.bind(names);
        assert!(
            bind.matched() >= 40,
            "{name}: only {} parts matched the rig",
            bind.matched()
        );
        let layer = |time: f32| AnimLayer {
            anim: a,
            bind: &bind,
            time,
            weight: 1.0,
        };
        for k in 0..=20 {
            let t = k as f32 / 20.0;
            let now = std::time::Instant::now();
            rig.pose(&[layer(t)], &Controllers::NONE, &mut pose);
            t_total += now.elapsed();
            samples += 1;
            for (i, m) in pose.bones().iter().enumerate() {
                let finite = m.quat.iter().chain(&m.trans).all(|v| v.is_finite());
                assert!(finite, "{name} t={t} bone {} not finite", rig.bone_name(i));
                let len = m.quat.iter().map(|c| c * c).sum::<f32>().sqrt();
                assert!(
                    (len - 1.0).abs() < 1e-3,
                    "{name} bone {} quat len {len}",
                    rig.bone_name(i)
                );
                let r = m.trans.iter().map(|c| c * c).sum::<f32>().sqrt();
                // Weapon attach tags in the pistol/hold animations roam far from the body.
                assert!(
                    r < 250.0 || rig.bone_name(i).starts_with("tag_"),
                    "{name} t={t} bone {} at {:?}",
                    rig.bone_name(i),
                    m.trans
                );
            }
        }
        // Continuity: a tiny step must not move any bone more than a fraction of a unit
        // (a misdecoded track would jump between keyframes).
        for k in 1..20 {
            let t = (k as f32 + 0.5) / 20.0;
            rig.pose(&[layer(t)], &Controllers::NONE, &mut pose);
            rig.pose(&[layer(t + 1e-4)], &Controllers::NONE, &mut next);
            for (m, n) in pose.bones().iter().zip(next.bones()) {
                let dist = math_dist(&m.trans, &n.trans);
                worst_jump = worst_jump.max(dist);
            }
        }
        if a.looping {
            rig.pose(&[layer(0.0)], &Controllers::NONE, &mut pose);
            rig.pose(&[layer(0.99999)], &Controllers::NONE, &mut next);
            for (i, (m, n)) in pose.bones().iter().zip(next.bones()).enumerate() {
                let dist = math_dist(&m.trans, &n.trans);
                if rig.bone_name(i) != "tag_origin" {
                    worst_loop = worst_loop.max(dist);
                }
            }
        }
    }
    println!(
        "{} anims: worst step jump {worst_jump:.3}, worst loop seam {worst_loop:.3}, {:.1} us/pose over {samples} poses",
        d.anims.len(),
        t_total.as_secs_f64() * 1e6 / f64::from(samples)
    );
    assert!(worst_jump < 1.0, "anim step jump {worst_jump}");
}

fn math_dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Hit locations of horizontal shots at height `z`, from eight directions across the body,
/// merged left/right.
fn profile(rig: &Rig, pose: &Pose, z: f32) -> std::collections::BTreeMap<&'static str, u32> {
    let mut set = std::collections::BTreeMap::new();
    for dir in 0..8 {
        let (s, c) = ((dir as f32) * 45.0f32.to_radians()).sin_cos();
        for yo in -10..=10 {
            let yo = yo as f32;
            let from = [200.0 * c - yo * s, 200.0 * s + yo * c, z];
            let to = [-200.0 * c - yo * s, -200.0 * s + yo * c, z];
            if let Some(h) = locational_trace(rig, pose, &from, &to, &BULLET_PRIORITY, 1.0) {
                let n = h.location().name();
                let n = n
                    .strip_prefix("left_")
                    .or_else(|| n.strip_prefix("right_"))
                    .unwrap_or(n);
                *set.entry(n).or_insert(0) += 1;
            }
        }
    }
    set
}

fn dominant(rig: &Rig, pose: &Pose, z: f32) -> &'static str {
    profile(rig, pose, z)
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map_or("none", |(k, _)| k)
}

/// Highest z any horizontal shot hits.
fn top(rig: &Rig, pose: &Pose) -> f32 {
    (0..90)
        .rev()
        .map(|z| z as f32)
        .find(|z| !profile(rig, pose, *z).is_empty())
        .unwrap()
}

#[test]
fn standing_pose_has_locations_at_the_expected_heights() {
    let Some(d) = data() else { return };
    let rig = player_rig(d);
    for t in [0.0, 0.5] {
        let pose = pose_anim(
            d,
            &rig,
            "pb_stand_alert",
            t,
            &controllers::compute(&ControllerInput::default()),
        );
        let head_top = top(&rig, &pose);
        assert!((60.0..=70.0).contains(&head_top), "standing top {head_top}");
        // The head bone itself sits in the 55-65 band (the helmet bone above it).
        let hz = pose.bones[rig.bone_index("j_head").unwrap()].trans[2];
        assert!((54.0..=66.0).contains(&hz), "j_head z {hz}");
        for (z, want) in [
            (62.0, "helmet"),
            (52.0, "torso_upper"),
            (38.0, "torso_lower"),
            (25.0, "leg_upper"),
            (10.0, "leg_lower"),
            (1.0, "foot"),
        ] {
            assert_eq!(
                dominant(&rig, &pose, z),
                want,
                "t={t} z={z}: {:?}",
                profile(&rig, &pose, z)
            );
        }
    }
}

#[test]
fn shots_at_the_head_from_the_front_are_head_or_helmet() {
    let Some(d) = data() else { return };
    let rig = player_rig(d);
    let pose = pose_anim(d, &rig, "pb_stand_alert", 0.0, &Controllers::NONE);
    let head = pose.bones[rig.bone_index("j_head").unwrap()].trans;
    for dz in [0.0, 4.0, 8.0] {
        let z = head[2] + dz;
        let h = locational_trace(
            &rig,
            &pose,
            &[head[0] + 150.0, head[1], z],
            &[head[0] - 150.0, head[1], z],
            &BULLET_PRIORITY,
            1.0,
        )
        .unwrap_or_else(|| panic!("miss at head+{dz}"));
        assert!(h.location().is_head(), "head+{dz}: {:?}", h.location());
        let n = h.normal;
        assert!(((n[0] * n[0] + n[1] * n[1] + n[2] * n[2]) - 1.0).abs() < 1e-3);
        assert!(
            n[0] > 0.0,
            "normal {n:?} faces away from a shot travelling in -x"
        );
    }
}

#[test]
fn stance_changes_the_height_of_the_hit_volumes() {
    let Some(d) = data() else { return };
    let rig = player_rig(d);
    let tops: Vec<f32> = [
        ("pb_stand_alert", false),
        ("pb_crouch_alert", false),
        ("pb_prone_aim", true),
    ]
    .into_iter()
    .map(|(n, prone)| {
        let ctl = controllers::compute(&ControllerInput {
            prone,
            ..Default::default()
        });
        top(&rig, &pose_anim(d, &rig, n, 0.0, &ctl))
    })
    .collect();
    println!("stand/crouch/prone top: {tops:?}");
    assert!(tops[0] - tops[1] > 15.0, "{tops:?}");
    assert!((38.0..=50.0).contains(&tops[1]), "crouch {tops:?}");
    assert!((11.0..=24.0).contains(&tops[2]), "prone {tops:?}");
}

#[test]
fn running_and_aiming_poses_stay_on_the_body() {
    let Some(d) = data() else { return };
    let rig = player_rig(d);
    // Looking 60 degrees down or 60 up, running: still a standing-height, finite body.
    for pitch in [-60.0, 0.0, 60.0] {
        let ctl = controllers::compute(&ControllerInput {
            view_pitch: pitch,
            move_dir: 40.0,
            ..Default::default()
        });
        let pose = pose_anim(d, &rig, "pb_combatrun_forward_loop", 0.3, &ctl);
        let t = top(&rig, &pose);
        assert!((50.0..=75.0).contains(&t), "pitch {pitch}: top {t}");
    }
}
