// SPDX-License-Identifier: GPL-3.0-or-later
//! XModel: bones, collision, hit parts, surfaces/LODs in VERTEX/INDEX.

use super::error::{Result, ZoneError};
use super::gfx::{Material, Name, material_ptr_at};
use super::phys::{self, PhysPreset};
use super::stream::{Addr, Block, Fields, Ptr, Stream};
use std::sync::Arc;

const XMODEL_SIZE: u32 = 220;
const SURFACE_SIZE: u32 = 56;
const VERTEX_SIZE: u32 = 32;
const PLANE_SIZE: u32 = 20;
const TRI_SIZE: u32 = 6;

#[derive(Debug)]
pub struct CollisionNode {
    pub mins: [u16; 3],
    pub maxs: [u16; 3],
    pub child_begin: u16,
    pub child_count: u16,
}

#[derive(Debug)]
pub struct CollisionTree {
    pub trans: [f32; 3],
    pub scale: [f32; 3],
    pub nodes: Arc<[CollisionNode]>,
    /// First triangle index of each leaf.
    pub leafs: Arc<[u16]>,
}

/// A run of vertices bound to one bone, with the triangles over them.
#[derive(Debug)]
pub struct RigidVertList {
    pub bone_offset: u16,
    pub vert_count: u16,
    pub tri_offset: u16,
    pub tri_count: u16,
    pub collision_tree: Option<Arc<CollisionTree>>,
}

#[derive(Debug)]
pub struct Surface {
    pub tile_mode: u8,
    pub deformed: bool,
    pub vert_count: u16,
    pub tri_count: u16,
    pub zone_handle: u8,
    pub base_tri_index: u16,
    pub base_vert_index: u16,
    /// Vertices blended over 1, 2, 3 and 4 bones.
    pub blend_counts: [i16; 4],
    pub blends: Arc<[u16]>,
    pub vert_list: Arc<[RigidVertList]>,
    pub part_bits: [i32; 4],
    /// Packed 32-byte vertices. Empty when the consumer drops presentation data.
    pub verts: Arc<[u8]>,
    /// Triangle index triples. Empty when the consumer drops presentation data.
    pub tri_indices: Arc<[u16]>,
}

#[derive(Debug)]
pub struct LodInfo {
    pub dist: f32,
    pub surf_count: u16,
    pub surf_index: u16,
    pub part_bits: [i32; 4],
    pub lod: i8,
    pub smc_index_plus_one: i8,
    pub smc_alloc_bits: i8,
}

#[derive(Debug)]
pub struct CollisionTri {
    pub plane: [f32; 4],
    pub svec: [f32; 4],
    pub tvec: [f32; 4],
}

#[derive(Debug)]
pub struct CollisionSurface {
    pub tris: Arc<[CollisionTri]>,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub bone_index: i32,
    pub contents: i32,
    pub surface_flags: i32,
}

#[derive(Debug)]
pub struct BoneInfo {
    pub bounds: [[f32; 3]; 2],
    pub offset: [f32; 3],
    pub radius_squared: f32,
}

/// Bone pose: rotation quaternion, translation and its weight.
#[derive(Debug)]
pub struct BaseMat {
    pub quat: [f32; 4],
    pub trans: [f32; 3],
    pub trans_weight: f32,
}

#[derive(Debug)]
pub struct Plane {
    pub normal: [f32; 3],
    pub dist: f32,
    pub kind: u8,
    pub sign_bits: u8,
}

#[derive(Debug)]
pub struct BrushSide {
    pub plane: Option<Arc<Plane>>,
    pub material_num: u32,
    pub first_adjacent_side_offset: i16,
    pub edge_count: u8,
}

#[derive(Debug)]
pub struct Brush {
    pub mins: [f32; 3],
    pub contents: i32,
    pub maxs: [f32; 3],
    pub sides: Arc<[BrushSide]>,
    pub axial_material_num: [[i16; 3]; 2],
    pub base_adjacent_side: Arc<[u8]>,
    pub first_adjacent_side_offsets: [[i16; 3]; 2],
    pub edge_count: [[u8; 3]; 2],
    /// One per side; shared with `sides[i].plane` when the zone aliases them.
    pub planes: Arc<[Arc<Plane>]>,
}

#[derive(Debug)]
pub struct PhysGeom {
    pub brush: Option<Arc<Brush>>,
    pub kind: i32,
    pub orientation: [[f32; 3]; 3],
    pub offset: [f32; 3],
    pub half_lengths: [f32; 3],
}

