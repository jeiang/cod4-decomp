// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand-built clipmaps for the unit tests, and the install-gated mp_crash loader.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use assets::zone::clipmap::*;
use assets::zone::{Asset, Consumer, Zone};

use super::CollisionWorld;
use crate::Vec3;

pub(crate) const SOLID_TEST_MATERIAL: u16 = 0;

/// One brush: its bounds, contents and any extra (non-axial) side planes `(normal, dist)`.
pub(crate) struct BrushSpec {
    pub mins: Vec3,
    pub maxs: Vec3,
    pub contents: i32,
    pub planes: Vec<(Vec3, f32)>,
}

impl BrushSpec {
    pub fn aabb(mins: Vec3, maxs: Vec3, contents: i32) -> Self {
        Self {
            mins,
            maxs,
            contents,
            planes: Vec::new(),
        }
    }
}

/// A map made of one BSP leaf holding `world` brushes and terrain triangles, plus brush models
/// `*1..` that each hold the brushes of one entry of `models`.
#[derive(Default)]
pub(crate) struct MapSpec {
    pub world: Vec<BrushSpec>,
    pub models: Vec<Vec<BrushSpec>>,
    /// Terrain triangles `[a, b, c]`, one collision tree each, all of `terrain_contents`.
    pub triangles: Vec<[Vec3; 3]>,
    pub terrain_contents: i32,
}

