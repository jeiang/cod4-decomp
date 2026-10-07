// SPDX-License-Identifier: GPL-3.0-or-later
//! clipMap_t (MP and SP share one loader) and MapEnts.
//!
//! References that point into another array of the same clipmap (a node's
//! plane, a brush's sides, a partition's borders, ...) are stored as indices
//! into that array.

use super::error::{Result, ZoneError};
use super::fx::FxEffectDef;
use super::gfx::{Name, raw_of};
pub use super::gfxworld::Plane;
use super::phys::PhysPreset;
use super::stream::{Addr, Block, Fields, Ptr, Stream};
use super::xmodel::{self, XModel};
use std::sync::Arc;

/// The entity string of a map (`MapEnts`).
#[derive(Debug)]
pub struct MapEnts {
    pub name: Name,
    /// The entity text as stored, including the trailing NUL.
    pub entity_string: Vec<u8>,
}

const MAP_ENTS_SIZE: u32 = 12;

fn map_ents(s: &mut Stream, h: &[u8]) -> Result<MapEnts> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let text = f.ptr()?;
    let len = count(f.i32())?;
    let name = s.string(name)?;
    let entity_string = match text {
        Ptr::Null => Vec::new(),
        Ptr::Follow => s.load(1, len)?.1,
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    };
    Ok(MapEnts {
        name,
        entity_string,
    })
}

pub(super) fn map_ents_ptr(s: &mut Stream, p: Ptr) -> Result<Option<Arc<MapEnts>>> {
    s.temp_asset(p, 4, MAP_ENTS_SIZE, map_ents)
}

#[derive(Debug)]
pub struct StaticModel {
    pub model: Option<Arc<XModel>>,
    pub origin: [f32; 3],
    pub inv_scaled_axis: [[f32; 3]; 3],
    pub abs_min: [f32; 3],
    pub abs_max: [f32; 3],
}

/// A surface a collision face refers to by number.
#[derive(Debug)]
pub struct CollisionMaterial {
    pub name: String,
    pub surface_flags: i32,
    pub content_flags: i32,
}

#[derive(Debug)]
pub struct BrushSide {
    /// Index into [`Clipmap::planes`].
    pub plane: Option<u32>,
    pub material: u32,
    pub first_adjacent_side_offset: i16,
    pub edge_count: u8,
}

#[derive(Debug)]
pub struct Node {
    /// Index into [`Clipmap::planes`].
    pub plane: Option<u32>,
    pub children: [i16; 2],
}

#[derive(Debug)]
pub struct Leaf {
    pub first_coll_aabb_index: u16,
    pub coll_aabb_count: u16,
    pub brush_contents: i32,
    pub terrain_contents: i32,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub leaf_brush_node: i32,
    pub cluster: i16,
}

/// The brush indices of a leaf-brush node leaf.
#[derive(Debug)]
pub enum LeafBrushes {
    /// `leaf_brush_count` entries of [`Clipmap::leaf_brushes`] from this index.
    Shared(u32),
    Own(Arc<[u16]>),
}

#[derive(Debug)]
pub enum LeafBrushNodeData {
    Children {
        dist: f32,
        range: f32,
        child_offset: [u16; 2],
    },
    /// `leaf_brush_count > 0`.
    Leaf(Option<LeafBrushes>),
}

#[derive(Debug)]
pub struct LeafBrushNode {
    pub axis: i8,
    pub leaf_brush_count: i16,
    pub contents: i32,
    pub data: LeafBrushNodeData,
}

#[derive(Debug)]
pub struct CollisionBorder {
    pub dist_eq: [f32; 3],
    pub z_base: f32,
    pub z_slope: f32,
    pub start: f32,
    pub length: f32,
}

#[derive(Debug)]
pub struct CollisionPartition {
    pub tri_count: u8,
    pub border_count: u8,
    pub first_tri: i32,
    /// Index into [`Clipmap::borders`] of the first of `border_count` borders.
    pub borders: Option<u32>,
}

#[derive(Debug)]
pub struct CollisionAabbTree {
    pub origin: [f32; 3],
    pub half_size: [f32; 3],
    pub material_index: u16,
    pub child_count: u16,
    /// First child index, or the partition index for a leaf.
    pub index: i32,
}