#[derive(Debug)]
pub struct PhysGeomList {
    pub geoms: Arc<[PhysGeom]>,
    pub center_of_mass: [f32; 3],
    pub moments_of_inertia: [f32; 3],
    pub products_of_inertia: [f32; 3],
}

#[derive(Debug)]
pub struct XModel {
    pub name: Name,
    pub num_bones: u8,
    pub num_root_bones: u8,
    pub lod_ramp_type: u8,
    /// Script string indices.
    pub bone_names: Arc<[u16]>,
    pub parent_list: Arc<[u8]>,
    pub quats: Arc<[[i16; 4]]>,
    /// Four floats per non-root bone, as stored.
    pub trans: Arc<[f32]>,
    pub part_classification: Arc<[u8]>,
    pub base_mat: Arc<[BaseMat]>,
    pub surfs: Arc<[Surface]>,
    pub materials: Arc<[Option<Arc<Material>>]>,
    pub lod_info: [LodInfo; 4],
    pub coll_surfs: Arc<[CollisionSurface]>,
    pub contents: i32,
    pub bone_info: Arc<[BoneInfo]>,
    pub radius: f32,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub num_lods: u16,
    pub coll_lod: i16,
    pub mem_usage: i32,
    pub flags: u8,
    pub bad: bool,
    pub phys_preset: Option<Arc<PhysPreset>>,
    pub phys_geoms: Option<Arc<PhysGeomList>>,
}

fn i16s(f: &mut Fields) -> i16 {
    f.u16() as i16
}

fn f3(f: &mut Fields) -> [f32; 3] {
    [f.f32(), f.f32(), f.f32()]
}

fn plane(_: &mut Stream, h: &[u8]) -> Result<Plane> {
    plane_fields(&mut Fields::new(h))
}

fn plane_fields(f: &mut Fields) -> Result<Plane> {
    let normal = f3(f);
    let dist = f.f32();
    let (kind, sign_bits) = (f.u8(), f.u8());
    f.skip(2);
    Ok(Plane {
        normal,
        dist,
        kind,
        sign_bits,
    })
}

fn brush(s: &mut Stream, h: &[u8]) -> Result<Brush> {
    let mut f = Fields::new(h);
    let mins = f3(&mut f);
    let contents = f.i32();
    let maxs = f3(&mut f);
    let num_sides = f.u32();
    let sides = f.ptr()?;
    let axial_material_num = [
        [i16s(&mut f), i16s(&mut f), i16s(&mut f)],
        [i16s(&mut f), i16s(&mut f), i16s(&mut f)],
    ];
    let adjacent = f.ptr()?;
    let first_adjacent_side_offsets = [
        [i16s(&mut f), i16s(&mut f), i16s(&mut f)],
        [i16s(&mut f), i16s(&mut f), i16s(&mut f)],
    ];
    let edge_count = [f.bytes(), f.bytes()];
    f.skip(2);
    let total_edges = f.u32();
    let planes = f.ptr()?;
    let sides = s.array(sides, num_sides, 4, 12, |s, f| {
        let plane_ptr = f.ptr()?;
        let material_num = f.u32();
        let first_adjacent_side_offset = i16s(f);
        let edge_count = f.u8();
        Ok(BrushSide {
            plane: s.shared(plane_ptr, 4, PLANE_SIZE, plane)?,
            material_num,
            first_adjacent_side_offset,
            edge_count,
        })
    })?;
    let base_adjacent_side = s.array(adjacent, total_edges, 1, 1, |_, f| Ok(f.u8()))?;
    let planes: Arc<[Arc<Plane>]> = match planes {
        // The planes are the ones the sides already loaded, back to back.
        Ptr::Offset(a) => (0..num_sides)
            .map(|i| {
                s.lookup::<Arc<Plane>>(Addr {
                    block: a.block,
                    offset: a.offset + i * PLANE_SIZE,
                })
            })
            .collect::<Result<_>>()?,
        p => s
            .array(p, num_sides, 4, PLANE_SIZE, |_, f| {
                plane_fields(f).map(Arc::new)
            })?
            .iter()
            .cloned()
            .collect(),
    };
    Ok(Brush {
        mins,
        contents,
        maxs,
        sides,
        axial_material_num,
        base_adjacent_side,
        first_adjacent_side_offsets,
        edge_count,
        planes,
    })
}

