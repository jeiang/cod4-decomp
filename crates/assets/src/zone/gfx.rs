// SPDX-License-Identifier: GPL-3.0-only
//! Typed decoders for the `_load`-zone asset types: material, technique set
//! (passes, vertex declarations, shader blobs), image header, and raw file.

use super::error::{Result, ZoneError};
use super::stream::{Addr, Fields, Ptr, Stream};
use std::sync::Arc;

pub type Name = Option<Arc<str>>;

#[derive(Debug)]
pub struct RawFile {
    pub name: Name,
    /// `len + 1` bytes as stored (a trailing NUL follows the `len` content bytes).
    pub data: Vec<u8>,
}

pub(super) fn raw_file(s: &mut Stream, h: &[u8]) -> Result<RawFile> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let len = f.i32();
    let buffer = f.ptr()?;
    let len = u32::try_from(len).map_err(|_| ZoneError::Invalid("negative rawfile length"))?;
    let name = s.string(name)?;
    let data = match buffer {
        Ptr::Follow => s.load(1, len + 1)?.1,
        Ptr::Null => Vec::new(),
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    };
    Ok(RawFile { name, data })
}

pub(super) fn raw_of(p: Ptr) -> u32 {
    match p {
        Ptr::Null => 0,
        Ptr::Follow => 0xFFFF_FFFF,
        Ptr::Insert => 0xFFFF_FFFE,
        Ptr::Offset(a) => (((a.block as u32) << 28) | a.offset) + 1,
    }
}

