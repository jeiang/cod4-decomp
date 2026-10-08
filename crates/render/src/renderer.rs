// SPDX-License-Identifier: GPL-3.0-or-later
//! The frame: cull, build the draw lists, fill constant banks, record the sun shadow pass and the scene pass.

use crate::art::MapArt;
use crate::codeconst::{self, FrameConsts, LightConsts, Object, tex as ctex};
use crate::cull::{self, Frustum};
use crate::dynmesh::{self, DynMesh};
use crate::gpu::Gpu;
use crate::material::{BANK_BYTES, Materials, Prepared, SamplerKey, Target, VertexKind};
use crate::post::{self, PostParams};
use crate::scene::{MapData, Mesh, Scene, model_matrix};
use crate::skin::{self, ModelInstance, ModelKind};
use crate::sunshadow::{self, SunShadow};
use crate::texture::{Tex, TextureCache};
use crate::timing::GpuTimer;
use assets::zone::gfx::Material;
use glam::{Mat4, Vec3, Vec4};
use sm3::SamplerDim;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use wgpu::util::DeviceExt;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SHADOW_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const NEAR: f32 = 4.0;
/// The projection has no far plane worth the name: the sky shader drops the translation part of the matrix and so
/// lands at `z / w = r`, which must stay below one to survive clipping.
const DEPTH_SCALE: f32 = 0.99999;
/// The view model's share of the depth range, in front of everything the world draws.
const VIEWMODEL_DEPTH: f32 = 0.05;
/// Linear filtering, linear mips, clamped.
const CODE_SAMPLER: u8 = 0x72;
const TECH_BUILD_FLOATZ: usize = 1;
const TECH_BUILD_SHADOWMAP_DEPTH: usize = 2;
const TECH_BUILD_SHADOWMAP_COLOR: usize = 3;
const TECH_UNLIT: usize = 4;
const TECH_EMISSIVE: usize = 5;
const TECH_LIT: usize = 7;
const TECH_LIT_SUN: usize = 8;
const TECH_LIT_SUN_SHADOW: usize = 9;
const TECH_LIT_SPOT: usize = 10;
const TECH_LIT_OMNI: usize = 12;
const LIGHT_KIND_OMNI: u8 = 2;
const LIGHT_KIND_SPOT: u8 = 3;
const SUN_SHADOW_TECHS: [usize; 5] = [
    TECH_LIT_SUN_SHADOW,
    TECH_LIT_SUN,
    TECH_LIT,
    TECH_UNLIT,
    TECH_EMISSIVE,
];
const SUN_TECHS: [usize; 4] = [TECH_LIT_SUN, TECH_LIT, TECH_UNLIT, TECH_EMISSIVE];
const SPOT_TECHS: [usize; 4] = [TECH_LIT_SPOT, TECH_LIT, TECH_UNLIT, TECH_EMISSIVE];
const OMNI_TECHS: [usize; 4] = [TECH_LIT_OMNI, TECH_LIT, TECH_UNLIT, TECH_EMISSIVE];
const LIT: [usize; 3] = [TECH_LIT, TECH_UNLIT, TECH_EMISSIVE];
/// Surface flag: casts a shadow into the sun shadow map.
const SURFACE_CASTS_SUN_SHADOW: u8 = 1;

/// A camera. Game space: x forward, y left, z up; angles in radians.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub origin: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Horizontal field of view; the vertical one follows from the aspect ratio (Hor+).
    pub fov_x: f32,
    pub time: f32,
}

impl View {
    pub fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
        )
    }

    /// View and projection matrices. Like the original's, the view is a pure rotation to the left-handed y-up camera
    /// space: positions are made camera-relative by the world matrices (see [`FrameConsts`]).
    pub fn matrices(&self, aspect: f32) -> (Mat4, Mat4) {
        let conv = Mat4::from_cols(
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(-1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::W,
        );
        let dir = conv.transform_vector3(self.forward());
        let f = dir.normalize();
        let s = Vec3::Y.cross(f).normalize();
        let u = f.cross(s);
        let look = Mat4::from_cols(
            Vec4::new(s.x, u.x, f.x, 0.0),
            Vec4::new(s.y, u.y, f.y, 0.0),
            Vec4::new(s.z, u.z, f.z, 0.0),
            Vec4::W,
        );
        let w = 1.0 / (self.fov_x * 0.5).tan();
        let h = w * aspect;
        let r = DEPTH_SCALE;
        let proj = Mat4::from_cols(
            Vec4::new(w, 0.0, 0.0, 0.0),
            Vec4::new(0.0, h, 0.0, 0.0),
            Vec4::new(0.0, 0.0, r, 1.0),
            Vec4::new(0.0, 0.0, -r * NEAR, 0.0),
        );
        (look * conv, proj)
    }

    /// Projection times the view with the eye translation: world space to clip space, for visibility tests.
    pub fn clip_from_world(&self, aspect: f32) -> Mat4 {
        let (v, p) = self.matrices(aspect);
        p * v * Mat4::from_translation(-self.origin)
    }
}

/// How the sun's shadow map is built and read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowMode {
    /// No shadow maps: sun-lit surfaces use the plain lit-sun technique.
    Off,
    /// A depth texture sampled with a comparison sampler: the `hsm` technique sets and `build shadowmap depth`.
    Depth,
    /// Depth encoded in a float colour target and compared by the shader: the `sm` sets and `build shadowmap color`.
    Color,
}

/// What the frame draws.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub shadows: ShadowMode,
    pub fog: bool,
    /// Spot and omni primary lights; off draws their surfaces with the plain lit technique.
    pub primary_lights: bool,
    /// `r_showMissingLightGrid`: a dynamic model (the player's gun, other players) outside the light grid is drawn
    /// in rainbow colours, a level designer's aid. Off, it takes the grid's default lighting.
    pub show_missing_light_grid: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            shadows: ShadowMode::Depth,
            fog: true,
            primary_lights: true,
            show_missing_light_grid: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub cells: usize,
    pub surfaces: usize,
    pub models: usize,
    pub draws: usize,
    pub shadow_draws: usize,
    pub pipelines_missing: usize,
    pub cpu_ms: f64,
    /// GPU time of the most recent frame whose timestamps have come back (a few frames behind), if the device can
    /// time passes.
    pub gpu_ms: Option<f64>,
    /// This frame's number for [`Renderer::take_gpu_times`]; 0 when the device cannot time passes.
    pub frame: u64,
}

struct Draw {
    prepared: Rc<Prepared>,
    pipeline: Arc<wgpu::RenderPipeline>,
    tex_bg: Arc<wgpu::BindGroup>,
    mesh: Arc<Mesh>,
    vs: u32,
    ps: u32,
    first_index: u32,
    count: u32,
    base_vertex: i32,
    /// Byte range of the frame's skinned vertex buffer, for a dynamic model.
    vb: Option<(u64, u64)>,
    /// Byte offset into the mesh's vertex buffer, where the device cannot draw with a base vertex.
    vb_offset: u64,
    sky: bool,
    order: (bool, u8, u32),
}

