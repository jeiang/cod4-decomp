// SPDX-License-Identifier: GPL-3.0-or-later
//! One map's GPU data: world mesh, model meshes, the model-lighting volume, and the sun.

use crate::gpu::Gpu;
use crate::lightgrid::{LightingEnv, ModelLighting, SightTrace};
use crate::texture::{self, Tex};
use crate::art::MapArt;
use assets::zone::gfx::{Material, TechniqueSet};
use assets::zone::world::{ComPrimaryLight, LightDef};
use assets::zone::gfxworld::GfxWorld;
use assets::zone::xmodel::XModel;
use assets::zone::{Asset, DecodeFilter, XAssetType, Zone};
use glam::{Mat4, Vec3};
use sim::cm::CollisionWorld;
use sm3::SamplerDim;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub struct Mesh {
    pub vb: wgpu::Buffer,
    pub ib: wgpu::Buffer,
}

/// A decoded map zone: the world, its primary lights, and the post-process materials, ready for [`Scene::new`].
pub struct MapData {
    pub world: Arc<GfxWorld>,
    /// Technique sets of the map zone, `common_mp` and `code_post_gfx_mp`; materials reference sets of other zones by
    /// name.
    pub techsets: Vec<Arc<TechniqueSet>>,
    /// The map's primary lights (`ComWorld`), indexed like `primary_light_index` of surfaces and models.
    pub com_lights: Arc<[ComPrimaryLight]>,
    /// The light definitions the primary lights name (attenuation ramps).
    pub light_defs: Vec<Arc<LightDef>>,
    /// The map's collision data, for the sight tests of the light grid.
    pub clipmap: Option<Arc<assets::zone::clipmap::Clipmap>>,
    /// The full-screen materials of `code_post_gfx_mp` (glow, depth of field, film, shell shock, and the filters).
    pub post_materials: Vec<Arc<Material>>,
    /// Art settings of the map: its fog and the vision file with the glow and film values.
    pub art: MapArt,
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
    /// Decode `zone/english/<map>.ff`, `common_mp.ff` and `code_post_gfx_mp.ff` under the install root.
    pub fn load(install: &Path, map: &str) -> Result<MapData, LoadError> {
        let mut world = None;
        let mut com_world = None;
        let mut clipmap = None;
        let mut techsets = Vec::new();
        let mut light_defs = Vec::new();
        let mut post_materials = Vec::new();
        let mut art_script = None;
        let mut vision_files: HashMap<String, String> = HashMap::new();
        let vision_name = format!("vision/{map}.vision");
        let art_name = format!("maps/createart/{map}_art.gsc");
        for zone in ["code_post_gfx_mp", "common_mp", map] {
            let path = install.join("zone/english").join(format!("{zone}.ff"));
            let file = std::fs::File::open(path).map_err(LoadError::Io)?;
            let z = Zone::open(std::io::BufReader::new(file)).map_err(LoadError::Zone)?;
            let keep = Keep(zone);
            z.decode(&keep, |a| match a {
                Asset::GfxWorld(w) => world = Some(w),
                Asset::ComWorld(c) => com_world = Some(c),
                Asset::Clipmap(c) if zone == map => clipmap = Some(c),
                Asset::LightDef(l) => light_defs.push(l),
                Asset::TechniqueSet(t) => techsets.push(t),
                Asset::Material(m) if zone == "code_post_gfx_mp" => post_materials.push(m),
                Asset::RawFile(r) => {
                    let name = r.name.as_deref().unwrap_or_default();
                    let text = || {
                        let end = r.data.iter().position(|&b| b == 0).unwrap_or(r.data.len());
                        String::from_utf8_lossy(&r.data[..end]).into_owned()
                    };
                    if name == art_name {
                        art_script = Some(text());
                    } else if name == vision_name || name == "vision/default.vision" {
                        vision_files.insert(name.to_owned(), text());
                    }
                }
                _ => {}
            })
            .map_err(LoadError::Zone)?;
        }
        let vision = vision_files
            .remove(&vision_name)
            .or_else(|| vision_files.remove("vision/default.vision"));
        Ok(MapData {
            world: world.ok_or(LoadError::NoWorld)?,
            techsets,
            com_lights: com_world.map(|c| c.primary_lights.clone()).unwrap_or_else(|| Arc::from([])),
            light_defs,
            clipmap,
            post_materials,
            art: MapArt::parse(art_script.as_deref(), vision.as_deref()),
        })
    }
}

/// Decode filter: technique sets from every zone, post-process materials from `code_post_gfx_mp`, the vision and art
/// raw files from `common_mp`, everything the renderer draws from the map zone.
struct Keep<'a>(&'a str);

impl DecodeFilter for Keep<'_> {
    fn keep(&self, ty: XAssetType) -> bool {
        match self.0 {
            "code_post_gfx_mp" => matches!(ty, XAssetType::TechniqueSet | XAssetType::Material),
            "common_mp" => matches!(ty, XAssetType::TechniqueSet | XAssetType::RawFile),
            _ => true,
        }
    }
    fn keep_presentation(&self) -> bool {
        true
    }
}

pub struct Scene {
    pub world: Arc<GfxWorld>,
    pub world_mesh: Arc<Mesh>,
    pub lighting: ModelLighting,
    pub lighting_tex: Arc<Tex>,
    /// The map's collision world, for sight traces.
    pub collision: Option<Arc<CollisionWorld>>,
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
        let collision = data.clipmap.clone().map(|c| Arc::new(CollisionWorld::new(c)));
        let env = LightingEnv {
            sight: collision.as_deref().map(|c| c as &dyn SightTrace),
            lights: &data.com_lights,
        };
        let lighting = ModelLighting::new(&world, 1024, &env);
        let lighting_tex = Arc::new(upload_lighting(gpu, &lighting));
        let (sun_dir, sun_color) = match &world.sun_light {
            Some(l) => (Vec3::from(l.dir), Vec3::from(l.color)),
            None => (
                Vec3::new(0.3, 0.2, 0.9).normalize(),
                Vec3::from(world.sun_color_from_bsp),
            ),
        };
        Scene {
            world,
            world_mesh,
            lighting,
            lighting_tex,
            collision,
            sun_dir,
            sun_color,
            models: HashMap::new(),
        }
    }

    /// GPU buffers of surface `index` of `model`, created on first use.
    pub fn model_mesh(
        &mut self,
        gpu: &Gpu,
        model: &Arc<XModel>,
        index: usize,
    ) -> Option<Arc<Mesh>> {
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
            vb: gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: model.name.as_deref(),
                    contents: &s.verts,
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            ib: gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
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
