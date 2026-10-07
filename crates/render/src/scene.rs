// SPDX-License-Identifier: GPL-3.0-or-later
//! One map's GPU data: world mesh, model meshes, the model-lighting volume, and the sun.

use crate::gpu::Gpu;
use crate::lightgrid::ModelLighting;
use crate::texture::{self, Tex};
use assets::zone::gfx::{Material, TechniqueSet};
use assets::zone::gfxworld::GfxWorld;
use assets::zone::xmodel::XModel;
use assets::zone::{Asset, DecodeFilter, XAssetType, Zone};
use glam::{Mat4, Vec3};
use sm3::SamplerDim;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub struct Mesh {
    pub vb: wgpu::Buffer,
    pub ib: wgpu::Buffer,
}

/// A decoded map zone: the world and the sun, ready for [`Scene::new`].
pub struct MapData {
    pub world: Arc<GfxWorld>,
    /// Technique sets of the map zone and `common_mp`; materials reference sets of other zones by name.
    pub techsets: Vec<Arc<TechniqueSet>>,
}

#[derive(Debug)]
pub enum LoadError {
    Zone(assets::zone::ZoneError),
    Io(std::io::Error),
    NoWorld,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Zone(e) => write!(f, "{e}"),
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::NoWorld => f.write_str("the zone has no GfxWorld"),
        }
    }
}

impl std::error::Error for LoadError {}

impl MapData {
    /// Decode `zone/english/<map>.ff` and `common_mp.ff` under the install root.
    pub fn load(install: &Path, map: &str) -> Result<MapData, LoadError> {
        let mut world = None;
        let mut techsets = Vec::new();
        for zone in ["common_mp", map] {
            let path = install.join("zone/english").join(format!("{zone}.ff"));
            let file = std::fs::File::open(path).map_err(LoadError::Io)?;
            let z = Zone::open(std::io::BufReader::new(file)).map_err(LoadError::Zone)?;
            let keep = Keep(zone == map);
            z.decode(&keep, |a| match a {
                Asset::GfxWorld(w) => world = Some(w),
                Asset::TechniqueSet(t) => techsets.push(t),
                _ => {}
            })
            .map_err(LoadError::Zone)?;
        }
        Ok(MapData {
            world: world.ok_or(LoadError::NoWorld)?,
            techsets,
        })
    }
}

/// Decode filter: technique sets always, the rest only for the map zone.
struct Keep(bool);

impl DecodeFilter for Keep {
    fn keep(&self, ty: XAssetType) -> bool {
        self.0 || ty == XAssetType::TechniqueSet
    }
    fn keep_presentation(&self) -> bool {
        true
    }
}

pub struct Scene {
    pub world: Arc<GfxWorld>,
    pub world_mesh: Arc<Mesh>,
    pub sky_surfaces: HashSet<u32>,
    pub lighting: ModelLighting,
    pub lighting_tex: Arc<Tex>,
    pub sun_dir: Vec3,
    pub sun_color: Vec3,
    models: HashMap<(usize, usize), Arc<Mesh>>,
}

impl Scene {
    pub fn new(gpu: &Gpu, data: &MapData) -> Scene {
        let world = data.world.clone();
        let dev = &gpu.device;
        let mut indices = world.indices.clone();
        indices.resize(indices.len().next_multiple_of(2), 0);
        let world_mesh = Arc::new(Mesh {
            vb: dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("world vertices"),
                contents: &world.vertices,
                usage: wgpu::BufferUsages::VERTEX,
            }),
            ib: dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("world indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
        });
        let lighting = ModelLighting::new(&world, 1024);
        let lighting_tex = Arc::new(upload_lighting(gpu, &lighting));
        let (sun_dir, sun_color) = match &world.sun_light {
            Some(l) => (Vec3::from(l.dir), Vec3::from(l.color)),
            None => (Vec3::new(0.3, 0.2, 0.9).normalize(), Vec3::from(world.sun_color_from_bsp)),
        };
        let sky_surfaces = world.sky_start_surfs.iter().map(|&s| s as u32).collect();
        Scene {
            world,
            world_mesh,
            sky_surfaces,
            lighting,
            lighting_tex,
            sun_dir,
            sun_color,
            models: HashMap::new(),
        }
    }

    /// GPU buffers of surface `index` of `model`, created on first use.
    pub fn model_mesh(&mut self, gpu: &Gpu, model: &Arc<XModel>, index: usize) -> Option<Arc<Mesh>> {
        let key = (Arc::as_ptr(model) as usize, index);
        if let Some(m) = self.models.get(&key) {
            return Some(m.clone());
        }
        let s = model.surfs.get(index)?;
        if s.verts.is_empty() || s.tri_indices.is_empty() {
            return None;
        }
        let mut idx: Vec<u16> = s.tri_indices.to_vec();
        idx.resize(idx.len().next_multiple_of(2), 0);
        let mesh = Arc::new(Mesh {
            vb: gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: model.name.as_deref(),
                contents: &s.verts,
                usage: wgpu::BufferUsages::VERTEX,
            }),
            ib: gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: model.name.as_deref(),
                contents: bytemuck::cast_slice(&idx),
                usage: wgpu::BufferUsages::INDEX,
            }),
        });
        self.models.insert(key, mesh.clone());
        Some(mesh)
    }

    /// Every distinct material the map's surfaces and static models use.
    pub fn materials(&self) -> Vec<Arc<Material>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut add = |m: &Option<Arc<Material>>| {
            if let Some(m) = m
                && seen.insert(Arc::as_ptr(m) as usize)
            {
                out.push(m.clone());
            }
        };
        for s in &self.world.dpvs.surfaces {
            add(&s.material);
        }
        for m in &self.world.dpvs.smodel_draw_insts {
            if let Some(model) = &m.model {
                for mat in model.materials.iter() {
                    add(mat);
                }
            }
        }
        out
    }
}

pub fn model_matrix(m: &assets::zone::gfxworld::StaticModel) -> Mat4 {
    let a = |i: usize| Vec3::from(m.axis[i]) * m.scale;
    Mat4::from_cols(
        a(0).extend(0.0),
        a(1).extend(0.0),
        a(2).extend(0.0),
        Vec3::from(m.origin).extend(1.0),
    )
}

fn upload_lighting(gpu: &Gpu, l: &ModelLighting) -> Tex {
    let (w, h, d) = l.size();
    texture::upload(
        gpu,
        "model lighting",
        SamplerDim::D3,
        [w, h, d],
        1,
        wgpu::TextureFormat::Bgra8Unorm,
        l.texels(),
    )
}