/// A spot or omni primary light, ready for the constant banks.
struct PrimaryLight {
    kind: u8,
    consts: LightConsts,
    attenuation: Option<(Arc<Tex>, u8)>,
}

/// The sun shadow map and its stand-ins for frames without one.
struct ShadowTargets {
    mode: ShadowMode,
    /// Depth of the map; in depth mode this is the texture the shaders compare against.
    depth: wgpu::TextureView,
    /// Colour-encoded depth, in colour mode.
    color: Option<wgpu::TextureView>,
    /// What the shaders sample.
    tex: Arc<Tex>,
}

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
struct TexKey {
    prep: u32,
    lightmap: u8,
    probe: u8,
    light: u8,
}

/// Which list of draws a pass builds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PassKind {
    Scene,
    SunShadow,
    /// The scene's view-space depth for the post chain's depth of field.
    FloatZ,
}

/// Visible geometry to draw.
struct Items<'a> {
    surfaces: &'a [u32],
    smodels: &'a [u32],
}

/// One skinned surface of a dynamic model, skinned into the frame's vertex buffer.
struct DynSurf {
    inst: usize,
    surf: usize,
    vb: (u64, u64),
    light: u8,
    obj: Object,
}

/// A run of a [`DynMesh`]'s vertices, written into the frame's dynamic vertex buffer.
struct MeshDraw {
    mesh: usize,
    vb: (u64, u64),
    count: u32,
    light: u8,
    obj: Object,
}

/// Object-independent banks by (prepared pass, light): the vertex and the pixel bank offsets.
type SharedBanks = HashMap<(u32, u8), (Option<u32>, Option<u32>)>;

#[derive(Default)]
struct BuildCounts {
    models: usize,
    missing: usize,
}

pub struct Renderer {
    pub gpu: Arc<Gpu>,
    pub scene: Scene,
    pub materials: Materials,
    pub textures: TextureCache,
    ring: Vec<[f32; 4]>,
    ring_buf: wgpu::Buffer,
    ring_cap: usize,
    vs_bg: wgpu::BindGroup,
    ps_bg: wgpu::BindGroup,
    bools: wgpu::Buffer,
    depth: Option<(wgpu::TextureView, (u32, u32))>,
    tex_bgs: HashMap<TexKey, Arc<wgpu::BindGroup>>,
    pub clear: [f64; 3],
    pub settings: Settings,
    /// Fog, glow and film of the map.
    pub art: MapArt,
    lights: Vec<Option<PrimaryLight>>,
    shadow: Option<ShadowTargets>,
    /// Stand-in shadow textures for passes that bind a shadow slot while the map is off.
    shadow_dummy: (Arc<Tex>, Arc<Tex>),
    pub timer: Option<GpuTimer>,
    /// What the post chain does this frame; starts as the map's own glow and film.
    pub post: PostParams,
    pub(crate) post_state: post::State,
    /// Skinned models to draw in the next [`Renderer::render`]; the caller refills the list every frame.
    pub dynamic_models: Vec<ModelInstance>,
    /// Sprites and decals to draw in the next [`Renderer::render`], in drawing order within a material.
    pub dynamic_meshes: Vec<DynMesh>,
    /// An index buffer that counts up from zero, for draws of unindexed dynamic triangles.
    count_mesh: Arc<Mesh>,
    /// Horizontal field of view of the view model; `None` uses the view's.
    pub viewmodel_fov_x: Option<f32>,
    dyn_vb: wgpu::Buffer,
    dyn_cap: u64,
    dyn_bytes: Vec<u8>,
}

impl Renderer {
    pub fn new(
        gpu: Arc<Gpu>,
        scene: Scene,
        data: &MapData,
        mut textures: TextureCache,
    ) -> Renderer {
        let mut materials = Materials::new(&gpu);
        materials.add_techsets(&data.techsets);
        let ring_cap = 1 << 16;
        let ring_buf = new_ring(&gpu, ring_cap);
        let bools = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bool constants"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        let (vs_bg, ps_bg) = ring_groups(&gpu, &materials, &ring_buf, &bools);
        let lights = data
            .com_lights
            .iter()
            .map(|l| {
                if l.kind != LIGHT_KIND_OMNI && l.kind != LIGHT_KIND_SPOT || l.radius <= 0.0 {
                    return None;
                }
                let def = l.def_name.as_deref().and_then(|n| {
                    data.light_defs
                        .iter()
                        .find(|d| d.name.as_deref() == Some(n))
                });
                let attenuation = def.and_then(|d| {
                    let img = d.attenuation_image.as_ref()?;
                    let tex = textures.image(&gpu, img)?;
                    Some((tex, d.attenuation_sampler_state))
                });
                let width = attenuation.as_ref().map_or(32.0, |(t, _)| t.width as f32);
                let (scale, bias) = if l.kind == LIGHT_KIND_SPOT {
                    let s = 1.0 / (l.cos_half_fov_inner - l.cos_half_fov_outer).max(1e-4);
                    (s, -s * l.cos_half_fov_outer)
                } else {
                    (0.0, 0.0)
                };
                Some(PrimaryLight {
                    kind: l.kind,
                    attenuation,
                    consts: LightConsts {
                        origin: Vec3::from(l.origin),
                        dir: Vec3::from(l.dir),
                        color: Vec3::from(l.color),
                        radius: l.radius,
                        spot_factors: [scale, bias, f32::from(l.exponent), 0.0],
                        falloff_placement: [
                            width * (1.0 / 512.0),
                            0.0,
                            def.map_or(0.0, |d| d.lmap_lookup_start as f32 * (1.0 / 512.0)),
                            0.0,
                        ],
                    },
                })
            })
            .collect();
        let gpu_for_dyn = gpu.clone();
        let shadow_dummy = dummy_shadow(&gpu);
        let timer = GpuTimer::new(&gpu);
        let post_state = post::State::new(&gpu, data);
        let mut r = Renderer {
            gpu,
            scene,
            materials,
            textures,
            ring: Vec::new(),
            ring_buf,
            ring_cap,
            vs_bg,
            ps_bg,
            bools,
            depth: None,
            tex_bgs: HashMap::new(),
            clear: [0.45, 0.43, 0.38],
            settings: Settings::default(),
            art: data.art.clone(),
            post: PostParams::from_art(&data.art),
            post_state,
            lights,
            shadow: None,
            shadow_dummy,
            timer,
            dynamic_models: Vec::new(),
            dynamic_meshes: Vec::new(),
            count_mesh: Arc::new(counting_mesh(&gpu_for_dyn)),
            viewmodel_fov_x: None,
            dyn_vb: new_dyn_vb(&gpu_for_dyn, 1 << 20),
            dyn_cap: 1 << 20,
            dyn_bytes: Vec::new(),
        };
        r.prewarm();
        r
    }

