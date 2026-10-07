// SPDX-License-Identifier: GPL-3.0-or-later
//! The collision world: a decoded clipmap plus the flattened tables the traces read.

use std::sync::Arc;

use assets::zone::clipmap::{Clipmap, LeafBrushNodeData, LeafBrushes};

use crate::Vec3;

/// Distance of a plane that can never constrain anything (a side or node whose plane the zone
/// did not resolve).
const NEVER: f32 = 1.0e30;

/// A BSP node with its plane resolved.
#[derive(Debug, Clone, Copy)]
pub(super) struct Node {
    pub normal: Vec3,
    pub dist: f32,
    /// 0..=2 for an axis-aligned plane, 3 otherwise.
    pub kind: u8,
    /// Child node index, or `-1 - leaf` for a leaf.
    pub children: [i16; 2],
}

/// A brush side with its plane and surface resolved.
#[derive(Debug, Clone, Copy)]
pub(super) struct Side {
    pub normal: Vec3,
    pub dist: f32,
    pub material: u32,
}

/// A leaf-brush k-d node; a leaf of that tree lists brushes in [`CollisionWorld::lb_list`].
#[derive(Debug, Clone, Copy)]
pub(super) struct LbNode {
    pub contents: i32,
    pub axis: u8,
    /// `> 0`: a leaf of that many brushes; `< 0`: a node whose first child is stored inline at
    /// the next index and which also splits; `0`: a node that splits.
    pub count: i16,
    pub dist: f32,
    pub range: f32,
    pub child_offset: [u16; 2],
    /// First brush of the list when `count > 0`.
    pub first_brush: u32,
}

/// What a trace or a point query runs against: the map, one of its brush models, or a
/// stand-in box (`CM_TempBoxModel`) that entities without a brush model clip with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipModel {
    /// The static world.
    World,
    /// `*N`: an inline model of the map.
    Submodel(u16),
    /// A capsule of this extent that blocks anything matching `contents`.
    Box {
        mins: Vec3,
        maxs: Vec3,
        contents: i32,
    },
}

/// The static collision data of one map, prepared for traces. Immutable once built, so one
/// instance serves any number of concurrent readers.
#[derive(Debug)]
pub struct CollisionWorld {
    pub(super) cm: Arc<Clipmap>,
    pub(super) nodes: Box<[Node]>,
    pub(super) sides: Box<[Side]>,
    pub(super) lb_nodes: Box<[LbNode]>,
    pub(super) lb_list: Box<[u16]>,
}

impl CollisionWorld {
    pub fn new(cm: Arc<Clipmap>) -> Self {
        let plane = |i: Option<u32>| i.and_then(|i| cm.planes.get(i as usize));
        let nodes = cm
            .nodes
            .iter()
            .map(|n| match plane(n.plane) {
                Some(p) => Node {
                    normal: p.normal,
                    dist: p.dist,
                    kind: p.kind.min(3),
                    children: n.children,
                },
                None => Node {
                    normal: [0.0, 0.0, 1.0],
                    dist: NEVER,
                    kind: 2,
                    children: n.children,
                },
            })
            .collect();
        let sides = cm
            .brush_sides
            .iter()
            .map(|s| match plane(s.plane) {
                Some(p) => Side {
                    normal: p.normal,
                    dist: p.dist,
                    material: s.material,
                },
                None => Side {
                    normal: [0.0, 0.0, 1.0],
                    dist: NEVER,
                    material: s.material,
                },
            })
            .collect();
        let mut lb_list = Vec::new();
        let lb_nodes = cm
            .leaf_brush_nodes
            .iter()
            .map(|n| {
                let mut node = LbNode {
                    contents: n.contents,
                    axis: n.axis.clamp(0, 2) as u8,
                    count: n.leaf_brush_count,
                    dist: 0.0,
                    range: 0.0,
                    child_offset: [0; 2],
                    first_brush: 0,
                };
                match &n.data {
                    LeafBrushNodeData::Children {
                        dist,
                        range,
                        child_offset,
                    } => {
                        node.dist = *dist;
                        node.range = *range;
                        node.child_offset = *child_offset;
                    }
                    LeafBrushNodeData::Leaf(list) => {
                        node.first_brush = lb_list.len() as u32;
                        let want = n.leaf_brush_count.max(0) as usize;
                        let start = lb_list.len();
                        match list {
                            Some(LeafBrushes::Shared(i)) => {
                                let i = *i as usize;
                                lb_list.extend_from_slice(
                                    cm.leaf_brushes.get(i..i + want).unwrap_or(&[]),
                                );
                            }
                            Some(LeafBrushes::Own(b)) => lb_list.extend_from_slice(&b[..]),
                            None => node.contents = 0,
                        }
                        lb_list.truncate(start + want);
                        node.count = (lb_list.len() - start) as i16;
                    }
                }
                node
            })
            .collect();
        Self {
            cm,
            nodes,
            sides,
            lb_nodes,
            lb_list: lb_list.into_boxed_slice(),
        }
    }

    /// The decoded map this world was built from.
    pub fn clipmap(&self) -> &Clipmap {
        &self.cm
    }

    /// Number of brush models, the world model (index 0) included.
    pub fn num_submodels(&self) -> usize {
        self.cm.cmodels.len()
    }

    /// Bounds of brush model `model` (`CM_ModelBounds`); the world model spans the map.
    pub fn model_bounds(&self, model: u16) -> Option<(Vec3, Vec3)> {
        self.cm
            .cmodels
            .get(usize::from(model))
            .map(|m| (m.mins, m.maxs))
    }

    /// The contents a brush model is made of (`CM_ContentsOfModel`).
    pub fn model_contents(&self, model: u16) -> i32 {
        self.cm
            .cmodels
            .get(usize::from(model))
            .map_or(0, |m| m.leaf.brush_contents | m.leaf.terrain_contents)
    }

    /// The visibility cluster of a leaf (`-1` for solid/outside leaves).
    pub fn leaf_cluster(&self, leaf: u16) -> i16 {
        self.cm.leafs.get(usize::from(leaf)).map_or(-1, |l| l.cluster)
    }
}
