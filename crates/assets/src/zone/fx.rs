// SPDX-License-Identifier: GPL-3.0-only
//! FxEffectDef and FxImpactTable.

use super::error::{Result, ZoneError};
use super::gfx::{self, Material, Name};
use super::stream::{Addr, Fields, Ptr, Stream};
use super::xmodel::{self, XModel};
use std::sync::Arc;

/// Element types, as stored in [`FxElemDef::elem_type`].
pub mod elem {
    pub const SPRITE_BILLBOARD: u8 = 0;
    pub const SPRITE_ORIENTED: u8 = 1;
    pub const TAIL: u8 = 2;
    pub const TRAIL: u8 = 3;
    pub const CLOUD: u8 = 4;
    pub const MODEL: u8 = 5;
    pub const OMNI_LIGHT: u8 = 6;
    pub const SPOT_LIGHT: u8 = 7;
    pub const SOUND: u8 = 8;
    pub const DECAL: u8 = 9;
    pub const RUNNER: u8 = 10;
}

/// Bits of [`FxElemDef::flags`] (from how the original's update and draw code tests them).
pub mod flags {
    /// The spawn origin offset is in the effect's frame instead of the world's.
    pub const SPAWN_RELATIVE_TO_EFFECT: i32 = 0x2;
    pub const SPAWN_OFFSET_MASK: i32 = 0x30;
    pub const SPAWN_OFFSET_SPHERE: i32 = 0x10;
    pub const SPAWN_OFFSET_CYLINDER: i32 = 0x20;
    /// Which frame the local velocity is in: none (world axes), the effect's frame when the element spawned, its
    /// frame now, or the direction of the spawn offset.
    pub const RUN_MASK: i32 = 0xC0;
    pub const RUN_RELATIVE_TO_SPAWN: i32 = 0x40;
    pub const RUN_RELATIVE_TO_EFFECT: i32 = 0x80;
    pub const RUN_RELATIVE_TO_OFFSET: i32 = 0xC0;
    pub const USE_COLLISION: i32 = 0x100;
    pub const DIE_ON_TOUCH: i32 = 0x200;
    pub const DRAW_PAST_FOG: i32 = 0x400;
    /// The local velocity samples apply.
    pub const HAS_VELOCITY_LOCAL: i32 = 0x100_0000;
    /// The world velocity samples apply.
    pub const HAS_VELOCITY_WORLD: i32 = 0x200_0000;
    /// A model element is a rigid body in the physics world (its model's PhysPreset), not a particle.
    pub const USE_MODEL_PHYSICS: i32 = 0x800_0000;
    /// Sprites scale their two axes separately.
    pub const NONUNIFORM_SCALE: i32 = 0x1000_0000;
}

/// `base + random * amplitude` range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range<T> {
    pub base: T,
    pub amplitude: T,
}

#[derive(Debug)]
pub struct FxEffectDef {
    pub name: Name,
    pub flags: i32,
    pub total_size: i32,
    pub msec_looping_life: i32,
    pub looping_count: u32,
    pub one_shot_count: u32,
    pub emission_count: u32,
    /// Looping elements, then one-shot, then emission.
    pub elems: Arc<[FxElemDef]>,
}

/// Velocity of one sample in one frame of reference.
#[derive(Debug)]
pub struct VelFrame {
    pub velocity: Range<[f32; 3]>,
    pub total_delta: Range<[f32; 3]>,
}

#[derive(Debug)]
pub struct VelSample {
    pub local: VelFrame,
    pub world: VelFrame,
}

#[derive(Debug)]
pub struct VisState {
    pub color: [u8; 4],
    pub rotation_delta: f32,
    pub rotation_total: f32,
    pub size: [f32; 2],
    pub scale: f32,
}

#[derive(Debug)]
pub struct VisSample {
    pub base: VisState,
    pub amplitude: VisState,
}