    /// GPU times of frames that finished since the last call: `(FrameStats::frame, milliseconds)`. Results trail the
    /// frame by a few frames.
    pub fn take_gpu_times(&mut self) -> Vec<(u64, f64)> {
        self.timer
            .as_mut()
            .map_or_else(Vec::new, GpuTimer::take_finished)
    }

    /// Wait for the frames in flight and return their GPU times, as [`Renderer::take_gpu_times`].
    pub fn flush_gpu_times(&mut self) -> Vec<(u64, f64)> {
        match self.timer.as_mut() {
            Some(t) => {
                t.flush(&self.gpu);
                t.take_finished()
            }
            None => Vec::new(),
        }
    }

    fn hsm(&self) -> bool {
        self.settings.shadows == ShadowMode::Depth
    }

    /// Translate and prepare every technique the map's surfaces can use, so frames do not hitch on first sight.
    fn prewarm(&mut self) {
        let hsm = self.hsm();
        for m in self.scene.materials() {
            for kind in [VertexKind::World, VertexKind::Model] {
                for techs in [
                    &SUN_SHADOW_TECHS[..],
                    &SUN_TECHS,
                    &LIT,
                    &SPOT_TECHS,
                    &OMNI_TECHS,
                ] {
                    self.prepare(&m, techs, kind, hsm);
                }
                self.prepare(&m, &[self.shadow_tech()], kind, hsm);
            }
        }
    }

    fn shadow_tech(&self) -> usize {
        if self.settings.shadows == ShadowMode::Color {
            TECH_BUILD_SHADOWMAP_COLOR
        } else {
            TECH_BUILD_SHADOWMAP_DEPTH
        }
    }

    /// Build the pipelines for rendering into `format`, again to keep them off the frame path. Returns how many.
    pub fn warm(&mut self, format: wgpu::TextureFormat) -> usize {
        let hsm = self.hsm();
        let scene = Target {
            color: Some(format),
            depth: Some(DEPTH_FORMAT),
        };
        let mut n = 0;
        for m in self.scene.materials() {
            for kind in [VertexKind::World, VertexKind::Model] {
                for techs in [
                    &SUN_SHADOW_TECHS[..],
                    &SUN_TECHS,
                    &LIT,
                    &SPOT_TECHS,
                    &OMNI_TECHS,
                ] {
                    if let Some(p) = self.prepare(&m, techs, kind, hsm) {
                        self.materials.pipeline(&self.gpu, &p, scene);
                        n += 1;
                    }
                }
                if self.settings.shadows != ShadowMode::Off
                    && let Some(p) = self.prepare(&m, &[self.shadow_tech()], kind, hsm)
                {
                    self.materials.pipeline(&self.gpu, &p, self.shadow_target());
                    n += 1;
                }
            }
        }
        n
    }

    fn shadow_target(&self) -> Target {
        Target {
            color: (self.settings.shadows == ShadowMode::Color).then_some(SHADOW_COLOR_FORMAT),
            depth: Some(DEPTH_FORMAT),
        }
    }

    /// The prepared pass for `techs` of `m`, for debugging tools.
    pub fn inspect(
        &mut self,
        m: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
    ) -> Option<Rc<Prepared>> {
        let hsm = self.hsm();
        self.prepare(m, techs, kind, hsm)
    }

    fn prepare(
        &mut self,
        m: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
        hsm: bool,
    ) -> Option<Rc<Prepared>> {
        self.materials
            .prepare(&self.gpu, &mut self.textures, m, techs, kind, hsm)
    }

    fn ensure_depth(&mut self, size: (u32, u32)) {
        if self.depth.as_ref().is_none_or(|d| d.1 != size) {
            let t = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some((t.create_view(&Default::default()), size));
        }
    }