fn phys_geom_list(s: &mut Stream, h: &[u8]) -> Result<PhysGeomList> {
    let mut f = Fields::new(h);
    let count = f.u32();
    let geoms = f.ptr()?;
    let (center_of_mass, moments_of_inertia, products_of_inertia) =
        (f3(&mut f), f3(&mut f), f3(&mut f));
    let geoms = s.array(geoms, count, 4, 68, |s, f| {
        let brush_ptr = f.ptr()?;
        let kind = f.i32();
        let orientation = [f3(f), f3(f), f3(f)];
        let (offset, half_lengths) = (f3(f), f3(f));
        Ok(PhysGeom {
            brush: s.shared(brush_ptr, 4, 80, brush)?,
            kind,
            orientation,
            offset,
            half_lengths,
        })
    })?;
    Ok(PhysGeomList {
        geoms,
        center_of_mass,
        moments_of_inertia,
        products_of_inertia,
    })
}

fn collision_tree(s: &mut Stream, h: &[u8]) -> Result<CollisionTree> {
    let mut f = Fields::new(h);
    let trans = f3(&mut f);
    let scale = f3(&mut f);
    let node_count = f.u32();
    let nodes = f.ptr()?;
    let leaf_count = f.u32();
    let leafs = f.ptr()?;
    let nodes = s.array(nodes, node_count, 16, 16, |_, f| {
        Ok(CollisionNode {
            mins: [f.u16(), f.u16(), f.u16()],
            maxs: [f.u16(), f.u16(), f.u16()],
            child_begin: f.u16(),
            child_count: f.u16(),
        })
    })?;
    let leafs = s.array(leafs, leaf_count, 2, 2, |_, f| Ok(f.u16()))?;
    Ok(CollisionTree {
        trans,
        scale,
        nodes,
        leafs,
    })
}

/// A VERTEX/INDEX payload array: `len` bytes at 16-byte alignment, shared by address.
fn presentation(s: &mut Stream, p: Ptr, block: Block, len: u32) -> Result<Arc<[u8]>> {
    match p {
        Ptr::Null => Ok(Arc::from(Vec::new())),
        Ptr::Offset(a) => s.lookup::<Arc<[u8]>>(a),
        Ptr::Insert => Err(ZoneError::Invalid("insert pointer on surface data")),
        Ptr::Follow => {
            s.push(block);
            let (at, bytes) = s.load_presentation(16, len)?;
            s.pop()?;
            let v: Arc<[u8]> = bytes.into();
            s.register(at, v.clone());
            Ok(v)
        }
    }
}

fn surface(s: &mut Stream, f: &mut Fields) -> Result<Surface> {
    let tile_mode = f.u8();
    let deformed = f.u8() != 0;
    let vert_count = f.u16();
    let tri_count = f.u16();
    let zone_handle = f.u8();
    f.skip(1);
    let base_tri_index = f.u16();
    let base_vert_index = f.u16();
    let tris = f.ptr()?;
    let blend_counts = [i16s(f), i16s(f), i16s(f), i16s(f)];
    let blends = f.ptr()?;
    let verts = f.ptr()?;
    let vert_list_count = f.u32();
    let vert_list = f.ptr()?;
    let part_bits = [f.i32(), f.i32(), f.i32(), f.i32()];

    let blend_total = blend_counts
        .iter()
        .zip([1i32, 3, 5, 7])
        .map(|(c, w)| i32::from(*c) * w)
        .sum::<i32>();
    let blend_total =
        u32::try_from(blend_total).map_err(|_| ZoneError::Invalid("negative blend count"))?;
    let blends = s.array(blends, blend_total, 2, 2, |_, f| Ok(f.u16()))?;
    let verts = presentation(s, verts, Block::Vertex, u32::from(vert_count) * VERTEX_SIZE)?;
    let vert_list = s.array(vert_list, vert_list_count, 4, 12, |s, f| {
        let bone_offset = f.u16();
        let vert_count = f.u16();
        let tri_offset = f.u16();
        let tri_count = f.u16();
        let tree = f.ptr()?;
        Ok(RigidVertList {
            bone_offset,
            vert_count,
            tri_offset,
            tri_count,
            collision_tree: s.shared(tree, 4, 40, collision_tree)?,
        })
    })?;
    let tri_bytes = presentation(s, tris, Block::Index, u32::from(tri_count) * TRI_SIZE)?;
    let tri_indices: Arc<[u16]> = tri_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    Ok(Surface {
        tile_mode,
        deformed,
        vert_count,
        tri_count,
        zone_handle,
        base_tri_index,
        base_vert_index,
        blend_counts,
        blends,
        vert_list,
        part_bits,
        verts,
        tri_indices,
    })
}