/// Visual payload of an element; which variant is fixed by its element type.
#[derive(Debug)]
pub enum FxVisuals {
    /// Light elements carry no visual.
    None,
    Materials(Arc<[Option<Arc<Material>>]>),
    Models(Arc<[Option<Arc<XModel>>]>),
    /// Sound alias names.
    Sounds(Arc<[Name]>),
    /// Names of the effects a runner spawns.
    Effects(Arc<[Name]>),
    /// Decal materials, two per visual.
    Decals(Arc<[[Option<Arc<Material>>; 2]]>),
}

#[derive(Debug)]
pub struct FxTrailDef {
    pub scroll_time_msec: i32,
    pub repeat_dist: i32,
    pub split_dist: i32,
    /// Per vertex: position, normal, texture coordinate.
    pub verts: Arc<[[f32; 5]]>,
    pub indices: Arc<[u16]>,
}

#[derive(Debug)]
pub struct FxElemDef {
    pub flags: i32,
    /// Looping: interval msec and count. One-shot: count base and amplitude.
    pub spawn: [i32; 2],
    pub spawn_range: Range<f32>,
    pub fade_in_range: Range<f32>,
    pub fade_out_range: Range<f32>,
    pub spawn_frustum_cull_radius: f32,
    pub spawn_delay_msec: Range<i32>,
    pub life_span_msec: Range<i32>,
    pub spawn_origin: [Range<f32>; 3],
    pub spawn_offset_radius: Range<f32>,
    pub spawn_offset_height: Range<f32>,
    pub spawn_angles: [Range<f32>; 3],
    pub angular_velocity: [Range<f32>; 3],
    pub initial_rotation: Range<f32>,
    pub gravity: Range<f32>,
    pub reflection_factor: Range<f32>,
    /// behavior, index, fps, loop count, column index bits, row index bits.
    pub atlas: [u8; 6],
    pub atlas_entry_count: i16,
    pub elem_type: u8,
    pub vel_samples: Arc<[VelSample]>,
    pub vis_samples: Arc<[VisSample]>,
    pub visuals: FxVisuals,
    pub coll_mins: [f32; 3],
    pub coll_maxs: [f32; 3],
    /// Effects are referenced by name; resolution is the caller's.
    pub effect_on_impact: Name,
    pub effect_on_death: Name,
    pub effect_emitted: Name,
    pub emit_dist: Range<f32>,
    pub emit_dist_variance: Range<f32>,
    pub trail: Option<Arc<FxTrailDef>>,
    pub sort_order: u8,
    pub lighting_frac: u8,
    pub use_item_clip: bool,
}

const ELEM_SIZE: u32 = 252;
const EFFECT_SIZE: u32 = 32;

fn frange(f: &mut Fields) -> Range<f32> {
    Range {
        base: f.f32(),
        amplitude: f.f32(),
    }
}

fn irange(f: &mut Fields) -> Range<i32> {
    Range {
        base: f.i32(),
        amplitude: f.i32(),
    }
}

fn vec3(f: &mut Fields) -> [f32; 3] {
    [f.f32(), f.f32(), f.f32()]
}

fn vec3_range(f: &mut Fields) -> Range<[f32; 3]> {
    Range {
        base: vec3(f),
        amplitude: vec3(f),
    }
}

fn vel_frame(f: &mut Fields) -> VelFrame {
    VelFrame {
        velocity: vec3_range(f),
        total_delta: vec3_range(f),
    }
}

fn vis_state(f: &mut Fields) -> VisState {
    VisState {
        color: f.bytes(),
        rotation_delta: f.f32(),
        rotation_total: f.f32(),
        size: [f.f32(), f.f32()],
        scale: f.f32(),
    }
}

/// `count` pointer-sized visual slots, each decoded by `one`. A count of one
/// or less stores the visual in the pointer itself.
fn visual_slots<T: std::any::Any + Send + Sync>(
    s: &mut Stream,
    slot: Option<Addr>,
    p: Ptr,
    count: u32,
    mut one: impl FnMut(&mut Stream, Option<Addr>, Ptr) -> Result<T>,
) -> Result<Arc<[T]>> {
    if count > 1 {
        s.array(p, count, 4, 4, |s, f| {
            let slot = f.slot();
            let p = f.ptr()?;
            one(s, slot, p)
        })
    } else {
        Ok(Arc::from(vec![one(s, slot, p)?]))
    }
}