    /// The sun shadow map for the current mode, created on first use.
    fn ensure_shadow(&mut self) {
        let mode = self.settings.shadows;
        if mode == ShadowMode::Off || self.shadow.as_ref().is_some_and(|s| s.mode == mode) {
            return;
        }
        let dev = &self.gpu.device;
        let size = wgpu::Extent3d {
            width: sunshadow::SIZE,
            height: sunshadow::HEIGHT,
            depth_or_array_layers: 1,
        };
        let make = |label, format, usage| {
            dev.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let bind = wgpu::TextureUsages::TEXTURE_BINDING;
        let (depth, color, sampled) = if mode == ShadowMode::Depth {
            let t = make("sun shadow depth", DEPTH_FORMAT, attach | bind);
            let v = t.create_view(&Default::default());
            (v.clone(), None, v)
        } else {
            let d = make("sun shadow z", DEPTH_FORMAT, attach);
            let c = make("sun shadow color", SHADOW_COLOR_FORMAT, attach | bind);
            let cv = c.create_view(&Default::default());
            (d.create_view(&Default::default()), Some(cv.clone()), cv)
        };
        self.shadow = Some(ShadowTargets {
            mode,
            depth,
            color,
            tex: Arc::new(Tex {
                view: sampled,
                dim: SamplerDim::D2,
                width: sunshadow::SIZE,
            }),
        });
        self.tex_bgs.clear();
    }

    /// Code textures `p` samples, resolved for a surface with lightmap `lm`, reflection probe `probe` and primary
    /// light `light`.
    fn tex_group(
        &mut self,
        p: &Rc<Prepared>,
        lm: u8,
        probe: u8,
        light: u8,
    ) -> Arc<wgpu::BindGroup> {
        let key = TexKey {
            prep: p.id(),
            lightmap: lm,
            probe,
            light,
        };
        if let Some(g) = self.tex_bgs.get(&key) {
            return g.clone();
        }
        let world = self.scene.world.clone();
        let mut codes: HashMap<u32, (Arc<Tex>, SamplerKey)> = HashMap::new();
        for id in p.code_textures() {
            let image = match id {
                ctex::LIGHTMAP_PRIMARY => world
                    .lightmaps
                    .get(usize::from(lm))
                    .and_then(|l| l.primary.clone()),
                ctex::LIGHTMAP_SECONDARY => world
                    .lightmaps
                    .get(usize::from(lm))
                    .and_then(|l| l.secondary.clone()),
                ctex::REFLECTION_PROBE => world
                    .reflection_probes
                    .get(usize::from(probe))
                    .and_then(|r| r.image.clone()),
                ctex::SKY => world.sky_image.clone(),
                ctex::OUTDOOR => world.outdoor_image.clone(),
                _ => None,
            };
            let entry = match (id, image) {
                (ctex::MODEL_LIGHTING, _) => {
                    Some((self.scene.lighting_tex.clone(), SamplerKey::State(0xE2)))
                }
                (ctex::SHADOWMAP_SUN | ctex::SHADOWMAP_SPOT, _) => {
                    let compare = p.slots.iter().any(|s| {
                        matches!(s.source, crate::material::TexSource::Code(c) if c == id)
                            && s.fetch == crate::material::Fetch::Compare
                    });
                    let real = self.shadow.as_ref().filter(|s| {
                        id == ctex::SHADOWMAP_SUN && (s.mode == ShadowMode::Depth) == compare
                    });
                    Some(match real {
                        Some(s) => (s.tex.clone(), shadow_sampler(compare)),
                        None => {
                            let d = if compare {
                                self.shadow_dummy.0.clone()
                            } else {
                                self.shadow_dummy.1.clone()
                            };
                            (d, shadow_sampler(compare))
                        }
                    })
                }
                (ctex::LIGHT_ATTENUATION, _) => self
                    .lights
                    .get(usize::from(light))
                    .and_then(Option::as_ref)
                    .and_then(|l| l.attenuation.clone())
                    .map(|(t, st)| (t, SamplerKey::State(st))),
                (ctex::WHITE, _) => Some((
                    self.textures.solid(&self.gpu, SamplerDim::D2, [255; 4]),
                    CODE_SAMPLER.into(),
                )),
                (ctex::BLACK, _) => Some((
                    self.textures
                        .solid(&self.gpu, SamplerDim::D2, [0, 0, 0, 255]),
                    CODE_SAMPLER.into(),
                )),
                (ctex::IDENTITY_NORMAL_MAP, _) => Some((
                    self.textures
                        .solid(&self.gpu, SamplerDim::D2, [128, 128, 255, 255]),
                    CODE_SAMPLER.into(),
                )),
                (ctex::SKY, Some(i)) => self
                    .textures
                    .image(&self.gpu, &i)
                    .map(|t| (t, world.sky_sampler_state.into())),
                (_, Some(i)) => self
                    .textures
                    .image(&self.gpu, &i)
                    .map(|t| (t, CODE_SAMPLER.into())),
                _ => None,
            };
            if let Some(e) = entry {
                codes.insert(id, e);
            }
        }
        let bg = Arc::new(
            self.materials
                .bind_textures(&self.gpu, &mut self.textures, p, &|id| {
                    codes.get(&id).cloned()
                }),
        );
        self.tex_bgs.insert(key, bg.clone());
        bg
    }

    /// Room for `regs` registers in the constant ring: the byte offset of the bank and the registers to fill.
    pub(crate) fn alloc(&mut self, regs: usize) -> (u32, &mut [[f32; 4]]) {
        let at = self.ring.len();
        self.ring.resize(at + regs, [0.0; 4]);
        ((at * 16) as u32, &mut self.ring[at..])
    }

    /// The techniques of a surface or model lit by primary light `light`, best first.
    fn scene_techs(&self, light: u8, sun_shadows: bool) -> &'static [usize] {
        if u32::from(light) == self.scene.world.sun_primary_light_index {
            return if sun_shadows {
                &SUN_SHADOW_TECHS
            } else {
                &SUN_TECHS
            };
        }
        match self.lights.get(usize::from(light)).and_then(Option::as_ref) {
            Some(l) if self.settings.primary_lights && l.kind == LIGHT_KIND_SPOT => &SPOT_TECHS,
            Some(l) if self.settings.primary_lights && l.kind == LIGHT_KIND_OMNI => &OMNI_TECHS,
            _ => &LIT,
        }
    }

    /// Build the draws of one pass.
    #[allow(clippy::too_many_arguments)]
    fn build_draws(
        &mut self,
        kind: PassKind,
        frame: &FrameConsts,
        light_frames: &HashMap<u8, FrameConsts>,
        items: &Items,
        target: Target,
        sun_shadows: bool,
        counts: &mut BuildCounts,
        eye: Vec3,
    ) -> Vec<Draw> {
        let hsm = self.hsm();
        let shadow_tech = [self.shadow_tech()];
        let floatz_tech = [TECH_BUILD_FLOATZ];
        let mut draws: Vec<Draw> = Vec::new();
        // Banks that do not depend on the object are shared by every draw with the same pass and light.
        let mut shared = SharedBanks::new();
        let world = self.scene.world.clone();

        for &si in items.surfaces {
            let Some(surf) = world.dpvs.surfaces.get(si as usize) else {
                continue;
            };
            let Some(mat) = &surf.material else { continue };
            let light = if kind == PassKind::Scene {
                surf.primary_light_index
            } else {
                0
            };
            let techs: &[usize] = match kind {
                PassKind::Scene => self.scene_techs(light, sun_shadows),
                PassKind::SunShadow => &shadow_tech,
                PassKind::FloatZ => &floatz_tech,
            };
            let Some(prep) = self.prepare(mat, techs, VertexKind::World, hsm) else {
                if kind == PassKind::Scene {
                    counts.missing += 1;
                }
                continue;
            };
            let fc = light_frames.get(&light).unwrap_or(frame);
            let (vs, ps) = self.banks(&prep, fc, &Object::default(), &mut shared, light);
            let pipeline = self.materials.pipeline(&self.gpu, &prep, target);
            let tex_bg = self.tex_group(
                &prep,
                surf.lightmap_index,
                surf.reflection_probe_index,
                light,
            );
            draws.push(Draw {
                sky: kind == PassKind::Scene && is_sky(mat),
                order: (false, mat.sort_key, si),
                prepared: prep,
                pipeline,
                tex_bg,
                mesh: self.scene.world_mesh.clone(),
                vs,
                ps,
                first_index: surf.base_index as u32,
                count: u32::from(surf.tri_count) * 3,
                base_vertex: if self.gpu.base_vertex {
                    surf.first_vertex
                } else {
                    0
                },
                vb: None,
                vb_offset: if self.gpu.base_vertex {
                    0
                } else {
                    surf.first_vertex as u64 * VertexKind::World.stride()
                },
            });
        }

        for &mi in items.smodels {
            let Some(inst) = world.dpvs.smodel_draw_insts.get(mi as usize) else {
                continue;
            };
            let Some(model) = inst.model.clone() else {
                continue;
            };
            let origin = Vec3::from(inst.origin);
            let dist = origin.distance(eye);
            if inst.cull_dist > 0.0 && dist > inst.cull_dist {
                continue;
            }
            let lod = pick_lod(&model, dist);
            let info = &model.lod_info[lod];
            let obj = Object {
                world: model_matrix(inst),
                base_lighting: self
                    .scene
                    .lighting
                    .base_coords(self.scene.lighting.static_model_handle(mi as usize)),
            };
            let light = if kind == PassKind::Scene {
                inst.primary_light_index
            } else {
                0
            };
            let techs: &[usize] = match kind {
                PassKind::Scene => self.scene_techs(light, sun_shadows),
                PassKind::SunShadow => &shadow_tech,
                PassKind::FloatZ => &floatz_tech,
            };
            let fc = light_frames.get(&light).unwrap_or(frame);
            let mut counted = false;
            for s in 0..usize::from(info.surf_count) {
                let idx = usize::from(info.surf_index) + s;
                let Some(Some(mat)) = model.materials.get(idx) else {
                    continue;
                };
                let Some(prep) = self.prepare(mat, techs, VertexKind::Model, hsm) else {
                    continue;
                };
                let Some(mesh) = self.scene.model_mesh(&self.gpu, &model, idx) else {
                    continue;
                };
                let (vs, ps) = self.banks(&prep, fc, &obj, &mut shared, light);
                let pipeline = self.materials.pipeline(&self.gpu, &prep, target);
                let tex_bg = self.tex_group(&prep, 0, inst.reflection_probe_index, light);
                let tris = u32::from(model.surfs[idx].tri_count) * 3;
                draws.push(Draw {
                    sky: false,
                    order: (false, mat.sort_key, 1 << 31 | mi),
                    prepared: prep,
                    pipeline,
                    tex_bg,
                    mesh,
                    vs,
                    ps,
                    first_index: 0,
                    count: tris,
                    base_vertex: 0,
                    vb: None,
                    vb_offset: 0,
                });
                counted = true;
            }
            counts.models += usize::from(counted);
        }
        draws.sort_by_key(|d| (!d.sky, d.order, d.prepared.id()));
        draws
    }

