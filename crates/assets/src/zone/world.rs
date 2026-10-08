// SPDX-License-Identifier: GPL-3.0-only
//! ComWorld (primary lights), GameWorldMp and GfxLightDef.

use super::error::Result;
use super::gfx::{GfxImage, Name, image_ptr};
use super::stream::{Fields, Ptr, Stream};
use std::sync::Arc;

/// A map's static primary light as the common world stores it.
#[derive(Debug)]
pub struct ComPrimaryLight {
    pub kind: u8,
    pub can_use_shadow_map: bool,
    pub exponent: u8,
    pub color: [f32; 3],
    pub dir: [f32; 3],
    pub origin: [f32; 3],
    pub radius: f32,
    pub cos_half_fov_outer: f32,
    pub cos_half_fov_inner: f32,
    pub cos_half_fov_expanded: f32,
    pub rotation_limit: f32,
    pub translation_limit: f32,
    pub def_name: Name,
}

#[derive(Debug)]
pub struct ComWorld {
    pub name: Name,
    pub is_in_use: bool,
    pub primary_lights: Arc<[ComPrimaryLight]>,
}

pub(super) fn load_com_world(s: &mut Stream, p: Ptr) -> Result<Option<Arc<ComWorld>>> {
    s.temp_asset(p, 4, 16, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let is_in_use = f.i32() != 0;
        let count = f.u32();
        let lights = f.ptr()?;
        let name = s.string(name)?;
        let primary_lights = s.array(lights, count, 4, 68, |s, f| {
            let kind = f.u8();
            let can_use_shadow_map = f.u8() != 0;
            let exponent = f.u8();
            f.skip(1);
            let v3 = |f: &mut Fields| [f.f32(), f.f32(), f.f32()];
            let (color, dir, origin) = (v3(f), v3(f), v3(f));
            let radius = f.f32();
            let (outer, inner, expanded) = (f.f32(), f.f32(), f.f32());
            let (rotation_limit, translation_limit) = (f.f32(), f.f32());
            let def_name = s.string(f.ptr()?)?;
            Ok(ComPrimaryLight {
                kind,
                can_use_shadow_map,
                exponent,
                color,
                dir,
                origin,
                radius,
                cos_half_fov_outer: outer,
                cos_half_fov_inner: inner,
                cos_half_fov_expanded: expanded,
                rotation_limit,
                translation_limit,
                def_name,
            })
        })?;
        Ok(ComWorld {
            name,
            is_in_use,
            primary_lights,
        })
    })
}

/// The multiplayer game world carries only its name (no path data).
#[derive(Debug)]
pub struct GameWorldMp {
    pub name: Name,
}

pub(super) fn load_game_world_mp(s: &mut Stream, p: Ptr) -> Result<Option<Arc<GameWorldMp>>> {
    s.temp_asset(p, 4, 4, |s, h| {
        let name = s.string(Fields::new(h).ptr()?)?;
        Ok(GameWorldMp { name })
    })
}

#[derive(Debug)]
pub struct LightDef {
    pub name: Name,
    pub attenuation_image: Option<Arc<GfxImage>>,
    pub attenuation_sampler_state: u8,
    pub lmap_lookup_start: i32,
}

pub(super) fn load_light_def(s: &mut Stream, p: Ptr) -> Result<Option<Arc<LightDef>>> {
    s.temp_asset(p, 4, 16, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let image = f.ptr()?;
        let attenuation_sampler_state = f.u8();
        f.skip(3);
        let lmap_lookup_start = f.i32();
        let name = s.string(name)?;
        let attenuation_image = image_ptr(s, image)?;
        Ok(LightDef {
            name,
            attenuation_image,
            attenuation_sampler_state,
            lmap_lookup_start,
        })
    })
}