fn visuals(s: &mut Stream, kind: u8, count: u32, slot: Option<Addr>, p: Ptr) -> Result<FxVisuals> {
    Ok(match kind {
        elem::DECAL => FxVisuals::Decals(s.array(p, count, 4, 8, |s, f| {
            let (sa, a) = (f.slot(), f.ptr()?);
            let (sb, b) = (f.slot(), f.ptr()?);
            Ok([
                gfx::material_ptr_at(s, sa, a)?,
                gfx::material_ptr_at(s, sb, b)?,
            ])
        })?),
        elem::SPRITE_BILLBOARD | elem::SPRITE_ORIENTED | elem::TAIL | elem::TRAIL | elem::CLOUD => {
            FxVisuals::Materials(visual_slots(s, slot, p, count, gfx::material_ptr_at)?)
        }
        elem::MODEL => FxVisuals::Models(visual_slots(s, slot, p, count, xmodel::load_at)?),
        elem::SOUND => FxVisuals::Sounds(visual_slots(s, slot, p, count, |s, _, p| s.string(p))?),
        elem::RUNNER => FxVisuals::Effects(visual_slots(s, slot, p, count, |s, _, p| s.string(p))?),
        _ => FxVisuals::None,
    })
}

fn trail(s: &mut Stream, h: &[u8]) -> Result<FxTrailDef> {
    let mut f = Fields::new(h);
    let (scroll_time_msec, repeat_dist, split_dist) = (f.i32(), f.i32(), f.i32());
    let vert_count = f.u32();
    let verts = f.ptr()?;
    let ind_count = f.u32();
    let inds = f.ptr()?;
    let verts = s.array(verts, vert_count, 4, 20, |_, f| {
        Ok([f.f32(), f.f32(), f.f32(), f.f32(), f.f32()])
    })?;
    let indices = s.array(inds, ind_count, 2, 2, |_, f| Ok(f.u16()))?;
    Ok(FxTrailDef {
        scroll_time_msec,
        repeat_dist,
        split_dist,
        verts,
        indices,
    })
}

fn elem_def(s: &mut Stream, f: &mut Fields) -> Result<FxElemDef> {
    let flags = f.i32();
    let spawn = [f.i32(), f.i32()];
    let spawn_range = frange(f);
    let fade_in_range = frange(f);
    let fade_out_range = frange(f);
    let spawn_frustum_cull_radius = f.f32();
    let spawn_delay_msec = irange(f);
    let life_span_msec = irange(f);
    let spawn_origin = [frange(f), frange(f), frange(f)];
    let spawn_offset_radius = frange(f);
    let spawn_offset_height = frange(f);
    let spawn_angles = [frange(f), frange(f), frange(f)];
    let angular_velocity = [frange(f), frange(f), frange(f)];
    let initial_rotation = frange(f);
    let gravity = frange(f);
    let reflection_factor = frange(f);
    let atlas = f.bytes();
    let atlas_entry_count = i16::from_le_bytes(f.bytes());
    let elem_type = f.u8();
    let visual_count = u32::from(f.u8());
    let vel_intervals = u32::from(f.u8());
    let vis_intervals = u32::from(f.u8());
    let (vel_p, vis_p) = (f.ptr()?, f.ptr()?);
    let visuals_slot = f.slot();
    let visuals_p = f.ptr()?;
    let coll_mins = vec3(f);
    let coll_maxs = vec3(f);
    let (on_impact, on_death, emitted) = (f.ptr()?, f.ptr()?, f.ptr()?);
    let emit_dist = frange(f);
    let emit_dist_variance = frange(f);
    let trail_p = f.ptr()?;
    let sort_order = f.u8();
    let lighting_frac = f.u8();
    let use_item_clip = f.u8() != 0;

    let vel_samples = s.array(vel_p, vel_intervals + 1, 4, 96, |_, f| {
        Ok(VelSample {
            local: vel_frame(f),
            world: vel_frame(f),
        })
    })?;
    let vis_samples = s.array(vis_p, vis_intervals + 1, 4, 48, |_, f| {
        Ok(VisSample {
            base: vis_state(f),
            amplitude: vis_state(f),
        })
    })?;
    let visuals = visuals(s, elem_type, visual_count, visuals_slot, visuals_p)?;
    let effect_on_impact = s.string(on_impact)?;
    let effect_on_death = s.string(on_death)?;
    let effect_emitted = s.string(emitted)?;
    let trail = s.shared(trail_p, 4, 28, trail)?;
    Ok(FxElemDef {
        flags,
        spawn,
        spawn_range,
        fade_in_range,
        fade_out_range,
        spawn_frustum_cull_radius,
        spawn_delay_msec,
        life_span_msec,
        spawn_origin,
        spawn_offset_radius,
        spawn_offset_height,
        spawn_angles,
        angular_velocity,
        initial_rotation,
        gravity,
        reflection_factor,
        atlas,
        atlas_entry_count,
        elem_type,
        vel_samples,
        vis_samples,
        visuals,
        coll_mins,
        coll_maxs,
        effect_on_impact,
        effect_on_death,
        effect_emitted,
        emit_dist,
        emit_dist_variance,
        trail,
        sort_order,
        lighting_frac,
        use_item_clip,
    })
}