    /// The reflection probe closest to `p`: a model that moves has no baked probe index.
    fn nearest_probe(&self, p: [f32; 3]) -> u8 {
        let d = |o: &[f32; 3]| (0..3).map(|k| (o[k] - p[k]).powi(2)).sum::<f32>();
        self.scene
            .world
            .reflection_probes
            .iter()
            .enumerate()
            .min_by(|a, b| d(&a.1.origin).total_cmp(&d(&b.1.origin)))
            .map_or(0, |(i, _)| i as u8)
    }

    /// Skins every dynamic model's surfaces into the frame's vertex buffer and lights the models.
    fn prepare_dynamic(
        &mut self,
        insts: &[ModelInstance],
        meshes: &[DynMesh],
        eye: Vec3,
    ) -> (Vec<DynSurf>, Vec<MeshDraw>) {
        self.scene.begin_dynamic_lighting();
        self.dyn_bytes.clear();
        let mut out = Vec::new();
        for (ii, inst) in insts.iter().enumerate() {
            let model = &inst.model;
            let dist = Vec3::from(inst.origin).distance(eye);
            let lod = inst.lod.unwrap_or_else(|| pick_lod(model, dist)).min(3);
            let info = &model.lod_info[lod];
            let mats = skin::skin_matrices(model, &inst.bones);
            let (base_lighting, light) = match self
                .scene
                .light_point(inst.light_origin, self.settings.show_missing_light_grid)
            {
                Some((h, l)) => (self.scene.lighting.base_coords(h), l),
                None => (
                    Object::default().base_lighting,
                    self.scene.world.sun_primary_light_index as u8,
                ),
            };
            let obj = Object {
                world: inst.world_matrix(),
                base_lighting,
            };
            for s in 0..usize::from(info.surf_count) {
                let idx = usize::from(info.surf_index) + s;
                let Some(surf) = model.surfs.get(idx) else {
                    continue;
                };
                if surf.verts.is_empty()
                    || surf.tri_indices.is_empty()
                    || skin::is_hidden(surf, &inst.hidden_parts)
                {
                    continue;
                }
                let at = self.dyn_bytes.len() as u64;
                skin::skin_surface(surf, &mats, &mut self.dyn_bytes);
                if !skin::check_skinned(&self.dyn_bytes[at as usize..], model.radius) {
                    eprintln!(
                        "skinning fault: surface {idx} of {} left the model",
                        model.name.as_deref().unwrap_or("?")
                    );
                    // Leave the surface out rather than stretch triangles across the screen.
                    self.dyn_bytes.truncate(at as usize);
                    continue;
                }
                out.push(DynSurf {
                    inst: ii,
                    surf: idx,
                    vb: (at, self.dyn_bytes.len() as u64 - at),
                    light,
                    obj,
                });
            }
        }
        let mut mesh_draws = Vec::new();
        for (mi, m) in meshes.iter().enumerate() {
            let show_missing = self.settings.show_missing_light_grid;
            let (base_lighting, light) = match m
                .light_origin
                .and_then(|o| self.scene.light_point(o, show_missing))
            {
                Some((h, l)) => (self.scene.lighting.base_coords(h), l),
                None => (
                    Object::default().base_lighting,
                    self.scene.world.sun_primary_light_index as u8,
                ),
            };
            for run in m.verts.chunks(dynmesh::MAX_DRAW_VERTS) {
                let at = self.dyn_bytes.len() as u64;
                for v in run {
                    dynmesh::pack(v, &mut self.dyn_bytes);
                }
                mesh_draws.push(MeshDraw {
                    mesh: mi,
                    vb: (at, self.dyn_bytes.len() as u64 - at),
                    count: run.len() as u32 / 3 * 3,
                    light,
                    obj: Object {
                        world: Mat4::IDENTITY,
                        base_lighting,
                    },
                });
            }
        }
        self.scene.upload_dynamic_lighting(&self.gpu);
        let need = self.dyn_bytes.len() as u64;
        if need > self.dyn_cap {
            self.dyn_cap = need.next_power_of_two();
            self.dyn_vb = new_dyn_vb(&self.gpu, self.dyn_cap);
        }
        if need > 0 {
            self.gpu
                .queue
                .write_buffer(&self.dyn_vb, 0, &self.dyn_bytes);
        }
        (out, mesh_draws)
    }

    /// Draws of the dynamic meshes for the scene pass.
    fn build_meshes(
        &mut self,
        frame: &FrameConsts,
        light_frames: &HashMap<u8, FrameConsts>,
        meshes: &[DynMesh],
        runs: &[MeshDraw],
        target: Target,
        sun_shadows: bool,
    ) -> Vec<Draw> {
        let hsm = self.hsm();
        let mut shared = SharedBanks::new();
        let mut draws = Vec::new();
        for (n, d) in runs.iter().enumerate() {
            let mat = &meshes[d.mesh].material;
            let techs = self.scene_techs(d.light, sun_shadows);
            let Some(prep) = self.prepare(mat, techs, VertexKind::Model, hsm) else {
                continue;
            };
            let fc = light_frames.get(&d.light).unwrap_or(frame);
            let (vs, ps) = self.banks(&prep, fc, &d.obj, &mut shared, d.light);
            let pipeline = self.materials.pipeline(&self.gpu, &prep, target);
            let probe = meshes[d.mesh]
                .light_origin
                .map_or(0, |o| self.nearest_probe(o));
            let tex_bg = self.tex_group(&prep, 0, probe, d.light);
            draws.push(Draw {
                sky: false,
                order: (false, mat.sort_key, 1 << 30 | n as u32),
                prepared: prep,
                pipeline,
                tex_bg,
                mesh: self.count_mesh.clone(),
                vs,
                ps,
                first_index: 0,
                count: d.count,
                base_vertex: 0,
                vb: Some(d.vb),
                vb_offset: 0,
            });
        }
        draws
    }

