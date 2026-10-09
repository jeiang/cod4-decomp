// SPDX-License-Identifier: GPL-3.0-only
//! Techset passes to GPU draw state: shader translation, constant and sampler binding, pipelines.
//!
//! A [`Prepared`] is one (material, technique, vertex layout): the translated vertex and pixel shaders, the register
//! writes that fill their constant banks, where each sampler's texture comes from, and the pipeline (built lazily per
//! target format). Translation happens the first time a technique is prepared, which the world load does for every
//! surface material up front.

use crate::codeconst::{self, FrameConsts, Object};
use crate::gpu::Gpu;
use crate::state::StateBits;
use crate::texture::{Tex, TextureCache};
use crate::water::WaterSim;
use assets::zone::gfx::{ArgValue, Material, Pass, TechniqueSet, TextureSource};
use sm3::{Options, SamplerDim, Stage, Translation, VertexFix};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use web_time::Instant;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Which sampler object a texture binding uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SamplerKey {
    /// A material sampler-state byte (filter, mip and clamp bits).
    State(u8),
    /// Depth-texture comparison with bilinear filtering: hardware shadow-map lookups (`hsm` techniques).
    ShadowCompare,
    /// Nearest, clamped, unfiltered: the colour-encoded shadow map of the `sm` techniques.
    ShadowRaw,
}

impl From<u8> for SamplerKey {
    fn from(s: u8) -> Self {
        SamplerKey::State(s)
    }
}

/// How a pixel shader reads a texture binding, which fixes the bind group layout entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Fetch {
    /// Filtered float texture.
    Filtered,
    /// A depth texture sampled with a comparison sampler.
    Compare,
    /// An unfilterable float texture with a non-filtering sampler.
    Raw,
}

/// Bytes of one constant bank slice (256 `vec4`s).
pub const BANK_BYTES: u64 = 4096;

/// Which vertex buffer layout a draw uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexKind {
    /// 44-byte `GfxWorldVertex`.
    World,
    /// 32-byte `GfxPackedVertex` (xmodel surfaces).
    Model,
    /// 28-byte full-screen quad vertex: position at 0, colour at 16, texture coordinates at 20.
    Screen,
    /// 20-byte `GfxPosTexVertex` (a particle cloud's sprites): position at 0, texture coordinates at 12.
    Cloud,
}

impl VertexKind {
    pub fn stride(self) -> u64 {
        match self {
            VertexKind::World => 44,
            VertexKind::Model => 32,
            VertexKind::Screen => 28,
            VertexKind::Cloud => 20,
        }
    }

    /// How the stock vertex shaders expect the byte-packed streams: normals, tangents and packed texture
    /// coordinates as unnormalized bytes, colours as `D3DCOLOR`.
    fn fixes(self) -> BTreeMap<(u32, u32), VertexFix> {
        let mut m = BTreeMap::from([
            ((3, 0), VertexFix::Scale255),
            ((6, 0), VertexFix::Scale255),
            ((5, 2), VertexFix::Scale255),
            ((10, 0), VertexFix::Bgra),
            ((10, 1), VertexFix::Bgra),
        ]);
        if self == VertexKind::Model {
            m.insert((5, 0), VertexFix::Scale255);
        }
        m
    }

    /// Attribute (format, offset) for a stream source id of the vertex declaration routing.
    fn source(self, src: u8) -> Option<(wgpu::VertexFormat, u64)> {
        use wgpu::VertexFormat as F;
        Some(match (self, src) {
            (_, 0) => (F::Float32x3, 0),
            (VertexKind::Cloud, 1) => return None,
            (VertexKind::Cloud, 2) => (F::Float32x2, 12),
            (_, 1) => (F::Unorm8x4, 16),
            (VertexKind::World, 2) => (F::Float32x4, 20),
            (VertexKind::World, 3) => (F::Unorm8x4, 36),
            (VertexKind::World, 4) => (F::Unorm8x4, 40),
            (VertexKind::World, 5) => (F::Float32x2, 28),
            (VertexKind::Model, 2) => (F::Unorm8x4, 20),
            (VertexKind::Model, 3) => (F::Unorm8x4, 24),
            (VertexKind::Model, 4) => (F::Unorm8x4, 28),
            (VertexKind::Screen, 2) => (F::Float32x2, 20),
            _ => return None,
        })
    }
}

/// Where a vertex shader input comes from, as a destination slot of the declaration routing.
fn dest_slot(usage: u32, index: u32) -> Option<u8> {
    Some(match usage {
        0 => 0,
        3 => 1,
        10 => 2 + index as u8,
        5 => 4 + index as u8,
        6 => 4 + index as u8,
        _ => return None,
    })
}