/// Pixel data of an image that carries it in the zone (lightmaps, probes, generated).
#[derive(Debug)]
pub struct ImageLoadDef {
    pub level_count: u8,
    pub flags: u8,
    pub dimensions: [u16; 3],
    /// D3D format code.
    pub format: i32,
    /// Texels; empty when the consumer drops presentation data.
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct GfxImage {
    pub map_type: u32,
    pub picmip: [u8; 2],
    pub no_picmip: bool,
    pub semantic: u8,
    pub track: u8,
    pub card_memory: [i32; 2],
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub category: u8,
    pub delay_load_pixels: bool,
    pub name: Name,
    pub load_def: Option<Arc<ImageLoadDef>>,
}

pub(super) const IMAGE_SIZE: u32 = 36;

pub(super) fn image(s: &mut Stream, h: &[u8]) -> Result<GfxImage> {
    let mut f = Fields::new(h);
    let map_type = f.u32();
    let texture = f.ptr()?;
    let picmip = f.bytes();
    let no_picmip = f.u8() != 0;
    let semantic = f.u8();
    let track = f.u8();
    f.skip(3);
    let card_memory = [f.i32(), f.i32()];
    let (width, height, depth) = (f.u16(), f.u16(), f.u16());
    let category = f.u8();
    let delay_load_pixels = f.u8() != 0;
    let name = f.ptr()?;
    // Member order in the stream: name, then texture.
    let name = s.string(name)?;
    let load_def = s.temp_ptr(texture, 4, 16, false, |s, b| {
        let mut f = Fields::new(b);
        let level_count = f.u8();
        let flags = f.u8();
        let dimensions = [f.u16(), f.u16(), f.u16()];
        let format = f.i32();
        let size = f.u32();
        let data = s.load_presentation(1, size)?.1;
        Ok(ImageLoadDef {
            level_count,
            flags,
            dimensions,
            format,
            data,
        })
    })?;
    Ok(GfxImage {
        map_type,
        picmip,
        no_picmip,
        semantic,
        track,
        card_memory,
        width,
        height,
        depth,
        category,
        delay_load_pixels,
        name,
        load_def,
    })
}

pub(super) fn image_ptr(s: &mut Stream, p: Ptr) -> Result<Option<Arc<GfxImage>>> {
    image_ptr_at(s, None, p)
}

pub(super) fn material_ptr(s: &mut Stream, p: Ptr) -> Result<Option<Arc<Material>>> {
    material_ptr_at(s, None, p)
}

pub(super) fn techset_ptr(s: &mut Stream, p: Ptr) -> Result<Option<Arc<TechniqueSet>>> {
    techset_ptr_at(s, None, p)
}

/// The `_at` forms take the address of the pointer field ([`Fields::slot`]);
/// use them whenever the field sits in a loaded array or struct.
pub(super) fn image_ptr_at(
    s: &mut Stream,
    slot: Option<Addr>,
    p: Ptr,
) -> Result<Option<Arc<GfxImage>>> {
    s.temp_asset_at(slot, p, 4, IMAGE_SIZE, image)
}

pub(super) fn material_ptr_at(
    s: &mut Stream,
    slot: Option<Addr>,
    p: Ptr,
) -> Result<Option<Arc<Material>>> {
    s.temp_asset_at(slot, p, 4, MATERIAL_SIZE, material)
}

pub(super) fn techset_ptr_at(
    s: &mut Stream,
    slot: Option<Addr>,
    p: Ptr,
) -> Result<Option<Arc<TechniqueSet>>> {
    s.temp_asset_at(slot, p, 4, TECHSET_SIZE, techset)
}

#[derive(Debug)]
pub struct Water {
    pub float_time: f32,
    pub m: i32,
    pub n: i32,
    pub lx: f32,
    pub lz: f32,
    pub gravity: f32,
    pub wind_velocity: f32,
    pub wind_direction: [f32; 2],
    pub amplitude: f32,
    pub code_constant: [f32; 4],
    /// Wave spectrum; empty when the consumer drops presentation data.
    pub h0: Arc<[[f32; 2]]>,
    pub w_term: Arc<[f32]>,
    pub image: Option<Arc<GfxImage>>,
}

fn water(s: &mut Stream, h: &[u8]) -> Result<Water> {
    let mut f = Fields::new(h);
    let float_time = f.f32();
    let (h0, w_term) = (f.ptr()?, f.ptr()?);
    let (m, n) = (f.i32(), f.i32());
    let (lx, lz, gravity, wind_velocity) = (f.f32(), f.f32(), f.f32(), f.f32());
    let wind_direction = [f.f32(), f.f32()];
    let amplitude = f.f32();
    let code_constant = [f.f32(), f.f32(), f.f32(), f.f32()];
    let image = f.ptr()?;
    let count = u32::try_from(i64::from(m) * i64::from(n))
        .map_err(|_| ZoneError::Invalid("bad water grid"))?;
    let mut h0 = s.array(h0, count, 4, 8, |_, f| Ok([f.f32(), f.f32()]))?;
    let mut w_term = s.array(w_term, count, 4, 4, |_, f| Ok(f.f32()))?;
    if !s.keep_presentation() {
        h0 = Arc::from([]);
        w_term = Arc::from([]);
    }
    let image = image_ptr(s, image)?;
    Ok(Water {
        float_time,
        m,
        n,
        lx,
        lz,
        gravity,
        wind_velocity,
        wind_direction,
        amplitude,
        code_constant,
        h0,
        w_term,
        image,
    })
}

#[derive(Debug)]
pub enum TextureSource {
    Image(Option<Arc<GfxImage>>),
    /// Semantic `TS_WATER_MAP` (11).
    Water(Option<Arc<Water>>),
}

#[derive(Debug)]
pub struct TextureDef {
    pub name_hash: u32,
    pub name_start: u8,
    pub name_end: u8,
    /// Packed sampler state: filter (3 bits), mipmap (2), clamp u/v/w (1 each).
    pub sampler_state: u8,
    pub semantic: u8,
    pub source: TextureSource,
}

#[derive(Debug)]
pub struct ConstantDef {
    pub name_hash: u32,
    pub name: [u8; 12],
    pub literal: [f32; 4],
}

#[derive(Debug)]
pub struct Material {
    pub name: Name,
    pub game_flags: u8,
    pub sort_key: u8,
    pub atlas_rows: u8,
    pub atlas_columns: u8,
    pub draw_surf: u64,
    pub surface_type_bits: u32,
    pub hash_index: u16,
    pub state_bits_entry: [u8; 34],
    pub state_flags: u8,
    pub camera_region: u8,
    /// `None` when the consumer drops presentation data.
    pub technique_set: Option<Arc<TechniqueSet>>,
    /// Image references keep name/format metadata only under a server consumer.
    pub textures: Arc<[TextureDef]>,
    /// Empty when the consumer drops presentation data.
    pub constants: Arc<[ConstantDef]>,
    /// Packed GPU state words, two per entry; empty when the consumer drops presentation data.
    pub state_bits: Arc<[[u32; 2]]>,
}

pub(super) const MATERIAL_SIZE: u32 = 80;
const TS_WATER_MAP: u8 = 11;

pub(super) fn material(s: &mut Stream, h: &[u8]) -> Result<Material> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let (game_flags, sort_key, atlas_rows, atlas_columns) = (f.u8(), f.u8(), f.u8(), f.u8());
    let draw_surf = f.u64();
    let surface_type_bits = f.u32();
    let hash_index = f.u16();
    f.skip(2);
    let state_bits_entry = f.bytes();
    let (tex_n, const_n, bits_n) = (f.u8(), f.u8(), f.u8());
    let (state_flags, camera_region) = (f.u8(), f.u8());
    f.skip(1);
    let (tech, tex, cons, bits) = (f.ptr()?, f.ptr()?, f.ptr()?, f.ptr()?);