    /// Draws of the dynamic models of `want` for one pass.
    #[allow(clippy::too_many_arguments)]
    fn build_dynamic(
        &mut self,
        kind: PassKind,
        frame: &FrameConsts,
        light_frames: &HashMap<u8, FrameConsts>,
        insts: &[ModelInstance],
        surfs: &[DynSurf],
        want: ModelKind,
        target: Target,
        sun_shadows: bool,
    ) -> Vec<Draw> {
        let hsm = self.hsm();
        let shadow_tech = [self.shadow_tech()];
        let floatz_tech = [TECH_BUILD_FLOATZ];
        let mut shared = SharedBanks::new();
        let mut draws = Vec::new();
        for (n, d) in surfs.iter().enumerate() {
            let inst = &insts[d.inst];
            if inst.kind != want {
                continue;
            }
            let Some(Some(mat)) = inst.model.materials.get(d.surf) else {
                continue;
            };
            let light = if kind == PassKind::Scene { d.light } else { 0 };
            let techs: &[usize] = match kind {
                PassKind::Scene => self.scene_techs(light, sun_shadows && want == ModelKind::World),
                PassKind::SunShadow => &shadow_tech,
                PassKind::FloatZ => &floatz_tech,
            };
            let Some(prep) = self.prepare(mat, techs, VertexKind::Model, hsm) else {
                continue;
            };
            let Some(mesh) = self.scene.model_mesh(&self.gpu, &inst.model, d.surf) else {
                continue;
            };
            let fc = light_frames.get(&light).unwrap_or(frame);
            let (vs, ps) = self.banks(&prep, fc, &d.obj, &mut shared, light);
            let pipeline = self.materials.pipeline(&self.gpu, &prep, target);
            let probe = self.nearest_probe(inst.light_origin);
            let tex_bg = self.tex_group(&prep, 0, probe, light);
            draws.push(Draw {
                sky: false,
                order: (false, mat.sort_key, 1 << 30 | n as u32),
                prepared: prep,
                pipeline,
                tex_bg,
                mesh,
                vs,
                ps,
                first_index: 0,
                count: u32::from(inst.model.surfs[d.surf].tri_count) * 3,
                base_vertex: 0,
                vb: Some(d.vb),
                vb_offset: 0,
            });
        }
        draws
    }

    /// Constant banks of one draw: object-independent banks are filled once per pass and light.
    fn banks(
        &mut self,
        prep: &Rc<Prepared>,
        frame: &FrameConsts,
        obj: &Object,
        shared: &mut SharedBanks,
        light: u8,
    ) -> (u32, u32) {
        let key = (prep.id(), light);
        let cached = shared.get(&key).copied().unwrap_or_default();
        let vs = match cached.0 {
            Some(o) if !prep.vs_per_object() => o,
            _ => {
                let (o, b) = self.alloc(prep.vs_regs());
                prep.fill_vs(b, frame, obj);
                o
            }
        };
        let ps = match cached.1 {
            Some(o) if !prep.ps_per_object() => o,
            _ => {
                let (o, b) = self.alloc(prep.ps_regs());
                prep.fill_ps(b, frame, obj);
                o
            }
        };
        shared.insert(key, (Some(vs), Some(ps)));
        (vs, ps)
    }

