// SPDX-License-Identifier: GPL-3.0-only
//! The frame: cull, build the draw lists, fill constant banks, record the sun shadow pass and the scene pass.

use crate::art::MapArt;
use crate::codeconst::{self, FrameConsts, LightConsts, Object, tex as ctex};
use crate::cookie;
use crate::cull::{self, Frustum};
use crate::dlight::{self, DynLight};
use crate::dynmesh::{self, DynMesh};
use crate::gpu::Gpu;
use crate::material::{BANK_BYTES, Materials, Prepared, SamplerKey, Target, VertexKind};
use crate::post::{self, PostParams};
use crate::scene::{MapData, Mesh, Scene, model_matrix};
use crate::skin::{self, ModelInstance, ModelKind};
use crate::spotshadow;
use crate::sun::{self, Sun};
use crate::sunshadow::{self, SunShadow};
use crate::texture::{Tex, TextureCache};
use crate::timing::GpuTimer;
use assets::zone::gfx::Material;
use assets::zone::xmodel::XModel;
use glam::{Mat4, Vec3, Vec4};
use sm3::SamplerDim;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use web_time::{Duration, Instant};
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
const TECH_COOKIE_CASTER: usize = 30;
const TECH_COOKIE_RECEIVER: usize = 31;
const COOKIE_CASTER_TECHS: [usize; 1] = [TECH_COOKIE_CASTER];
const COOKIE_RECEIVER_TECHS: [usize; 1] = [TECH_COOKIE_RECEIVER];
const COOKIE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const TECH_LIT_SPOT_SHADOW: usize = 11;
const TECH_LIGHT_SPOT: usize = 21;
const TECH_LIGHT_OMNI: usize = 22;
const TECH_LIGHT_SPOT_SHADOW: usize = 23;
const SPOT_SHADOW_TECHS: [usize; 5] = [
    TECH_LIT_SPOT_SHADOW,
    TECH_LIT_SPOT,
    TECH_LIT,
    TECH_UNLIT,
    TECH_EMISSIVE,
];
const LIGHT_SPOT_SHADOW_TECHS: [usize; 2] = [TECH_LIGHT_SPOT_SHADOW, TECH_LIGHT_SPOT];
/// The `light omni` and `light spot` techniques: what a dynamic light adds to a surface, on top of what was drawn.
const LIGHT_OMNI_TECHS: [usize; 1] = [TECH_LIGHT_OMNI];
const LIGHT_SPOT_TECHS: [usize; 1] = [TECH_LIGHT_SPOT];
/// The `light` key of the texture groups of dynamic lights: past every primary light's index.
const DLIGHT_KEY: u16 = 0x100;
/// The most world surfaces and static models one dynamic light draws; the original keeps 512 per light.
const MAX_LIGHT_SURFACES: usize = 512;
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
    /// Rolls the view clockwise by this many radians.
    pub roll: f32,
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
        let (sin, cos) = self.roll.sin_cos();
        let (s, u) = (s * cos - u * sin, u * cos + s * sin);
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

/// The scene pass's multisampled attachments and what they were made for.
struct Msaa {
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    key: ((u32, u32), wgpu::TextureFormat, u32),
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
    /// `r_specular`: off, the sun's specular highlight is dropped.
    pub specular: bool,
    /// `r_dof_enable`: off, the map's and the game's depth of field is not drawn.
    pub dof: bool,
    /// `r_glow_allowed`: off, the glow is not drawn.
    pub glow: bool,
    /// `r_aaSamples`: samples per pixel of the scene pass; the renderer uses the most the device offers up to this.
    pub aa_samples: u32,
    /// `r_aspectRatio`: the shape of the screen when its pixels are not square; `None` takes the target's.
    pub aspect: Option<f32>,
    /// `r_dlightLimit`: the most dynamic lights drawn at once (at most four); zero draws none.
    pub dlight_limit: usize,
    /// The `r_spotLight*` dvars, which shape the spot lights the effects add.
    pub spot: dlight::SpotParams,
    /// `sc_enable`: with shadow maps off, dynamic models cast shadow cookies.
    pub cookies: bool,
    /// `sc_count`: the most cookies per frame (the renderer draws at most [`cookie::MAX`]).
    pub cookie_count: usize,
    /// `sm_spotEnable`: the map's spot lights that can cast shadows get shadow maps (with `shadows` on).
    pub spot_shadows: bool,
    /// `r_spotLightShadows`: so does a spot light an effect adds.
    pub dynamic_spot_shadows: bool,
    /// `sm_maxLights`: the most spot lights with a shadow map at once (at most four).
    pub max_shadow_lights: usize,
    /// `sm_spotShadowFadeTime`: seconds a spot shadow takes to fade in or out.
    pub spot_fade_time: f32,
    /// `r_drawSun`: off, the map's sun sprite, lens flare and glare are not drawn.
    pub draw_sun: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            shadows: ShadowMode::Depth,
            fog: true,
            primary_lights: true,
            show_missing_light_grid: false,
            specular: true,
            dof: true,
            glow: true,
            aa_samples: 1,
            aspect: None,
            dlight_limit: dlight::MAX_VISIBLE,
            spot: dlight::SpotParams::default(),
            cookies: true,
            cookie_count: 24,
            spot_shadows: true,
            dynamic_spot_shadows: true,
            max_shadow_lights: spotshadow::TILES as usize,
            spot_fade_time: 1.0,
            draw_sun: true,
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
    /// Dynamic lights drawn, and the draws they added to the scene pass.
    pub lights: usize,
    pub light_draws: usize,
    /// Shadow cookies drawn.
    pub cookies: usize,
    pub pipelines_missing: usize,
    pub cpu_ms: f64,
    /// GPU time of the most recent frame whose timestamps have come back (a few frames behind), if the device can
    /// time passes.
    pub gpu_ms: Option<f64>,
    /// This frame's number for [`Renderer::take_gpu_times`]; 0 when the device cannot time passes.
    pub frame: u64,
}

/// How far [`Renderer::warm_step`] has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// The enumeration [`Renderer::warm_step`] walks across frames.
struct Warm {
    format: wgpu::TextureFormat,
    jobs: Vec<(Arc<Prepared>, Target)>,
    built: Vec<bool>,
    done: usize,
    cursor: usize,
    /// Time spent in, and the number of, the builds [`Renderer::warm_step`] did.
    spent: Duration,
    timed: u32,
    /// Pipelines frames wanted and skipped, by (pass id, target); built first.
    demand: HashSet<(u32, Target)>,
}

impl Warm {
    fn mean_build(&self) -> Duration {
        self.spent / self.timed.max(1)
    }
}

struct Draw {
    prepared: Arc<Prepared>,
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
    can_shadow: bool,
    cos_outer: f32,
    cos_inner: f32,
    consts: LightConsts,
    attenuation: Option<(Arc<Tex>, u8)>,
}

/// The `light_dynamic` definition the effects' lights take their attenuation ramp from.
struct DlightDef {
    attenuation: Option<(Arc<Tex>, u8)>,
    width: f32,
    lookup_start: i32,
}

/// The pipelines that write one alpha value over a rectangle of the scene: before a dynamic light draws, its surfaces
/// weigh themselves by the destination alpha so overlapping triangles light a pixel once.
struct AlphaFill {
    key: (wgpu::TextureFormat, u32),
    zero: wgpu::RenderPipeline,
    one: wgpu::RenderPipeline,
}

/// The atlas of shadow cookies: one square tile of a stack for each caster.
struct CookieAtlas {
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    tex: Arc<Tex>,
}