#[derive(Debug)]
pub struct CModel {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub radius: f32,
    pub leaf: Leaf,
}

#[derive(Debug)]
pub struct Brush {
    pub mins: [f32; 3],
    pub contents: i32,
    pub maxs: [f32; 3],
    pub num_sides: u32,
    /// Index into [`Clipmap::brush_sides`] of the first side.
    pub sides: Option<u32>,
    pub axial_material_num: [[i16; 3]; 2],
    /// Index into [`Clipmap::brush_edges`].
    pub base_adjacent_side: Option<u32>,
    pub first_adjacent_side_offsets: [[i16; 3]; 2],
    pub edge_count: [[u8; 3]; 2],
}

#[derive(Debug)]
pub struct Placement {
    pub quat: [f32; 4],
    pub origin: [f32; 3],
}

#[derive(Debug)]
pub struct XModelPiece {
    pub model: Option<Arc<XModel>>,
    pub offset: [f32; 3],
}

#[derive(Debug)]
pub struct XModelPieces {
    pub name: Name,
    pub pieces: Arc<[XModelPiece]>,
}

/// A dynamic entity definition. The pose, client and collision lists that
/// accompany it are runtime state and are not kept.
#[derive(Debug)]
pub struct DynEntityDef {
    /// 1 clutter, 2 destructible.
    pub kind: u32,
    pub pose: Placement,
    pub model: Option<Arc<XModel>>,
    pub brush_model: u16,
    pub physics_brush_model: u16,
    pub destroy_fx: Option<Arc<FxEffectDef>>,
    pub destroy_pieces: Option<Arc<XModelPieces>>,
    pub phys_preset: Option<Arc<PhysPreset>>,
    pub health: i32,
    pub center_of_mass: [f32; 3],
    pub moments_of_inertia: [f32; 3],
    pub products_of_inertia: [f32; 3],
    pub contents: i32,
}

#[derive(Debug)]
pub struct Clipmap {
    pub name: Name,
    pub is_in_use: i32,
    pub planes: Arc<[Plane]>,
    pub static_models: Arc<[StaticModel]>,
    pub materials: Arc<[CollisionMaterial]>,
    pub brush_sides: Arc<[BrushSide]>,
    pub brush_edges: Arc<[u8]>,
    pub nodes: Arc<[Node]>,
    pub leafs: Arc<[Leaf]>,
    pub leaf_brush_nodes: Arc<[LeafBrushNode]>,
    pub leaf_brushes: Arc<[u16]>,
    pub leaf_surfaces: Arc<[u32]>,
    pub verts: Arc<[[f32; 3]]>,
    /// Three indices per triangle.
    pub tri_indices: Arc<[u16]>,
    pub tri_edge_is_walkable: Arc<[u8]>,
    pub borders: Arc<[CollisionBorder]>,
    pub partitions: Arc<[CollisionPartition]>,
    pub aabb_trees: Arc<[CollisionAabbTree]>,
    pub cmodels: Arc<[CModel]>,
    pub brushes: Arc<[Brush]>,
    pub num_clusters: i32,
    pub cluster_bytes: i32,
    /// `num_clusters * cluster_bytes` bytes.
    pub visibility: Arc<[u8]>,
    pub vised: i32,
    pub map_ents: Option<Arc<MapEnts>>,
    /// The collision-box brush when it is stored inline.
    pub box_brush: Option<Brush>,
    pub box_model: CModel,
    /// The two dynamic-entity sets (static, then moving).
    pub dyn_entities: [Arc<[DynEntityDef]>; 2],
    pub checksum: u32,
}

const CLIPMAP_SIZE: u32 = 284;
const BRUSH_SIZE: u32 = 80;
const DYN_ENTITY_DEF_SIZE: u32 = 96;

/// Runtime dynamic-entity lists: (element size) of pose, client, collision.
const DYN_RUNTIME_SIZES: [u32; 3] = [32, 12, 20];

fn count(v: i32) -> Result<u32> {
    u32::try_from(v).map_err(|_| ZoneError::Invalid("negative clipmap count"))
}