pub struct Compiled {
    pub module: wgpu::ShaderModule,
    pub tr: Translation,
}

pub(crate) enum TexSource {
    Image(Arc<Tex>, u8),
    /// A water height map, by its key in [`Materials::waters`].
    Water(usize, u8),
    Code(u32),
    Missing(u8),
}

/// One sampler register the pixel shader reads.
pub struct SamplerSlot {
    pub register: u32,
    pub dim: SamplerDim,
    pub fetch: Fetch,
    pub(crate) source: TexSource,
    fallback: [u8; 4],
}

/// Attachment formats a pipeline renders into; `color` is `None` for depth-only passes, `depth` for the full-screen
/// post passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub color: Option<wgpu::TextureFormat>,
    pub depth: Option<wgpu::TextureFormat>,
    /// Samples per pixel of the attachments (1, 2 or 4).
    pub samples: u32,
}

/// Technique flag: the shader samples `RESOLVED_POST_SUN` (heat haze, shockwaves); drawn only with distortion on.
pub const TECH_NEEDS_POST_SUN: u16 = 0x1;
/// Technique flag: the shader samples `FLOATZ` to fade where it meets geometry (soft particles).
pub const TECH_Z_FEATHER: u16 = 0x20;

pub struct Prepared {
    pub name: String,
    pub technique: usize,
    /// The technique's flags: see [`TECH_NEEDS_POST_SUN`] and [`TECH_Z_FEATHER`].
    pub flags: u16,
    pub vs: Arc<Compiled>,
    pub ps: Arc<Compiled>,
    pub state: StateBits,
    pub kind: VertexKind,
    pub slots: Vec<SamplerSlot>,
    /// Group 2 layout for this pass's samplers.
    pub tex_layout: Arc<wgpu::BindGroupLayout>,
    attrs: Vec<wgpu::VertexAttribute>,
    vs_static: Vec<(u32, [f32; 4])>,
    ps_static: Vec<(u32, [f32; 4])>,
    vs_code: Vec<(u32, u32, u32)>,
    ps_code: Vec<(u32, u32, u32)>,
    pipelines: Mutex<HashMap<Target, Arc<wgpu::RenderPipeline>>>,
    id: u32,
}

impl Prepared {
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Fill a vertex bank of [`Prepared::vs_regs`] registers.
    pub fn fill_vs(&self, out: &mut [[f32; 4]], frame: &FrameConsts, obj: &Object) {
        fill(out, &self.vs_static, &self.vs_code, frame, obj);
    }

    /// Fill a pixel bank of [`Prepared::ps_regs`] registers.
    pub fn fill_ps(&self, out: &mut [[f32; 4]], frame: &FrameConsts, obj: &Object) {
        fill(out, &self.ps_static, &self.ps_code, frame, obj);
    }

    /// Registers the vertex bank needs, rounded up to whole 256-byte steps.
    pub fn vs_regs(&self) -> usize {
        regs(&self.vs_static, &self.vs_code)
    }

    pub fn ps_regs(&self) -> usize {
        regs(&self.ps_static, &self.ps_code)
    }

    fn object_dependent(code: &[(u32, u32, u32)]) -> bool {
        code.iter().any(|&(_, id, _)| {
            id >= codeconst::FIRST_MATRIX
                || matches!(
                    id,
                    codeconst::BASE_LIGHTING_COORDS
                        | codeconst::PARTICLE_CLOUD_MATRIX
                        | codeconst::PARTICLE_CLOUD_COLOR
                )
        })
    }

    /// Whether any vertex-stage constant depends on the object.
    pub fn vs_per_object(&self) -> bool {
        Self::object_dependent(&self.vs_code)
    }

    /// Whether any pixel-stage constant depends on the object.
    pub fn ps_per_object(&self) -> bool {
        Self::object_dependent(&self.ps_code)
    }

    pub fn ps_wgsl(&self) -> &str {
        &self.ps.tr.wgsl
    }

    pub fn vs_wgsl(&self) -> &str {
        &self.vs.tr.wgsl
    }

