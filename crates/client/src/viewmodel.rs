// SPDX-License-Identifier: GPL-3.0-or-later
//! The first-person weapon: the hands model with the gun attached at `tag_weapon`, animated by the weapon's
//! `viewmodel_*` animations from the player state's weapon state machine.
//!
//! The original plays the animation the weapon code last started (`weapAnim` in the player state, which this
//! reimplementation's `PlayerState` does not carry), so the animation is derived from the state instead: each
//! `weapon_state` names one of the weapon's animation slots and `weapon_time`, the time left in the state, gives how
//! far through it is. Aiming down sights plays `ADS_UP` (rising) or `ADS_DOWN` (falling) by `weapon_pos_frac`.
//! Bob is the original's angle bob (`CalculateWeaponPosition_BobAngles`) with its stock amplitudes; the idle sway,
//! recoil kick and the stance offsets of the weapon file are not applied.

use assets::zone::weapon::WeaponDef;
use assets::zone::xmodel::XModel;
use render::{ModelInstance, ModelKind};
use server::content::{Content, PlayerAnim};
use sim::pm::{PlayerState, weapon_state as ws};
use sim::skel::{AnimBinding, AnimLayer, Controllers, Pose, Rig, RigModel};
use std::sync::Arc;

/// `weapAnimFiles_t`: indices into [`WeaponDef::anims`].
pub mod slot {
    pub const IDLE: usize = 1;
    pub const FIRE: usize = 3;
    pub const RECHAMBER: usize = 6;
    pub const MELEE: usize = 7;
    pub const RELOAD: usize = 9;
    pub const RELOAD_START: usize = 11;
    pub const RELOAD_END: usize = 12;
    pub const RAISE: usize = 13;
    pub const DROP: usize = 15;
    pub const SPRINT_IN: usize = 22;
    pub const SPRINT_LOOP: usize = 23;
    pub const SPRINT_OUT: usize = 24;
    pub const ADS_FIRE: usize = 28;
    pub const ADS_UP: usize = 31;
    pub const ADS_DOWN: usize = 32;
    pub const COUNT: usize = 33;
}

/// `cg_bobWeaponAmplitude`.
const BOB_AMPLITUDE: f32 = 0.16;
/// `cg_bobWeaponMax`.
const BOB_MAX: f32 = 6.0;

/// What to play: an animation slot and the normalised time in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playing {
    pub slot: usize,
    pub time: f32,
}

/// The slot a weapon state plays and how long the state lasts in milliseconds (0 for a held pose).
fn state_slot(state: u8, ads: f32, w: &WeaponDef) -> (usize, i32) {
    match state {
        ws::RAISING | ws::RAISING_ALTSWITCH => (slot::RAISE, w.raise_time),
        ws::DROPPING | ws::DROPPING_QUICK => (slot::DROP, w.drop_time),
        ws::FIRING if ads > 0.5 => (slot::ADS_FIRE, w.fire_time),
        ws::FIRING => (slot::FIRE, w.fire_time),
        ws::RECHAMBERING => (slot::RECHAMBER, w.rechamber_time),
        ws::RELOADING | ws::RELOADING_INTERUPT => (slot::RELOAD, w.reload_time),
        ws::RELOAD_START | ws::RELOAD_START_INTERUPT => (slot::RELOAD_START, w.reload_start_time),
        ws::RELOAD_END => (slot::RELOAD_END, w.reload_end_time),
        ws::MELEE_INIT | ws::MELEE_FIRE | ws::MELEE_END => (slot::MELEE, w.melee_time),
        ws::SPRINT_RAISE => (slot::SPRINT_IN, w.sprint_in_time),
        ws::SPRINT_LOOP => (slot::SPRINT_LOOP, 0),
        ws::SPRINT_DROP => (slot::SPRINT_OUT, w.sprint_out_time),
        _ => (slot::IDLE, 0),
    }
}

