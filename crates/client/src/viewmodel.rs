// SPDX-License-Identifier: GPL-3.0-only
//! The first-person weapon: the hands model with the gun attached at `tag_weapon`, animated by the weapon's
//! `viewmodel_*` animations from the player state's weapon state machine.
//!
//! The original plays the animation the weapon code last started (`weapAnim` in the player state, which this
//! reimplementation's `PlayerState` does not carry), so the animation is derived from the state instead: each
//! `weapon_state` names one of the weapon's animation slots and `weapon_time`, the time left in the state, gives how
//! far through it is. Aiming down sights layers `ADS_UP` (rising) or `ADS_DOWN` (falling) by `weapon_pos_frac` over it.
//! Bob is the original's angle bob (`CalculateWeaponPosition_BobAngles`) with its stock amplitudes, and the gun's recoil
//! is the original's spring ([`GunRecoil`]). The idle sway and the stance offsets of the weapon file are not applied
//! yet (ticket #193). The weapon's `hideTags` hide the sights and mounts of the variants it is not; [`Sight`] carries
//! what aiming does outside the model, the zoom field of view and the scope overlay of a sniper rifle.

use assets::zone::gfx::Material;
use assets::zone::weapon::WeaponDef;
use assets::zone::xmodel::XModel;
use render::{ModelInstance, ModelKind};
use server::content::{Content, PlayerAnim};
use sim::pm::{PlayerState, VIEW_CROUCH, VIEW_PRONE, pmf, weapon_state as ws};
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

/// Ground speed to bob speed (`BG_CalculateWeaponPosition_BobOffset`).
const BOB_SPEED: f32 = 0.16;
/// The bob angle cap, degrees.
const BOB_MAX: f32 = 10.0;

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

/// Picks the animation for a state: what the weapon is doing, or its idle.
pub fn select(state: u8, weapon_time: i32, ads: f32, sprint_clock: f32, w: &WeaponDef) -> Playing {
    let (s, total) = state_slot(state, ads, w);
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

/// The aim-down-sights layer. `ADS_UP` and `ADS_DOWN` animate only the `tag_ads` bone, which carries the whole view
/// model to the sights; the original keeps one of them on top of whatever else plays (`PlayADSAnim`), so it is a layer,
/// never a replacement of the idle. `rising` tells up from down while `ads` is between hip and aimed.
pub fn ads_layer(ads: f32, rising: bool) -> Playing {
    if rising || ads >= 1.0 {
        Playing {
            slot: slot::ADS_UP,
            time: ads,
        }
    } else {
        Playing {
            slot: slot::ADS_DOWN,
            time: 1.0 - ads,
        }
    }
}

struct Slot {
    anim: Arc<PlayerAnim>,
    bind: AnimBinding,
    /// Seconds one loop takes.
    length: f32,
}

/// The scope picture of a weapon with an overlay (`adsOverlay*`), drawn over the screen centre while aimed.
#[derive(Clone)]
pub struct Overlay {
    pub material: Arc<Material>,
    /// Width and height in the 640 by 480 virtual screen.
    pub size: [f32; 2],
    pub alpha: f32,
}

/// What aiming does besides moving the view model: the zoom and the scope overlay (`CG_GetViewFov`,
/// `CG_DrawWeapReticle`).
#[derive(Clone)]
pub struct Sight {
    /// The field of view in degrees at full zoom (`adsZoomFov`).
    pub zoom_fov: f32,
    /// How far the zoom has come, 0 to 1.
    pub zoom: f32,
    /// Set once the overlay shows; the original draws no gun then.
    pub overlay: Option<Overlay>,
}

/// How far the zoom has come at aim fraction `ads`, when it runs over the last `frac` of the way in (`adsZoomInFrac`)
/// or the first `frac` of the way out (`adsZoomOutFrac`).
pub fn zoom_frac(ads: f32, frac: f32) -> f32 {
    if ads >= 1.0 {
        return 1.0;
    }
    ((ads - (1.0 - frac)) / frac.max(1e-3)).clamp(0.0, 1.0)
}

/// One weapon's view model and animation state.
pub struct ViewModel {
    def: Arc<WeaponDef>,
    hands: Arc<XModel>,
    gun: Arc<XModel>,
    /// Surfaces of the gun hidden for this weapon (`hideTags`): the sights and mounts of the variants it is not.
    hidden: [u32; 4],
    rig: Rig,
    slots: Vec<Option<Slot>>,
    pose: Pose,
    last_ads: f32,
    rising: bool,
    sprint_clock: f32,
    playing: Playing,
    /// Indices of `tag_flash` and `tag_brass` among the gun's bones.
    flash_bone: Option<usize>,
    brass_bone: Option<usize>,
    tags: Option<ViewTags>,
    recoil: GunRecoil,
}

/// The gun's recoil spring (`BG_CalculateWeaponPosition_GunRecoil`): shots give its pitch and yaw a speed, which
/// moves the gun's offset (degrees) until the weapon file's pull back and decay bring it to rest.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GunRecoil {
    offset: [f32; 2],
    speed: [f32; 2],
}