    /// Human-readable binding table, for debugging.
    pub fn describe(&self) -> String {
        use std::fmt::Write;
        let mut o = format!(
            "{} technique {} state {:?}\n",
            self.name, self.technique, self.state.0
        );
        for (stage, c, st) in [
            ("VS", &self.vs_code, &self.vs_static),
            ("PS", &self.ps_code, &self.ps_static),
        ] {
            let refl = if stage == "VS" {
                &self.vs.tr.reflection
            } else {
                &self.ps.tr.reflection
            };
            for &(r, id, row) in c {
                let _ = writeln!(
                    o,
                    "  {stage} c{r} <- code {} row {row} (ctab {:?})",
                    codeconst::NAMES.get(id as usize).unwrap_or(&"?"),
                    refl.constant_name(r)
                );
            }
            for (r, v) in st {
                let _ = writeln!(
                    o,
                    "  {stage} c{r} <- {v:?} (ctab {:?})",
                    refl.constant_name(*r)
                );
            }
        }
        for s in &self.slots {
            let src = match &s.source {
                TexSource::Image(..) => "material image".to_owned(),
                TexSource::Water(..) => "water height map".to_owned(),
                TexSource::Code(id) => format!(
                    "code {}",
                    codeconst::TEXTURE_NAMES.get(*id as usize).unwrap_or(&"?")
                ),
                TexSource::Missing(_) => "missing".to_owned(),
            };
            let _ = writeln!(
                o,
                "  s{} {:?} {:?} <- {src}",
                s.register,
                self.ps.tr.reflection.sampler_name(s.register),
                s.dim
            );
        }
        o
    }

    pub fn code_textures(&self) -> impl Iterator<Item = u32> + '_ {
        self.slots.iter().filter_map(|s| match s.source {
            TexSource::Code(id) => Some(id),
            _ => None,
        })
    }
}

/// Registers a bank must hold: one past the highest written, in 16-register (256-byte) steps, at least one step.
fn regs(statics: &[(u32, [f32; 4])], code: &[(u32, u32, u32)]) -> usize {
    let top = statics
        .iter()
        .map(|&(r, _)| r)
        .chain(code.iter().map(|&(r, _, _)| r))
        .max()
        .map_or(0, |r| r as usize + 1);
    top.next_multiple_of(16).max(16)
}

fn fill(
    out: &mut [[f32; 4]],
    statics: &[(u32, [f32; 4])],
    code: &[(u32, u32, u32)],
    frame: &FrameConsts,
    obj: &Object,
) {
    for &(r, v) in statics {
        out[r as usize & 255] = v;
    }
    for &(r, id, row) in code {
        out[r as usize & 255] = frame.value(id, row, obj);
    }
}

/// Program address, alpha test, vertex layout (vertex shaders only).
type ShaderKey = (usize, Option<[u32; 2]>, Option<VertexKind>, bool);

/// Caches shared by all prepared passes.
pub struct Materials {
    shaders: HashMap<ShaderKey, Option<Arc<Compiled>>>,
    layouts: HashMap<Vec<(u32, SamplerDim, Fetch)>, Arc<wgpu::BindGroupLayout>>,
    prepared: HashMap<(usize, usize, VertexKind, bool), Option<Arc<Prepared>>>,
    samplers: HashMap<SamplerKey, Arc<wgpu::Sampler>>,
    pub vs_layout: wgpu::BindGroupLayout,
    pub ps_layout: wgpu::BindGroupLayout,
    techsets: HashMap<String, Arc<TechniqueSet>>,
    next_id: u32,
    deadline: Mutex<Option<Instant>>,
    deferred: AtomicUsize,
    demand: Mutex<HashSet<(u32, Target)>>,
    /// Material name to why it could not be prepared.
    pub failures: BTreeMap<String, String>,
    /// The ocean simulations of the water textures prepared so far, by `Arc` address of their [`Water`].
    pub waters: HashMap<usize, WaterSim>,
    /// `r_texFilterAnisoMax`: the most anisotropic filtering a sampler asks for; set before the samplers are made.
    pub aniso_max: u16,
}

fn bank_entry(
    binding: u32,
    stage: wgpu::ShaderStages,
    dynamic: bool,
    size: u64,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stage,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    }
}