    let name = s.string(name)?;
    let mut technique_set = s.temp_asset(tech, 4, TECHSET_SIZE, techset)?;
    let textures = s.array(tex, tex_n.into(), 4, 12, |s, f| {
        let name_hash = f.u32();
        let (name_start, name_end, sampler_state, semantic) = (f.u8(), f.u8(), f.u8(), f.u8());
        let slot = f.slot();
        let u = f.ptr()?;
        let source = if semantic == TS_WATER_MAP {
            TextureSource::Water(s.shared(u, 4, 68, water)?)
        } else {
            TextureSource::Image(image_ptr_at(s, slot, u)?)
        };
        Ok(TextureDef {
            name_hash,
            name_start,
            name_end,
            sampler_state,
            semantic,
            source,
        })
    })?;
    let constants = s.array(cons, const_n.into(), 16, 32, |_, f| {
        Ok(ConstantDef {
            name_hash: f.u32(),
            name: f.bytes(),
            literal: [f.f32(), f.f32(), f.f32(), f.f32()],
        })
    })?;
    let state_bits = s.array(bits, bits_n.into(), 4, 8, |_, f| Ok([f.u32(), f.u32()]))?;
    // Shaders, constants and GPU state are renderer-only: read, then released.
    let (mut constants, mut state_bits) = (constants, state_bits);
    if !s.keep_presentation() {
        technique_set = None;
        constants = Arc::from([]);
        state_bits = Arc::from([]);
    }
    Ok(Material {
        name,
        game_flags,
        sort_key,
        atlas_rows,
        atlas_columns,
        draw_surf,
        surface_type_bits,
        hash_index,
        state_bits_entry,
        state_flags,
        camera_region,
        technique_set,
        textures,
        constants,
        state_bits,
    })
}

#[derive(Debug)]
pub struct VertexDecl {
    pub stream_count: u8,
    pub has_optional_source: bool,
    pub is_loaded: bool,
    /// (source, destination) per routing slot.
    pub routing: [(u8, u8); 16],
}

fn vertex_decl(_: &mut Stream, h: &[u8]) -> Result<VertexDecl> {
    let mut f = Fields::new(h);
    let stream_count = f.u8();
    let has_optional_source = f.u8() != 0;
    let is_loaded = f.u8() != 0;
    f.skip(1);
    let mut routing = [(0, 0); 16];
    for r in &mut routing {
        *r = (f.u8(), f.u8());
    }
    Ok(VertexDecl {
        stream_count,
        has_optional_source,
        is_loaded,
        routing,
    })
}

/// A D3D9 shader (vs or ps) as shipped: SM2/SM3 bytecode with its constant table.
#[derive(Debug)]
pub struct Shader {
    pub name: Name,
    pub load_for_renderer: u16,
    /// Bytecode as little-endian dwords.
    pub program: Vec<u32>,
}

fn shader(s: &mut Stream, h: &[u8]) -> Result<Shader> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    f.skip(4); // runtime shader handle
    let program = f.ptr()?;
    let size = f.u16();
    let load_for_renderer = f.u16();
    let name = s.string(name)?;
    let program = match program {
        Ptr::Follow => {
            let (_, b) = s.load_presentation(4, u32::from(size) * 4)?;
            b.as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect()
        }
        Ptr::Null => Vec::new(),
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    };
    Ok(Shader {
        name,
        load_for_renderer,
        program,
    })
}

#[derive(Debug)]
pub enum ArgValue {
    /// Argument types 1 and 7.
    Literal(Arc<[f32; 4]>),
    /// Argument types 3 and 5.
    CodeConst {
        index: u16,
        first_row: u8,
        row_count: u8,
    },
    /// Argument type 4.
    CodeSampler(u32),
    /// Argument types 0, 2 and 6.
    NameHash(u32),
}

#[derive(Debug)]
pub struct ShaderArg {
    /// `MaterialShaderArgumentType` (0..=7).
    pub kind: u16,
    /// Destination register.
    pub dest: u16,
    pub value: ArgValue,
}

#[derive(Debug)]
pub struct Pass {
    pub vertex_decl: Option<Arc<VertexDecl>>,
    pub vertex_shader: Option<Arc<Shader>>,
    pub pixel_shader: Option<Arc<Shader>>,
    pub per_prim_arg_count: u8,
    pub per_obj_arg_count: u8,
    pub stable_arg_count: u8,
    pub custom_sampler_flags: u8,
    pub args: Vec<ShaderArg>,
}