/// The integration step of the spring, seconds.
const RECOIL_STEP: f32 = 0.005;

impl GunRecoil {
    pub fn kick(&mut self, speed: [f32; 2]) {
        self.speed[0] += speed[0];
        self.speed[1] += speed[1];
    }

    /// Advances the spring by `dt` seconds; the weapon file's hip and ADS values are blended by `ads`. A weapon
    /// that cannot aim down sights has no recoil spring.
    pub fn step(&mut self, dt: f32, ads: f32, w: &WeaponDef) {
        if w.aim_down_sight == 0 {
            return;
        }
        let blend = |hip: f32, aimed: f32| (aimed - hip) * ads + hip;
        let accel = blend(w.hip_gun_kick_accel, w.ads_gun_kick_accel);
        let max = blend(w.hip_gun_kick_speed_max, w.ads_gun_kick_speed_max);
        let decay = blend(w.hip_gun_kick_speed_decay, w.ads_gun_kick_speed_decay);
        let rest = blend(w.hip_gun_kick_static_decay, w.ads_gun_kick_static_decay);
        let mut left = dt;
        while left > 0.0 {
            let ft = left.min(RECOIL_STEP);
            left -= ft;
            let pitch_done = spring(
                &mut self.offset[0],
                &mut self.speed[0],
                ft,
                [w.gun_max_pitch, accel, max, decay, rest],
            );
            let yaw_done = spring(
                &mut self.offset[1],
                &mut self.speed[1],
                ft,
                [w.gun_max_yaw, accel, max, decay, rest],
            );
            if pitch_done && yaw_done {
                break;
            }
        }
    }
}

/// One axis of the spring for `dt` seconds (`..._GunRecoil_SingleAngle`); `[cap, accel, speed max, speed decay,
/// static decay]`. Returns whether the axis is at rest.
fn spring(
    offset: &mut f32,
    speed: &mut f32,
    dt: f32,
    [cap, accel, max, decay, rest]: [f32; 5],
) -> bool {
    if offset.abs() < 0.25 && speed.abs() < 1.0 {
        *offset = 0.0;
        *speed = 0.0;
        return true;
    }
    *offset += *speed * dt;
    if *offset > cap {
        *offset = cap;
        *speed = speed.min(0.0);
    } else if *offset < -cap {
        *offset = -cap;
        *speed = speed.max(0.0);
    }
    if *offset > 0.0 {
        *speed -= accel * dt;
    } else if *offset < 0.0 {
        *speed += accel * dt;
    }
    *speed -= *speed * decay * dt;
    if *speed <= 0.0 {
        *speed = (*speed + rest * dt).min(0.0);
    } else {
        *speed = (*speed - rest * dt).max(0.0);
    }
    *speed = speed.clamp(-max, max);
    false
}