impl MapSpec {
    pub fn build(self) -> Arc<Clipmap> {
        let mut planes = Vec::new();
        let mut sides = Vec::new();
        let mut brushes = Vec::new();
        let mut lb_nodes = vec![LeafBrushNode {
            axis: 0,
            leaf_brush_count: 0,
            contents: 0,
            data: LeafBrushNodeData::Children {
                dist: 0.0,
                range: 0.0,
                child_offset: [0, 0],
            },
        }];
        let mut leafs = Vec::new();
        let mut cmodels = Vec::new();
        let groups = std::iter::once(self.world).chain(self.models);
        for (g, group) in groups.enumerate() {
            let mut list = Vec::new();
            let mut contents = 0;
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for b in group {
                let first = sides.len() as u32;
                for (n, d) in &b.planes {
                    planes.push(Plane {
                        normal: *n,
                        dist: *d,
                        kind: 3,
                        sign_bits: 0,
                    });
                    sides.push(BrushSide {
                        plane: Some(planes.len() as u32 - 1),
                        material: 0,
                        first_adjacent_side_offset: 0,
                        edge_count: 0,
                    });
                }
                list.push(brushes.len() as u16);
                contents |= b.contents;
                for i in 0..3 {
                    lo[i] = lo[i].min(b.mins[i]);
                    hi[i] = hi[i].max(b.maxs[i]);
                }
                brushes.push(Brush {
                    mins: b.mins,
                    contents: b.contents,
                    maxs: b.maxs,
                    num_sides: b.planes.len() as u32,
                    sides: (!b.planes.is_empty()).then_some(first),
                    axial_material_num: [[0; 3]; 2],
                    base_adjacent_side: None,
                    first_adjacent_side_offsets: [[0; 3]; 2],
                    edge_count: [[0; 3]; 2],
                });
            }
            let node = if list.is_empty() {
                0
            } else {
                lb_nodes.push(LeafBrushNode {
                    axis: 0,
                    leaf_brush_count: list.len() as i16,
                    contents,
                    data: LeafBrushNodeData::Leaf(Some(LeafBrushes::Own(list.into()))),
                });
                lb_nodes.len() as i32 - 1
            };
            if lo[0] > hi[0] {
                (lo, hi) = ([-4096.0; 3], [4096.0; 3]);
            }
            let terrain = g == 0 && !self.triangles.is_empty();
            let leaf = Leaf {
                first_coll_aabb_index: 0,
                coll_aabb_count: u16::from(terrain),
                brush_contents: contents,
                terrain_contents: if terrain { self.terrain_contents } else { 0 },
                mins: if g == 0 { [-4096.0; 3] } else { lo },
                maxs: if g == 0 { [4096.0; 3] } else { hi },
                leaf_brush_node: node,
                cluster: 0,
            };
            cmodels.push(CModel {
                mins: leaf.mins,
                maxs: leaf.maxs,
                radius: 0.0,
                leaf: Leaf { ..leaf },
            });
            if g == 0 {
                leafs.push(leaf);
            }
        }

        let mut verts = Vec::new();
        let mut tri_indices = Vec::new();
        let mut partitions = Vec::new();
        let mut aabb_trees = Vec::new();
        // One collision tree per triangle, listed as children of one root.
        for (i, t) in self.triangles.iter().enumerate() {
            let base = verts.len() as u16;
            verts.extend_from_slice(t);
            tri_indices.extend_from_slice(&[base, base + 1, base + 2]);
            partitions.push(CollisionPartition {
                tri_count: 1,
                border_count: 0,
                first_tri: i as i32,
                borders: None,
            });
        }
        if !self.triangles.is_empty() {
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in &verts {
                for i in 0..3 {
                    lo[i] = lo[i].min(v[i]);
                    hi[i] = hi[i].max(v[i]);
                }
            }
            let n = partitions.len();
            aabb_trees.push(CollisionAabbTree {
                origin: [0.0; 3],
                half_size: [0.0; 3],
                material_index: SOLID_TEST_MATERIAL,
                child_count: n as u16,
                index: 1,
            });
            for i in 0..n {
                let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                for v in &verts[3 * i..3 * i + 3] {
                    for k in 0..3 {
                        lo[k] = lo[k].min(v[k]);
                        hi[k] = hi[k].max(v[k]);
                    }
                }
                aabb_trees.push(CollisionAabbTree {
                    origin: std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5),
                    half_size: std::array::from_fn(|k| (hi[k] - lo[k]) * 0.5),
                    material_index: SOLID_TEST_MATERIAL,
                    child_count: 0,
                    index: i as i32,
                });
            }
            let root = &mut aabb_trees[0];
            root.origin = std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5);
            root.half_size = std::array::from_fn(|k| (hi[k] - lo[k]) * 0.5);
        }

        Arc::new(Clipmap {
            name: None,
            is_in_use: 0,
            planes: planes.into(),
            static_models: Arc::from(Vec::new()),
            materials: Arc::from(vec![CollisionMaterial {
                name: "test".into(),
                surface_flags: 0x1234,
                content_flags: 1 | self.terrain_contents,
            }]),
            brush_sides: sides.into(),
            brush_edges: Arc::from(Vec::new()),
            nodes: Arc::from(vec![Node {
                plane: None,
                children: [-1, -1],
            }]),
            leafs: leafs.into(),
            leaf_brush_nodes: lb_nodes.into(),
            leaf_brushes: Arc::from(Vec::new()),
            leaf_surfaces: Arc::from(Vec::new()),
            verts: verts.into(),
            tri_indices: tri_indices.into(),
            tri_edge_is_walkable: Arc::from(vec![0xffu8; 4]),
            borders: Arc::from(Vec::new()),
            partitions: partitions.into(),
            aabb_trees: aabb_trees.into(),
            cmodels: cmodels.into(),
            brushes: brushes.into(),
            num_clusters: 1,
            cluster_bytes: 1,
            visibility: Arc::from(vec![0u8]),
            vised: 0,
            map_ents: None,
            box_brush: None,
            box_model: CModel {
                mins: [0.0; 3],
                maxs: [0.0; 3],
                radius: 0.0,
                leaf: Leaf {
                    first_coll_aabb_index: 0,
                    coll_aabb_count: 0,
                    brush_contents: -1,
                    terrain_contents: 0,
                    mins: [0.0; 3],
                    maxs: [0.0; 3],
                    leaf_brush_node: 0,
                    cluster: 0,
                },
            },
            dyn_entities: [Arc::from(Vec::new()), Arc::from(Vec::new())],
            checksum: 0,
        })
    }

    pub fn world(self) -> CollisionWorld {
        CollisionWorld::new(self.build())
    }
}

fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

fn load_install_map(map: &str) -> Option<Arc<Clipmap>> {
    let Some(root) = std::env::var_os("COD4_PATH") else {
        eprintln!("COD4_PATH not set; skipping");
        return None;
    };
    let english = find_ci(&PathBuf::from(root).join("zone"), "english")?;
    let file = std::fs::File::open(find_ci(&english, &format!("{map}.ff"))?).ok()?;
    let z = Zone::open(std::io::BufReader::new(file)).ok()?;
    let mut found = None;
    z.decode(&Consumer::Server, |a| {
        if let Asset::Clipmap(c) = a {
            found = Some(c);
        }
    })
    .ok()?;
    found
}

/// The decoded mp_crash clipmap, or `None` (the test then skips) without `COD4_PATH`.
pub(crate) fn crash_map() -> Option<Arc<Clipmap>> {
    static CRASH: LazyLock<Option<Arc<Clipmap>>> = LazyLock::new(|| load_install_map("mp_crash"));
    CRASH.clone()
}
