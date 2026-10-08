// SPDX-License-Identifier: GPL-3.0-only
//! The helicopter's rotors: the original's `CG_GetHelicopterAnims` loops the `bh_rotors` animation on the vehicle's
//! model, so the main and tail rotors turn while everything else stays at the model's rest pose.

use assets::zone::xmodel::XModel;
use server::content::{Content, PlayerAnim};
use sim::skel::{AnimBinding, AnimLayer, BoneMat, Controllers, Pose, Rig, RigModel};
use std::sync::Arc;

/// The animation the stock client plays on every helicopter.
pub const ROTOR_ANIM: &str = "bh_rotors";

/// A helicopter model and the looping rotor animation, posed from a clock.
pub struct Rotors {
    rig: Rig,
    bind: AnimBinding,
    anim: Arc<PlayerAnim>,
    /// Seconds one turn of the animation takes.
    length: f32,
    pose: Pose,
}

impl Rotors {
    /// Fails when the model or the animation is not in `content`.
    pub fn new(content: &Content, model: &Arc<XModel>) -> Result<Self, String> {
        let name = model.name.as_deref().unwrap_or("?");
        let names = content
            .model_bone_names(name)
            .ok_or_else(|| format!("model {name} has no bone names"))?;
        let names: Vec<&str> = names.iter().map(|n| &**n).collect();
        let rig = Rig::new(&[RigModel {
            model: model.clone(),
            bone_names: &names,
            attach: None,
        }])?;
        let anim = content
            .player_anim(ROTOR_ANIM)
            .ok_or_else(|| format!("animation {ROTOR_ANIM} not loaded"))?
            .clone();
        let bind = rig.bind(&anim.part_names);
        let length = (f32::from(anim.parts.num_frames) / anim.parts.frame_rate).max(0.001);
        Ok(Self {
            rig,
            bind,
            anim,
            length,
            pose: Pose::default(),
        })
    }

    /// The bones with the rotors at `clock` seconds into the loop.
    pub fn pose(&mut self, clock: f32) -> &[BoneMat] {
        let layer = AnimLayer {
            anim: &self.anim.parts,
            bind: &self.bind,
            time: clock / self.length,
            weight: 1.0,
        };
        self.rig.pose(&[layer], &Controllers::NONE, &mut self.pose);
        self.pose.bones()
    }

    #[cfg(test)]
    fn bone(&self, name: &str) -> Option<usize> {
        self.rig.bone_index(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::content::Install;

    /// The rotors turn between two times and nothing else of the helicopter moves: the body is the rest pose both
    /// times, and a rotor turns about its own joint rather than sliding off it.
    #[test]
    fn the_rotors_turn_and_the_body_stays() {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let install = Install::open(std::path::Path::new(&root)).expect("install");
        let mut content = Content::for_client();
        content
            .load_zone(&install, "common_mp", 4)
            .expect("common_mp");
        content.load_map(&install, "mp_backlot").expect("map");
        for name in ["vehicle_cobra_helicopter_fly", "vehicle_mi24p_hind_desert"] {
            let model = content
                .model(name)
                .unwrap_or_else(|| panic!("{name} not loaded"))
                .clone();
            let mut rotors = Rotors::new(&content, &model).expect("rotors");
            assert!(
                rotors.bind.matched() > 0,
                "{name}: the animation drives no bone"
            );
            let a = rotors.pose(0.0).to_vec();
            let b = rotors.pose(rotors.length * 0.1).to_vec();
            assert_eq!(a.len(), usize::from(model.num_bones));
            let main = rotors.bone("main_rotor_jnt").expect("main rotor bone");
            let tail = rotors.bone("tail_rotor_jnt").expect("tail rotor bone");
            for bone in [main, tail] {
                let d: f32 = (0..4)
                    .map(|i| (a[bone].quat[i] - b[bone].quat[i]).abs())
                    .sum();
                assert!(d > 0.01, "{name}: bone {bone} did not turn ({d})");
                let slid: f32 = (0..3)
                    .map(|i| (a[bone].trans[i] - b[bone].trans[i]).abs())
                    .sum();
                assert!(slid < 0.5, "{name}: bone {bone} slid {slid}");
            }
            let mut rest = Pose::default();
            rotors.rig.pose(&[], &Controllers::NONE, &mut rest);
            // Bones that are not under a rotor joint are the same at both times.
            for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                let under_rotor = [main, tail].iter().any(|r| {
                    let mut at = Some(i);
                    while let Some(j) = at {
                        if j == *r {
                            return true;
                        }
                        at = rotors.rig.parent(j);
                    }
                    false
                });
                if !under_rotor {
                    assert_eq!(x, y, "{name}: body bone {i} moved");
                    assert_eq!(
                        *x,
                        rest.bones()[i],
                        "{name}: body bone {i} left the rest pose"
                    );
                }
            }
        }
    }
}