impl Materials {
    pub fn new(gpu: &Gpu) -> Self {
        let vs_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vs constants"),
                entries: &[bank_entry(0, wgpu::ShaderStages::VERTEX, true, BANK_BYTES)],
            });
        let ps_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ps constants"),
                entries: &[
                    bank_entry(0, wgpu::ShaderStages::FRAGMENT, true, BANK_BYTES),
                    bank_entry(1, wgpu::ShaderStages::FRAGMENT, false, 32),
                ],
            });
        Materials {
            shaders: HashMap::new(),
            layouts: HashMap::new(),
            prepared: HashMap::new(),
            samplers: HashMap::new(),
            vs_layout,
            ps_layout,
            techsets: HashMap::new(),
            next_id: 0,
            deadline: Mutex::new(None),
            deferred: AtomicUsize::new(0),
            demand: Mutex::new(HashSet::new()),
            failures: BTreeMap::new(),
            waters: HashMap::new(),
            aniso_max: 16,
        }
    }

    fn compile(
        &mut self,
        gpu: &Gpu,
        program: &[u32],
        alpha: Option<sm3::AlphaTest>,
        vertex: Option<VertexKind>,
        hsm: bool,
    ) -> Option<Arc<Compiled>> {
        let key = (
            program.as_ptr() as usize,
            alpha.map(|a| [a.func as u32, a.reference.to_bits()]),
            vertex,
            hsm,
        );
        if let Some(c) = self.shaders.get(&key) {
            return c.clone();
        }
        let bytes: Vec<u8> = program.iter().flat_map(|t| t.to_le_bytes()).collect();
        let opts = Options {
            vertex_fixes: vertex.map(VertexKind::fixes).unwrap_or_default(),
            alpha_test: alpha,
            ..Options::default()
        };
        let mut translated = sm3::translate(&bytes, &opts).ok();
        if hsm
            && vertex.is_none()
            && let Some(tr) = &translated
        {
            // Hardware shadow maps: the shadow-map samplers become depth textures with comparison.
            let cmp: std::collections::BTreeSet<u32> = tr
                .reflection
                .samplers
                .keys()
                .copied()
                .filter(|&r| {
                    tr.reflection
                        .sampler_name(r)
                        .is_some_and(|n| n.to_ascii_lowercase().starts_with("shadowmapsampler"))
                })
                .collect();
            if !cmp.is_empty() {
                let opts = Options {
                    comparison_samplers: cmp,
                    ..opts
                };
                translated = sm3::translate(&bytes, &opts).ok();
            }
        }
        let compiled = translated.map(|tr| {
            let module = gpu
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: None,
                    source: wgpu::ShaderSource::Wgsl(tr.wgsl.as_str().into()),
                });
            Arc::new(Compiled { module, tr })
        });
        self.shaders.insert(key, compiled.clone());
        compiled
    }

    pub fn sampler(&mut self, gpu: &Gpu, key: SamplerKey) -> Arc<wgpu::Sampler> {
        self.samplers
            .entry(key)
            .or_insert_with(|| {
                let state = match key {
                    SamplerKey::State(s) => s,
                    SamplerKey::ShadowCompare => {
                        return Arc::new(gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                            label: Some("shadow compare"),
                            address_mode_u: wgpu::AddressMode::ClampToEdge,
                            address_mode_v: wgpu::AddressMode::ClampToEdge,
                            mag_filter: wgpu::FilterMode::Linear,
                            min_filter: wgpu::FilterMode::Linear,
                            compare: Some(wgpu::CompareFunction::LessEqual),
                            ..Default::default()
                        }));
                    }
                    SamplerKey::ShadowRaw => {
                        return Arc::new(gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                            label: Some("shadow raw"),
                            address_mode_u: wgpu::AddressMode::ClampToEdge,
                            address_mode_v: wgpu::AddressMode::ClampToEdge,
                            ..Default::default()
                        }));
                    }
                };
                let s = u32::from(state);
                let addr = |bit: u32| {
                    if s & bit != 0 {
                        wgpu::AddressMode::ClampToEdge
                    } else {
                        wgpu::AddressMode::Repeat
                    }
                };
                let nearest = s & 7 == 1;
                let filter = if nearest {
                    wgpu::FilterMode::Nearest
                } else {
                    wgpu::FilterMode::Linear
                };
                let mip = match (s >> 3) & 3 {
                    0 => None,
                    1 => Some(wgpu::MipmapFilterMode::Nearest),
                    _ => Some(wgpu::MipmapFilterMode::Linear),
                };
                let aniso = match s & 7 {
                    3 => 2,
                    4 => 4,
                    _ => 1,
                }
                .min(self.aniso_max.max(1));
                Arc::new(gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                    address_mode_u: addr(0x20),
                    address_mode_v: addr(0x40),
                    address_mode_w: addr(0x80),
                    mag_filter: filter,
                    min_filter: filter,
                    mipmap_filter: mip.unwrap_or(wgpu::MipmapFilterMode::Nearest),
                    lod_max_clamp: if mip.is_none() { 0.0 } else { 32.0 },
                    anisotropy_clamp: if mip == Some(wgpu::MipmapFilterMode::Linear) && !nearest {
                        aniso
                    } else {
                        1
                    },
                    ..Default::default()
                }))
            })
            .clone()
    }

    fn tex_layout(
        &mut self,
        gpu: &Gpu,
        sig: Vec<(u32, SamplerDim, Fetch)>,
    ) -> Arc<wgpu::BindGroupLayout> {
        self.layouts
            .entry(sig.clone())
            .or_insert_with(|| {
                let mut entries = Vec::new();
                for &(reg, dim, fetch) in &sig {
                    let sample_type = match fetch {
                        Fetch::Filtered => wgpu::TextureSampleType::Float { filterable: true },
                        Fetch::Compare => wgpu::TextureSampleType::Depth,
                        Fetch::Raw => wgpu::TextureSampleType::Float { filterable: false },
                    };
                    let sampler = match fetch {
                        Fetch::Filtered => wgpu::SamplerBindingType::Filtering,
                        Fetch::Compare => wgpu::SamplerBindingType::Comparison,
                        Fetch::Raw => wgpu::SamplerBindingType::NonFiltering,
                    };
                    entries.push(wgpu::BindGroupLayoutEntry {
                        binding: reg,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type,
                            view_dimension: match dim {
                                SamplerDim::D2 => wgpu::TextureViewDimension::D2,
                                SamplerDim::Cube => wgpu::TextureViewDimension::Cube,
                                SamplerDim::D3 => wgpu::TextureViewDimension::D3,
                            },
                            multisampled: false,
                        },
                        count: None,
                    });
                    entries.push(wgpu::BindGroupLayoutEntry {
                        binding: reg + sm3::SAMPLER_BINDING_OFFSET,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(sampler),
                        count: None,
                    });
                }
                Arc::new(
                    gpu.device
                        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                            label: Some("textures"),
                            entries: &entries,
                        }),
                )
            })
            .clone()
    }

    /// Register technique sets by name, for materials whose own set is a by-name reference to another zone.
    pub fn add_techsets(&mut self, sets: &[Arc<TechniqueSet>]) {
        for t in sets {
            if let Some(n) = &t.name
                && t.techniques.iter().any(Option::is_some)
            {
                self.techsets
                    .insert(n.trim_start_matches(',').to_owned(), t.clone());
            }
        }
    }

    /// The material's technique set; `hsm` selects the hardware-shadow-map twin (`..._hsm_...`) of a `..._sm_...` set
    /// when the loaded zones have it.
    fn techset(&self, mat: &Material, hsm: bool) -> Option<Arc<TechniqueSet>> {
        let own = mat.technique_set.as_ref()?;
        let name = own.name.as_deref()?.trim_start_matches(',');
        if hsm
            && let Some(twin) = name
                .contains("_sm_")
                .then(|| self.techsets.get(&name.replace("_sm_", "_hsm_")))
                .flatten()
        {
            return Some(twin.clone());
        }
        // A material of one zone may carry a stub of a techset another zone defines (no shader tokens).
        let complete = own.techniques.iter().flatten().any(|t| {
            t.passes.iter().any(|p| {
                p.vertex_shader
                    .as_ref()
                    .is_some_and(|v| !v.program.is_empty())
            })
        });
        if complete {
            return Some(own.clone());
        }
        self.techsets.get(name).cloned().or_else(|| {
            own.techniques
                .iter()
                .any(Option::is_some)
                .then(|| own.clone())
        })
    }

    /// Advance every water simulation to `time` seconds.
    pub fn update_waters(&mut self, gpu: &Gpu, time: f32) {
        for w in self.waters.values_mut() {
            w.update(gpu, time);
        }
    }

    /// Whether the material's techset has technique `tech`.
    pub fn has_technique(&self, mat: &Material, tech: usize, hsm: bool) -> bool {
        self.techset(mat, hsm).is_some_and(|set| {
            set.techniques
                .get(tech)
                .is_some_and(|x| x.as_ref().is_some_and(|x| !x.passes.is_empty()))
        })
    }

    /// The first technique of `techs` the material's techset has, as a drawable pass. Cached per material.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        textures: &mut TextureCache,
        mat: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
        hsm: bool,
    ) -> Option<Arc<Prepared>> {
        let set = self.techset(mat, hsm)?;
        let tech = techs.iter().copied().find(|&t| {
            set.techniques
                .get(t)
                .is_some_and(|x| x.as_ref().is_some_and(|x| !x.passes.is_empty()))
        });
        let Some(tech) = tech else {
            let have: Vec<usize> = (0..set.techniques.len())
                .filter(|&i| set.techniques[i].is_some())
                .collect();
            self.failures.insert(
                mat.name.as_deref().unwrap_or("?").to_owned(),
                format!(
                    "techset {:?} has techniques {have:?}, wanted {techs:?}",
                    set.name
                ),
            );
            return None;
        };
        let key = (Arc::as_ptr(mat) as usize, tech, kind, hsm);
        if let Some(p) = self.prepared.get(&key) {
            return p.clone();
        }
        let name = mat.name.as_deref().unwrap_or("?").to_owned();
        let built = self.build(gpu, textures, mat, tech, kind, hsm);
        let p = match built {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                self.failures.insert(name, e);
                None
            }
        };
        self.prepared.insert(key, p.clone());
        p
    }

    fn build(
        &mut self,
        gpu: &Gpu,
        textures: &mut TextureCache,
        mat: &Arc<Material>,
        tech: usize,
        kind: VertexKind,
        hsm: bool,
    ) -> Result<Prepared, String> {
        let set = self.techset(mat, hsm).ok_or("no techset")?;
        let technique = set.techniques[tech].as_ref().ok_or("no technique")?;
        let pass: &Pass = &technique.passes[0];
        let state = mat
            .state_bits_entry
            .get(tech)
            .and_then(|&i| mat.state_bits.get(usize::from(i)))
            .map_or(StateBits([0x800 | 0x8000 | 0x1800_0000, 0xD]), |&b| {
                StateBits(b)
            });
        let vsh = pass.vertex_shader.as_ref().ok_or("no vertex shader")?;
        let psh = pass.pixel_shader.as_ref().ok_or("no pixel shader")?;
        let vs = self
            .compile(gpu, &vsh.program, None, Some(kind), hsm)
            .ok_or("vertex shader does not translate")?;
        let ps = self
            .compile(gpu, &psh.program, state.alpha_test(), None, hsm)
            .ok_or("pixel shader does not translate")?;
        if vs.tr.stage != Stage::Vertex || ps.tr.stage != Stage::Pixel {
            return Err("shader stage mismatch".into());
        }

        let literal_of = |hash: u32| {
            mat.constants
                .iter()
                .find(|c| c.name_hash == hash)
                .map_or([0.0; 4], |c| c.literal)
        };
        let (mut vs_static, mut ps_static) = (Vec::new(), Vec::new());
        let (mut vs_code, mut ps_code) = (Vec::new(), Vec::new());
        let mut sampler_src: HashMap<u32, TexSource> = HashMap::new();
        for a in &pass.args {
            let dest = u32::from(a.dest);
            match (&a.value, a.kind) {
                (ArgValue::NameHash(h), 0) => vs_static.push((dest, literal_of(*h))),
                (ArgValue::NameHash(h), 6) => ps_static.push((dest, literal_of(*h))),
                (ArgValue::Literal(l), 1) => vs_static.push((dest, **l)),
                (ArgValue::Literal(l), 7) => ps_static.push((dest, **l)),
                (
                    ArgValue::CodeConst {
                        index,
                        first_row,
                        row_count,
                    },
                    k @ (3 | 5),
                ) => {
                    let out = if k == 3 { &mut vs_code } else { &mut ps_code };
                    for r in 0..u32::from((*row_count).max(1)) {
                        out.push((dest + r, u32::from(*index), u32::from(*first_row) + r));
                    }
                }
                (ArgValue::NameHash(h), 2) => {
                    let tex = mat.textures.iter().find(|t| t.name_hash == *h);
                    let src = match tex.map(|t| (&t.source, t.sampler_state)) {
                        Some((TextureSource::Image(Some(img)), st)) => {
                            match textures.image(gpu, img) {
                                Some(t) => TexSource::Image(t, st),
                                None => TexSource::Missing(st),
                            }
                        }
                        Some((TextureSource::Water(Some(w)), st)) => {
                            let key = Arc::as_ptr(w) as usize;
                            if let std::collections::hash_map::Entry::Vacant(e) =
                                self.waters.entry(key)
                                && let Some(sim) = WaterSim::new(gpu, w.clone())
                            {
                                e.insert(sim);
                            }
                            if self.waters.contains_key(&key) {
                                TexSource::Water(key, st)
                            } else {
                                TexSource::Missing(st)
                            }
                        }
                        Some((_, st)) => TexSource::Missing(st),
                        None => TexSource::Missing(0x72),
                    };
                    sampler_src.insert(dest, src);
                }
                (ArgValue::CodeSampler(id), 4) => {
                    sampler_src.insert(dest, TexSource::Code(*id));
                }
                _ => {}
            }
        }

        // Texture slots follow what the pixel shader actually samples.
        let mut slots = Vec::new();
        for (&reg, u) in &ps.tr.reflection.samplers {
            let name = ps
                .tr
                .reflection
                .sampler_name(reg)
                .unwrap_or("")
                .to_ascii_lowercase();
            let fallback = if name.contains("normal") {
                [128, 128, 255, 255]
            } else if u.dim == SamplerDim::Cube {
                [60, 70, 80, 255]
            } else if name.contains("modellighting") || name.contains("floatz") {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            };
            let source = match sampler_src.remove(&reg) {
                Some(s) => s,
                None => match codeconst::texture_from_ctab_name(&name) {
                    Some(id) => TexSource::Code(id),
                    None => TexSource::Missing(0x72),
                },
            };
            let fetch = match &source {
                _ if u.comparison => Fetch::Compare,
                // The shadow map's colour encoding and the 32-bit float-Z are not filterable.
                TexSource::Code(id)
                    if *id == codeconst::tex::SHADOWMAP_SUN
                        || *id == codeconst::tex::SHADOWMAP_SPOT
                        || *id == codeconst::tex::FLOATZ =>
                {
                    Fetch::Raw
                }
                _ => Fetch::Filtered,
            };
            slots.push(SamplerSlot {
                register: reg,
                dim: u.dim,
                fetch,
                source,
                fallback,
            });
        }
        let tex_layout = self.tex_layout(
            gpu,
            slots.iter().map(|s| (s.register, s.dim, s.fetch)).collect(),
        );

        // Vertex attributes: shader input semantic -> routing destination -> source stream -> buffer offset.
        let decl = pass.vertex_decl.as_ref().ok_or("no vertex declaration")?;
        let routing = &decl.routing[..usize::from(decl.stream_count).min(16)];
        let mut attrs = Vec::new();
        for input in &vs.tr.reflection.inputs {
            let found = dest_slot(input.usage, input.index)
                .and_then(|d| routing.iter().find(|r| r.1 == d))
                .and_then(|r| kind.source(r.0));
            // An input the declaration does not route reads the position stream: valid, and unused by lit shading.
            let (format, offset) = found.unwrap_or((wgpu::VertexFormat::Float32x3, 0));
            attrs.push(wgpu::VertexAttribute {
                format,
                offset,
                shader_location: input.location,
            });
        }
        self.next_id += 1;
        Ok(Prepared {
            name: mat.name.as_deref().unwrap_or("?").to_owned(),
            technique: tech,
            flags: technique.flags,
            vs,
            ps,
            state,
            kind,
            slots,
            tex_layout,
            attrs,
            vs_static,
            ps_static,
            vs_code,
            ps_code,
            pipelines: Mutex::new(HashMap::new()),
            id: self.next_id,
        })
    }

    /// Stop building pipelines in [`Materials::pipeline`] after `deadline` (`None`: never stop). Shader linking can
    /// dominate a frame (serial on WebGL2), so a client that must keep presenting spreads the builds over frames:
    /// draws whose pipeline is not ready are skipped and remembered as demanded, see [`Materials::take_demand`].
    pub fn set_deadline(&self, deadline: Option<Instant>) {
        *lock(&self.deadline) = deadline;
    }

    /// How many [`Materials::pipeline`] calls came back empty since the last call, and which pipelines they wanted.
    pub fn take_deferred(&self) -> (usize, HashSet<(u32, Target)>) {
        let demand = std::mem::take(&mut *lock(&self.demand));
        (self.deferred.swap(0, Ordering::Relaxed), demand)
    }

    /// Whether `p` has a pipeline for `target` already.
    pub fn has_pipeline(&self, p: &Prepared, target: Target) -> bool {
        lock(&p.pipelines).contains_key(&target)
    }

    /// The pipeline for `p` rendering into `target`, built on first use; `None` when it is not built and the
    /// [deadline](Materials::set_deadline) has passed. The draw is skipped for now.
    pub fn pipeline(
        &self,
        gpu: &Gpu,
        p: &Prepared,
        target: Target,
    ) -> Option<Arc<wgpu::RenderPipeline>> {
        if let Some(built) = lock(&p.pipelines).get(&target) {
            return Some(built.clone());
        }
        if lock(&self.deadline).is_some_and(|d| Instant::now() >= d) {
            self.deferred.fetch_add(1, Ordering::Relaxed);
            lock(&self.demand).insert((p.id, target));
            return None;
        }
        Some(self.pipeline_now(gpu, p, target))
    }

    /// The pipeline for `p` rendering into `target`, built now whatever the deadline.
    pub fn pipeline_now(
        &self,
        gpu: &Gpu,
        p: &Prepared,
        target: Target,
    ) -> Arc<wgpu::RenderPipeline> {
        let mut map = lock(&p.pipelines);
        map.entry(target)
            .or_insert_with(|| {
                let layout = gpu
                    .device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: None,
                        bind_group_layouts: &[
                            Some(&self.vs_layout),
                            Some(&self.ps_layout),
                            Some(&p.tex_layout),
                        ],
                        immediate_size: 0,
                    });
                let s = p.state;
                Arc::new(
                    gpu.device
                        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                            label: Some(&p.name),
                            layout: Some(&layout),
                            vertex: wgpu::VertexState {
                                module: &p.vs.module,
                                entry_point: Some("main"),
                                compilation_options: Default::default(),
                                buffers: &[Some(wgpu::VertexBufferLayout {
                                    array_stride: p.kind.stride(),
                                    step_mode: wgpu::VertexStepMode::Vertex,
                                    attributes: &p.attrs,
                                })],
                            },
                            fragment: Some(wgpu::FragmentState {
                                module: &p.ps.module,
                                entry_point: Some("main"),
                                compilation_options: Default::default(),
                                targets: &[target.color.map(|format| wgpu::ColorTargetState {
                                    format,
                                    blend: s.blend(),
                                    write_mask: s.color_write(),
                                })],
                            }),
                            primitive: wgpu::PrimitiveState {
                                topology: wgpu::PrimitiveTopology::TriangleList,
                                front_face: wgpu::FrontFace::Cw,
                                cull_mode: s.cull(),
                                ..Default::default()
                            },
                            depth_stencil: target.depth.map(|format| wgpu::DepthStencilState {
                                format,
                                depth_write_enabled: Some(s.depth_write()),
                                depth_compare: Some(s.depth_compare()),
                                // The scene's depth buffer has no stencil; the light techniques' stencil test (they
                                // draw only where the light's rectangle was cleared) is left to the alpha weighting.
                                stencil: if format.has_stencil_aspect() {
                                    s.stencil()
                                } else {
                                    wgpu::StencilState::default()
                                },
                                bias: s.depth_bias(),
                            }),
                            multisample: wgpu::MultisampleState {
                                count: target.samples.max(1),
                                ..Default::default()
                            },
                            multiview_mask: None,
                            cache: None,
                        }),
                )
            })
            .clone()
    }

    /// Group 2 bind group for `p`. `code` resolves a code texture id to a texture (and sampler state) for this draw.
    pub fn bind_textures(
        &mut self,
        gpu: &Gpu,
        textures: &mut TextureCache,
        p: &Prepared,
        code: &dyn Fn(u32) -> Option<(Arc<Tex>, SamplerKey)>,
    ) -> wgpu::BindGroup {
        let mut views: Vec<(u32, Arc<Tex>, Arc<wgpu::Sampler>)> = Vec::new();
        for s in &p.slots {
            let (tex, state) = match &s.source {
                TexSource::Image(t, st) => (Some(t.clone()), SamplerKey::State(*st)),
                TexSource::Water(key, st) => (
                    self.waters.get(key).map(|w| w.tex.clone()),
                    SamplerKey::State(*st),
                ),
                TexSource::Code(id) => match code(*id) {
                    Some((t, st)) => (Some(t), st),
                    None => (None, SamplerKey::State(0x72)),
                },
                TexSource::Missing(st) => (None, SamplerKey::State(*st)),
            };
            let tex = tex
                .filter(|t| t.dim == s.dim)
                .unwrap_or_else(|| textures.solid(gpu, s.dim, s.fallback));
            // A raw slot's layout wants a non-filtering sampler, whatever the material asked for.
            let state = if s.fetch == Fetch::Raw {
                SamplerKey::ShadowRaw
            } else {
                state
            };
            let sampler = self.sampler(gpu, state);
            views.push((s.register, tex, sampler));
        }
        let mut entries = Vec::new();
        for (reg, tex, sampler) in &views {
            entries.push(wgpu::BindGroupEntry {
                binding: *reg,
                resource: wgpu::BindingResource::TextureView(&tex.view),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: reg + sm3::SAMPLER_BINDING_OFFSET,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &p.tex_layout,
            entries: &entries,
        })
    }
}
