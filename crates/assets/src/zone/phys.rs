// SPDX-License-Identifier: GPL-3.0-or-later
//! PhysPreset.

use super::error::Result;
use super::gfx::Name;
use super::stream::{Fields, Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct PhysPreset {
    pub name: Name,
    pub kind: i32,
    pub mass: f32,
    pub bounce: f32,
    pub friction: f32,
    pub bullet_force_scale: f32,
    pub explosive_force_scale: f32,
    pub snd_alias_prefix: Name,
    pub pieces_spread_fraction: f32,
    pub pieces_upward_velocity: f32,
    pub temp_default_to_cylinder: bool,
}

const PRESET_SIZE: u32 = 44;

fn preset(s: &mut Stream, h: &[u8]) -> Result<PhysPreset> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let kind = f.i32();
    let (mass, bounce, friction) = (f.f32(), f.f32(), f.f32());
    let (bullet_force_scale, explosive_force_scale) = (f.f32(), f.f32());
    let prefix = f.ptr()?;
    let (pieces_spread_fraction, pieces_upward_velocity) = (f.f32(), f.f32());
    let temp_default_to_cylinder = f.u8() != 0;
    Ok(PhysPreset {
        name: s.string(name)?,
        kind,
        mass,
        bounce,
        friction,
        bullet_force_scale,
        explosive_force_scale,
        snd_alias_prefix: s.string(prefix)?,
        pieces_spread_fraction,
        pieces_upward_velocity,
        temp_default_to_cylinder,
    })
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<PhysPreset>>> {
    s.temp_asset(p, 4, PRESET_SIZE, preset)
}