#[derive(Debug)]
pub struct Technique {
    pub name: Name,
    pub flags: u16,
    pub passes: Vec<Pass>,
}

fn technique(s: &mut Stream, p: Ptr) -> Result<Option<Arc<Technique>>> {
    match p {
        Ptr::Null => return Ok(None),
        Ptr::Offset(a) => return s.lookup::<Arc<Technique>>(a).map(Some),
        Ptr::Insert => return Err(ZoneError::BadPointer(0xFFFF_FFFE)),
        Ptr::Follow => {}
    }
    // Header, then the passes; member order is passes first, then the name.
    let (at, hdr) = s.load(4, 8)?;
    let mut f = Fields::new(&hdr);
    let name = f.ptr()?;
    let flags = f.u16();
    let pass_count = u32::from(f.u16());
    let (_, pb) = s.load(4, pass_count * 20)?;
    let mut passes = Vec::with_capacity(pass_count as usize);
    for c in pb.as_chunks::<20>().0 {
        let mut f = Fields::new(c);
        let (decl, vs, ps) = (f.ptr()?, f.ptr()?, f.ptr()?);
        let (per_prim, per_obj, stable, custom) = (f.u8(), f.u8(), f.u8(), f.u8());
        let args = f.ptr()?;
        let vertex_decl = s.shared(decl, 4, 100, vertex_decl)?;
        let vertex_shader = s.shared(vs, 4, 16, shader)?;
        let pixel_shader = s.shared(ps, 4, 16, shader)?;
        let n = u32::from(per_prim) + u32::from(per_obj) + u32::from(stable);
        let args = match args {
            Ptr::Follow => shader_args(s, n)?,
            Ptr::Null if n == 0 => Vec::new(),
            p => return Err(ZoneError::BadPointer(raw_of(p))),
        };
        passes.push(Pass {
            vertex_decl,
            vertex_shader,
            pixel_shader,
            per_prim_arg_count: per_prim,
            per_obj_arg_count: per_obj,
            stable_arg_count: stable,
            custom_sampler_flags: custom,
            args,
        });
    }
    let name = s.string(name)?;
    let t = Arc::new(Technique {
        name,
        flags,
        passes,
    });
    s.register(at, t.clone());
    Ok(Some(t))
}

fn shader_args(s: &mut Stream, n: u32) -> Result<Vec<ShaderArg>> {
    let (_, b) = s.load(4, n * 8)?;
    let mut out = Vec::with_capacity(n as usize);
    for c in b.as_chunks::<8>().0 {
        let mut f = Fields::new(c);
        let (kind, dest) = (f.u16(), f.u16());
        let value = match kind {
            1 | 7 => {
                let p = f.ptr()?;
                let lit = s.shared(p, 4, 16, |_, b| {
                    let mut f = Fields::new(b);
                    Ok([f.f32(), f.f32(), f.f32(), f.f32()])
                })?;
                ArgValue::Literal(lit.ok_or(ZoneError::Invalid("null literal argument"))?)
            }
            3 | 5 => ArgValue::CodeConst {
                index: f.u16(),
                first_row: f.u8(),
                row_count: f.u8(),
            },
            4 => ArgValue::CodeSampler(f.u32()),
            0 | 2 | 6 => ArgValue::NameHash(f.u32()),
            _ => return Err(ZoneError::Invalid("unknown shader argument type")),
        };
        out.push(ShaderArg { kind, dest, value });
    }
    Ok(out)
}

#[derive(Debug)]
pub struct TechniqueSet {
    pub name: Name,
    pub world_vert_format: u8,
    pub has_been_uploaded: bool,
    /// One slot per `MaterialTechniqueType` (34).
    pub techniques: Vec<Option<Arc<Technique>>>,
}

pub(super) const TECHSET_SIZE: u32 = 148;

pub(super) fn techset(s: &mut Stream, h: &[u8]) -> Result<TechniqueSet> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let world_vert_format = f.u8();
    let has_been_uploaded = f.u8() != 0;
    f.skip(2 + 4); // unused, padding, remapped set (never stored)
    let ptrs = (0..34).map(|_| f.ptr()).collect::<Result<Vec<_>>>()?;
    let name = s.string(name)?;
    let techniques = ptrs
        .into_iter()
        .map(|p| technique(s, p))
        .collect::<Result<Vec<_>>>()?;
    Ok(TechniqueSet {
        name,
        world_vert_format,
        has_been_uploaded,
        techniques,
    })
}