    /// Draw the world seen from `view` into `target`.
    pub fn render(
        &mut self,
        view: &View,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> FrameStats {
        let t0 = web_time::Instant::now();
        self.ensure_depth(size);
        self.ensure_shadow();
        self.ring.clear();
        if let Some(t) = self.timer.as_mut() {
            t.begin_frame(&self.gpu);
        }
        let aspect = size.0 as f32 / size.1 as f32;
        let (v, p) = view.matrices(aspect);
        let mut frame = FrameConsts::new(v, p, view.origin);
        frame.set_sun(self.scene.sun_dir, self.scene.sun_color, 1.0);
        frame.set_fog(if self.settings.fog {
            self.art.fog.as_ref()
        } else {
            None
        });
        if let Some(f) = self.art.fog.as_ref().filter(|_| self.settings.fog) {
            self.clear = f.color.map(f64::from);
        }
        frame.vec[codeconst::GAMETIME as usize] = [view.time; 4];
        frame.vec[codeconst::ZNEAR as usize] = [NEAR * 0.984_375, 0.0, 0.0, 0.0];
        frame.vec[codeconst::RENDER_TARGET_SIZE as usize] = [
            size.0 as f32,
            size.1 as f32,
            1.0 / size.0 as f32,
            1.0 / size.1 as f32,
        ];
        let lookup = self.scene.lighting.lookup_scale();
        frame.vec[codeconst::LIGHTING_LOOKUP_SCALE as usize] = lookup;
        frame.outdoor_lookup = Mat4::from_cols_array(&self.scene.world.outdoor_lookup_matrix);

        let world = self.scene.world.clone();
        let frustum = Frustum::from_clip(&view.clip_from_world(aspect));
        let vis = cull::visible(&world, view.origin, &frustum);
        let mut stats = FrameStats {
            cells: vis.cells,
            surfaces: vis.surfaces.len(),
            ..Default::default()
        };
        let mut counts = BuildCounts::default();
        let insts = std::mem::take(&mut self.dynamic_models);
        let meshes = std::mem::take(&mut self.dynamic_meshes);
        let (dynsurfs, mesh_runs) = self.prepare_dynamic(&insts, &meshes, view.origin);

        // The sun shadow map.
        let has_sun = world.sun_light.is_some() && self.shadow.is_some();
        let sun = has_sun.then(|| {
            let fwd = view.forward();
            let t = (view.fov_x * 0.5).tan();
            let cam = sunshadow::Camera {
                eye: view.origin,
                forward: fwd,
                tan_half: [t, t / aspect],
                z_near: NEAR,
            };
            SunShadow::new(
                &cam,
                self.scene.sun_dir,
                Vec3::from(world.mins),
                Vec3::from(world.maxs),
            )
        });
        let mut shadow_lists: Vec<Vec<Draw>> = Vec::new();
        if let Some(ss) = &sun {
            frame.shadow_lookup = ss.lookup;
            frame.vec[codeconst::SHADOWMAP_SWITCH_PARTITION as usize] = ss.switch_partition;
            frame.vec[codeconst::SHADOWMAP_SCALE as usize] = ss.scale;
            let color = self.settings.shadows == ShadowMode::Color;
            let target = self.shadow_target();
            for part in &ss.partitions {
                let mut pf = FrameConsts::new(
                    part.view_proj * Mat4::from_translation(view.origin),
                    Mat4::IDENTITY,
                    view.origin,
                );
                pf.vec[codeconst::SHADOWMAP_POLYGON_OFFSET as usize] = if color {
                    part.offset_color
                } else {
                    part.offset_depth
                };
                let (surfaces, smodels) =
                    shadow_casters(&world, &Frustum::from_clip(&part.view_proj));
                let items = Items {
                    surfaces: &surfaces,
                    smodels: &smodels,
                };
                let mut list = self.build_draws(
                    PassKind::SunShadow,
                    &pf,
                    &HashMap::new(),
                    &items,
                    target,
                    false,
                    &mut counts,
                    view.origin,
                );
                list.extend(self.build_dynamic(
                    PassKind::SunShadow,
                    &pf,
                    &HashMap::new(),
                    &insts,
                    &dynsurfs,
                    ModelKind::World,
                    target,
                    false,
                ));
                stats.shadow_draws += list.len();
                shadow_lists.push(list);
            }
        }

        // Frames of the lights that have spot or omni constants.
        let mut light_frames: HashMap<u8, FrameConsts> = HashMap::new();
        if self.settings.primary_lights {
            for (i, l) in self.lights.iter().enumerate() {
                if let Some(l) = l {
                    let mut f = frame.clone();
                    f.set_light(&l.consts);
                    light_frames.insert(i as u8, f);
                }
            }
        }
        let items = Items {
            surfaces: &vis.surfaces,
            smodels: &vis.smodels,
        };
        let scene_target = Target {
            color: Some(format),
            depth: Some(DEPTH_FORMAT),
        };
        let mut draws = self.build_draws(
            PassKind::Scene,
            &frame,
            &light_frames,
            &items,
            scene_target,
            sun.is_some(),
            &mut counts,
            view.origin,
        );
        draws.extend(self.build_dynamic(
            PassKind::Scene,
            &frame,
            &light_frames,
            &insts,
            &dynsurfs,
            ModelKind::World,
            scene_target,
            sun.is_some(),
        ));
        draws.extend(self.build_meshes(
            &frame,
            &light_frames,
            &meshes,
            &mesh_runs,
            scene_target,
            sun.is_some(),
        ));
        draws.sort_by_key(|d| (!d.sky, d.order, d.prepared.id()));
        // The view model has its own projection; its depth range is squeezed in front of the world's.
        let vm_fov = self.viewmodel_fov_x.unwrap_or(view.fov_x);
        let vm_proj = View {
            fov_x: vm_fov,
            ..*view
        }
        .matrices(aspect)
        .1;
        let with_proj = |f: &FrameConsts| {
            let mut f = f.clone();
            f.proj = vm_proj;
            f
        };
        let vm_frame = with_proj(&frame);
        let vm_lights: HashMap<u8, FrameConsts> = light_frames
            .iter()
            .map(|(k, f)| (*k, with_proj(f)))
            .collect();
        let vm_draws = self.build_dynamic(
            PassKind::Scene,
            &vm_frame,
            &vm_lights,
            &insts,
            &dynsurfs,
            ModelKind::ViewModel,
            scene_target,
            false,
        );
        stats.models += insts.len();
        stats.models = counts.models;
        stats.pipelines_missing = counts.missing;
        stats.draws = draws.len();

        // The post chain: depth of field wants the scene's depth as a second list of the same surfaces.
        let floatz = self.post.dof_active().then(|| {
            let mut zf = frame.clone();
            zf.vec[codeconst::DEPTH_FROM_CLIP as usize] = [0.0, 0.0, 0.0, 1.0];
            let zview = self.post_state.floatz_view(&self.gpu, size, format);
            let mut list = self.build_draws(
                PassKind::FloatZ,
                &zf,
                &HashMap::new(),
                &items,
                Target {
                    color: Some(post::FLOATZ_FORMAT),
                    depth: Some(DEPTH_FORMAT),
                },
                false,
                &mut counts,
                view.origin,
            );
            list.extend(self.build_dynamic(
                PassKind::FloatZ,
                &zf,
                &HashMap::new(),
                &insts,
                &dynsurfs,
                ModelKind::World,
                Target {
                    color: Some(post::FLOATZ_FORMAT),
                    depth: Some(DEPTH_FORMAT),
                },
                false,
            ));
            (zview, list)
        });
        let chain = post::build(self, size, format, target, NEAR);

        self.upload_ring();
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        if let (Some(ss), Some(sh)) = (&sun, &self.shadow) {
            let _ = ss;
            let color_att = sh.color.as_ref().map(|v| wgpu::RenderPassColorAttachment {
                view: v,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            });
            let timestamps = self.timer.as_mut().and_then(|t| t.pass("sun shadow"));
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sun shadow"),
                color_attachments: &[color_att],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &sh.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timestamps,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (k, list) in shadow_lists.iter().enumerate() {
                let vp = SunShadow::viewport(k);
                rp.set_viewport(vp[0], vp[1], vp[2], vp[3], 0.0, 1.0);
                record(&mut rp, list, &self.vs_bg, &self.ps_bg, None, &self.dyn_vb);
            }
        }
        if let Some((zview, list)) = &floatz {
            let depth = &self.depth.as_ref().expect("depth").0;
            let timestamps = self.timer.as_mut().and_then(|t| t.pass("floatz"));
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("floatz"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: zview,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: post::FLOATZ_CLEAR,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timestamps,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            record(&mut rp, list, &self.vs_bg, &self.ps_bg, None, &self.dyn_vb);
        }
        {
            let depth = &self.depth.as_ref().expect("depth").0;
            let timestamps = self.timer.as_mut().and_then(|t| t.pass("scene"));
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: chain.scene.as_ref().unwrap_or(target),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: self.clear[0],
                            g: self.clear[1],
                            b: self.clear[2],
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timestamps,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            record(
                &mut rp,
                &draws,
                &self.vs_bg,
                &self.ps_bg,
                Some((size.0 as f32, size.1 as f32)),
                &self.dyn_vb,
            );
            if !vm_draws.is_empty() {
                rp.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, VIEWMODEL_DEPTH);
                record(
                    &mut rp,
                    &vm_draws,
                    &self.vs_bg,
                    &self.ps_bg,
                    None,
                    &self.dyn_vb,
                );
            }
        }
        post::record(
            &mut enc,
            &chain,
            &self.post_state,
            &self.vs_bg,
            &self.ps_bg,
            self.timer.as_mut(),
        );
        if let Some(t) = self.timer.as_mut() {
            t.resolve(&mut enc);
        }
        self.gpu.queue.submit([enc.finish()]);
        if let Some(t) = self.timer.as_mut() {
            t.submitted();
            stats.gpu_ms = t.last_total_ms;
            stats.frame = t.frame();
        }
        self.dynamic_models = insts;
        self.dynamic_meshes = meshes;
        stats.cpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
        stats
    }

    fn upload_ring(&mut self) {
        // Banks overlap in the buffer; the last one still needs a full bank of room behind it.
        let need = self.ring.len() + 256;
        if need > self.ring_cap {
            self.ring_cap = need.next_power_of_two();
            self.ring_buf = new_ring(&self.gpu, self.ring_cap);
            (self.vs_bg, self.ps_bg) =
                ring_groups(&self.gpu, &self.materials, &self.ring_buf, &self.bools);
        }
        self.gpu
            .queue
            .write_buffer(&self.ring_buf, 0, bytemuck::cast_slice(&self.ring));
    }
}

/// Record `draws` into `rp`. `sky_view` carries the target size of a scene pass, whose sky draws use the far end of
/// the depth range.
fn record(
    rp: &mut wgpu::RenderPass<'_>,
    draws: &[Draw],
    vs_bg: &wgpu::BindGroup,
    ps_bg: &wgpu::BindGroup,
    sky_view: Option<(f32, f32)>,
    dyn_vb: &wgpu::Buffer,
) {
    let mut cur_pipe: Option<*const wgpu::RenderPipeline> = None;
    let mut cur_tex: Option<*const wgpu::BindGroup> = None;
    let mut cur_mesh: Option<(*const Mesh, u64)> = None;
    let mut sky_range = false;
    for d in draws {
        if let Some((w, h)) = sky_view
            && d.sky != sky_range
        {
            sky_range = d.sky;
            let depth = if sky_range { 1.0 } else { 0.0 };
            rp.set_viewport(0.0, 0.0, w, h, depth, 1.0);
        }
        if cur_pipe != Some(Arc::as_ptr(&d.pipeline)) {
            rp.set_pipeline(&d.pipeline);
            cur_pipe = Some(Arc::as_ptr(&d.pipeline));
        }
        rp.set_bind_group(0, vs_bg, &[d.vs]);
        rp.set_bind_group(1, ps_bg, &[d.ps]);
        if cur_tex != Some(Arc::as_ptr(&d.tex_bg)) {
            rp.set_bind_group(2, &*d.tex_bg, &[]);
            cur_tex = Some(Arc::as_ptr(&d.tex_bg));
        }
        if let Some((at, len)) = d.vb {
            rp.set_vertex_buffer(0, dyn_vb.slice(at..at + len));
            rp.set_index_buffer(d.mesh.ib.slice(..), wgpu::IndexFormat::Uint16);
            cur_mesh = None;
        } else if cur_mesh != Some((Arc::as_ptr(&d.mesh), d.vb_offset)) {
            rp.set_vertex_buffer(0, d.mesh.vb.slice(d.vb_offset..));
            rp.set_index_buffer(d.mesh.ib.slice(..), wgpu::IndexFormat::Uint16);
            cur_mesh = Some((Arc::as_ptr(&d.mesh), d.vb_offset));
        }
        rp.draw_indexed(d.first_index..d.first_index + d.count, d.base_vertex, 0..1);
    }
}

/// Surfaces and static models whose shadow can fall into a partition's frustum.
fn shadow_casters(world: &assets::zone::gfxworld::GfxWorld, f: &Frustum) -> (Vec<u32>, Vec<u32>) {
    let surfaces = world
        .dpvs
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.flags & SURFACE_CASTS_SUN_SHADOW != 0
                && !f.culls(Vec3::from(s.bounds[0]), Vec3::from(s.bounds[1]))
        })
        .map(|(i, _)| i as u32)
        .collect();
    let sun = world.sun_primary_light_index as usize;
    let smodels = world
        .shadow_geometry
        .get(sun)
        .map(|g| g.smodel_index.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|&&i| {
            world
                .dpvs
                .smodel_draw_insts
                .get(usize::from(i))
                .is_some_and(|m| {
                    let r = m.model.as_ref().map_or(0.0, |m| m.radius) * m.scale;
                    let o = Vec3::from(m.origin);
                    !f.culls(o - Vec3::splat(r), o + Vec3::splat(r))
                })
        })
        .map(|&i| u32::from(i))
        .collect();
    (surfaces, smodels)
}

