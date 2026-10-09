// SPDX-License-Identifier: GPL-3.0-only
// Derived from KisakCOD (https://github.com/KisakJarvis/KisakCOD), game_mp/g_misc_mp.cpp (`turret_clientaim`,
// `turret_controller`): copyright the KisakCOD contributors; the original is Call of Duty 4 by Infinity Ward /
// Activision.
//! A mounted turret's gun: how far it swings with its gunner's view, where its tags go as it does, and where that puts
//! the gunner. The server and the predicting client both call this, so they place the gunner alike.

use std::sync::Arc;

use super::quat;
use super::rig::{Controllers, Pose, Rig};
use crate::Vec3;
use crate::cm::Collide;
use crate::contents;
use crate::pm::math::angle_delta;

/// `turret_clientaim`'s `gunAngles`: the view's pitch and yaw from the way the turret faces, held in the arcs (`min` and
/// `max` from the turret's angles, `[pitch, yaw]`).
pub fn gun_angles(view: Vec3, turret: Vec3, min: [f32; 2], max: [f32; 2]) -> [f32; 3] {
    let swing = |i: usize| angle_delta(view[i], turret[i]).clamp(min[i], max[i]);
    [swing(0), swing(1), 0.0]
}

/// The arcs a gunner's view clamp (`viewAngleClampBase` and `Range`, both `[pitch, yaw]`) holds the view in, as
/// `(min, max)` from the way a turret facing `turret` points.
pub fn arcs_of_clamp(base: [f32; 2], range: [f32; 2], turret: Vec3) -> ([f32; 2], [f32; 2]) {
    let mid: [f32; 2] = std::array::from_fn(|i| angle_delta(base[i], turret[i]));
    (
        std::array::from_fn(|i| mid[i] - range[i]),
        std::array::from_fn(|i| mid[i] + range[i]),
    )
}

/// Where `tag` of the turret model `rig` is in the world, the turret at `origin` facing `angles`, its gun swung by
/// `gun` (`turret_controller`).
pub fn tag_point(rig: &Rig, gun: [f32; 3], tag: &str, origin: Vec3, angles: Vec3) -> Option<Vec3> {
    let mut pose = Pose::default();
    let ctl = Controllers {
        turret: Some(gun),
        ..Controllers::NONE
    };
    rig.pose(&[], &ctl, &mut pose);
    let local = pose.bones().get(rig.bone_index(tag)?)?.trans;
    let w = quat::rotate(&quat::from_angles(&angles), &local);
    Some([origin[0] + w[0], origin[1] + w[1], origin[2] + w[2]])
}

/// The feet of a gunner whose eye is at `eye` (`G_PlayerTurretPositionAndBlend`): the eye's height below it, then down
/// to the floor when that is within 60 units.
pub fn gunner_feet(world: &dyn Collide, eye: Vec3, view_height: f32, ignore: u16) -> Vec3 {
    let mut feet = [eye[0], eye[1], eye[2] - view_height];
    let end = [eye[0], eye[1], eye[2] - 60.0];
    let tr = world.trace(
        eye,
        end,
        [0.0; 3],
        [0.0; 3],
        ignore,
        contents::MASK_DEADSOLID,
    );
    if tr.fraction < 1.0 {
        feet[2] = eye[2] + (end[2] - eye[2]) * tr.fraction;
    }
    feet
}

/// What the predicting client needs to put its own gunner where the server does: the turret's rig, where it stands and
/// the arcs the view is held in.
#[derive(Clone)]
pub struct Seat {
    pub rig: Arc<Rig>,
    pub origin: Vec3,
    pub angles: Vec3,
}

impl Seat {
    /// The feet of the gunner looking along `view`, the view held by `base` and `range` (the player state's
    /// `view_angle_clamp_base` and `_range`).
    pub fn feet(
        &self,
        world: &dyn Collide,
        view: Vec3,
        base: [f32; 2],
        range: [f32; 2],
        view_height: f32,
        client: u16,
    ) -> Option<Vec3> {
        let (min, max) = arcs_of_clamp(base, range, self.angles);
        let gun = gun_angles(view, self.angles, min, max);
        let eye = tag_point(&self.rig, gun, "tag_player", self.origin, self.angles)?;
        Some(gunner_feet(world, eye, view_height, client))
    }
}
