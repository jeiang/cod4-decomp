// SPDX-License-Identifier: GPL-3.0-or-later
//! GfxWorld: the renderable map (geometry, visibility cells, lighting, static
//! models). Bulk payload (vertices, indices, light grid texels) is presentation
//! data and is empty when the consumer drops it.

use super::error::{Result, ZoneError};
use super::gfx::{
    GfxImage, Material, Name, image_ptr, image_ptr_at, material_ptr, material_ptr_at, raw_of,
};
use super::stream::{Addr, Block, Fields, Ptr, Stream};
use super::world::{LightDef, load_light_def};
use super::xmodel::{XModel, load_at as load_xmodel_at};
use std::sync::Arc;

type V3 = [f32; 3];

fn v3(f: &mut Fields) -> V3 {
    [f.f32(), f.f32(), f.f32()]
}

/// A collision/visibility plane (`cplane_s`). Shared with other assets that
/// point back at the same plane array.
#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub normal: V3,
    pub dist: f32,
    pub kind: u8,
    pub sign_bits: u8,
}

/// The sun as the map compiler parsed it.
#[derive(Debug)]
pub struct SunParse {
    pub name: String,
    pub ambient_scale: f32,
    pub ambient_color: V3,
    pub diffuse_fraction: f32,
    pub sun_light: f32,
    pub sun_color: V3,
    pub diffuse_color: V3,
    pub diffuse_color_has_been_set: bool,
    pub angles: V3,
}

#[derive(Debug)]
pub struct GfxLight {
    pub kind: u8,
    pub can_use_shadow_map: bool,
    pub color: V3,
    pub dir: V3,
    pub origin: V3,
    pub radius: f32,
    pub cos_half_fov_outer: f32,
    pub cos_half_fov_inner: f32,
    pub exponent: i32,
    pub spot_shadow_index: u32,
    pub def: Option<Arc<LightDef>>,
}

#[derive(Debug)]
pub struct ReflectionProbe {
    pub origin: V3,
    pub image: Option<Arc<GfxImage>>,
}

#[derive(Debug)]
pub struct Portal {
    pub plane: [f32; 4],
    pub plane_side: [u8; 3],
    /// Index into [`GfxWorld::cells`] of the cell behind the portal.
    pub cell: u32,
    pub vertices: Vec<V3>,
    pub hull_axis: [V3; 2],
}

#[derive(Debug)]
pub struct AabbTree {
    pub mins: V3,
    pub maxs: V3,
    pub child_count: u16,
    pub surface_count: u16,
    pub start_surf_index: u16,
    pub surface_count_no_decal: u16,
    pub start_surf_index_no_decal: u16,
    pub smodel_indexes: Vec<u16>,
    pub children_offset: i32,
}

#[derive(Debug)]
pub struct Cell {
    pub mins: V3,
    pub maxs: V3,
    pub aabb_trees: Vec<AabbTree>,
    pub portals: Vec<Portal>,
    pub cull_groups: Vec<i32>,
    pub reflection_probes: Vec<u8>,
}

#[derive(Debug)]
pub struct Lightmap {
    pub primary: Option<Arc<GfxImage>>,
    pub secondary: Option<Arc<GfxImage>>,
}

/// Light volume grid. `entries` (4 bytes each) and `colors` (56 RGB triples
/// each) are presentation payload.
#[derive(Debug)]
pub struct LightGrid {
    pub has_light_regions: bool,
    pub sun_primary_light_index: u32,
    pub mins: [u16; 3],
    pub maxs: [u16; 3],
    pub row_axis: u32,
    pub col_axis: u32,
    pub row_data_start: Vec<u16>,
    pub raw_row_data: Vec<u8>,
    pub entry_count: u32,
    pub entries: Vec<u8>,
    pub color_count: u32,
    pub colors: Vec<u8>,
}

#[derive(Debug)]
pub struct BrushModel {
    pub writable_mins: V3,
    pub writable_maxs: V3,
    pub bounds: [V3; 2],
    pub surface_count: u16,
    pub start_surf_index: u16,
    pub surface_count_no_decal: u16,
}

#[derive(Debug)]
pub struct MaterialMemory {
    pub material: Option<Arc<Material>>,
    pub memory: i32,
}