/// One cookie of a frame: its tile, the caster's draws into it and the draws of the surfaces it darkens.
struct CookiePass {
    tile: u32,
    casters: Vec<Draw>,
    receivers: Vec<Draw>,
}

/// A spot light that casts a shadow this frame.
#[derive(Clone, Copy)]
struct SpotTile {
    fade: f32,
    /// World position to `(u, v, depth, w)` of the light's tile of the atlas.
    lookup: Mat4,
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
    light: u16,
}

/// Which list of draws a pass builds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PassKind {
    Scene,
    /// A shadow map's casters: the sun's partitions and the spot lights' tiles.
    ShadowMap,
    /// The scene's view-space depth for the post chain's depth of field.
    FloatZ,
    /// A shadow cookie's caster, drawn from the sun into a tile of the cookie atlas.
    CookieCaster,
    /// The surfaces a shadow cookie darkens.
    CookieReceiver,
    /// What a dynamic light adds: its spot (`true`) or omni technique, over the surfaces it reaches.
    DynLight {
        spot: bool,
        shadow: bool,
    },
}

impl PassKind {
    fn is_light(self) -> bool {
        matches!(self, PassKind::DynLight { .. })
    }

    /// Passes of techniques that a material may lack (the others fall back to simpler ones, or are the lit pass).
    fn optional_tech(self) -> bool {
        !matches!(self, PassKind::Scene | PassKind::FloatZ)
    }
}

/// Visible geometry to draw.
struct Items<'a> {
    surfaces: &'a [u32],
    smodels: &'a [u32],
}

/// One skinned surface of a dynamic model, skinned into the frame's vertex buffer.
#[derive(Clone, Copy)]
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