/// Where the gun's muzzle and ejection port are in the world.
#[derive(Clone, Copy, Debug)]
pub struct ViewTags {
    pub flash: Option<fx::Frame>,
    pub brass: Option<fx::Frame>,
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
        let mut hidden = [0u32; 4];
        let tags = weapon
            .internal_name
            .as_deref()
            .and_then(|n| content.weapon_hide_tags(n))
            .map_or(&[][..], |t| &t[..]);
        for tag in tags {
            let i = gn
                .iter()
                .position(|b| b.eq_ignore_ascii_case(tag))
                .ok_or_else(|| format!("{tag} is not a bone of the gun"))?;
            hidden[i >> 5] |= 0x8000_0000 >> (i & 31);
        }
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
        let bone = |n: &str| gn.iter().position(|b| &**b == n);
        let (flash_bone, brass_bone) = (bone("tag_flash"), bone("tag_brass"));
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
            hidden,
            rig,
            slots,
            pose: Pose::default(),
            last_ads: 0.0,
            rising: true,
            sprint_clock: 0.0,
            recoil: GunRecoil::default(),
            playing: Playing {
                slot: slot::IDLE,
                time: 0.0,
            },
            flash_bone,
            brass_bone,
            tags: None,
        })
    }

    /// The muzzle and the ejection port as of the last update.
    pub fn tags(&self) -> Option<ViewTags> {
        self.tags
    }

    /// The zoom and scope overlay of the last update.
    pub fn sight(&self) -> Sight {
        let w = &self.def;
        let zoom = if self.slots[slot::ADS_UP].is_some() {
            let frac = if self.rising {
                w.ads_zoom_in_frac
            } else {
                w.ads_zoom_out_frac
            };
            zoom_frac(self.last_ads, frac)
        } else {
            0.0
        };
        let overlay = w
            .overlay_material
            .clone()
            .filter(|_| w.overlay_reticle != 0 && zoom > 0.01)
            .map(|material| Overlay {
                material,
                size: [w.overlay_width, w.overlay_height],
                alpha: zoom,
            });
        Sight {
            zoom_fov: w.ads_zoom_fov,
            zoom,
            overlay,
        }
    }

    /// Gives the gun's recoil spring the speed of the shots since the last update.
    pub fn kick_gun(&mut self, speed: [f32; 2]) {
        self.recoil.kick(speed);
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
        let sights = ads_layer(ads, self.rising);
        let mut layers = Vec::with_capacity(2);
        for l in [Some(p), (ads > 0.0).then_some(sights)]
            .into_iter()
            .flatten()
        {
            if let Some(s) = &self.slots[l.slot] {
                layers.push(AnimLayer {
                    anim: &s.anim.parts,
                    bind: &s.bind,
                    time: l.time,
                    weight: 1.0,
                });
            }
        }
        self.rig.pose(&layers, &Controllers::NONE, &mut self.pose);
        let eye = [
            ps.origin[0],
            ps.origin[1],
            ps.origin[2] + ps.view_height_current,
        ];
        let mut angles = ps.viewangles;
        // The sights line up with the aim: the weapon file's pitch for the aimed pose.
        angles[0] += self.def.ads_aim_pitch * ads;
        let bob = bob_angles(ps, ads, &self.def);
        for (a, b) in angles.iter_mut().zip(bob) {
            *a += b;
        }
        self.recoil.step(dt, ads, &self.def);
        angles[0] += self.recoil.offset[0];
        angles[1] += self.recoil.offset[1];
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
            if std::sync::Arc::ptr_eq(model, &self.gun) {
                m.hidden_parts = self.hidden;
            }
            out.push(m);
        }
        let gun = &out[1];
        let frame = |i: Option<usize>| -> Option<fx::Frame> {
            let b = gun.bones.get(i?)?;
            let m = gun.world_matrix()
                * glam::Mat4::from_rotation_translation(
                    glam::Quat::from_xyzw(b.quat[0], b.quat[1], b.quat[2], b.quat[3]).normalize(),
                    glam::Vec3::from(b.trans),
                );
            Some(fx::Frame {
                origin: m.w_axis.truncate(),
                axis: [
                    m.x_axis.truncate(),
                    m.y_axis.truncate(),
                    m.z_axis.truncate(),
                ],
            })
        };
        self.tags = Some(ViewTags {
            flash: frame(self.flash_bone),
            brass: frame(self.brass_bone),
        });
        out
    }
}

/// `bg_bobAmplitude{Standing,Ducked,Prone,Sprinting}`: (horizontal, vertical) degrees of bob per unit of bob speed.
fn bob_amplitude(ps: &PlayerState) -> (f32, f32) {
    if ps.view_height_target == VIEW_PRONE {
        (0.02, 0.005)
    } else if ps.view_height_target == VIEW_CROUCH {
        (0.0075, 0.0075)
    } else if ps.pm_flags & pmf::SPRINTING != 0 {
        (0.02, 0.014)
    } else {
        (0.007, 0.007)
    }
}