#[derive(Debug)]
pub struct SunFlare {
    pub has_valid_data: bool,
    pub sprite_material: Option<Arc<Material>>,
    pub flare_material: Option<Arc<Material>>,
    pub sprite_size: f32,
    pub flare_min_size: f32,
    pub flare_min_dot: f32,
    pub flare_max_size: f32,
    pub flare_max_dot: f32,
    pub flare_max_alpha: f32,
    pub flare_fade_in_time: i32,
    pub flare_fade_out_time: i32,
    pub blind_min_dot: f32,
    pub blind_max_dot: f32,
    pub blind_max_darken: f32,
    pub blind_fade_in_time: i32,
    pub blind_fade_out_time: i32,
    pub glare_min_dot: f32,
    pub glare_max_dot: f32,
    pub glare_max_lighten: f32,
    pub glare_fade_in_time: i32,
    pub glare_fade_out_time: i32,
    pub sun_fx_position: V3,
}

#[derive(Debug)]
pub struct ShadowGeometry {
    pub sorted_surf_index: Vec<u16>,
    pub smodel_index: Vec<u16>,
}

#[derive(Debug)]
pub struct LightRegionAxis {
    pub dir: V3,
    pub mid_point: f32,
    pub half_size: f32,
}

#[derive(Debug)]
pub struct LightRegionHull {
    pub kdop_mid_point: [f32; 9],
    pub kdop_half_size: [f32; 9],
    pub axes: Vec<LightRegionAxis>,
}

#[derive(Debug)]
pub struct StaticModelInst {
    pub mins: V3,
    pub maxs: V3,
    pub ground_lighting: u32,
}

#[derive(Debug)]
pub struct StaticModel {
    pub cull_dist: f32,
    pub origin: V3,
    pub axis: [V3; 3],
    pub scale: f32,
    pub model: Option<Arc<XModel>>,
    pub cache_index: [u16; 4],
    pub reflection_probe_index: u8,
    pub primary_light_index: u8,
    pub lighting_handle: u16,
    pub flags: u8,
}

#[derive(Debug)]
pub struct Surface {
    pub vertex_layer_data: i32,
    pub first_vertex: i32,
    pub vertex_count: u16,
    pub tri_count: u16,
    pub base_index: i32,
    pub material: Option<Arc<Material>>,
    pub lightmap_index: u8,
    pub reflection_probe_index: u8,
    pub primary_light_index: u8,
    pub flags: u8,
    pub bounds: [V3; 2],
}

#[derive(Debug)]
pub struct CullGroup {
    pub mins: V3,
    pub maxs: V3,
    pub surface_count: i32,
    pub start_surf_index: i32,
}

/// Static visibility geometry.
#[derive(Debug)]
pub struct DpvsStatic {
    pub smodel_count: u32,
    pub static_surface_count: u32,
    pub static_surface_count_no_decal: u32,
    pub lit_surfs: [u32; 2],
    pub decal_surfs: [u32; 2],
    pub emissive_surfs: [u32; 2],
    pub smodel_vis_data_count: u32,
    pub surface_vis_data_count: u32,
    pub sorted_surf_index: Vec<u16>,
    pub smodel_insts: Vec<StaticModelInst>,
    pub surfaces: Vec<Surface>,
    pub cull_groups: Vec<CullGroup>,
    pub smodel_draw_insts: Vec<StaticModel>,
}

#[derive(Debug)]
pub struct GfxWorld {
    pub name: Name,
    pub base_name: Name,
    pub planes: Arc<[Plane]>,
    /// BSP node indices for the visibility tree.
    pub nodes: Vec<u16>,
    /// Triangle index buffer; presentation payload.
    pub indices: Vec<u16>,
    pub surface_count: i32,
    pub sky_start_surfs: Vec<i32>,
    pub sky_image: Option<Arc<GfxImage>>,
    pub sky_sampler_state: u8,
    pub vertex_count: u32,
    /// `vertex_count` packed 44-byte world vertices; presentation payload.
    pub vertices: Vec<u8>,
    pub vertex_layer_data_size: u32,
    /// Per-layer vertex stream; presentation payload.
    pub vertex_layer_data: Vec<u8>,
    pub sun_parse: SunParse,
    pub sun_light: Option<Arc<GfxLight>>,
    pub sun_color_from_bsp: V3,
    pub sun_primary_light_index: u32,
    pub primary_light_count: u32,
    pub cull_group_count: i32,
    pub reflection_probes: Vec<ReflectionProbe>,
    pub cell_bits_count: i32,
    pub cells: Vec<Cell>,
    pub lightmaps: Vec<Lightmap>,
    pub light_grid: LightGrid,
    pub models: Vec<BrushModel>,
    pub mins: V3,
    pub maxs: V3,
    pub checksum: u32,
    pub material_memory: Vec<MaterialMemory>,
    pub sun: SunFlare,
    pub outdoor_lookup_matrix: [f32; 16],
    pub outdoor_image: Option<Arc<GfxImage>>,
    pub shadow_geometry: Vec<ShadowGeometry>,
    pub light_regions: Vec<Vec<LightRegionHull>>,
    pub dpvs: DpvsStatic,
    /// Dynamic entity counts: `[models, brushes]`.
    pub dyn_ent_client_count: [u32; 2],
}