fn effect(s: &mut Stream, h: &[u8]) -> Result<FxEffectDef> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let flags = f.i32();
    let total_size = f.i32();
    let msec_looping_life = f.i32();
    let looping_count = f.u32();
    let one_shot_count = f.u32();
    let emission_count = f.u32();
    let elems = f.ptr()?;
    let name = s.string(name)?;
    let count = looping_count
        .checked_add(one_shot_count)
        .and_then(|n| n.checked_add(emission_count))
        .ok_or(ZoneError::Invalid("bad effect element count"))?;
    let elems = s.array(elems, count, 4, ELEM_SIZE, elem_def)?;
    Ok(FxEffectDef {
        name,
        flags,
        total_size,
        msec_looping_life,
        looping_count,
        one_shot_count,
        emission_count,
        elems,
    })
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<FxEffectDef>>> {
    load_at(s, None, p)
}

pub(super) fn load_at(
    s: &mut Stream,
    slot: Option<Addr>,
    p: Ptr,
) -> Result<Option<Arc<FxEffectDef>>> {
    s.temp_asset_at(slot, p, 4, EFFECT_SIZE, effect)
}

/// Impact effects of one surface type: 29 non-flesh and 4 flesh surfaces.
#[derive(Debug)]
pub struct FxImpactEntry {
    pub nonflesh: [Option<Arc<FxEffectDef>>; 29],
    pub flesh: [Option<Arc<FxEffectDef>>; 4],
}

#[derive(Debug)]
pub struct FxImpactTable {
    pub name: Name,
    pub table: Arc<[FxImpactEntry]>,
}

fn impact(s: &mut Stream, h: &[u8]) -> Result<FxImpactTable> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let table = f.ptr()?;
    let name = s.string(name)?;
    let table = s.array(table, 12, 4, 132, |s, f| {
        let mut ptrs = [(None, Ptr::Null); 33];
        for p in &mut ptrs {
            *p = (f.slot(), f.ptr()?);
        }
        let mut loaded = Vec::with_capacity(33);
        for (slot, p) in ptrs {
            loaded.push(load_at(s, slot, p)?);
        }
        let flesh = loaded.split_off(29);
        Ok(FxImpactEntry {
            nonflesh: loaded.try_into().expect("29 entries"),
            flesh: flesh.try_into().expect("4 entries"),
        })
    })?;
    Ok(FxImpactTable { name, table })
}

pub(super) fn load_impact(s: &mut Stream, p: Ptr) -> Result<Option<Arc<FxImpactTable>>> {
    s.temp_asset(p, 4, 8, impact)
}