/// What one dynamic light draws over the scene.
struct LightPass {
    /// `(x, y, width, height)` in pixels: the part of the target the light can reach.
    rect: [u32; 4],
    draws: Vec<Draw>,
    /// The same for the view model, which has its own projection and depth range.
    vm_draws: Vec<Draw>,
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
    /// The multisampled colour and depth of the scene pass, while `r_aaSamples` asks for more than one sample.
    msaa: Option<Msaa>,
    tex_bgs: HashMap<TexKey, Arc<wgpu::BindGroup>>,
    pub clear: [f64; 3],
    pub settings: Settings,
    /// Fog, glow and film of the map.
    pub art: MapArt,
    lights: Vec<Option<PrimaryLight>>,
    dlight_def: DlightDef,
    alpha_fill: Option<AlphaFill>,
    /// The lights the effects add; the caller refills the list every frame.
    pub dynamic_lights: Vec<DynLight>,
    shadow: Option<ShadowTargets>,
    /// The shadow cookie atlas, made when a frame first needs it.
    cookie: Option<CookieAtlas>,
    /// The depth atlas of the spot light shadows.
    spot_shadow: Option<ShadowTargets>,
    /// The spot lights with a shadow map, or fading out of having one; an entry's tile is its position.
    spot_history: Vec<spotshadow::Entry>,
    /// This frame's shadowed spot lights, by primary light index.
    spot_tiles: HashMap<u8, SpotTile>,
    /// The spot lights that could cast a shadow last frame.
    spot_in_use: Vec<u32>,
    last_time: Option<f32>,
    /// Stand-in shadow textures for passes that bind a shadow slot while the map is off.
    shadow_dummy: (Arc<Tex>, Arc<Tex>),
    pub timer: Option<GpuTimer>,
    /// What the post chain does this frame; starts as the map's own glow and film.
    pub post: PostParams,
    pub(crate) post_state: post::State,
    /// The map's sun sprite, lens flare and glare; its overlay is drawn at the end of the post chain.
    pub(crate) sun: Sun,
    warm: Option<Warm>,
    /// Skinned models to draw in the next [`Renderer::render`]; the caller refills the list every frame.
    pub dynamic_models: Vec<ModelInstance>,
    /// Materials of models the map does not contain but a match draws (players, weapons), warmed with the map's.
    warm_extra: Vec<Arc<Material>>,
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
                    can_shadow: l.can_use_shadow_map,
                    cos_outer: l.cos_half_fov_outer,
                    cos_inner: l.cos_half_fov_inner,
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
        let dlight_def = data
            .light_defs
            .iter()
            .find(|d| d.name.as_deref() == Some("light_dynamic"))
            .map_or(
                DlightDef {
                    attenuation: None,
                    width: 32.0,
                    lookup_start: 0,
                },
                |d| {
                    let attenuation = d.attenuation_image.as_ref().and_then(|img| {
                        Some((textures.image(&gpu, img)?, d.attenuation_sampler_state))
                    });
                    DlightDef {
                        width: attenuation.as_ref().map_or(32.0, |(t, _)| t.width as f32),
                        attenuation,
                        lookup_start: d.lmap_lookup_start,
                    }
                },
            );
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
            msaa: None,
            tex_bgs: HashMap::new(),
            clear: [0.45, 0.43, 0.38],
            settings: Settings::default(),
            art: data.art.clone(),
            post: PostParams::from_art(&data.art),
            post_state,
            sun: Sun::new(&gpu_for_dyn),
            warm: None,
            lights,
            dlight_def,
            alpha_fill: None,
            dynamic_lights: Vec::new(),
            shadow: None,
            cookie: None,
            spot_shadow: None,
            spot_history: Vec::new(),
            spot_tiles: HashMap::new(),
            spot_in_use: Vec::new(),
            last_time: None,
            shadow_dummy,
            timer,
            dynamic_models: Vec::new(),
            warm_extra: Vec::new(),
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
        if self.cookies_possible() {
            for m in self.scene.materials() {
                self.prepare(&m, &COOKIE_CASTER_TECHS, VertexKind::Model, hsm);
                for kind in [VertexKind::World, VertexKind::Model] {
                    self.prepare(&m, &COOKIE_RECEIVER_TECHS, kind, hsm);
                }
            }
        }
        if self.spot_shadows_possible() {
            for m in self.scene.materials() {
                for kind in [VertexKind::World, VertexKind::Model] {
                    self.prepare(&m, &SPOT_SHADOW_TECHS, kind, hsm);
                }
            }
        }
        // The light pass of the effects' omni lights, after everything a frame without them draws.
        if self.settings.dlight_limit > 0 {
            for m in self.scene.materials() {
                for kind in [VertexKind::World, VertexKind::Model] {
                    if self.materials.has_technique(&m, TECH_LIGHT_OMNI, hsm) {
                        self.prepare(&m, &LIGHT_OMNI_TECHS, kind, hsm);
                    }
                }
            }
        }
    }

    /// Whether this map has a spot light that can cast a shadow and the settings let it.
    fn spot_shadows_possible(&self) -> bool {
        self.settings.shadows != ShadowMode::Off
            && self.settings.spot_shadows
            && self.settings.primary_lights
            && self
                .lights
                .iter()
                .flatten()
                .any(|l| l.kind == LIGHT_KIND_SPOT && l.can_shadow)
    }

    fn shadow_tech(&self) -> usize {
        if self.settings.shadows == ShadowMode::Color {
            TECH_BUILD_SHADOWMAP_COLOR
        } else {
            TECH_BUILD_SHADOWMAP_DEPTH
        }
    }

    /// Every (pass, target) pair a frame into `format` can draw with, in the order materials come.
    /// Models the map does not contain but the match will draw (players, weapons): their materials get their pipelines
    /// with [`Renderer::warm`], so the first sight of them does not stall a frame.
    pub fn warm_models<'a>(&mut self, models: impl IntoIterator<Item = &'a Arc<XModel>>) {
        let mut seen: HashSet<usize> = self
            .warm_extra
            .iter()
            .map(|m| Arc::as_ptr(m) as usize)
            .collect();
        for model in models {
            for m in model.materials.iter().flatten() {
                if seen.insert(Arc::as_ptr(m) as usize) {
                    self.warm_extra.push(m.clone());
                }
            }
        }
    }

    fn warm_jobs(&mut self, format: wgpu::TextureFormat) -> Vec<(Arc<Prepared>, Target)> {
        let hsm = self.hsm();
        let scene = Target {
            color: Some(format),
            depth: Some(DEPTH_FORMAT),
            samples: self.samples(format),
        };
        let mut jobs = Vec::new();
        let materials: Vec<Arc<Material>> = self
            .scene
            .materials()
            .into_iter()
            .chain(self.warm_extra.iter().cloned())
            .collect();
        for m in &materials {
            for kind in [VertexKind::World, VertexKind::Model] {
                for techs in [
                    &SUN_SHADOW_TECHS[..],
                    &SUN_TECHS,
                    &LIT,
                    &SPOT_TECHS,
                    &OMNI_TECHS,
                ] {
                    if let Some(p) = self.prepare(m, techs, kind, hsm) {
                        jobs.push((p, scene));
                    }
                }
                if self.settings.shadows != ShadowMode::Off
                    && let Some(p) = self.prepare(m, &[self.shadow_tech()], kind, hsm)
                {
                    jobs.push((p, self.shadow_target()));
                }
            }
        }
        // The shadowed spot techniques of a map that has spot lights that can cast one.
        if self.spot_shadows_possible() {
            for m in &materials {
                for kind in [VertexKind::World, VertexKind::Model] {
                    if let Some(p) = self.prepare(m, &SPOT_SHADOW_TECHS, kind, hsm) {
                        jobs.push((p, scene));
                    }
                }
            }
        }
        // The shadow cookies of a frame without shadow maps.
        if self.cookies_possible() {
            for m in &materials {
                if let Some(p) = self.prepare(m, &COOKIE_CASTER_TECHS, VertexKind::Model, hsm) {
                    jobs.push((p, Self::cookie_target()));
                }
                for kind in [VertexKind::World, VertexKind::Model] {
                    if let Some(p) = self.prepare(m, &COOKIE_RECEIVER_TECHS, kind, hsm) {
                        jobs.push((p, scene));
                    }
                }
            }
        }
        // The effects' omni lights come last: a load that stops early still draws everything else.
        if self.settings.dlight_limit > 0 {
            for m in &materials {
                for kind in [VertexKind::World, VertexKind::Model] {
                    if self.materials.has_technique(m, TECH_LIGHT_OMNI, hsm)
                        && let Some(p) = self.prepare(m, &LIGHT_OMNI_TECHS, kind, hsm)
                    {
                        jobs.push((p, scene));
                    }
                }
            }
        }
        jobs
    }

    /// Build the pipelines for rendering into `format`, again to keep them off the frame path. Returns how many.
    /// Blocks until all are built; [`Renderer::warm_step`] does the same across frames.
    pub fn warm(&mut self, format: wgpu::TextureFormat) -> usize {
        self.warm_progress(format, &|_, _| {})
    }

    /// [`Renderer::warm`] spread over half the cores (the driver's shader compiler dominates a native map load), with
    /// `progress(done, total)` called as each pipeline finishes, from any of the worker threads. The browser has no
    /// threads: one after another there.
    pub fn warm_progress(
        &mut self,
        format: wgpu::TextureFormat,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> usize {
        let jobs = self.warm_jobs(format);
        let done = AtomicUsize::new(0);
        let total = jobs.len();
        #[cfg(target_arch = "wasm32")]
        for (p, target) in &jobs {
            self.materials.pipeline_now(&self.gpu, p, *target);
            progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let next = AtomicUsize::new(0);
            // Half the cores: the window thread keeps presenting while this runs.
            let workers = (std::thread::available_parallelism().map_or(4, usize::from) / 2).max(1);
            std::thread::scope(|s| {
                for _ in 0..workers.min(total) {
                    s.spawn(|| {
                        while let Some((p, target)) = jobs.get(next.fetch_add(1, Ordering::Relaxed))
                        {
                            self.materials.pipeline_now(&self.gpu, p, *target);
                            progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                        }
                    });
                }
            });
        }
        total
    }

    /// Build pipelines for up to `budget` (at least one if any is left), those the last frames wanted and could not
    /// draw first, then the rest in [`Renderer::warm`]'s order. Call it once per frame before
    /// [`Renderer::render`]: until every pipeline is built the frame will not compile any itself (its draws are
    /// skipped and counted in [`FrameStats::pipelines_missing`]); afterwards a frame builds one that turns up late
    /// (something `warm` did not list, first drawn) itself, as on native.
    pub fn warm_step(&mut self, format: wgpu::TextureFormat, budget: Duration) -> Progress {
        let start = Instant::now();
        if self.warm.as_ref().is_none_or(|w| w.format != format) {
            let jobs = self.warm_jobs(format);
            let built = vec![false; jobs.len()];
            self.warm = Some(Warm {
                format,
                jobs,
                built,
                done: 0,
                cursor: 0,
                spent: Duration::ZERO,
                timed: 0,
                demand: HashSet::new(),
            });
        }
        let end = start + budget;
        let w = self.warm.as_mut().expect("set above");
        let (_, demand) = self.materials.take_deferred();
        w.demand.extend(demand);
        // Demanded first, then the cursor.
        let mut rush: Vec<usize> = (0..w.jobs.len())
            .filter(|&i| !w.built[i] && w.demand.contains(&(w.jobs[i].0.id(), w.jobs[i].1)))
            .collect();
        w.demand.clear();
        rush.reverse();
        let mut first = true;
        // Stop early enough that the next build (about twice the average, builds vary a lot) fits the budget.
        while w.done < w.jobs.len() && (first || Instant::now() + w.mean_build() * 2 < end) {
            first = false;
            let began = Instant::now();
            let n = rush.pop().unwrap_or_else(|| {
                while w.built[w.cursor] {
                    w.cursor += 1;
                }
                w.cursor
            });
            let (p, t) = &w.jobs[n];
            self.materials.pipeline_now(&self.gpu, p, *t);
            w.built[n] = true;
            w.done += 1;
            w.spent += began.elapsed();
            w.timed += 1;
        }
        let progress = Progress {
            done: w.done,
            total: w.jobs.len(),
        };
        self.materials
            .set_deadline((progress.done < progress.total).then(Instant::now));
        progress
    }

    fn shadow_target(&self) -> Target {
        Target {
            color: (self.settings.shadows == ShadowMode::Color).then_some(SHADOW_COLOR_FORMAT),
            depth: Some(DEPTH_FORMAT),
            samples: 1,
        }
    }

    /// The prepared pass for `techs` of `m`, for debugging tools.
    pub fn inspect(
        &mut self,
        m: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
    ) -> Option<Arc<Prepared>> {
        let hsm = self.hsm();
        self.prepare(m, techs, kind, hsm)
    }

    fn prepare(
        &mut self,
        m: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
        hsm: bool,
    ) -> Option<Arc<Prepared>> {
        self.materials
            .prepare(&self.gpu, &mut self.textures, m, techs, kind, hsm)
    }

    /// The post parameters as the settings allow them: depth of field and glow can be switched off.
    pub(crate) fn post_params(&self) -> crate::post::PostParams {
        let mut p = self.post.clone();
        if !self.settings.dof {
            p.dof = None;
        }
        if !self.settings.glow {
            p.glow.enabled = false;
        }
        p
    }

    /// The sample count the scene pass uses: `r_aaSamples` lowered to the largest count the device supports for the
    /// colour and depth formats.
    pub fn samples(&self, format: wgpu::TextureFormat) -> u32 {
        let ok = |f: wgpu::TextureFormat, n: u32| {
            self.gpu
                .adapter
                .get_texture_format_features(f)
                .flags
                .sample_count_supported(n)
        };
        [4u32, 2]
            .into_iter()
            .find(|&n| n <= self.settings.aa_samples && ok(format, n) && ok(DEPTH_FORMAT, n))
            .unwrap_or(1)
    }

    fn ensure_msaa(&mut self, size: (u32, u32), format: wgpu::TextureFormat, samples: u32) {
        if samples <= 1 {
            self.msaa = None;
            return;
        }
        let key = (size, format, samples);
        if self.msaa.as_ref().is_some_and(|m| m.key == key) {
            return;
        }
        let make = |label, format| {
            self.gpu
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        self.msaa = Some(Msaa {
            color: make("scene msaa colour", format),
            depth: make("scene msaa depth", DEPTH_FORMAT),
            key,
        });
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
        self.shadow = Some(shadow_targets(
            &self.gpu,
            mode,
            (sunshadow::SIZE, sunshadow::HEIGHT),
            "sun shadow",
        ));
        self.tex_bgs.clear();
    }

    /// Whether frames draw shadow cookies: shadow maps are off and the settings and the map (a sun) allow them.
    fn cookies_possible(&self) -> bool {
        self.settings.shadows == ShadowMode::Off
            && self.settings.cookies
            && self.settings.cookie_count > 0
            && self.scene.world.sun_light.is_some()
    }

    fn cookie_target() -> Target {
        Target {
            color: Some(COOKIE_FORMAT),
            depth: Some(DEPTH_FORMAT),
            samples: 1,
        }
    }

    fn ensure_cookie_atlas(&mut self) {
        if self.cookie.is_some() {
            return;
        }
        let size = wgpu::Extent3d {
            width: cookie::TILE,
            height: cookie::TILE * cookie::MAX as u32,
            depth_or_array_layers: 1,
        };
        let make = |label, format, usage| {
            self.gpu
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let color = make(
            "shadow cookies",
            COOKIE_FORMAT,
            attach | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        self.cookie = Some(CookieAtlas {
            tex: Arc::new(Tex {
                view: color.clone(),
                dim: SamplerDim::D2,
                width: cookie::TILE,
            }),
            color,
            depth: make("shadow cookie depth", DEPTH_FORMAT, attach),
        });
        self.tex_bgs.clear();
    }

    /// The spot light shadow atlas for the current mode, created on first use.
    fn ensure_spot_shadow(&mut self) {
        let mode = self.settings.shadows;
        if mode == ShadowMode::Off {
            if self.spot_shadow.take().is_some() {
                self.spot_history.clear();
                self.spot_in_use.clear();
                self.spot_tiles.clear();
                self.tex_bgs.clear();
            }
            return;
        }
        if self.spot_shadow.as_ref().is_some_and(|s| s.mode == mode) {
            return;
        }
        self.spot_shadow = Some(shadow_targets(
            &self.gpu,
            mode,
            (spotshadow::TILE, spotshadow::TILE * spotshadow::TILES),
            "spot shadow",
        ));
        self.tex_bgs.clear();
    }

    /// Code textures `p` samples, resolved for a surface with lightmap `lm`, reflection probe `probe` and primary
    /// light `light`.
    fn tex_group(
        &mut self,
        p: &Arc<Prepared>,
        lm: u8,
        probe: u8,
        light: u16,
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
                    let map = if id == ctex::SHADOWMAP_SUN {
                        self.shadow.as_ref()
                    } else {
                        self.spot_shadow.as_ref()
                    };
                    let real = map.filter(|s| (s.mode == ShadowMode::Depth) == compare);
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
                (ctex::LIGHT_ATTENUATION, _) => if light == DLIGHT_KEY {
                    self.dlight_def.attenuation.clone()
                } else {
                    self.lights
                        .get(usize::from(light))
                        .and_then(Option::as_ref)
                        .and_then(|l| l.attenuation.clone())
                }
                .map(|(t, st)| (t, SamplerKey::State(st))),
                (ctex::SHADOWCOOKIE, _) => self
                    .cookie
                    .as_ref()
                    .map(|c| (c.tex.clone(), CODE_SAMPLER.into())),
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
            Some(l) if self.settings.primary_lights && l.kind == LIGHT_KIND_SPOT => {
                if self.spot_tiles.contains_key(&light) {
                    &SPOT_SHADOW_TECHS
                } else {
                    &SPOT_TECHS
                }
            }
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
                PassKind::ShadowMap => &shadow_tech,
                PassKind::FloatZ => &floatz_tech,
                PassKind::DynLight { spot, shadow } => light_techs(spot, shadow),
                PassKind::CookieCaster => &COOKIE_CASTER_TECHS,
                PassKind::CookieReceiver => &COOKIE_RECEIVER_TECHS,
            };
            if kind.optional_tech() && !self.materials.has_technique(mat, techs[0], hsm) {
                continue;
            }
            let Some(prep) = self.prepare(mat, techs, VertexKind::World, hsm) else {
                if kind == PassKind::Scene {
                    counts.missing += 1;
                }
                continue;
            };
            let fc = light_frames.get(&light).unwrap_or(frame);
            let (vs, ps) = self.banks(&prep, fc, &Object::default(), &mut shared, light);
            let Some(pipeline) = self.materials.pipeline(&self.gpu, &prep, target) else {
                continue;
            };
            let tex_bg = self.tex_group(
                &prep,
                surf.lightmap_index,
                surf.reflection_probe_index,
                tex_light(kind, light),
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
                PassKind::ShadowMap => &shadow_tech,
                PassKind::FloatZ => &floatz_tech,
                PassKind::DynLight { spot, shadow } => light_techs(spot, shadow),
                PassKind::CookieCaster => &COOKIE_CASTER_TECHS,
                PassKind::CookieReceiver => &COOKIE_RECEIVER_TECHS,
            };
            let fc = light_frames.get(&light).unwrap_or(frame);
            let mut counted = false;
            for s in 0..usize::from(info.surf_count) {
                let idx = usize::from(info.surf_index) + s;
                let Some(Some(mat)) = model.materials.get(idx) else {
                    continue;
                };
                if kind.optional_tech() && !self.materials.has_technique(mat, techs[0], hsm) {
                    continue;
                }
                let Some(prep) = self.prepare(mat, techs, VertexKind::Model, hsm) else {
                    continue;
                };
                let Some(mesh) = self.scene.model_mesh(&self.gpu, &model, idx) else {
                    continue;
                };
                let (vs, ps) = self.banks(&prep, fc, &obj, &mut shared, light);
                let Some(pipeline) = self.materials.pipeline(&self.gpu, &prep, target) else {
                    continue;
                };
                let tex_bg = self.tex_group(
                    &prep,
                    0,
                    inst.reflection_probe_index,
                    tex_light(kind, light),
                );
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

    /// The reflection probe of a model that moves (it has no baked index), as `R_CalcReflectionProbeIndex` picks it: the
    /// nearest of the probes of the cell holding `p`, or of all but the default probe 0 outside every cell. A probe
    /// from another room lights the model with that room's sky and walls.
    fn nearest_probe(&self, p: [f32; 3]) -> u8 {
        let w = &self.scene.world;
        let d = |i: u8| {
            let o = &w.reflection_probes[usize::from(i)].origin;
            (0..3).map(|k| (o[k] - p[k]).powi(2)).sum::<f32>()
        };
        let in_cell = crate::cull::cell_for_point(w, Vec3::from(p)).and_then(|c| w.cells.get(c));
        let all = (1..w.reflection_probes.len().min(255) as u8).collect::<Vec<_>>();
        let probes = in_cell.map_or(&all, |c| &c.reflection_probes);
        probes
            .iter()
            .copied()
            .filter(|&i| usize::from(i) < w.reflection_probes.len())
            .min_by(|&a, &b| d(a).total_cmp(&d(b)))
            .unwrap_or(0)
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
            let Some(pipeline) = self.materials.pipeline(&self.gpu, &prep, target) else {
                continue;
            };
            let probe = meshes[d.mesh]
                .light_origin
                .map_or(0, |o| self.nearest_probe(o));
            let tex_bg = self.tex_group(&prep, 0, probe, u16::from(d.light));
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
                PassKind::ShadowMap => &shadow_tech,
                PassKind::FloatZ => &floatz_tech,
                PassKind::DynLight { spot, shadow } => light_techs(spot, shadow),
                PassKind::CookieCaster => &COOKIE_CASTER_TECHS,
                PassKind::CookieReceiver => &COOKIE_RECEIVER_TECHS,
            };
            if kind.optional_tech() && !self.materials.has_technique(mat, techs[0], hsm) {
                continue;
            }
            let Some(prep) = self.prepare(mat, techs, VertexKind::Model, hsm) else {
                continue;
            };
            let Some(mesh) = self.scene.model_mesh(&self.gpu, &inst.model, d.surf) else {
                continue;
            };
            let fc = light_frames.get(&light).unwrap_or(frame);
            let (vs, ps) = self.banks(&prep, fc, &d.obj, &mut shared, light);
            let Some(pipeline) = self.materials.pipeline(&self.gpu, &prep, target) else {
                continue;
            };
            let probe = self.nearest_probe(inst.light_origin);
            let tex_bg = self.tex_group(&prep, 0, probe, tex_light(kind, light));
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
        prep: &Arc<Prepared>,
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

    /// The frame constants a spot shadow map is built with: `view_proj` takes world positions to the tile's clip space.
    fn spot_build_frame(&self, view_proj: &Mat4, zf: f32, eye: Vec3) -> FrameConsts {
        let mut pf = FrameConsts::new(
            *view_proj * Mat4::from_translation(eye),
            Mat4::IDENTITY,
            eye,
        );
        pf.vec[codeconst::SHADOWMAP_POLYGON_OFFSET as usize] =
            if self.settings.shadows == ShadowMode::Color {
                [2.0 * zf / (zf - 1.0), 0.0, 0.0, 0.0]
            } else {
                spotshadow::POLYGON_OFFSET
            };
        pf
    }

    /// Chooses the map's spot lights that get a shadow map this frame (the best scoring of those the visible geometry
    /// is lit by, held for a while by the fade history) and builds their casters' draws: the geometry the map's shadow
    /// lists name inside the cone, and the dynamic models in it. Returns the draws by tile.
    fn build_spot_shadows(
        &mut self,
        view: &View,
        vis: &cull::Visible,
        insts: &[ModelInstance],
        dynsurfs: &[DynSurf],
        dt: f32,
        counts: &mut BuildCounts,
    ) -> Vec<(u32, Vec<Draw>)> {
        self.spot_tiles.clear();
        if self.spot_shadow.is_none()
            || !self.settings.spot_shadows
            || !self.settings.primary_lights
        {
            self.spot_history.clear();
            self.spot_in_use.clear();
            return Vec::new();
        }
        let world = self.scene.world.clone();
        let mut used: HashSet<u8> = vis
            .surfaces
            .iter()
            .filter_map(|&si| world.dpvs.surfaces.get(si as usize))
            .map(|s| s.primary_light_index)
            .collect();
        used.extend(
            vis.smodels
                .iter()
                .filter_map(|&mi| world.dpvs.smodel_draw_insts.get(mi as usize))
                .map(|m| m.primary_light_index),
        );
        let forward = view.forward();
        let mut scored: Vec<(f32, u8)> = used
            .into_iter()
            .filter_map(|i| {
                let l = self.lights.get(usize::from(i))?.as_ref()?;
                (l.kind == LIGHT_KIND_SPOT && l.can_shadow).then(|| {
                    let c = spotshadow::Candidate {
                        origin: l.consts.origin,
                        dir: l.consts.dir,
                        radius: l.consts.radius,
                        color: l.consts.color,
                    };
                    (spotshadow::score(view.origin, forward, &c), i)
                })
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let wanted: Vec<u32> = scored.iter().map(|s| u32::from(s.1)).collect();
        let in_use: Vec<u32> = scored.iter().map(|s| u32::from(s.1)).collect();
        spotshadow::update(
            &mut self.spot_history,
            &wanted,
            &self.spot_in_use,
            dt,
            self.settings.spot_fade_time,
            self.settings.max_shadow_lights,
        );
        self.spot_in_use = in_use;
        let target = self.shadow_target();
        let mut out = Vec::new();
        for (index, entry) in self.spot_history.clone().iter().enumerate() {
            let id = entry.id as u8;
            let Some(l) = self.lights.get(usize::from(id)).and_then(Option::as_ref) else {
                continue;
            };
            let (c, cos_inner, cos_outer) = (l.consts, l.cos_inner, l.cos_outer);
            let vp = spotshadow::view_proj(c.origin, c.dir, cos_outer, c.radius, 0.0);
            let index = index as u32;
            self.spot_tiles.insert(
                id,
                SpotTile {
                    fade: entry.fade,
                    lookup: spotshadow::lookup(&vp, index),
                },
            );
            let pf = self.spot_build_frame(&vp, c.radius, view.origin);
            let f = Frustum::from_clip(&vp);
            let casters = world.shadow_geometry.get(usize::from(id));
            let surfaces: Vec<u32> =
                casters
                    .map(|g| g.sorted_surf_index.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|&i| u32::from(i))
                    .filter(|&si| {
                        world.dpvs.surfaces.get(si as usize).is_some_and(|s| {
                            !f.culls(Vec3::from(s.bounds[0]), Vec3::from(s.bounds[1]))
                        })
                    })
                    .collect();
            let smodels: Vec<u32> = casters
                .map(|g| g.smodel_index.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|&i| u32::from(i))
                .filter(|&mi| {
                    world
                        .dpvs
                        .smodel_draw_insts
                        .get(mi as usize)
                        .is_some_and(|m| {
                            let r = m.model.as_ref().map_or(0.0, |m| m.radius) * m.scale;
                            let o = Vec3::from(m.origin);
                            !f.culls(o - Vec3::splat(r), o + Vec3::splat(r))
                        })
                })
                .collect();
            let cone = DynLight {
                origin: c.origin,
                color: c.color,
                radius: c.radius,
                spot: Some(dlight::SpotCone {
                    dir: c.dir,
                    cos_inner,
                    cos_outer,
                    near: 0.0,
                }),
            };
            let near: Vec<DynSurf> = dynsurfs
                .iter()
                .copied()
                .filter(|d| {
                    let inst = &insts[d.inst];
                    cone.reaches_sphere(Vec3::from(inst.origin), inst.model.radius)
                })
                .collect();
            let mut list = self.build_draws(
                PassKind::ShadowMap,
                &pf,
                &HashMap::new(),
                &Items {
                    surfaces: &surfaces,
                    smodels: &smodels,
                },
                target,
                false,
                counts,
                view.origin,
            );
            list.extend(self.build_dynamic(
                PassKind::ShadowMap,
                &pf,
                &HashMap::new(),
                insts,
                &near,
                ModelKind::World,
                target,
                false,
            ));
            out.push((index, list));
        }
        out
    }

    /// The shadow cookies of this frame, when shadow maps are off: the dynamic models nearest the eye cast one each, as a
    /// silhouette seen from the sun, onto the surfaces and the other models around them.
    #[allow(clippy::too_many_arguments)]
    fn build_cookies(
        &mut self,
        view: &View,
        frame: &FrameConsts,
        vis: &cull::Visible,
        insts: &[ModelInstance],
        dynsurfs: &[DynSurf],
        scene_target: Target,
        counts: &mut BuildCounts,
    ) -> Vec<CookiePass> {
        if !self.cookies_possible() {
            return Vec::new();
        }
        let casting: Vec<usize> = (0..insts.len())
            .filter(|&i| insts[i].casts_cookie && insts[i].model.radius > 0.0)
            .collect();
        let casters: Vec<cookie::Caster> = casting
            .iter()
            .map(|&i| cookie::Caster {
                origin: Vec3::from(insts[i].origin),
                radius: insts[i].model.radius,
            })
            .collect();
        let planned = cookie::plan(
            &casters,
            view.origin,
            self.scene.sun_dir,
            self.settings.cookie_count,
        );
        if planned.is_empty() {
            return Vec::new();
        }
        self.ensure_cookie_atlas();
        let world = self.scene.world.clone();
        let mut out = Vec::new();
        for (tile, c) in planned.iter().enumerate() {
            let inst = casting[c.caster];
            let mut cf = FrameConsts::new(
                c.view_proj * Mat4::from_translation(view.origin),
                Mat4::IDENTITY,
                view.origin,
            );
            cf.shadow_lookup = c.lookup;
            let own: Vec<DynSurf> = dynsurfs
                .iter()
                .copied()
                .filter(|d| d.inst == inst)
                .collect();
            let casters = self.build_dynamic(
                PassKind::CookieCaster,
                &cf,
                &HashMap::new(),
                insts,
                &own,
                ModelKind::World,
                Self::cookie_target(),
                false,
            );
            // The shadow falls on what is near the caster: a box around it, twice its size.
            let (lo, hi) = (
                c.centre - Vec3::splat(c.radius * 2.0),
                c.centre + Vec3::splat(c.radius * 2.0),
            );
            let mut surfaces: Vec<u32> = vis
                .surfaces
                .iter()
                .copied()
                .filter(|&si| {
                    world.dpvs.surfaces.get(si as usize).is_some_and(|s| {
                        let (a, b) = (Vec3::from(s.bounds[0]), Vec3::from(s.bounds[1]));
                        a.cmple(hi).all() && b.cmpge(lo).all()
                    })
                })
                .collect();
            nearest(&mut surfaces, c.centre, |i| {
                Vec3::from(world.dpvs.surfaces[i as usize].bounds[0])
                    .midpoint(Vec3::from(world.dpvs.surfaces[i as usize].bounds[1]))
            });
            let mut smodels: Vec<u32> = vis
                .smodels
                .iter()
                .copied()
                .filter(|&mi| {
                    world
                        .dpvs
                        .smodel_draw_insts
                        .get(mi as usize)
                        .is_some_and(|m| {
                            let r = m.model.as_ref().map_or(0.0, |m| m.radius) * m.scale;
                            let o = Vec3::from(m.origin);
                            (o - Vec3::splat(r)).cmple(hi).all()
                                && (o + Vec3::splat(r)).cmpge(lo).all()
                        })
                })
                .collect();
            nearest(&mut smodels, c.centre, |i| {
                Vec3::from(world.dpvs.smodel_draw_insts[i as usize].origin)
            });
            let others: Vec<DynSurf> = dynsurfs
                .iter()
                .copied()
                .filter(|d| {
                    let i = &insts[d.inst];
                    d.inst != inst
                        && i.casts_cookie
                        && (Vec3::from(i.origin) - Vec3::splat(i.model.radius))
                            .cmple(hi)
                            .all()
                        && (Vec3::from(i.origin) + Vec3::splat(i.model.radius))
                            .cmpge(lo)
                            .all()
                })
                .collect();
            let mut rf = frame.clone();
            rf.shadow_lookup = c.lookup;
            rf.vec[codeconst::SHADOW_PARMS as usize] = [0.0, 0.0, 0.0, 1.0];
            let mut receivers = self.build_draws(
                PassKind::CookieReceiver,
                &rf,
                &HashMap::new(),
                &Items {
                    surfaces: &surfaces,
                    smodels: &smodels,
                },
                scene_target,
                false,
                counts,
                view.origin,
            );
            receivers.extend(self.build_dynamic(
                PassKind::CookieReceiver,
                &rf,
                &HashMap::new(),
                insts,
                &others,
                ModelKind::World,
                scene_target,
                false,
            ));
            receivers.sort_by_key(|d| (d.order, d.prepared.id()));
            out.push(CookiePass {
                tile: tile as u32,
                casters,
                receivers,
            });
        }
        out
    }

    /// The pipelines that write alpha 0 or 1 over a scissor rectangle of a scene pass into `format` with `samples`.
    fn ensure_alpha_fill(&mut self, format: wgpu::TextureFormat, samples: u32) {
        let key = (format, samples);
        if self.alpha_fill.as_ref().is_some_and(|f| f.key == key) {
            return;
        }
        let dev = &self.gpu.device;
        let make = |alpha: &str| {
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("alpha fill"),
                source: wgpu::ShaderSource::Wgsl(
                    format!(
                        "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {{
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}}
@fragment fn fs() -> @location(0) vec4<f32> {{ return vec4<f32>(0.0, 0.0, 0.0, {alpha}); }}"
                    )
                    .into(),
                ),
            });
            dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("alpha fill"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALPHA,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples.max(1),
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        self.alpha_fill = Some(AlphaFill {
            key,
            zero: make("0.0"),
            one: make("1.0"),
        });
    }

    /// The draws of the dynamic lights worth drawing this frame: the surfaces, static models and dynamic models each
    /// reaches, with its technique (`light omni` or `light spot`) and its constants.
    #[allow(clippy::too_many_arguments)]
    fn build_light_passes(
        &mut self,
        lights: &[DynLight],
        frame: &FrameConsts,
        vm_proj: &Mat4,
        vis: &cull::Visible,
        frustum: &Frustum,
        insts: &[ModelInstance],
        dynsurfs: &[DynSurf],
        clip_from_eye: &Mat4,
        eye: Vec3,
        size: (u32, u32),
        target: Target,
        spot_lists: &mut Vec<(u32, Vec<Draw>)>,
        counts: &mut BuildCounts,
    ) -> Vec<LightPass> {
        if lights.is_empty() || self.settings.dlight_limit == 0 {
            return Vec::new();
        }
        let chosen = dlight::select(lights, eye, frustum, self.settings.dlight_limit);
        if chosen.is_empty() {
            return Vec::new();
        }
        self.ensure_alpha_fill(target.color.expect("scene colour"), target.samples);
        let world = self.scene.world.clone();
        let mut out = Vec::new();
        for i in chosen {
            let l = &lights[i];
            let Some(rect) = dlight::screen_rect(clip_from_eye, l.origin - eye, l.radius, size)
            else {
                continue;
            };
            let consts = l.consts(self.dlight_def.width, self.dlight_def.lookup_start);
            let mut lf = frame.clone();
            lf.set_light(&consts);
            let mut surfaces: Vec<u32> = vis
                .surfaces
                .iter()
                .copied()
                .filter(|&si| {
                    world.dpvs.surfaces.get(si as usize).is_some_and(|s| {
                        l.reaches_box(Vec3::from(s.bounds[0]), Vec3::from(s.bounds[1]))
                    })
                })
                .collect();
            nearest(&mut surfaces, l.origin, |i| {
                Vec3::from(world.dpvs.surfaces[i as usize].bounds[0])
                    .midpoint(Vec3::from(world.dpvs.surfaces[i as usize].bounds[1]))
            });
            let mut smodels: Vec<u32> = vis
                .smodels
                .iter()
                .copied()
                .filter(|&mi| {
                    world
                        .dpvs
                        .smodel_draw_insts
                        .get(mi as usize)
                        .is_some_and(|m| {
                            let r = m.model.as_ref().map_or(0.0, |m| m.radius) * m.scale;
                            l.reaches_sphere(Vec3::from(m.origin), r)
                        })
                })
                .collect();
            nearest(&mut smodels, l.origin, |i| {
                Vec3::from(world.dpvs.smodel_draw_insts[i as usize].origin)
            });
            let near: Vec<DynSurf> = dynsurfs
                .iter()
                .copied()
                .filter(|d| {
                    let inst = &insts[d.inst];
                    l.reaches_sphere(Vec3::from(inst.origin), inst.model.radius)
                })
                .collect();
            // A spot light of an effect takes a free tile of the shadow atlas, if shadows are on.
            let tile = (self.spot_shadow.is_some()
                && self.settings.dynamic_spot_shadows
                && spot_lists.len() < spotshadow::TILES as usize)
                .then_some(l.spot.as_ref())
                .flatten()
                .map(|c| {
                    let index = spot_lists.len() as u32;
                    let vp = spotshadow::view_proj(l.origin, c.dir, c.cos_outer, l.radius, c.near);
                    (index, vp)
                });
            if let Some((index, vp)) = tile {
                let lookup = spotshadow::lookup(&vp, index);
                lf.shadow_lookup = lookup;
                lf.vec[codeconst::SPOT_SHADOWMAP_PIXEL_ADJUST as usize] = spotshadow::PIXEL_ADJUST;
                lf.vec[codeconst::LIGHT_SPOTFACTORS as usize][3] = 1.0;
                let pf = self.spot_build_frame(&vp, l.radius, eye);
                let mut list = self.build_draws(
                    PassKind::ShadowMap,
                    &pf,
                    &HashMap::new(),
                    &Items {
                        surfaces: &surfaces,
                        smodels: &smodels,
                    },
                    self.shadow_target(),
                    false,
                    counts,
                    eye,
                );
                list.extend(self.build_dynamic(
                    PassKind::ShadowMap,
                    &pf,
                    &HashMap::new(),
                    insts,
                    &near,
                    ModelKind::World,
                    self.shadow_target(),
                    false,
                ));
                spot_lists.push((index, list));
            }
            let kind = PassKind::DynLight {
                spot: l.spot.is_some(),
                shadow: tile.is_some(),
            };
            let mut draws = self.build_draws(
                kind,
                &lf,
                &HashMap::new(),
                &Items {
                    surfaces: &surfaces,
                    smodels: &smodels,
                },
                target,
                false,
                counts,
                eye,
            );
            draws.extend(self.build_dynamic(
                kind,
                &lf,
                &HashMap::new(),
                insts,
                &near,
                ModelKind::World,
                target,
                false,
            ));
            let mut vm_frame = lf.clone();
            vm_frame.proj = *vm_proj;
            let vm_draws = self.build_dynamic(
                kind,
                &vm_frame,
                &HashMap::new(),
                insts,
                &near,
                ModelKind::ViewModel,
                target,
                false,
            );
            draws.sort_by_key(|d| (d.order, d.prepared.id()));
            out.push(LightPass {
                rect,
                draws,
                vm_draws,
            });
        }
        out
    }

    /// Draw the world seen from `view` into `target`.
    pub fn render(
        &mut self,
        view: &View,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> FrameStats {
        let t0 = Instant::now();
        self.ensure_depth(size);
        self.ensure_shadow();
        self.ring.clear();
        if let Some(t) = self.timer.as_mut() {
            t.begin_frame(&self.gpu);
        }
        let aspect = self
            .settings
            .aspect
            .unwrap_or(size.0 as f32 / size.1 as f32);
        let (v, p) = view.matrices(aspect);
        let mut frame = FrameConsts::new(v, p, view.origin);
        frame.set_sun(
            self.scene.sun_dir,
            self.scene.sun_color,
            if self.settings.specular { 1.0 } else { 0.0 },
        );
        frame.set_fog(if self.settings.fog {
            self.art.fog.as_ref()
        } else {
            None
        });
        if let Some(f) = self.art.fog.as_ref().filter(|_| self.settings.fog) {
            self.clear = f.color.map(f64::from);
        }
        self.materials.update_waters(&self.gpu, view.time);
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
        let mut meshes = std::mem::take(&mut self.dynamic_meshes);
        let caller_meshes = meshes.len();
        let sun_cam = sun::Cam {
            origin: view.origin,
            forward: view.forward(),
            clip: p * v,
            size,
            near: NEAR,
            time_ms: (view.time * 1000.0) as i32,
        };
        let probe_target = sun::ProbeTarget {
            color: format,
            depth: DEPTH_FORMAT,
            samples: self.samples(format),
        };
        let sun_quad = self.sun.begin_frame(
            &self.gpu,
            &world.sun,
            self.settings.draw_sun,
            &sun_cam,
            self.scene.collision.as_deref(),
            probe_target,
        );
        if let (Some(quad), Some(mat)) = (sun_quad, world.sun.sprite_material.clone()) {
            meshes.push(sun::sprite_mesh(mat, quad));
        }
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
                    PassKind::ShadowMap,
                    &pf,
                    &HashMap::new(),
                    &items,
                    target,
                    false,
                    &mut counts,
                    view.origin,
                );
                list.extend(self.build_dynamic(
                    PassKind::ShadowMap,
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

        // The spot lights that cast a shadow this frame, and their shadow maps' casters.
        self.ensure_spot_shadow();
        let dt = self
            .last_time
            .map_or(0.0, |t| (view.time - t).clamp(0.0, 0.1));
        self.last_time = Some(view.time);
        let mut spot_lists =
            self.build_spot_shadows(view, &vis, &insts, &dynsurfs, dt, &mut counts);
        let spot_primary_draws: usize = spot_lists.iter().map(|l| l.1.len()).sum();
        stats.shadow_draws += spot_primary_draws;

        // Frames of the lights that have spot or omni constants.
        let mut light_frames: HashMap<u8, FrameConsts> = HashMap::new();
        if self.settings.primary_lights {
            for (i, l) in self.lights.iter().enumerate() {
                if let Some(l) = l {
                    let mut f = frame.clone();
                    let mut consts = l.consts;
                    let tile = self.spot_tiles.get(&(i as u8));
                    if let Some(t) = tile {
                        consts.spot_factors[3] = t.fade;
                        f.shadow_lookup = t.lookup;
                        f.vec[codeconst::SPOT_SHADOWMAP_PIXEL_ADJUST as usize] =
                            spotshadow::PIXEL_ADJUST;
                    }
                    f.set_light(&consts);
                    light_frames.insert(i as u8, f);
                }
            }
        }
        let items = Items {
            surfaces: &vis.surfaces,
            smodels: &vis.smodels,
        };
        let samples = self.samples(format);
        self.ensure_msaa(size, format, samples);
        let scene_target = Target {
            color: Some(format),
            depth: Some(DEPTH_FORMAT),
            samples,
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
        let dlights = std::mem::take(&mut self.dynamic_lights);
        let light_passes = self.build_light_passes(
            &dlights,
            &frame,
            &vm_proj,
            &vis,
            &frustum,
            &insts,
            &dynsurfs,
            &(p * v),
            view.origin,
            size,
            scene_target,
            &mut spot_lists,
            &mut counts,
        );
        let cookies = self.build_cookies(
            view,
            &frame,
            &vis,
            &insts,
            &dynsurfs,
            scene_target,
            &mut counts,
        );
        stats.cookies = cookies.len();
        stats.draws += cookies.iter().map(|c| c.receivers.len()).sum::<usize>();
        stats.light_draws = light_passes
            .iter()
            .map(|l| l.draws.len() + l.vm_draws.len())
            .sum();
        stats.lights = light_passes.len();
        stats.shadow_draws +=
            spot_lists.iter().map(|l| l.1.len()).sum::<usize>() - spot_primary_draws;
        self.dynamic_lights = dlights;
        stats.models += insts.len();
        stats.models = counts.models;
        let (deferred, demand) = self.materials.take_deferred();
        stats.pipelines_missing = counts.missing + deferred;
        if let Some(w) = self.warm.as_mut() {
            w.demand.extend(demand);
        }
        stats.draws += draws.len();

        // The post chain: depth of field wants the scene's depth as a second list of the same surfaces.
        let floatz = self.post_params().dof_active().then(|| {
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
                    samples: 1,
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
                    samples: 1,
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
        if let (false, Some(atlas)) = (cookies.is_empty(), &self.cookie) {
            let timestamps = self.timer.as_mut().and_then(|t| t.pass("shadow cookies"));
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow cookies"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &atlas.color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Grey: no shadow where nothing was drawn.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.5,
                            g: 0.5,
                            b: 0.5,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &atlas.depth,
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
            for c in &cookies {
                let vp = cookie::viewport(c.tile);
                rp.set_viewport(vp[0], vp[1], vp[2], vp[3], 0.0, 1.0);
                record(
                    &mut rp,
                    &c.casters,
                    &self.vs_bg,
                    &self.ps_bg,
                    None,
                    &self.dyn_vb,
                );
            }
        }
        if let (false, Some(sh)) = (spot_lists.is_empty(), &self.spot_shadow) {
            let color_att = sh.color.as_ref().map(|v| wgpu::RenderPassColorAttachment {
                view: v,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            });
            let timestamps = self.timer.as_mut().and_then(|t| t.pass("spot shadow"));
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("spot shadow"),
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
            for (tile, list) in &spot_lists {
                let vp = spotshadow::viewport(*tile);
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
            let final_view = chain.scene.as_ref().unwrap_or(target);
            let (depth, color, resolve) = match &self.msaa {
                Some(m) => (&m.depth, &m.color, Some(final_view)),
                None => (&self.depth.as_ref().expect("depth").0, final_view, None),
            };
            let timestamps = self.timer.as_mut().and_then(|t| t.pass("scene"));
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: self.clear[0],
                            g: self.clear[1],
                            b: self.clear[2],
                            a: 1.0,
                        }),
                        store: if resolve.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
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
                occlusion_query_set: self.sun.query_set(),
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
            self.sun.draw_probe(&mut rp, size);
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
            if !cookies.is_empty() {
                rp.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
                for c in &cookies {
                    record(
                        &mut rp,
                        &c.receivers,
                        &self.vs_bg,
                        &self.ps_bg,
                        None,
                        &self.dyn_vb,
                    );
                }
            }
            if let Some(fill) = self
                .alpha_fill
                .as_ref()
                .filter(|_| !light_passes.is_empty())
            {
                for lp in &light_passes {
                    let [x, y, w, h] = lp.rect;
                    rp.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
                    rp.set_scissor_rect(x, y, w, h);
                    rp.set_pipeline(&fill.zero);
                    rp.draw(0..3, 0..1);
                    record(
                        &mut rp,
                        &lp.draws,
                        &self.vs_bg,
                        &self.ps_bg,
                        None,
                        &self.dyn_vb,
                    );
                    if !lp.vm_draws.is_empty() {
                        rp.set_viewport(
                            0.0,
                            0.0,
                            size.0 as f32,
                            size.1 as f32,
                            0.0,
                            VIEWMODEL_DEPTH,
                        );
                        record(
                            &mut rp,
                            &lp.vm_draws,
                            &self.vs_bg,
                            &self.ps_bg,
                            None,
                            &self.dyn_vb,
                        );
                        rp.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
                    }
                    rp.set_pipeline(&fill.one);
                    rp.draw(0..3, 0..1);
                }
                rp.set_scissor_rect(0, 0, size.0, size.1);
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
        self.sun.resolve(&mut enc);
        if let Some(t) = self.timer.as_mut() {
            t.resolve(&mut enc);
        }
        self.gpu.queue.submit([enc.finish()]);
        self.sun.submitted();
        if let Some(t) = self.timer.as_mut() {
            t.submitted();
            stats.gpu_ms = t.last_total_ms;
            stats.frame = t.frame();
        }
        self.dynamic_models = insts;
        meshes.truncate(caller_meshes);
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

/// The techniques of a dynamic light's pass.
fn light_techs(spot: bool, shadow: bool) -> &'static [usize] {
    match (spot, shadow) {
        (false, _) => &LIGHT_OMNI_TECHS,
        (true, false) => &LIGHT_SPOT_TECHS,
        (true, true) => &LIGHT_SPOT_SHADOW_TECHS,
    }
}

/// The `light` key of the texture group of a draw in pass `kind` of a surface lit by primary light `light`.
fn tex_light(kind: PassKind, light: u8) -> u16 {
    if kind.is_light() {
        DLIGHT_KEY
    } else {
        u16::from(light)
    }
}

/// A shadow map of `size` texels for `mode`: a depth texture the shaders compare against, or a depth buffer and a
/// float colour texture that carries the depth.
fn shadow_targets(gpu: &Gpu, mode: ShadowMode, size: (u32, u32), label: &str) -> ShadowTargets {
    let extent = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
    let make = |name: &str, format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("{label} {name}")),
            size: extent,
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
        let t = make("depth", DEPTH_FORMAT, attach | bind);
        let v = t.create_view(&Default::default());
        (v.clone(), None, v)
    } else {
        let d = make("z", DEPTH_FORMAT, attach);
        let c = make("color", SHADOW_COLOR_FORMAT, attach | bind);
        let cv = c.create_view(&Default::default());
        (d.create_view(&Default::default()), Some(cv.clone()), cv)
    };
    ShadowTargets {
        mode,
        depth,
        color,
        tex: Arc::new(Tex {
            view: sampled,
            dim: SamplerDim::D2,
            width: size.0,
        }),
    }
}

/// Keeps the [`MAX_LIGHT_SURFACES`] items nearest `centre` (by `at`), so a light's list does not depend on the order the
/// visible set came in.
fn nearest(items: &mut Vec<u32>, centre: Vec3, at: impl Fn(u32) -> Vec3) {
    if items.len() > MAX_LIGHT_SURFACES {
        items.sort_by(|&a, &b| {
            at(a)
                .distance_squared(centre)
                .total_cmp(&at(b).distance_squared(centre))
        });
        items.truncate(MAX_LIGHT_SURFACES);
    }
}