const GFXWORLD_SIZE: u32 = 0x2DC;

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<GfxWorld>>> {
    s.temp_asset(p, 4, GFXWORLD_SIZE, world)
}

fn count(v: i32) -> Result<u32> {
    u32::try_from(v).map_err(|_| ZoneError::Invalid("negative gfxworld count"))
}

/// Presentation-only bytes behind a pointer.
fn bulk(s: &mut Stream, p: Ptr, align: u32, len: u32) -> Result<Vec<u8>> {
    match p {
        Ptr::Null => Ok(Vec::new()),
        Ptr::Follow => Ok(s.load_presentation(align, len)?.1),
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

/// Runtime-block array: space is reserved, nothing is read.
fn runtime(s: &mut Stream, p: Ptr, align: u32, len: u32) -> Result<()> {
    match p {
        Ptr::Null => Ok(()),
        Ptr::Follow => {
            s.push(Block::Runtime);
            s.alloc(align, len)?;
            s.pop()
        }
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

fn mul(a: u32, b: u32) -> Result<u32> {
    a.checked_mul(b)
        .ok_or(ZoneError::Invalid("gfxworld count overflow"))
}

fn u16s(s: &mut Stream, p: Ptr, n: u32) -> Result<Arc<[u16]>> {
    s.array(p, n, 2, 2, |_, f| Ok(f.u16()))
}

fn u16_vec(s: &mut Stream, p: Ptr, n: u32) -> Result<Vec<u16>> {
    Ok(u16s(s, p, n)?.to_vec())
}

/// Elements of a non-shared array, loaded in the current block.
fn vec<T: Send + Sync + 'static>(
    s: &mut Stream,
    p: Ptr,
    n: u32,
    align: u32,
    size: u32,
    f: impl FnMut(&mut Stream, &mut Fields) -> Result<T>,
) -> Result<Vec<T>> {
    match p {
        Ptr::Null => Ok(Vec::new()),
        Ptr::Follow if n == 0 => Ok(Vec::new()),
        Ptr::Follow => {
            let len = mul(n, size)?;
            let (base, bytes) = s.load(align, len)?;
            elements(s, base, &bytes, size, f)
        }
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

fn elements<T>(
    s: &mut Stream,
    base: Addr,
    bytes: &[u8],
    size: u32,
    mut f: impl FnMut(&mut Stream, &mut Fields) -> Result<T>,
) -> Result<Vec<T>> {
    bytes
        .chunks_exact(size as usize)
        .enumerate()
        .map(|(i, c)| {
            let at = Addr {
                block: base.block,
                offset: base.offset + i as u32 * size,
            };
            f(s, &mut Fields::at(c, at))
        })
        .collect()
}

fn plane(f: &mut Fields) -> Plane {
    let normal = v3(f);
    let dist = f.f32();
    let kind = f.u8();
    let sign_bits = f.u8();
    f.skip(2);
    Plane {
        normal,
        dist,
        kind,
        sign_bits,
    }
}

fn world(s: &mut Stream, h: &[u8]) -> Result<GfxWorld> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let base_name = f.ptr()?;
    let plane_count = count(f.i32())?;
    let node_count = count(f.i32())?;
    let index_count = count(f.i32())?;
    let indices = f.ptr()?;
    let surface_count = f.i32();
    f.skip(4); // stream info pad, aligned
    let sky_surf_count = count(f.i32())?;
    let sky_start_surfs = f.ptr()?;
    let sky_image = f.ptr()?;
    let sky_sampler_state = f.u8();
    f.skip(3);
    let vertex_count = f.u32();
    let vertices = f.ptr()?;
    f.skip(4);
    let vertex_layer_data_size = f.u32();
    let layer_data = f.ptr()?;
    f.skip(4);
    let sun_parse = {
        let raw: [u8; 64] = f.bytes();
        let len = raw.iter().position(|&b| b == 0).unwrap_or(64);
        SunParse {
            name: String::from_utf8_lossy(&raw[..len]).into_owned(),
            ambient_scale: f.f32(),
            ambient_color: v3(&mut f),
            diffuse_fraction: f.f32(),
            sun_light: f.f32(),
            sun_color: v3(&mut f),
            diffuse_color: v3(&mut f),
            diffuse_color_has_been_set: {
                let b = f.u8() != 0;
                f.skip(3);
                b
            },
            angles: v3(&mut f),
        }
    };
    let sun_light = f.ptr()?;
    let sun_color_from_bsp = v3(&mut f);
    let sun_primary_light_index = f.u32();
    let primary_light_count = f.u32();
    let cull_group_count = f.i32();
    let probe_count = f.u32();
    let probes = f.ptr()?;
    let probe_textures = f.ptr()?;
    let cell_count = count(f.i32())?;
    let planes = f.ptr()?;
    let nodes = f.ptr()?;
    let scene_ent_cell_bits = f.ptr()?;
    let cell_bits_count = f.i32();
    let cells = f.ptr()?;
    let lightmap_count = count(f.i32())?;
    let lightmaps = f.ptr()?;
    let grid = GridHeader::read(&mut f)?;
    let lightmap_primary = f.ptr()?;
    let lightmap_secondary = f.ptr()?;
    let model_count = count(f.i32())?;
    let models = f.ptr()?;
    let mins = v3(&mut f);
    let maxs = v3(&mut f);
    let checksum = f.u32();
    let material_memory_count = count(f.i32())?;
    let material_memory = f.ptr()?;
    let sun = SunHeader::read(&mut f)?;
    let mut outdoor_lookup_matrix = [0.0; 16];
    for v in &mut outdoor_lookup_matrix {
        *v = f.f32();
    }
    let outdoor_image = f.ptr()?;
    let cell_caster_bits = f.ptr()?;
    let scene_dyn_model = f.ptr()?;
    let scene_dyn_brush = f.ptr()?;
    let light_entity_shadow_vis = f.ptr()?;
    let dyn_ent_shadow_vis = [f.ptr()?, f.ptr()?];
    let non_sun_light = f.ptr()?;
    let shadow_geom = f.ptr()?;
    let light_region = f.ptr()?;
    let dpvs_header = DpvsHeader::read(&mut f)?;
    let dyn_ent_client_word_count = [f.u32(), f.u32()];
    let dyn_ent_client_count = [f.u32(), f.u32()];
    let dyn_ent_cell_bits = [f.ptr()?, f.ptr()?];
    let mut dyn_ent_vis_data = [[Ptr::Null; 3]; 2];
    for row in &mut dyn_ent_vis_data {
        for p in row {
            *p = f.ptr()?;
        }
    }

    // Members in stream order.
    let name = s.string(name)?;
    let base_name = s.string(base_name)?;
    let index_bytes = bulk_indices(s, indices, index_count)?;
    let sky_start_surfs = s.array(sky_start_surfs, sky_surf_count, 4, 4, |_, f| Ok(f.i32()))?;
    let sky_image = image_ptr(s, sky_image)?;
    let sun_light = s.shared(sun_light, 4, 64, |s, b| {
        let mut f = Fields::new(b);
        let kind = f.u8();
        let can_use_shadow_map = f.u8() != 0;
        f.skip(2);
        let (color, dir, origin) = (v3(&mut f), v3(&mut f), v3(&mut f));
        let radius = f.f32();
        let (cos_half_fov_outer, cos_half_fov_inner) = (f.f32(), f.f32());
        let exponent = f.i32();
        let spot_shadow_index = f.u32();
        let def = load_light_def(s, f.ptr()?)?;
        Ok(GfxLight {
            kind,
            can_use_shadow_map,
            color,
            dir,
            origin,
            radius,
            cos_half_fov_outer,
            cos_half_fov_inner,
            exponent,
            spot_shadow_index,
            def,
        })
    })?;
    let reflection_probes = vec(s, probes, probe_count, 4, 16, |s, f| {
        let origin = v3(f);
        let slot = f.slot();
        let image = image_ptr_at(s, slot, f.ptr()?)?;
        Ok(ReflectionProbe { origin, image })
    })?;
    runtime(s, probe_textures, 4, mul(probe_count, 4)?)?;
    let planes = s.array(planes, plane_count, 4, 20, |_, f| Ok(plane(f)))?;
    let nodes = u16_vec(s, nodes, node_count)?;
    runtime(s, scene_ent_cell_bits, 4, mul(cell_count, 0x400)?)?;
    let cells = load_cells(s, cells, cell_count)?;
    let lightmaps = vec(s, lightmaps, lightmap_count, 4, 8, |s, f| {
        let slot = f.slot();
        let primary = image_ptr_at(s, slot, f.ptr()?)?;
        let slot = f.slot();
        let secondary = image_ptr_at(s, slot, f.ptr()?)?;
        Ok(Lightmap { primary, secondary })
    })?;
    let light_grid = grid.load(s)?;
    runtime(s, lightmap_primary, 4, mul(lightmap_count, 4)?)?;
    runtime(s, lightmap_secondary, 4, mul(lightmap_count, 4)?)?;
    let models = vec(s, models, model_count, 4, 56, |_, f| {
        let writable_mins = v3(f);
        let writable_maxs = v3(f);
        let bounds = [v3(f), v3(f)];
        Ok(BrushModel {
            writable_mins,
            writable_maxs,
            bounds,
            surface_count: f.u16(),
            start_surf_index: f.u16(),
            surface_count_no_decal: f.u16(),
        })
    })?;
    let material_memory = vec(s, material_memory, material_memory_count, 4, 8, |s, f| {
        let slot = f.slot();
        let material = material_ptr_at(s, slot, f.ptr()?)?;
        Ok(MaterialMemory {
            material,
            memory: f.i32(),
        })
    })?;
    let vertices = bulk(s, vertices, 4, mul(vertex_count, 44)?)?;
    let vertex_layer_data = bulk(s, layer_data, 1, vertex_layer_data_size)?;
    let sun = sun.load(s)?;
    let outdoor_image = image_ptr(s, outdoor_image)?;
    let cell_caster_words = mul(cell_count, cell_count.div_ceil(32))?;
    runtime(s, cell_caster_bits, 4, mul(cell_caster_words, 4)?)?;
    runtime(s, scene_dyn_model, 4, mul(dyn_ent_client_count[0], 8)?)?;
    runtime(s, scene_dyn_brush, 4, mul(dyn_ent_client_count[1], 4)?)?;
    let non_sun_lights = primary_light_count
        .checked_sub(sun_primary_light_index + 1)
        .ok_or(ZoneError::Invalid("sun light index past primary lights"))?;
    runtime(
        s,
        light_entity_shadow_vis,
        4,
        mul(mul(non_sun_lights, 0x1000)?, 4)?,
    )?;
    for (p, n) in dyn_ent_shadow_vis.into_iter().zip(dyn_ent_client_count) {
        runtime(s, p, 4, mul(mul(n, non_sun_lights)?, 4)?)?;
    }
    runtime(s, non_sun_light, 1, dyn_ent_client_count[0])?;
    let shadow_geometry = vec(s, shadow_geom, primary_light_count, 4, 12, |s, f| {
        let surfaces = u32::from(f.u16());
        let smodels = u32::from(f.u16());
        let sorted_surf_index = u16_vec(s, f.ptr()?, surfaces)?;
        let smodel_index = u16_vec(s, f.ptr()?, smodels)?;
        Ok(ShadowGeometry {
            sorted_surf_index,
            smodel_index,
        })
    })?;
    let light_regions = vec(s, light_region, primary_light_count, 4, 8, |s, f| {
        let hulls = f.u32();
        vec(s, f.ptr()?, hulls, 4, 80, |s, f| {
            let mut kdop_mid_point = [0.0; 9];
            let mut kdop_half_size = [0.0; 9];
            for v in &mut kdop_mid_point {
                *v = f.f32();
            }
            for v in &mut kdop_half_size {
                *v = f.f32();
            }
            let axes = f.u32();
            let axes = vec(s, f.ptr()?, axes, 4, 20, |_, f| {
                Ok(LightRegionAxis {
                    dir: v3(f),
                    mid_point: f.f32(),
                    half_size: f.f32(),
                })
            })?;
            Ok(LightRegionHull {
                kdop_mid_point,
                kdop_half_size,
                axes,
            })
        })
    })?;
    let dpvs = dpvs_header.load(s, surface_count, count(cull_group_count)?)?;
    for (i, &words) in dyn_ent_client_word_count.iter().enumerate() {
        runtime(s, dyn_ent_cell_bits[i], 4, mul(mul(words, cell_count)?, 4)?)?;
    }
    for (i, row) in dyn_ent_vis_data.iter().enumerate() {
        for &p in row {
            runtime(s, p, 16, mul(dyn_ent_client_word_count[i], 32)?)?;
        }
    }
    Ok(GfxWorld {
        name,
        base_name,
        planes,
        nodes,
        indices: index_bytes,
        surface_count,
        sky_start_surfs: sky_start_surfs.to_vec(),
        sky_image,
        sky_sampler_state,
        vertex_count,
        vertices,
        vertex_layer_data_size,
        vertex_layer_data,
        sun_parse,
        sun_light,
        sun_color_from_bsp,
        sun_primary_light_index,
        primary_light_count,
        cull_group_count,
        reflection_probes,
        cell_bits_count,
        cells,
        lightmaps,
        light_grid,
        models,
        mins,
        maxs,
        checksum,
        material_memory,
        sun,
        outdoor_lookup_matrix,
        outdoor_image,
        shadow_geometry,
        light_regions,
        dpvs,
        dyn_ent_client_count,
    })
}

fn bulk_indices(s: &mut Stream, p: Ptr, n: u32) -> Result<Vec<u16>> {
    let bytes = bulk(s, p, 2, mul(n, 2)?)?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect())
}

/// A `u16` array that later pointers may re-enter anywhere inside (the
/// stream's reusable-data rule applied to a sub-range).
fn shared_u16s(
    s: &mut Stream,
    seen: &mut Vec<(Addr, Vec<u16>)>,
    p: Ptr,
    n: u32,
) -> Result<Vec<u16>> {
    match p {
        Ptr::Null => Ok(Vec::new()),
        Ptr::Follow if n == 0 => Ok(Vec::new()),
        Ptr::Follow => {
            let (at, bytes) = s.load(2, mul(n, 2)?)?;
            let v: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            seen.push((at, v.clone()));
            Ok(v)
        }
        Ptr::Offset(a) => {
            let (start, v) = seen
                .iter()
                .rev()
                .find(|(at, v)| {
                    at.block == a.block
                        && a.offset >= at.offset
                        && a.offset <= at.offset + 2 * v.len() as u32
                })
                .ok_or(ZoneError::BadOffset(a))?;
            let from = ((a.offset - start.offset) / 2) as usize;
            v.get(from..from + n as usize)
                .map(<[u16]>::to_vec)
                .ok_or(ZoneError::BadOffset(a))
        }
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

fn load_cells(s: &mut Stream, p: Ptr, n: u32) -> Result<Vec<Cell>> {
    let (base, bytes) = match p {
        Ptr::Null => return Ok(Vec::new()),
        Ptr::Follow => s.load(4, mul(n, 56)?)?,
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    };
    let mut index_arrays: Vec<(Addr, Vec<u16>)> = Vec::new();
    elements(s, base, &bytes, 56, |s, f| {
        let mins = v3(f);
        let maxs = v3(f);
        let tree_count = count(f.i32())?;
        let trees = f.ptr()?;
        let portal_count = count(f.i32())?;
        let portals = f.ptr()?;
        let cull_count = count(f.i32())?;
        let cull_groups = f.ptr()?;
        let probe_count = u32::from(f.u8());
        f.skip(3);
        let probes = f.ptr()?;
        let aabb_trees = vec(s, trees, tree_count, 4, 44, |s, f| {
            let mins = v3(f);
            let maxs = v3(f);
            let child_count = f.u16();
            let surface_count = f.u16();
            let start_surf_index = f.u16();
            let surface_count_no_decal = f.u16();
            let start_surf_index_no_decal = f.u16();
            let smodel_count = u32::from(f.u16());
            let smodel_indexes = shared_u16s(s, &mut index_arrays, f.ptr()?, smodel_count)?;
            Ok(AabbTree {
                mins,
                maxs,
                child_count,
                surface_count,
                start_surf_index,
                surface_count_no_decal,
                start_surf_index_no_decal,
                smodel_indexes,
                children_offset: f.i32(),
            })
        })?;
        let portals = vec(s, portals, portal_count, 4, 68, |s, f| {
            f.skip(12);
            let plane = [f.f32(), f.f32(), f.f32(), f.f32()];
            let plane_side = f.bytes();
            f.skip(1);
            let cell = f.ptr()?;
            let vertices = f.ptr()?;
            let vertex_count = u32::from(f.u8());
            f.skip(3);
            let hull_axis = [v3(f), v3(f)];
            let cell = match cell {
                Ptr::Offset(Addr { block, offset })
                    if block == base.block && offset >= base.offset =>
                {
                    (offset - base.offset) / 56
                }
                p => return Err(ZoneError::BadPointer(raw_of(p))),
            };
            let vertices = vec(s, vertices, vertex_count, 4, 12, |_, f| Ok(v3(f)))?;
            Ok(Portal {
                plane,
                plane_side,
                cell,
                vertices,
                hull_axis,
            })
        })?;
        let cull_groups = vec(s, cull_groups, cull_count, 4, 4, |_, f| Ok(f.i32()))?;
        let reflection_probes = vec(s, probes, probe_count, 1, 1, |_, f| Ok(f.u8()))?;
        Ok(Cell {
            mins,
            maxs,
            aabb_trees,
            portals,
            cull_groups,
            reflection_probes,
        })
    })
}

struct GridHeader {
    has_light_regions: bool,
    sun_primary_light_index: u32,
    mins: [u16; 3],
    maxs: [u16; 3],
    row_axis: u32,
    col_axis: u32,
    row_data_start: Ptr,
    raw_row_data_size: u32,
    raw_row_data: Ptr,
    entry_count: u32,
    entries: Ptr,
    color_count: u32,
    colors: Ptr,
}

impl GridHeader {
    fn read(f: &mut Fields) -> Result<Self> {
        let has_light_regions = f.u8() != 0;
        f.skip(3);
        Ok(GridHeader {
            has_light_regions,
            sun_primary_light_index: f.u32(),
            mins: [f.u16(), f.u16(), f.u16()],
            maxs: [f.u16(), f.u16(), f.u16()],
            row_axis: f.u32(),
            col_axis: f.u32(),
            row_data_start: f.ptr()?,
            raw_row_data_size: f.u32(),
            raw_row_data: f.ptr()?,
            entry_count: f.u32(),
            entries: f.ptr()?,
            color_count: f.u32(),
            colors: f.ptr()?,
        })
    }

    fn load(self, s: &mut Stream) -> Result<LightGrid> {
        let axis = self.row_axis as usize;
        if axis > 2 {
            return Err(ZoneError::Invalid("light grid row axis"));
        }
        let rows = u32::from(self.maxs[axis])
            .checked_sub(u32::from(self.mins[axis]))
            .map(|d| d + 1)
            .ok_or(ZoneError::Invalid("light grid extent"))?;
        let row_data_start = u16_vec(s, self.row_data_start, rows)?;
        let raw_row_data = match self.raw_row_data {
            Ptr::Null => Vec::new(),
            p => s
                .array(p, self.raw_row_data_size, 1, 1, |_, f| Ok(f.u8()))?
                .to_vec(),
        };
        let entries = bulk(s, self.entries, 4, mul(self.entry_count, 4)?)?;
        let colors = bulk(s, self.colors, 4, mul(self.color_count, 168)?)?;
        Ok(LightGrid {
            has_light_regions: self.has_light_regions,
            sun_primary_light_index: self.sun_primary_light_index,
            mins: self.mins,
            maxs: self.maxs,
            row_axis: self.row_axis,
            col_axis: self.col_axis,
            row_data_start,
            raw_row_data,
            entry_count: self.entry_count,
            entries,
            color_count: self.color_count,
            colors,
        })
    }
}

struct SunHeader {
    has_valid_data: bool,
    sprite_material: Ptr,
    flare_material: Ptr,
    scalars: [u32; 18],
    sun_fx_position: V3,
}

impl SunHeader {
    fn read(f: &mut Fields) -> Result<Self> {
        let has_valid_data = f.u8() != 0;
        f.skip(3);
        let sprite_material = f.ptr()?;
        let flare_material = f.ptr()?;
        let mut scalars = [0; 18];
        for v in &mut scalars {
            *v = f.u32();
        }
        Ok(SunHeader {
            has_valid_data,
            sprite_material,
            flare_material,
            scalars,
            sun_fx_position: v3(f),
        })
    }

    fn load(self, s: &mut Stream) -> Result<SunFlare> {
        let sprite_material = material_ptr(s, self.sprite_material)?;
        let flare_material = material_ptr(s, self.flare_material)?;
        let fl = |i: usize| f32::from_bits(self.scalars[i]);
        let it = |i: usize| self.scalars[i] as i32;
        Ok(SunFlare {
            has_valid_data: self.has_valid_data,
            sprite_material,
            flare_material,
            sprite_size: fl(0),
            flare_min_size: fl(1),
            flare_min_dot: fl(2),
            flare_max_size: fl(3),
            flare_max_dot: fl(4),
            flare_max_alpha: fl(5),
            flare_fade_in_time: it(6),
            flare_fade_out_time: it(7),
            blind_min_dot: fl(8),
            blind_max_dot: fl(9),
            blind_max_darken: fl(10),
            blind_fade_in_time: it(11),
            blind_fade_out_time: it(12),
            glare_min_dot: fl(13),
            glare_max_dot: fl(14),
            glare_max_lighten: fl(15),
            glare_fade_in_time: it(16),
            glare_fade_out_time: it(17),
            sun_fx_position: self.sun_fx_position,
        })
    }
}

struct DpvsHeader {
    smodel_count: u32,
    static_surface_count: u32,
    static_surface_count_no_decal: u32,
    ranges: [u32; 6],
    smodel_vis_data_count: u32,
    surface_vis_data_count: u32,
    smodel_vis_data: [Ptr; 3],
    surface_vis_data: [Ptr; 3],
    lod_data: Ptr,
    sorted_surf_index: Ptr,
    smodel_insts: Ptr,
    surfaces: Ptr,
    cull_groups: Ptr,
    smodel_draw_insts: Ptr,
    surface_materials: Ptr,
    surface_casts_sun_shadow: Ptr,
}

impl DpvsHeader {
    fn read(f: &mut Fields) -> Result<Self> {
        let smodel_count = f.u32();
        let static_surface_count = f.u32();
        let static_surface_count_no_decal = f.u32();
        let ranges = [f.u32(), f.u32(), f.u32(), f.u32(), f.u32(), f.u32()];
        let smodel_vis_data_count = f.u32();
        let surface_vis_data_count = f.u32();
        let smodel_vis_data = [f.ptr()?, f.ptr()?, f.ptr()?];
        let surface_vis_data = [f.ptr()?, f.ptr()?, f.ptr()?];
        let h = DpvsHeader {
            smodel_count,
            static_surface_count,
            static_surface_count_no_decal,
            ranges,
            smodel_vis_data_count,
            surface_vis_data_count,
            smodel_vis_data,
            surface_vis_data,
            lod_data: f.ptr()?,
            sorted_surf_index: f.ptr()?,
            smodel_insts: f.ptr()?,
            surfaces: f.ptr()?,
            cull_groups: f.ptr()?,
            smodel_draw_insts: f.ptr()?,
            surface_materials: f.ptr()?,
            surface_casts_sun_shadow: f.ptr()?,
        };
        f.skip(4); // usage count
        Ok(h)
    }

    fn load(self, s: &mut Stream, surface_count: i32, cull_group_count: u32) -> Result<DpvsStatic> {
        let surface_count = count(surface_count)?;
        for p in self.smodel_vis_data {
            runtime(s, p, 1, self.smodel_count)?;
        }
        for p in self.surface_vis_data {
            runtime(s, p, 1, self.static_surface_count)?;
        }
        runtime(
            s,
            self.lod_data,
            128,
            mul(mul(self.smodel_vis_data_count, 2)?, 4)?,
        )?;
        let sorted_count = self.static_surface_count + self.static_surface_count_no_decal;
        let sorted_surf_index = u16_vec(s, self.sorted_surf_index, sorted_count)?;
        let smodel_insts = vec(s, self.smodel_insts, self.smodel_count, 4, 28, |_, f| {
            Ok(StaticModelInst {
                mins: v3(f),
                maxs: v3(f),
                ground_lighting: f.u32(),
            })
        })?;
        let surfaces = vec(s, self.surfaces, surface_count, 4, 48, |s, f| {
            let vertex_layer_data = f.i32();
            let first_vertex = f.i32();
            let vertex_count = f.u16();
            let tri_count = f.u16();
            let base_index = f.i32();
            let slot = f.slot();
            let material = material_ptr_at(s, slot, f.ptr()?)?;
            Ok(Surface {
                vertex_layer_data,
                first_vertex,
                vertex_count,
                tri_count,
                base_index,
                material,
                lightmap_index: f.u8(),
                reflection_probe_index: f.u8(),
                primary_light_index: f.u8(),
                flags: f.u8(),
                bounds: [v3(f), v3(f)],
            })
        })?;
        let cull_groups = vec(s, self.cull_groups, cull_group_count, 4, 32, |_, f| {
            Ok(CullGroup {
                mins: v3(f),
                maxs: v3(f),
                surface_count: f.i32(),
                start_surf_index: f.i32(),
            })
        })?;
        let smodel_draw_insts = vec(
            s,
            self.smodel_draw_insts,
            self.smodel_count,
            4,
            76,
            |s, f| {
                let cull_dist = f.f32();
                let origin = v3(f);
                let axis = [v3(f), v3(f), v3(f)];
                let scale = f.f32();
                let slot = f.slot();
                let model = load_xmodel_at(s, slot, f.ptr()?)?;
                Ok(StaticModel {
                    cull_dist,
                    origin,
                    axis,
                    scale,
                    model,
                    cache_index: [f.u16(), f.u16(), f.u16(), f.u16()],
                    reflection_probe_index: f.u8(),
                    primary_light_index: f.u8(),
                    lighting_handle: f.u16(),
                    flags: f.u8(),
                })
            },
        )?;
        runtime(
            s,
            self.surface_materials,
            4,
            mul(self.static_surface_count, 8)?,
        )?;
        runtime(
            s,
            self.surface_casts_sun_shadow,
            128,
            mul(self.surface_vis_data_count, 4)?,
        )?;
        Ok(DpvsStatic {
            smodel_count: self.smodel_count,
            static_surface_count: self.static_surface_count,
            static_surface_count_no_decal: self.static_surface_count_no_decal,
            lit_surfs: [self.ranges[0], self.ranges[1]],
            decal_surfs: [self.ranges[2], self.ranges[3]],
            emissive_surfs: [self.ranges[4], self.ranges[5]],
            smodel_vis_data_count: self.smodel_vis_data_count,
            surface_vis_data_count: self.surface_vis_data_count,
            sorted_surf_index,
            smodel_insts,
            surfaces,
            cull_groups,
            smodel_draw_insts,
        })
    }
}