fn shadow_sampler(compare: bool) -> SamplerKey {
    if compare {
        SamplerKey::ShadowCompare
    } else {
        SamplerKey::ShadowRaw
    }
}

/// 1x1 shadow textures for slots bound while the map has none: a depth texture and a float colour texture.
fn dummy_shadow(gpu: &Gpu) -> (Arc<Tex>, Arc<Tex>) {
    let make = |format, data: &[u8]| {
        let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow stand-in"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        if !data.is_empty() {
            gpu.queue.write_texture(
                t.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        Arc::new(Tex {
            view: t.create_view(&Default::default()),
            dim: SamplerDim::D2,
            width: 1,
        })
    };
    // Depth textures cannot be written from the CPU; one is cleared to "far" by a pass below.
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow depth stand-in"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = depth.create_view(&Default::default());
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear shadow stand-in"),
        color_attachments: &[],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    }));
    gpu.queue.submit([enc.finish()]);
    let colour = make(wgpu::TextureFormat::R32Float, &1.0f32.to_le_bytes());
    (
        Arc::new(Tex {
            view,
            dim: SamplerDim::D2,
            width: 1,
        }),
        colour,
    )
}

/// A mesh whose index buffer is 0, 1, 2, ...; the vertices come from the dynamic buffer.
fn counting_mesh(gpu: &Gpu) -> Mesh {
    let idx: Vec<u16> = (0..=u16::MAX).collect();
    Mesh {
        vb: gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unused vertices"),
            size: 16,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: false,
        }),
        ib: gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("counting indices"),
                contents: bytemuck::cast_slice(&idx),
                usage: wgpu::BufferUsages::INDEX,
            }),
    }
}

fn new_dyn_vb(gpu: &Gpu, size: u64) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("skinned vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn new_ring(gpu: &Gpu, regs: usize) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("constant banks"),
        size: regs as u64 * 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn ring_groups(
    gpu: &Gpu,
    m: &Materials,
    ring: &wgpu::Buffer,
    bools: &wgpu::Buffer,
) -> (wgpu::BindGroup, wgpu::BindGroup) {
    let bank = wgpu::BindingResource::Buffer(wgpu::BufferBinding {
        buffer: ring,
        offset: 0,
        size: wgpu::BufferSize::new(BANK_BYTES),
    });
    let vs = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("vs constants"),
        layout: &m.vs_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: bank.clone(),
        }],
    });
    let ps = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ps constants"),
        layout: &m.ps_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: bank,
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: bools.as_entire_binding(),
            },
        ],
    });
    (vs, ps)
}

/// First LOD whose range covers `dist`; the last LOD otherwise.
fn pick_lod(model: &assets::zone::xmodel::XModel, dist: f32) -> usize {
    let n = usize::from(model.num_lods).clamp(1, 4);
    (0..n - 1)
        .find(|&i| model.lod_info[i].dist > 0.0 && dist < model.lod_info[i].dist)
        .unwrap_or(n - 1)
}

fn is_sky(m: &Material) -> bool {
    m.technique_set
        .as_ref()
        .and_then(|t| t.name.as_deref())
        .is_some_and(|n| n.trim_start_matches(',').contains("sky"))
}