fn xmodel(s: &mut Stream, h: &[u8]) -> Result<XModel> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let (num_bones, num_root_bones, num_surfs, lod_ramp_type) = (f.u8(), f.u8(), f.u8(), f.u8());
    let (bone_names, parent_list, quats, trans, classification, base_mat) =
        (f.ptr()?, f.ptr()?, f.ptr()?, f.ptr()?, f.ptr()?, f.ptr()?);
    let (surfs, materials) = (f.ptr()?, f.ptr()?);
    let lod_info = std::array::from_fn(|_| LodInfo {
        dist: f.f32(),
        surf_count: f.u16(),
        surf_index: f.u16(),
        part_bits: [f.i32(), f.i32(), f.i32(), f.i32()],
        lod: f.u8() as i8,
        smc_index_plus_one: f.u8() as i8,
        smc_alloc_bits: {
            let v = f.u8() as i8;
            f.skip(1);
            v
        },
    });
    let coll_surfs = f.ptr()?;
    let num_coll_surfs = f.u32();
    let contents = f.i32();
    let bone_info = f.ptr()?;
    let radius = f.f32();
    let (mins, maxs) = (f3(&mut f), f3(&mut f));
    let num_lods = f.u16();
    let coll_lod = i16s(&mut f);
    f.skip(4);
    let mem_usage = f.i32();
    let flags = f.u8();
    let bad = f.u8() != 0;
    f.skip(2);
    let (phys_preset, phys_geoms) = (f.ptr()?, f.ptr()?);

    let name = s.string(name)?;
    let n = u32::from(num_bones);
    let non_root = n
        .checked_sub(u32::from(num_root_bones))
        .ok_or(ZoneError::Invalid("more root bones than bones"))?;
    let bone_names = s.array(bone_names, n, 2, 2, |_, f| Ok(f.u16()))?;
    let parent_list = s.array(parent_list, non_root, 1, 1, |_, f| Ok(f.u8()))?;
    let quats = s.array(quats, non_root, 2, 8, |_, f| {
        Ok([i16s(f), i16s(f), i16s(f), i16s(f)])
    })?;
    let trans = s.array(trans, non_root * 4, 4, 4, |_, f| Ok(f.f32()))?;
    let part_classification = s.array(classification, n, 1, 1, |_, f| Ok(f.u8()))?;
    let base_mat = s.array(base_mat, n, 4, 32, |_, f| {
        Ok(BaseMat {
            quat: [f.f32(), f.f32(), f.f32(), f.f32()],
            trans: f3(f),
            trans_weight: f.f32(),
        })
    })?;
    let surfs = s.array(surfs, num_surfs.into(), 4, SURFACE_SIZE, surface)?;
    let materials = s.array(materials, num_surfs.into(), 4, 4, |s, f| {
        let slot = f.slot();
        material_ptr_at(s, slot, f.ptr()?)
    })?;
    let coll_surfs = s.array(coll_surfs, num_coll_surfs, 4, 44, |s, f| {
        let tris = f.ptr()?;
        let tri_count = f.u32();
        let (mins, maxs) = (f3(f), f3(f));
        let (bone_index, contents, surface_flags) = (f.i32(), f.i32(), f.i32());
        let tris = s.array(tris, tri_count, 4, 48, |_, f| {
            Ok(CollisionTri {
                plane: [f.f32(), f.f32(), f.f32(), f.f32()],
                svec: [f.f32(), f.f32(), f.f32(), f.f32()],
                tvec: [f.f32(), f.f32(), f.f32(), f.f32()],
            })
        })?;
        Ok(CollisionSurface {
            tris,
            mins,
            maxs,
            bone_index,
            contents,
            surface_flags,
        })
    })?;
    let bone_info = s.array(bone_info, n, 4, 40, |_, f| {
        Ok(BoneInfo {
            bounds: [f3(f), f3(f)],
            offset: f3(f),
            radius_squared: f.f32(),
        })
    })?;
    let phys_preset = phys::load(s, phys_preset)?;
    let phys_geoms = s.shared(phys_geoms, 4, 44, phys_geom_list)?;
    Ok(XModel {
        name,
        num_bones,
        num_root_bones,
        lod_ramp_type,
        bone_names,
        parent_list,
        quats,
        trans,
        part_classification,
        base_mat,
        surfs,
        materials,
        lod_info,
        coll_surfs,
        contents,
        bone_info,
        radius,
        mins,
        maxs,
        num_lods,
        coll_lod,
        mem_usage,
        flags,
        bad,
        phys_preset,
        phys_geoms,
    })
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<XModel>>> {
    s.temp_asset(p, 4, XMODEL_SIZE, xmodel)
}