/// Weapon bob as angles (pitch, yaw, roll degrees), `BG_CalculateWeaponPosition_BobOffset`: the stance's bob
/// amplitude times the ground speed, capped, scaled by the weapon's `adsBobFactor` while aiming, and faded out
/// entirely for a scoped weapon.
pub fn bob_angles(ps: &PlayerState, ads: f32, w: &WeaponDef) -> [f32; 3] {
    use std::f32::consts::{FRAC_PI_2, PI, TAU};
    let (h, v) = bob_amplitude(ps);
    let cycle = f32::from(ps.bob_cycle) / 255.0 * TAU + TAU + PI / 4.0 + TAU;
    let speed = ps.velocity[0].hypot(ps.velocity[1]) * BOB_SPEED;
    let vertical = |cycle: f32, speed: f32| {
        let amp = (speed * v).min(BOB_MAX);
        ((cycle * 4.0 + FRAC_PI_2).sin() * 0.2 + (cycle * 2.0).sin()) * 0.75 * amp
    };
    let horizontal = |cycle: f32, speed: f32| cycle.sin() * (speed * h).min(BOB_MAX);
    let roll = horizontal(cycle - 0.471_238_9, speed * 1.5).min(0.0);
    let mut scale = 1.0 - (1.0 - w.ads_bob_factor) * ads;
    if w.overlay_reticle != 0 {
        scale *= 1.0 - ads;
    }
    [
        -vertical(cycle, speed) * scale,
        -horizontal(cycle, speed) * scale,
        roll * scale,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Affine3A, Quat, Vec3};
    use server::content::Install;

    /// A kick moves the gun back, the spring pulls it home and it comes to rest exactly at zero; the cap holds.
    #[test]
    fn the_gun_recoil_spring_kicks_and_settles() {
        let params = [8.0, 50.0, 400.0, 3.0, 20.0];
        let (mut offset, mut speed) = (0.0f32, 300.0f32);
        let mut peak = 0.0f32;
        let mut at_rest = false;
        for _ in 0..2000 {
            at_rest = spring(&mut offset, &mut speed, RECOIL_STEP, params);
            peak = peak.max(offset);
            assert!(offset <= params[0]);
            if at_rest {
                break;
            }
        }
        assert!(peak > 1.0, "the gun moved: {peak}");
        assert!(at_rest && offset == 0.0 && speed == 0.0);
    }

    #[test]
    fn the_sights_layer_rises_and_falls_with_the_aim() {
        assert_eq!(
            ads_layer(0.3, true),
            Playing {
                slot: slot::ADS_UP,
                time: 0.3
            }
        );
        let down = ads_layer(0.3, false);
        assert_eq!(down.slot, slot::ADS_DOWN);
        assert!((down.time - 0.7).abs() < 1e-6);
        assert_eq!(ads_layer(1.0, false).slot, slot::ADS_UP);
    }

    fn content() -> Option<Content> {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return None;
        };
        let install = Install::open(std::path::Path::new(&root)).expect("install");
        let mut content = Content::for_client();
        content.load_boot(&install).expect("boot zones");
        content.load_map(&install, "mp_backlot").expect("map");
        Some(content)
    }

    /// The weapons of the five stock classes: primary and sidearm with their attachments, and the grenades, read
    /// from `mp/classTable.csv` the way `_class.gsc` does (`stat + 1`/`+ 2` primary and attachment, `+ 3`/`+ 4` sidearm,
    /// `stat` and `+ 8` the grenades).
    fn class_loadouts(content: &Content) -> Vec<String> {
        let t = content.string_table("mp/classTable.csv").expect("table");
        let cols = t.column_count as usize;
        let cell = |r: usize, c: usize| t.values.get(r * cols + c).and_then(|n| n.as_deref());
        let find = |stat: usize| {
            let key = stat.to_string();
            (0..t.row_count as usize)
                .find(|&r| cell(r, 1) == Some(&key))
                .and_then(|r| cell(r, 4))
                .unwrap_or("")
                .to_owned()
        };
        let with = |weapon: String, attachment: String| match attachment.as_str() {
            "" | "none" => format!("{weapon}_mp"),
            a => format!("{weapon}_{a}_mp"),
        };
        let mut out = Vec::new();
        for stat in [200usize, 210, 220, 230, 240] {
            out.push(with(find(stat + 1), find(stat + 2)));
            out.push(with(find(stat + 3), find(stat + 4)));
            out.push(with(find(stat), String::new()));
            out.push(with(find(stat + 8), String::new()));
        }
        out.sort();
        out.dedup();
        out
    }

    /// Where the gun hangs and where it points in view space (x forward, y left, z up from the eye): the root bone's
    /// position and its forward axis.
    fn gun_root(inst: &[ModelInstance]) -> ([f32; 3], [f32; 3]) {
        let b = &inst[1].bones[0];
        let f = Quat::from_xyzw(b.quat[0], b.quat[1], b.quat[2], b.quat[3]) * Vec3::X;
        (b.trans, f.to_array())
    }

    /// [`gun_root`] in world space, with the instance's bob and aim angles applied (the view is level here).
    fn world_gun_root(inst: &[ModelInstance]) -> ([f32; 3], [f32; 3]) {
        let (t, f) = gun_root(inst);
        let m = inst[1].world_matrix();
        (
            m.transform_point3(Vec3::from(t)).to_array(),
            m.transform_vector3(Vec3::from(f)).to_array(),
        )
    }

    fn build(content: &Content, name: &str) -> Option<ViewModel> {
        let def = content.weapon(name)?.clone();
        let hands = content
            .model_names("viewhands_")
            .first()
            .map(|n| (*n).to_owned());
        ViewModel::new(content, &def, hands.as_deref()).ok()
    }

    /// The hip pose of every weapon in the zone that can be held, back at hip after aiming.
    fn hip_ads_hip(vm: &mut ViewModel) -> [Vec<ModelInstance>; 3] {
        let mut ps = PlayerState {
            view_height_current: 60.0,
            ..PlayerState::default()
        };
        let hip = vm.update(&ps, 0.01);
        ps.weapon_pos_frac = 1.0;
        let ads = vm.update(&ps, 0.01);
        ps.weapon_pos_frac = 0.0;
        let back = vm.update(&ps, 0.01);
        [hip, ads, back]
    }

    /// Every weapon of every default class (primaries and sidearms with their attachments, grenades): at the hip the
    /// gun hangs forward, low and to the right, pointing away from the eye; aimed it is centred on the view. A
    /// one-pose idle animation (the stock M16 family's: `num_frames == 0`) used to be skipped, which left the arms and
    /// gun at the eye, pointing back at it.
    #[test]
    fn every_class_weapon_hangs_low_right_at_the_hip_and_centres_when_aimed() {
        let Some(content) = content() else { return };
        let loadouts = class_loadouts(&content);
        assert!(
            loadouts.iter().any(|w| w == "m16_gl_mp"),
            "the assault class: {loadouts:?}"
        );
        for name in &loadouts {
            let mut vm = build(&content, name).unwrap_or_else(|| panic!("{name}: no view model"));
            let [hip, ads, back] = hip_ads_hip(&mut vm);
            let ((ht, hf), (at, _)) = (gun_root(&hip), gun_root(&ads));
            assert!(hf[0] > 0.9, "{name}: the gun points back at the eye {hf:?}");
            assert!(
                (5.0..20.0).contains(&ht[0]) && ht[1] <= 0.5 && (-9.0..-2.0).contains(&ht[2]),
                "{name}: hip pose {ht:?} is not forward, right and low"
            );
            if vm.slots[slot::ADS_UP].is_some() {
                assert!(
                    ht[1] < -1.0,
                    "{name}: hip pose {ht:?} is not off to the right"
                );
                assert!(
                    at[1].abs() < 0.7
                        && (0.0..20.0).contains(&at[0])
                        && (-7.0..0.0).contains(&at[2]),
                    "{name}: aimed pose {at:?} is not centred on the view"
                );
                assert!(
                    (ht[1] - at[1]).abs() > 1.5,
                    "{name}: aiming did not move it"
                );
            }
            assert_eq!(
                hip[1].bones, back[1].bones,
                "{name}: the hip pose did not come back"
            );
        }
    }

    /// Running at full aim, the sights of every weapon in the zone stay on the aim point: a run's whole bob cycle
    /// moves the gun's root under half a unit and turns it under a degree (the original's bob is a fraction of a
    /// degree; applied at the cap it swung the aim six degrees).
    #[test]
    fn the_sights_hold_steady_while_running_aimed() {
        let Some(content) = content() else { return };
        let mut checked = 0;
        for w in content.weapons() {
            let name = w.internal_name.as_deref().unwrap();
            let Some(mut vm) = build(&content, name) else {
                continue;
            };
            checked += 1;
            let mut ps = PlayerState {
                view_height_current: 60.0,
                weapon_pos_frac: 1.0,
                ..PlayerState::default()
            };
            let still = world_gun_root(&vm.update(&ps, 0.01));
            ps.velocity = [190.0, 0.0, 0.0];
            for cycle in 0..=255u8 {
                ps.bob_cycle = cycle;
                let (t, f) = world_gun_root(&vm.update(&ps, 0.01));
                let moved = (0..3)
                    .map(|i| (t[i] - still.0[i]).abs())
                    .fold(0.0, f32::max);
                let turned = (0..3)
                    .map(|i| (f[i] - still.1[i]).abs())
                    .fold(0.0, f32::max);
                assert!(
                    moved < 0.5,
                    "{name}: the sights moved {moved} at cycle {cycle}"
                );
                assert!(
                    turned < 0.017,
                    "{name}: the gun turned {turned} rad at cycle {cycle}"
                );
            }
        }
        assert!(checked > 80, "only {checked} weapons have a view model");
    }

    /// Every weapon in the zone with a view model keeps its gun pointing away from the eye, near the screen, at the
    /// hip and aimed, and the skinned meshes of hands and gun stay the size they are in the bind pose (stretched
    /// triangles across the gun came from bones left unposed).
    #[test]
    fn every_stock_weapon_points_away_and_skins_without_stretching() {
        let Some(content) = content() else { return };
        let mut checked = 0;
        for w in content.weapons() {
            let name = w.internal_name.as_deref().unwrap();
            let Some(mut vm) = build(&content, name) else {
                continue;
            };
            checked += 1;
            let [hip, ads, _] = hip_ads_hip(&mut vm);
            for (what, inst) in [("hip", &hip), ("aimed", &ads)] {
                let (t, f) = gun_root(inst);
                assert!(
                    f[0] > 0.3,
                    "{name} {what}: the gun points back at the eye {f:?}"
                );
                assert!(
                    t[0] > 0.0 && t.iter().all(|c| c.abs() < 40.0),
                    "{name} {what}: the gun is at {t:?}"
                );
                for m in inst {
                    let worst = worst_stretch(m);
                    assert!(
                        worst < 3.0,
                        "{name} {what}: {} has a triangle edge {worst:.1}x its bind pose length",
                        m.model.name.as_deref().unwrap_or("?")
                    );
                }
            }
        }
        assert!(checked > 80, "only {checked} weapons have a view model");
    }

    /// The models the stock character scripts hand to `setViewmodel`, with the zones loaded here.
    fn scripted_hands(content: &Content) -> Vec<String> {
        let mut out = Vec::new();
        for (name, bytes) in content.rawfiles() {
            if !name.starts_with("character/") {
                continue;
            }
            for l in String::from_utf8_lossy(bytes).lines() {
                let l = l.to_ascii_lowercase();
                let Some((_, rest)) = l.split_once("setviewmodel(\"") else {
                    continue;
                };
                let n = rest.split('"').next().unwrap_or("").to_owned();
                if content.model(&n).is_some() && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Whether the renderer can draw every surface of `m`: a material with a technique set for each.
    fn drawable(m: &XModel) -> bool {
        !m.materials.is_empty()
            && m.materials
                .iter()
                .all(|x| x.as_ref().is_some_and(|x| x.technique_set.is_some()))
    }

    /// How many lod-0 vertices of `m` fall inside the default view (4:3, 80 degrees across).
    fn visible_vertices(m: &ModelInstance) -> usize {
        let model = &m.model;
        let posed = render::skin::skin_matrices(model, &m.bones);
        let lod = &model.lod_info[0];
        let first = usize::from(lod.surf_index);
        let mut n = 0;
        for s in &model.surfs[first..first + usize::from(lod.surf_count)] {
            let mut b = Vec::new();
            render::skin::skin_surface(s, &posed, &mut b);
            for v in b.as_chunks::<32>().0 {
                let f = |k: usize| f32::from_le_bytes(v[k..k + 4].try_into().unwrap());
                let (x, y, z) = (f(0), f(4), f(8));
                n += usize::from(x > 1.0 && y.abs() < 0.84 * x && z.abs() < 0.63 * x);
            }
        }
        n
    }

    /// The arms are drawn with the gun: the hands the stock scripts give the players are models the renderer can draw,
    /// and the hands of every default-class weapon are posed into the view. The client once picked `viewhands_usmc`
    /// by name, a stub whose materials live in another zone, so no arms were drawn at all.
    #[test]
    fn the_hands_of_every_class_weapon_are_drawn_in_view() {
        let Some(content) = content() else { return };
        let scripted = scripted_hands(&content);
        assert!(
            scripted.iter().any(|n| n == "viewmodel_base_viewhands")
                && scripted.iter().any(|n| n == "viewhands_desert_opfor"),
            "the scripts' hands in the zones: {scripted:?}"
        );
        for n in &scripted {
            assert!(
                drawable(content.model(n).unwrap()),
                "{n} has materials the renderer cannot draw"
            );
        }
        for name in class_loadouts(&content) {
            for hands in std::iter::once(None).chain(scripted.iter().map(|n| Some(n.as_str()))) {
                let def = content.weapon(&name).unwrap().clone();
                let mut vm = ViewModel::new(&content, &def, hands).expect("view model");
                let [hip, _, _] = hip_ads_hip(&mut vm);
                let seen = visible_vertices(&hip[0]);
                // A thrown weapon is held low, mostly below the view.
                assert!(
                    vm.slots[slot::ADS_UP].is_none() || seen > 300,
                    "{name} with {hands:?}: only {seen} hand vertices in view"
                );
            }
        }
    }

    /// The sight attachments of the gun that are drawn (not hidden by the weapon's `hideTags`), by tag, with the centre
    /// of their posed mesh in view space (x forward, y left, z up from the eye). Tags without a surface are not drawn.
    fn drawn_sights(
        content: &Content,
        vm: &ViewModel,
        gun: &ModelInstance,
    ) -> Vec<(String, [f32; 3], f32)> {
        let model = &gun.model;
        let names = content
            .model_bone_names(model.name.as_deref().unwrap())
            .unwrap();
        let posed = render::skin::skin_matrices(model, &gun.bones);
        let lod = &model.lod_info[0];
        let first = usize::from(lod.surf_index);
        let mut out = Vec::new();
        for (i, tag) in names.iter().enumerate() {
            if !["sight", "acog", "scope"].iter().any(|k| tag.contains(k)) {
                continue;
            }
            let bit = |p: [i32; 4]| (p[i >> 5] as u32) & (0x8000_0000 >> (i & 31)) != 0;
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            let mut any = false;
            for sf in &model.surfs[first..first + usize::from(lod.surf_count)] {
                if !bit(sf.part_bits) || render::skin::is_hidden(sf, &vm.hidden) {
                    continue;
                }
                let mut b = Vec::new();
                render::skin::skin_surface(sf, &posed, &mut b);
                for v in b.as_chunks::<32>().0 {
                    let f = |k: usize| f32::from_le_bytes(v[k..k + 4].try_into().unwrap());
                    let p = Vec3::new(f(0), f(4), f(8));
                    (lo, hi, any) = (lo.min(p), hi.max(p), true);
                }
            }
            if any {
                out.push((tag.to_string(), ((lo + hi) * 0.5).to_array(), hi.z - lo.z));
            }
        }
        out
    }

    /// Aimed, a weapon shows one sight attachment (the variants share a model and the weapon's `hideTags` hide the
    /// others) and the sight it aims through is on the view's centre line. Pixels are those of a 1280x720 screen at
    /// the stock 80 degree field of view across 4:3 (a focal length of about 570 px).
    #[test]
    fn aimed_down_sights_one_sight_is_in_the_middle_of_the_screen() {
        const FOCAL_PX: f32 = 570.0;
        // An optic's mesh centre is its lens; an iron sight's is only roughly the aperture.
        const OPTIC_PX: f32 = 80.0;
        const IRON_PX: f32 = 100.0;
        let Some(content) = content() else { return };
        let mut aimed = 0;
        for w in content.weapons() {
            let name = w.internal_name.as_deref().unwrap();
            let Some(mut vm) = build(&content, name) else {
                continue;
            };
            // A scoped weapon shows its overlay and no gun when aimed.
            if vm.slots[slot::ADS_UP].is_none() || w.overlay_reticle != 0 {
                continue;
            }
            let [_, ads, _] = hip_ads_hip(&mut vm);
            let sights = drawn_sights(&content, &vm, &ads[1]);
            let optics: Vec<_> = sights
                .iter()
                .filter(|(t, ..)| t != "tag_iron_sight" && !t.contains("front"))
                .collect();
            assert!(optics.len() <= 1, "{name} draws {sights:?}");
            // The optic if there is one, else the iron sight (unless its surfaces are the gun's body too, as the RPD's).
            let Some((tag, c, _)) = optics.first().copied().or_else(|| {
                sights
                    .iter()
                    .find(|(t, _, height)| t == "tag_iron_sight" && *height < 4.0)
            }) else {
                continue;
            };
            aimed += 1;
            let off = |v: f32| (v / c[0] * FOCAL_PX).abs();
            let tolerance = if *tag == "tag_iron_sight" {
                IRON_PX
            } else {
                OPTIC_PX
            };
            assert!(
                c[0] > 1.0 && off(c[1]) < tolerance && off(c[2]) < tolerance,
                "{name}: {tag} is at {c:?}, {:.0} px right and {:.0} px low of the screen centre",
                off(c[1]),
                off(c[2])
            );
        }
        assert!(aimed > 50, "only {aimed} weapons checked");
    }

    /// Aiming zooms the world towards the weapon's zoom field of view, and a scoped weapon fades its overlay in over the
    /// same stretch and takes the gun off the screen; nothing else shows an overlay.
    #[test]
    fn aiming_zooms_and_scoped_weapons_draw_their_overlay() {
        let Some(content) = content() else { return };
        let (mut scoped, mut zoomed) = (0, 0);
        for w in content.weapons() {
            let name = w.internal_name.as_deref().unwrap();
            let Some(mut vm) = build(&content, name) else {
                continue;
            };
            let [_, _, _] = hip_ads_hip(&mut vm);
            let hip = vm.sight();
            assert!(
                hip.overlay.is_none() && hip.zoom == 0.0,
                "{name} zooms at the hip"
            );
            let ps = PlayerState {
                weapon_pos_frac: 1.0,
                ..PlayerState::default()
            };
            vm.update(&ps, 0.01);
            let aimed = vm.sight();
            if vm.slots[slot::ADS_UP].is_none() {
                continue;
            }
            assert_eq!(aimed.zoom, 1.0, "{name}");
            assert!(
                (1.0..=90.0).contains(&aimed.zoom_fov),
                "{name}: {} degrees",
                aimed.zoom_fov
            );
            zoomed += 1;
            match (&aimed.overlay, w.overlay_reticle != 0) {
                (Some(o), true) => {
                    scoped += 1;
                    assert_eq!(o.alpha, 1.0, "{name}");
                    assert_eq!(o.size, [w.overlay_width, w.overlay_height], "{name}");
                    assert!(
                        aimed.zoom_fov <= 30.0,
                        "{name} scopes at {}",
                        aimed.zoom_fov
                    );
                }
                (None, false) => {}
                (o, _) => panic!(
                    "{name}: overlay {} with reticle {}",
                    o.is_some(),
                    w.overlay_reticle
                ),
            }
        }
        assert!(
            scoped >= 5 && zoomed > 50,
            "{scoped} scoped, {zoomed} zooming weapons"
        );
    }

    #[test]
    fn the_zoom_runs_over_the_last_part_of_the_way_in_and_the_first_part_of_the_way_out() {
        assert_eq!(zoom_frac(0.0, 0.5), 0.0);
        assert_eq!(zoom_frac(0.5, 0.5), 0.0);
        assert!((zoom_frac(0.75, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(zoom_frac(1.0, 0.5), 1.0);
        assert!((zoom_frac(0.9, 0.25) - 0.6).abs() < 1e-5);
    }

    /// The longest edge of the posed lod-0 mesh relative to the same edge in the bind pose (edges shorter than a
    /// unit count as a unit).
    fn worst_stretch(m: &ModelInstance) -> f32 {
        let model = &m.model;
        let bind = vec![Affine3A::IDENTITY; usize::from(model.num_bones)];
        let posed = render::skin::skin_matrices(model, &m.bones);
        let lod = &model.lod_info[0];
        let first = usize::from(lod.surf_index);
        let mut worst = 0.0f32;
        for s in &model.surfs[first..first + usize::from(lod.surf_count)] {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            render::skin::skin_surface(s, &bind, &mut a);
            render::skin::skin_surface(s, &posed, &mut b);
            let at = |v: &[u8], i: usize| {
                let f = |k: usize| {
                    f32::from_le_bytes(v[32 * i + k..32 * i + k + 4].try_into().unwrap())
                };
                Vec3::new(f(0), f(4), f(8))
            };
            for t in s.tri_indices.as_chunks::<3>().0 {
                for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                    let (p, q) = (usize::from(t[i]), usize::from(t[j]));
                    if 32 * p.max(q) + 12 > a.len() || 32 * p.max(q) + 12 > b.len() {
                        continue;
                    }
                    let (l0, l1) = (
                        (at(&a, p) - at(&a, q)).length().max(1.0),
                        (at(&b, p) - at(&b, q)).length(),
                    );
                    worst = worst.max(l1 / l0);
                }
            }
        }
        worst
    }

    /// Aiming moves the whole view model a few units to the sights and keeps its orientation. Playing the sights
    /// animation instead of layering it over the idle threw the arms and gun about the screen.
    #[test]
    fn aiming_down_sights_shifts_the_weapon_without_flipping_it() {
        let Some(content) = content() else { return };
        let mut vm = build(&content, "m4_mp").expect("view model");
        let [hip, aimed, _] = hip_ads_hip(&mut vm);
        let mut moved = 0.0f32;
        for (h, a) in hip.iter().zip(&aimed) {
            assert_eq!(h.bones.len(), a.bones.len());
            for (hb, ab) in h.bones.iter().zip(&a.bones) {
                let d = (0..3)
                    .map(|i| (hb.trans[i] - ab.trans[i]).powi(2))
                    .sum::<f32>();
                moved = moved.max(d.sqrt());
                let dot: f32 = hb.quat.iter().zip(&ab.quat).map(|(x, y)| x * y).sum();
                assert!(
                    dot.abs() > 0.98,
                    "a bone turned by more than 12 degrees: {dot}"
                );
            }
        }
        assert!(moved > 1.0, "aiming did not move the weapon ({moved})");
        assert!(moved < 12.0, "aiming threw the weapon {moved} units");
    }

    /// Letting the sights down puts the weapon back exactly where the hip pose had it, off to the right and low, not in
    /// front of the view; nothing of the sights layer stays.
    #[test]
    fn the_hip_pose_comes_back_after_aiming() {
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
        let hands = content
            .model_names("viewhands_")
            .first()
            .map(|n| (*n).to_owned());
        for weapon in ["m4_mp", "ak47_mp", "mp5_mp"] {
            let def = content.weapon(weapon).expect("weapon").clone();
            let mut vm = ViewModel::new(&content, &def, hands.as_deref()).expect("view model");
            let mut ps = PlayerState {
                view_height_current: 60.0,
                ..PlayerState::default()
            };
            let hip = vm.update(&ps, 0.01);
            // The gun's root is right of the view axis and below it at the hip (view space: x forward, y left).
            let root = hip[1].bones[0].trans;
            assert!(
                root[1] < -1.5 && root[2] < -1.0,
                "{weapon}: hip gun at {root:?}"
            );
            for ads in [0.4, 1.0, 0.6, 0.0] {
                ps.weapon_pos_frac = ads;
                vm.update(&ps, 0.01);
            }
            let back = vm.update(&ps, 0.01);
            for (h, b) in hip.iter().zip(&back) {
                for (hb, bb) in h.bones.iter().zip(&b.bones) {
                    assert_eq!(
                        hb.trans, bb.trans,
                        "{weapon}: the hip pose did not come back"
                    );
                }
            }
            // And fully aimed it is centred sideways.
            ps.weapon_pos_frac = 1.0;
            let aimed = vm.update(&ps, 0.01)[1].bones[0].trans;
            assert!(aimed[1].abs() < 0.5, "{weapon}: aimed gun at {aimed:?}");
        }
    }
}