/// Picks the animation for a state. `rising` tells ADS up from down while `ads` is between hip and aimed.
pub fn select(
    state: u8,
    weapon_time: i32,
    ads: f32,
    rising: bool,
    sprint_clock: f32,
    w: &WeaponDef,
) -> Playing {
    let (s, total) = state_slot(state, ads, w);
    if s == slot::IDLE && ads > 0.0 {
        return if rising || ads >= 1.0 {
            Playing {
                slot: slot::ADS_UP,
                time: ads,
            }
        } else {
            Playing {
                slot: slot::ADS_DOWN,
                time: 1.0 - ads,
            }
        };
    }
    if s == slot::SPRINT_LOOP {
        return Playing {
            slot: s,
            time: sprint_clock,
        };
    }
    let time = if total > 0 {
        ((total - weapon_time) as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Playing { slot: s, time }
}

struct Slot {
    anim: Arc<PlayerAnim>,
    bind: AnimBinding,
    /// Seconds one loop takes.
    length: f32,
}

/// One weapon's view model and animation state.
pub struct ViewModel {
    def: Arc<WeaponDef>,
    hands: Arc<XModel>,
    gun: Arc<XModel>,
    rig: Rig,
    slots: Vec<Option<Slot>>,
    pose: Pose,
    last_ads: f32,
    rising: bool,
    sprint_clock: f32,
    playing: Playing,
}

impl ViewModel {
    /// Builds the view model of `weapon` with `hands` (else the weapon's own hand model). Fails when the models or the
    /// weapon's idle animation are not in `content`.
    pub fn new(
        content: &Content,
        weapon: &Arc<WeaponDef>,
        hands: Option<&str>,
    ) -> Result<Self, String> {
        let gun = weapon
            .gun_models
            .first()
            .cloned()
            .flatten()
            .ok_or("weapon has no view model")?;
        let hands_model = match hands {
            Some(n) => content
                .model(n)
                .cloned()
                .ok_or_else(|| format!("model {n} not loaded"))?,
            None => weapon
                .hand_model
                .clone()
                .ok_or("weapon has no hand model")?,
        };
        let names = |m: &XModel| -> Result<Vec<Arc<str>>, String> {
            let n = m.name.as_deref().unwrap_or("?");
            content
                .model_bone_names(n)
                .map(|b| b.to_vec())
                .ok_or_else(|| format!("model {n} has no bone names"))
        };
        let (hn, gn) = (names(&hands_model)?, names(&gun)?);
        let hr: Vec<&str> = hn.iter().map(|s| &**s).collect();
        let gr: Vec<&str> = gn.iter().map(|s| &**s).collect();
        let rig = Rig::new(&[
            RigModel {
                model: hands_model.clone(),
                bone_names: &hr,
                attach: None,
            },
            RigModel {
                model: gun.clone(),
                bone_names: &gr,
                attach: Some("tag_weapon"),
            },
        ])?;
        let mut slots: Vec<Option<Slot>> = (0..slot::COUNT).map(|_| None).collect();
        for (i, n) in weapon.anims.iter().enumerate().take(slot::COUNT) {
            let Some(name) = n.as_deref().filter(|n| !n.is_empty()) else {
                continue;
            };
            if let Some(a) = content.player_anim(name) {
                let p = &a.parts;
                slots[i] = Some(Slot {
                    bind: rig.bind(&a.part_names),
                    length: f32::from(p.num_frames) / p.frame_rate,
                    anim: a.clone(),
                });
            }
        }
        if slots[slot::IDLE].is_none() {
            return Err(format!(
                "idle animation of {} not loaded",
                weapon.internal_name.as_deref().unwrap_or("?")
            ));
        }
        Ok(Self {
            def: weapon.clone(),
            hands: hands_model,
            gun,
            rig,
            slots,
            pose: Pose::default(),
            last_ads: 0.0,
            rising: true,
            sprint_clock: 0.0,
            playing: Playing {
                slot: slot::IDLE,
                time: 0.0,
            },
        })
    }

    /// The animation of the last update.
    pub fn playing(&self) -> Playing {
        self.playing
    }

    /// Advances to the state in `ps` (`dt` seconds since the last call) and returns the hands and the gun.
    pub fn update(&mut self, ps: &PlayerState, dt: f32) -> Vec<ModelInstance> {
        let ads = ps.weapon_pos_frac;
        if ads != self.last_ads {
            self.rising = ads > self.last_ads;
            self.last_ads = ads;
        }
        let loop_len = self.slots[slot::SPRINT_LOOP]
            .as_ref()
            .map_or(1.0, |s| s.length.max(0.001));
        self.sprint_clock = (self.sprint_clock + dt / loop_len).fract();
        let mut p = select(
            ps.weapon_state,
            ps.weapon_time,
            ads,
            self.rising,
            self.sprint_clock,
            &self.def,
        );
        if self.slots[p.slot].is_none() {
            p = Playing {
                slot: slot::IDLE,
                time: 0.0,
            };
        }
        self.playing = p;
        if let Some(s) = &self.slots[p.slot] {
            self.rig.pose(
                &[AnimLayer {
                    anim: &s.anim.parts,
                    bind: &s.bind,
                    time: p.time,
                    weight: 1.0,
                }],
                &Controllers::NONE,
                &mut self.pose,
            );
        }
        let eye = [
            ps.origin[0],
            ps.origin[1],
            ps.origin[2] + ps.view_height_current,
        ];
        let mut angles = ps.viewangles;
        let bob = bob_angles(ps, ads, &self.def);
        for (a, b) in angles.iter_mut().zip(bob) {
            *a += b;
        }
        let bones = self.pose.bones();
        let nh = usize::from(self.hands.num_bones);
        let mut out = Vec::with_capacity(2);
        for (model, b) in [
            (&self.hands, &bones[..nh.min(bones.len())]),
            (&self.gun, bones.get(nh..).unwrap_or(&[])),
        ] {
            let mut m = ModelInstance::new(model.clone(), ModelKind::ViewModel);
            m.origin = eye;
            m.angles = angles;
            m.bones = b.to_vec();
            m.lod = Some(0);
            m.light_origin = eye;
            out.push(m);
        }
        out
    }
}

/// Weapon bob as angles (pitch, yaw, roll degrees): the original's angle offsets for the bob cycle at the player's
/// ground speed, scaled down while aiming.
pub fn bob_angles(ps: &PlayerState, ads: f32, w: &WeaponDef) -> [f32; 3] {
    let speed = (ps.velocity[0].hypot(ps.velocity[1]) * BOB_AMPLITUDE).min(BOB_MAX);
    let cycle = f32::from(ps.bob_cycle) / 256.0 * std::f32::consts::TAU;
    let scale = 1.0 - (1.0 - w.ads_bob_factor) * ads;
    [
        -(cycle * 2.0).sin() * speed * scale,
        -cycle.sin() * speed * scale,
        0.0,
    ]
}