fn v3(f: &mut Fields) -> [f32; 3] {
    [f.f32(), f.f32(), f.f32()]
}

/// Index of `p` within an array of `len` `stride`-byte elements at `base`
/// (one past the end is allowed: an empty range there).
fn index_in(base: Option<Addr>, p: Ptr, stride: u32, len: u32) -> Result<Option<u32>> {
    match p {
        Ptr::Null => Ok(None),
        Ptr::Offset(a) => match base {
            Some(b)
                if a.block == b.block
                    && a.offset >= b.offset
                    && (a.offset - b.offset) % stride == 0
                    && (a.offset - b.offset) / stride <= len =>
            {
                Ok(Some((a.offset - b.offset) / stride))
            }
            _ => Err(ZoneError::BadOffset(a)),
        },
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

/// An owned array behind a pointer that is only ever loaded inline: the whole
/// array is read first, then `f` decodes each element and its nested data.
fn array<T>(
    s: &mut Stream,
    p: Ptr,
    n: u32,
    align: u32,
    size: u32,
    mut f: impl FnMut(&mut Stream, &mut Fields) -> Result<T>,
) -> Result<(Option<Addr>, Arc<[T]>)> {
    match p {
        Ptr::Null => Ok((None, Arc::from(Vec::new()))),
        Ptr::Follow => {
            let len = n
                .checked_mul(size)
                .ok_or(ZoneError::Invalid("clipmap array too large"))?;
            let (at, bytes) = s.load(align, len)?;
            let mut v = Vec::with_capacity(n as usize);
            for (i, chunk) in bytes.chunks_exact(size as usize).enumerate() {
                let elem = Addr {
                    block: at.block,
                    offset: at.offset + i as u32 * size,
                };
                v.push(f(s, &mut Fields::at(chunk, elem))?);
            }
            Ok((Some(at), v.into()))
        }
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

fn leaf(f: &mut Fields) -> Leaf {
    let first_coll_aabb_index = f.u16();
    let coll_aabb_count = f.u16();
    let brush_contents = f.i32();
    let terrain_contents = f.i32();
    let mins = v3(f);
    let maxs = v3(f);
    let leaf_brush_node = f.i32();
    let cluster = f.u16() as i16;
    f.skip(2);
    Leaf {
        first_coll_aabb_index,
        coll_aabb_count,
        brush_contents,
        terrain_contents,
        mins,
        maxs,
        leaf_brush_node,
        cluster,
    }
}

fn cmodel(f: &mut Fields) -> CModel {
    CModel {
        mins: v3(f),
        maxs: v3(f),
        radius: f.f32(),
        leaf: leaf(f),
    }
}

fn i16s<const N: usize>(f: &mut Fields) -> [i16; N] {
    std::array::from_fn(|_| f.u16() as i16)
}

/// One brush header; its `sides` and `baseAdjacentSide` pointers must refer
/// into the clipmap's arrays.
fn brush(f: &mut Fields, sides: (Option<Addr>, u32), edges: (Option<Addr>, u32)) -> Result<Brush> {
    let mins = v3(f);
    let contents = f.i32();
    let maxs = v3(f);
    let num_sides = f.u32();
    let side_ptr = f.ptr()?;
    let axial_material_num = [i16s(f), i16s(f)];
    let edge_ptr = f.ptr()?;
    let first_adjacent_side_offsets = [i16s(f), i16s(f)];
    let edge_count = [f.bytes(), f.bytes()];
    Ok(Brush {
        mins,
        contents,
        maxs,
        num_sides,
        sides: index_in(sides.0, side_ptr, 12, sides.1)?,
        axial_material_num,
        base_adjacent_side: index_in(edges.0, edge_ptr, 1, edges.1)?,
        first_adjacent_side_offsets,
        edge_count,
    })
}

fn placement(f: &mut Fields) -> Placement {
    Placement {
        quat: [f.f32(), f.f32(), f.f32(), f.f32()],
        origin: v3(f),
    }
}

fn model_ref(s: &mut Stream, f: &mut Fields) -> Result<Option<Arc<XModel>>> {
    let slot = f.slot();
    let p = f.ptr()?;
    xmodel::load_at(s, slot, p)
}

fn model_pieces(s: &mut Stream, p: Ptr) -> Result<Option<Arc<XModelPieces>>> {
    s.shared(p, 4, 12, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let n = count(f.i32())?;
        let pieces = f.ptr()?;
        let name = s.string(name)?;
        let (_, pieces) = array(s, pieces, n, 4, 16, |s, f| {
            let model = model_ref(s, f)?;
            Ok(XModelPiece {
                model,
                offset: v3(f),
            })
        })?;
        Ok(XModelPieces { name, pieces })
    })
}

fn dyn_entity_def(s: &mut Stream, f: &mut Fields) -> Result<DynEntityDef> {
    let kind = f.u32();
    let pose = placement(f);
    let model = model_ref(s, f)?;
    let brush_model = f.u16();
    let physics_brush_model = f.u16();
    let destroy_fx = f.ptr()?;
    let destroy_pieces = f.ptr()?;
    let phys_preset = f.ptr()?;
    let health = f.i32();
    let center_of_mass = v3(f);
    let moments_of_inertia = v3(f);
    let products_of_inertia = v3(f);
    let contents = f.i32();
    let destroy_fx = super::fx::load(s, destroy_fx)?;
    let destroy_pieces = model_pieces(s, destroy_pieces)?;
    let phys_preset = super::phys::load(s, phys_preset)?;
    Ok(DynEntityDef {
        kind,
        pose,
        model,
        brush_model,
        physics_brush_model,
        destroy_fx,
        destroy_pieces,
        phys_preset,
        health,
        center_of_mass,
        moments_of_inertia,
        products_of_inertia,
        contents,
    })
}

fn clipmap(s: &mut Stream, h: &[u8]) -> Result<Clipmap> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let is_in_use = f.i32();
    let plane_count = count(f.i32())?;
    let planes = f.ptr()?;
    let num_static_models = f.u32();
    let static_models = f.ptr()?;
    let num_materials = f.u32();
    let materials = f.ptr()?;
    let num_brush_sides = f.u32();
    let brush_sides = f.ptr()?;
    let num_brush_edges = f.u32();
    let brush_edges = f.ptr()?;
    let num_nodes = f.u32();
    let nodes = f.ptr()?;
    let num_leafs = f.u32();
    let leafs = f.ptr()?;
    let leaf_brush_nodes_count = f.u32();
    let leaf_brush_nodes = f.ptr()?;
    let num_leaf_brushes = f.u32();
    let leaf_brushes = f.ptr()?;
    let num_leaf_surfaces = f.u32();
    let leaf_surfaces = f.ptr()?;
    let vert_count = f.u32();
    let verts = f.ptr()?;
    let tri_count = count(f.i32())?;
    let tri_indices = f.ptr()?;
    let walkable = f.ptr()?;
    let border_count = count(f.i32())?;
    let borders = f.ptr()?;
    let partition_count = count(f.i32())?;
    let partitions = f.ptr()?;
    let aabb_tree_count = count(f.i32())?;
    let aabb_trees = f.ptr()?;
    let num_sub_models = f.u32();
    let cmodels = f.ptr()?;
    let num_brushes = u32::from(f.u16());
    f.skip(2);
    let brushes = f.ptr()?;
    let num_clusters = f.i32();
    let cluster_bytes = f.i32();
    let visibility = f.ptr()?;
    let vised = f.i32();
    let map_ents = f.ptr()?;
    let box_brush = f.ptr()?;
    let box_model = cmodel(&mut f);
    let dyn_count = [u32::from(f.u16()), u32::from(f.u16())];
    let dyn_defs = [f.ptr()?, f.ptr()?];
    let dyn_runtime = [
        [f.ptr()?, f.ptr()?],
        [f.ptr()?, f.ptr()?],
        [f.ptr()?, f.ptr()?],
    ];
    let checksum = f.u32();

    let name = s.string(name)?;
    // The plane array is shared with the GfxWorld that was decoded before.
    let (planes_at, planes) = match planes {
        Ptr::Offset(a) => (Some(a), s.lookup::<Arc<[Plane]>>(a)?),
        p => array(s, p, plane_count, 4, 20, |_, f| {
            let normal = v3(f);
            let dist = f.f32();
            let kind = f.u8();
            let sign_bits = f.u8();
            Ok(Plane {
                normal,
                dist,
                kind,
                sign_bits,
            })
        })?,
    };
    let plane_ref =
        |f: &mut Fields| -> Result<Option<u32>> { index_in(planes_at, f.ptr()?, 20, plane_count) };
    let (_, static_models) = array(s, static_models, num_static_models, 4, 80, |s, f| {
        f.skip(4);
        let model = model_ref(s, f)?;
        Ok(StaticModel {
            model,
            origin: v3(f),
            inv_scaled_axis: [v3(f), v3(f), v3(f)],
            abs_min: v3(f),
            abs_max: v3(f),
        })
    })?;
    let (_, materials) = array(s, materials, num_materials, 4, 72, |_, f| {
        let raw: [u8; 64] = f.bytes();
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        Ok(CollisionMaterial {
            name: String::from_utf8_lossy(&raw[..end]).into_owned(),
            surface_flags: f.i32(),
            content_flags: f.i32(),
        })
    })?;
    let (sides_at, brush_sides) = array(s, brush_sides, num_brush_sides, 4, 12, |_, f| {
        let plane = plane_ref(f)?;
        Ok(BrushSide {
            plane,
            material: f.u32(),
            first_adjacent_side_offset: f.u16() as i16,
            edge_count: f.u8(),
        })
    })?;
    let (edges_at, brush_edges) = array(s, brush_edges, num_brush_edges, 1, 1, |_, f| Ok(f.u8()))?;
    let (_, nodes) = array(s, nodes, num_nodes, 4, 8, |_, f| {
        Ok(Node {
            plane: plane_ref(f)?,
            children: i16s(f),
        })
    })?;
    let (_, leafs) = array(s, leafs, num_leafs, 4, 44, |_, f| Ok(leaf(f)))?;
    let (leaf_brushes_at, leaf_brushes) =
        array(s, leaf_brushes, num_leaf_brushes, 2, 2, |_, f| Ok(f.u16()))?;
    let (_, leaf_brush_nodes) = array(
        s,
        leaf_brush_nodes,
        leaf_brush_nodes_count,
        4,
        20,
        |s, f| {
            let axis = f.u8() as i8;
            f.skip(1);
            let leaf_brush_count = f.u16() as i16;
            let contents = f.i32();
            let data = if leaf_brush_count > 0 {
                let p = f.ptr()?;
                LeafBrushNodeData::Leaf(leaf_brush_list(
                    s,
                    p,
                    leaf_brush_count as u32,
                    (leaf_brushes_at, num_leaf_brushes),
                )?)
            } else {
                LeafBrushNodeData::Children {
                    dist: f.f32(),
                    range: f.f32(),
                    child_offset: [f.u16(), f.u16()],
                }
            };
            Ok(LeafBrushNode {
                axis,
                leaf_brush_count,
                contents,
                data,
            })
        },
    )?;
    let (_, leaf_surfaces) = array(
        s,
        leaf_surfaces,
        num_leaf_surfaces,
        4,
        4,
        |_, f| Ok(f.u32()),
    )?;
    let (_, verts) = array(s, verts, vert_count, 4, 12, |_, f| Ok(v3(f)))?;
    let (_, tri_indices) = array(s, tri_indices, 3 * tri_count, 2, 2, |_, f| Ok(f.u16()))?;
    let (_, tri_edge_is_walkable) = array(
        s,
        walkable,
        (3 * tri_count).div_ceil(32) * 4,
        1,
        1,
        |_, f| Ok(f.u8()),
    )?;
    let (borders_at, borders) = array(s, borders, border_count, 4, 28, |_, f| {
        Ok(CollisionBorder {
            dist_eq: v3(f),
            z_base: f.f32(),
            z_slope: f.f32(),
            start: f.f32(),
            length: f.f32(),
        })
    })?;
    let (_, partitions) = array(s, partitions, partition_count, 4, 12, |_, f| {
        let tri_count = f.u8();
        let border_len = f.u8();
        f.skip(2);
        let first_tri = f.i32();
        let p = f.ptr()?;
        Ok(CollisionPartition {
            tri_count,
            border_count: border_len,
            first_tri,
            borders: index_in(borders_at, p, 28, border_count)?,
        })
    })?;
    let (_, aabb_trees) = array(s, aabb_trees, aabb_tree_count, 4, 32, |_, f| {
        Ok(CollisionAabbTree {
            origin: v3(f),
            half_size: v3(f),
            material_index: f.u16(),
            child_count: f.u16(),
            index: f.i32(),
        })
    })?;
    let (_, cmodels) = array(s, cmodels, num_sub_models, 4, 72, |_, f| Ok(cmodel(f)))?;
    let (_, brushes) = array(s, brushes, num_brushes, 16, BRUSH_SIZE, |_, f| {
        brush(f, (sides_at, num_brush_sides), (edges_at, num_brush_edges))
    })?;
    let (_, visibility) = array(
        s,
        visibility,
        (num_clusters.max(0) as u32)
            .checked_mul(cluster_bytes.max(0) as u32)
            .ok_or(ZoneError::Invalid("visibility too large"))?,
        1,
        1,
        |_, f| Ok(f.u8()),
    )?;
    let map_ents = map_ents_ptr(s, map_ents)?;
    let box_brush = match box_brush {
        Ptr::Follow => {
            let (_, b) = s.load(16, BRUSH_SIZE)?;
            Some(brush(
                &mut Fields::new(&b),
                (sides_at, num_brush_sides),
                (edges_at, num_brush_edges),
            )?)
        }
        // Points into `brushes`.
        _ => None,
    };
    let mut defs = [Arc::from(Vec::new()), Arc::from(Vec::new())];
    for i in 0..2 {
        defs[i] = array(
            s,
            dyn_defs[i],
            dyn_count[i],
            4,
            DYN_ENTITY_DEF_SIZE,
            dyn_entity_def,
        )?
        .1;
    }
    for (sizes, lists) in DYN_RUNTIME_SIZES.iter().zip(dyn_runtime) {
        for (list, n) in lists.into_iter().zip(dyn_count) {
            if list != Ptr::Null {
                s.push(Block::Runtime);
                s.alloc(4, n * sizes)?;
                s.pop()?;
            }
        }
    }
    Ok(Clipmap {
        name,
        is_in_use,
        planes,
        static_models,
        materials,
        brush_sides,
        brush_edges,
        nodes,
        leafs,
        leaf_brush_nodes,
        leaf_brushes,
        leaf_surfaces,
        verts,
        tri_indices,
        tri_edge_is_walkable,
        borders,
        partitions,
        aabb_trees,
        cmodels,
        brushes,
        num_clusters,
        cluster_bytes,
        visibility,
        vised,
        map_ents,
        box_brush,
        box_model,
        dyn_entities: defs,
        checksum,
    })
}

/// The brush list of a leaf: stored inline, shared by an earlier leaf, or a
/// slice of the clipmap's own `leaf_brushes`.
fn leaf_brush_list(
    s: &mut Stream,
    p: Ptr,
    n: u32,
    main: (Option<Addr>, u32),
) -> Result<Option<LeafBrushes>> {
    match p {
        Ptr::Null => Ok(None),
        Ptr::Follow => {
            let (at, bytes) = s.load(2, n * 2)?;
            let mut f = Fields::new(&bytes);
            let list: Arc<[u16]> = (0..n).map(|_| f.u16()).collect();
            s.register(at, list.clone());
            Ok(Some(LeafBrushes::Own(list)))
        }
        Ptr::Offset(a) => match index_in(main.0, p, 2, main.1) {
            Ok(i) => Ok(i.map(LeafBrushes::Shared)),
            Err(_) => s.lookup::<Arc<[u16]>>(a).map(|l| Some(LeafBrushes::Own(l))),
        },
        Ptr::Insert => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<Clipmap>>> {
    s.temp_asset(p, 4, CLIPMAP_SIZE, clipmap)
}
