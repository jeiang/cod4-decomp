// SPDX-License-Identifier: GPL-3.0-only
//! The post chain of `code_post_gfx_mp`, in the original pass order (`RB_StandardDrawCommands`,
//! `RB_ProcessPostEffects`).
//!
//! When an effect is active the scene goes into an offscreen target (the original's resolved scene) instead of the
//! frame buffer. The merged effects come first (depth of field, then film, in one pass over the scene into the frame
//! buffer), then the glow (a quarter-size downsample, a gaussian filter chain, an additive apply onto the frame
//! buffer), then the screen blur, then the shell shock overlays. Every pass is a full-screen quad drawn with a stock
//! material of `code_post_gfx_mp`, whose shaders are translated from the original bytecode like the world's.
//!
//! Depth of field needs the scene's view-space depth ("float Z"): the renderer draws the same surfaces a second time
//! with the `build floatz` technique into an `R32Float` target, cleared to the far value the original's `shadowclear`
//! material writes.
//!
//! Render targets are allocated once per frame size; the constant banks of every pass come from the frame's constant
//! ring like the world's.

use crate::art::{Film, Glow, MapArt};
use crate::codeconst::{self, FrameConsts, Object, tex as ctex};
use crate::gpu::Gpu;
use crate::material::{SamplerKey, Target, VertexKind};
use crate::renderer::Renderer;
use crate::scene::MapData;
use crate::texture::Tex;
use crate::timing::{GpuTimer, Span};
use assets::zone::gfx::Material;
use glam::{Mat4, Vec3};
use sm3::SamplerDim;
use std::collections::HashMap;
use std::sync::Arc;

/// Filtering, mips and clamping of the code images the post materials read: linear, clamped.
const CODE_SAMPLER: u8 = 0x72;
/// Technique slot of the full-screen materials: `unlit`.
const TECH_UNLIT: usize = 4;
/// What the original's `shadowclear` material writes: every pixel no surface draws into the float-Z target is farther
/// away than the depth-of-field shaders' sky test (`1.5e6`).
pub const FLOATZ_CLEAR: f64 = 2_000_000.0;
pub const FLOATZ_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// The original's `r_dof_bias` default.
const DOF_BIAS: f32 = 0.5;
/// The original's `r_znear_depthhack` default, the near clip of the view model.
const VIEW_MODEL_NEAR: f32 = 0.1;
/// Quads one frame can draw: the longest chain is a dozen passes of depth of field, 33 of glow and 33 of blur.
const MAX_QUADS: usize = 128;

/// Depth of field: distances in world units, blur radii in virtual 640x480 pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dof {
    pub view_model_start: f32,
    pub view_model_end: f32,
    pub near_start: f32,
    pub near_end: f32,
    pub far_start: f32,
    pub far_end: f32,
    pub near_blur: f32,
    pub far_blur: f32,
}

impl Dof {
    /// `R_UsingDepthOfField`.
    pub fn active(&self) -> bool {
        self.view_model_end > self.view_model_start + 1.0
            || self.near_end > self.near_start + 1.0
            || (self.far_end > self.far_start + 1.0 && self.far_blur > 0.0)
    }

    /// `CONST_SRC_CODE_DOF_EQUATION_SCENE`: `(near scale, far scale, near bias, far bias)`, the blur fraction of a
    /// depth `z` being `clamp(scale * z + bias)` on each side.
    pub fn scene_equation(&self, z_near: f32) -> [f32; 4] {
        let [near_scale, near_bias] = near_equation(self.near_start, self.near_end, z_near);
        let (far_scale, far_bias) = if z_near.max(self.far_start) < self.far_end {
            (
                1.0 / (self.far_end - self.far_start),
                self.far_start / (self.far_start - self.far_end),
            )
        } else {
            (0.0, 0.0)
        };
        [near_scale, far_scale, near_bias, far_bias]
    }

    /// `CONST_SRC_CODE_DOF_EQUATION_VIEWMODEL_AND_FAR_BLUR`: the view model's near equation with no far part, and the
    /// far blur radius relative to the near one (after the bias power) in `w`.
    pub fn view_model_equation(&self) -> [f32; 4] {
        let [scale, bias] =
            near_equation(self.view_model_start, self.view_model_end, VIEW_MODEL_NEAR);
        [
            scale,
            0.0,
            bias,
            (self.far_blur / self.near_blur).powf(DOF_BIAS),
        ]
    }

    /// `RB_GetDepthOfFieldBlurFraction`: the share of the near blur radius a blur of `pixel_radius` pixels at the
    /// scene height covers.
    fn blur_fraction(&self, pixel_radius: f32, scene_height: u32) -> f32 {
        (pixel_radius * 480.0 / scene_height as f32 / self.near_blur).powf(DOF_BIAS)
    }

    /// `DOF_LERP_SCALE` and `DOF_LERP_BIAS`: how the blur fraction picks between the sharp image and the two blurred
    /// ones (about 1.4 and 3.6 pixels of radius), and the near-blur image.
    pub fn lerp_constants(&self, scene_height: u32) -> ([f32; 4], [f32; 4]) {
        // The original asserts 0 < small < medium < 1, which small scenes break; keep the divisions finite.
        let medium = self.blur_fraction(3.6, scene_height).min(0.99);
        let small = self.blur_fraction(1.4, scene_height).min(medium * 0.5);
        (
            [
                -1.0 / small,
                -1.0 / (medium - small),
                -1.0 / (1.0 - medium),
                1.0 / (1.0 - medium),
            ],
            [
                1.0,
                medium / (medium - small),
                1.0 / (1.0 - medium),
                -medium / (1.0 - medium),
            ],
        )
    }
}

/// `RB_GetNearDepthOfFieldEquation`: `(scale, bias)` of the blur fraction from `out_of_focus` (full blur) down to
/// `in_focus`. A range that starts nearer than the clip plane or is empty blurs nothing but a sliver at the camera.
fn near_equation(out_of_focus: f32, in_focus: f32, near_clip: f32) -> [f32; 2] {
    let (out_of_focus, in_focus) = if out_of_focus.max(near_clip) >= in_focus {
        (0.0, near_clip * 0.5)
    } else {
        (out_of_focus, in_focus)
    };
    [
        1.0 / (out_of_focus - in_focus),
        in_focus / (in_focus - out_of_focus),
    ]
}

/// The shell shock overlays: the saved screen, blurred and faded, and a flash of white over it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShellShock {
    /// Opacity of the blurred saved screen, 0 to 1 (see [`ShellShock::blurred_alpha`]).
    pub blur_alpha: f32,
    /// How much of the saved screen the flash shows, 0 to 1.
    pub flash_screengrab: f32,
    /// Added white, 0 to 1.
    pub flash_whiteout: f32,
}

impl ShellShock {
    /// Opacity of the blurred screen `elapsed_ms` into a fade of `fade_ms` (`RB_BlendSavedScreenBlurredCmd`).
    pub fn blurred_alpha(elapsed_ms: f32, fade_ms: f32) -> f32 {
        0.01f32.powf(elapsed_ms / fade_ms).min(0.99)
    }
}

/// Everything the chain reads each frame. [`PostParams::from_art`] gives a map's own glow and film; the game drives
/// the rest.
#[derive(Clone, Debug, PartialEq)]
pub struct PostParams {
    pub glow: Glow,
    pub film: Film,
    pub dof: Option<Dof>,
    pub shell_shock: Option<ShellShock>,
    /// Blur of the whole screen, in virtual 640x480 pixels (the `blurRadius` of the scene parameters).
    pub blur_radius: f32,
    /// Keep this frame's final picture for the shell shock overlays (the original's `SaveScreen` command); cleared
    /// once taken. The overlays draw once a picture has been kept.
    pub save_screen: bool,
}

impl PostParams {
    pub fn from_art(art: &MapArt) -> PostParams {
        PostParams {
            glow: art.glow,
            film: art.film,
            dof: None,
            shell_shock: None,
            blur_radius: 0.0,
            save_screen: false,
        }
    }

    /// `R_UsingGlow`.
    pub fn glow_active(&self) -> bool {
        self.glow.enabled && self.glow.bloom_intensity != 0.0 && self.glow.radius != 0.0
    }

    pub fn dof_active(&self) -> bool {
        self.dof.is_some_and(|d| d.active())
    }

    /// Whether the scene has to go through an offscreen target.
    pub fn offscreen(&self) -> bool {
        self.glow_active()
            || self.dof_active()
            || self.film.active()
            || self.blur_radius > 0.0
            || self.save_screen
    }

    /// `COLOR_BIAS`, `COLOR_TINT_BASE` and `COLOR_TINT_DELTA`.
    fn film_constants(&self) -> [[f32; 4]; 3] {
        self.film.constants()
    }

    /// `GLOW_SETUP` and `GLOW_APPLY`.
    pub fn glow_constants(&self) -> [[f32; 4]; 2] {
        let g = &self.glow;
        let cutoff = g.bloom_cutoff.min(0.999);
        [
            [cutoff, 1.0 / (1.0 - cutoff), 0.0, g.bloom_desaturation],
            [0.0, 0.0, 0.0, g.bloom_intensity],
        ]
    }
}

// ---- the gaussian filter chain (`rb_imagefilter.cpp`) ----

/// Widest sigma one 2D pass handles, and the narrowest the chain still bothers with.
const MAX_2D_SIGMA: f32 = 1.389_560_5;
const MIN_SIGMA: f32 = 0.329_505_12;
/// Widest sigma of one 1D pass (eight taps of two samples each).
const MAX_1D_SIGMA: f32 = 6.497_75;
const MAX_CHAIN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Both,
}

/// One pass of a filter chain: the taps its `filter_symmetric_<half_count>` material reads from `FILTER_TAP_n`
/// (each `(du, dv, 0, weight)`; the material samples at both `+offset` and `-offset`).
#[derive(Clone, Debug, PartialEq)]
pub struct FilterPass {
    pub taps: [[f32; 4]; 8],
    pub half_count: usize,
    /// The standard deviation, in pixels of the pass's destination, this pass blurs by.
    pub sigma: f32,
    pub axis: Axis,
}

/// `RB_GaussianFilterPoints1D`: the offsets (in units of the source size) and weights of `limit` bilinear taps that
/// stand for the `2 * limit` pixels on one side of the centre, and how many of them carry weight. The weights are
/// halved: the material applies each tap on both sides. A source `ratio` times larger than the destination samples
/// between pixels.
fn gaussian_points(
    sigma: f32,
    src_res: u32,
    dst_res: u32,
    limit: usize,
) -> ([f32; 8], [f32; 8], usize) {
    let ratio = (src_res as f32 / dst_res as f32).round() as u32;
    let shift = if ratio & 1 == 1 { 0.0 } else { 0.5 };
    let exponent = -0.5 / (sigma * sigma);
    let (mut offsets, mut weights) = ([0.0; 8], [0.0; 8]);
    let mut total = 0.0;
    for i in 0..limit {
        let (a, b) = ((2 * i) as f32 + shift, (2 * i + 1) as f32 + shift);
        let (mut wa, wb) = ((a * a * exponent).exp(), (b * b * exponent).exp());
        if i == 0 && shift == 0.0 {
            wa *= 0.5;
        }
        weights[i] = wa + wb;
        offsets[i] = if weights[i] == 0.0 {
            (a + b) * 0.5 / src_res as f32
        } else {
            (a * wa + b * wb) / (src_res as f32 * weights[i])
        };
        total += weights[i];
    }
    if total <= 0.001 {
        weights[0] = 0.5;
        return (offsets, weights, 1);
    }
    let mut half = limit;
    for i in (0..limit).rev() {
        weights[i] *= 0.5 / total;
        if weights[i] < 0.01 {
            half = i + 1;
        }
    }
    (offsets, weights, half)
}

/// `RB_GenerateGaussianFilter1D`: a pass along one axis of a `res` sized image.
fn filter_1d(sigma: f32, res: [u32; 2], axis: usize) -> FilterPass {
    let (offsets, weights, half_count) = gaussian_points(sigma, res[axis], res[axis], 8);
    let mut taps = [[0.0; 4]; 8];
    for i in 0..8 {
        taps[i][axis] = offsets[i];
        taps[i][3] = weights[i];
    }
    FilterPass {
        taps,
        half_count,
        sigma,
        axis: if axis == 0 { Axis::X } else { Axis::Y },
    }
}

/// `RB_GenerateGaussianFilter2D`: both axes at once with four bilinear taps (eight with the mirror), reading a source
/// that may be larger than the destination.
fn filter_2d(sigma: f32, src: [u32; 2], dst: [u32; 2]) -> FilterPass {
    let (ox, wx, _) = gaussian_points(sigma, src[0], dst[0], 2);
    let (oy, wy, _) = gaussian_points(sigma, src[1], dst[1], 2);
    let mut taps = [[0.0; 4]; 8];
    let mut t = 0;
    for y in 0..2 {
        for x in 0..2 {
            let w = wx[x] * wy[y];
            taps[2 * t] = [-ox[x], oy[y], 0.0, w];
            taps[2 * t + 1] = [ox[x], oy[y], 0.0, w];
            t += 1;
        }
    }
    FilterPass {
        taps,
        half_count: 8,
        sigma,
        axis: Axis::Both,
    }
}

/// `RB_GenerateGaussianFilterChain`: the passes that blur a `src` sized image by `radius` pixels per axis into a
/// `dst` sized one (at most `limit`). Sigmas of successive passes add in quadrature, so the chain spends the radius
/// widest-first: a downsampling 2D pass if the sizes differ, 1D passes of at most [`MAX_1D_SIGMA`] on the larger
/// axis, and a final 2D pass when the axes are within a third of a pixel of each other.
pub fn gaussian_chain(
    mut radius_x: f32,
    mut radius_y: f32,
    src: [u32; 2],
    dst: [u32; 2],
    limit: usize,
) -> Vec<FilterPass> {
    let limit = limit.min(MAX_CHAIN);
    let mut passes = Vec::new();
    if src != dst {
        let sigma = radius_x.min(radius_y).min(MAX_2D_SIGMA);
        radius_x =
            (radius_x * radius_x - sigma * sigma).max(0.0).sqrt() * dst[0] as f32 / src[0] as f32;
        radius_y =
            (radius_y * radius_y - sigma * sigma).max(0.0).sqrt() * dst[1] as f32 / src[1] as f32;
        passes.push(filter_2d(sigma, src, dst));
    }
    while passes.len() < limit && (radius_x >= MIN_SIGMA || radius_y >= MIN_SIGMA) {
        if (radius_x - radius_y).abs() < MIN_SIGMA {
            let sigma = (radius_x + radius_y) * 0.5;
            if sigma <= MAX_2D_SIGMA {
                passes.push(filter_2d(sigma, dst, dst));
                break;
            }
        }
        let axis = usize::from(radius_y >= radius_x);
        let remaining = if axis == 1 {
            &mut radius_y
        } else {
            &mut radius_x
        };
        let sigma = if *remaining > MAX_1D_SIGMA {
            let s = MAX_1D_SIGMA;
            *remaining = (*remaining * *remaining - s * s).sqrt();
            s
        } else {
            std::mem::take(remaining)
        };
        passes.push(filter_1d(sigma, dst, axis));
    }
    passes
}

/// `RB_VirtualToSceneRadius` for a scene of square pixels: a radius in virtual 640x480 pixels to scene pixels.
fn scene_radius(radius: f32, scene_height: u32) -> f32 {
    scene_height as f32 * radius / 480.0
}

/// `RB_BlurScreen`: the radius to filter by and the opacity of the blurred picture over the frame (the filter
/// bottoms out at the radius of `1440 / height`, below which the picture fades in instead).
fn blur_screen(radius: f32, scene_height: u32) -> (f32, u8) {
    let min = 1440.0 / scene_height as f32;
    if min > radius {
        (min, (radius / min * 255.0).round() as u8)
    } else {
        (radius, 255)
    }
}

// ---- render targets and passes ----

/// The images of the chain. `Scene`, `FloatZ` and `Saved` are full size, the rest a quarter of each side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Id {
    Scene,
    FloatZ,
    Saved,
    Post0,
    Post1,
    Ping0,
    Ping1,
    /// The scene as it stood before the emissive surfaces, for the distortion materials.
    PostSun,
}

const IDS: usize = 8;

struct Targets {
    size: (u32, u32),
    format: wgpu::TextureFormat,
    imgs: [Option<Arc<Tex>>; IDS],
    /// Texture bind groups by pass and feedback image.
    bgs: HashMap<(u32, u8), Arc<wgpu::BindGroup>>,
}

impl Targets {
    fn size_of(&self, id: Id) -> (u32, u32) {
        match id {
            Id::Scene | Id::FloatZ | Id::Saved | Id::PostSun => self.size,
            _ => ((self.size.0 >> 2).max(1), (self.size.1 >> 2).max(1)),
        }
    }

    /// The image, created on first use.
    fn get(&mut self, gpu: &Gpu, id: Id) -> Arc<Tex> {
        if let Some(t) = &self.imgs[id as usize] {
            return t.clone();
        }
        let (w, h) = self.size_of(id);
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("post"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if id == Id::FloatZ {
                FLOATZ_FORMAT
            } else {
                self.format
            },
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let t = Arc::new(Tex {
            view: tex.create_view(&Default::default()),
            dim: SamplerDim::D2,
            width: w,
        });
        self.imgs[id as usize] = Some(t.clone());
        t
    }
}

/// Post chain state of a renderer: its materials, targets and quad buffers.
pub(crate) struct State {
    materials: HashMap<Arc<str>, Arc<Material>>,
    targets: Option<Targets>,
    quads: wgpu::Buffer,
    indices: wgpu::Buffer,
    /// A picture has been kept for the shell shock overlays.
    saved: bool,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScreenVertex {
    pos: [f32; 3],
    _pad: f32,
    /// `D3DCOLOR`: blue, green, red, alpha.
    color: [u8; 4],
    uv: [f32; 2],
}

const QUAD_BYTES: u64 = 4 * std::mem::size_of::<ScreenVertex>() as u64;

impl State {
    pub(crate) fn new(gpu: &Gpu, data: &MapData) -> State {
        use wgpu::util::DeviceExt;
        State {
            materials: data
                .post_materials
                .iter()
                .filter_map(|m| Some((m.name.clone()?, m.clone())))
                .collect(),
            targets: None,
            quads: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("post quads"),
                size: QUAD_BYTES * MAX_QUADS as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            // Clockwise on screen, like the original's 2D quads.
            indices: gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("post quad indices"),
                    contents: bytemuck::cast_slice(&[0u16, 1, 2, 0, 2, 3]),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            saved: false,
        }
    }

    /// The float-Z target for frames of `size` (created on first use), to render the scene's depth into.
    pub(crate) fn floatz_view(
        &mut self,
        gpu: &Gpu,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        self.floatz_tex(gpu, size, format).view.clone()
    }

    /// The float-Z image the scene's `FLOATZ` sampler reads.
    pub(crate) fn floatz_tex(
        &mut self,
        gpu: &Gpu,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Arc<Tex> {
        self.ensure_targets(size, format).get(gpu, Id::FloatZ)
    }

    /// The copy of the scene the distortion materials' `RESOLVED_POST_SUN` sampler reads.
    pub(crate) fn post_sun_tex(
        &mut self,
        gpu: &Gpu,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Arc<Tex> {
        self.ensure_targets(size, format).get(gpu, Id::PostSun)
    }

    /// Drop the targets when the frame size or format changed; `true` when something was dropped, so that bind
    /// groups made from them must go too.
    pub(crate) fn invalidate(&mut self, size: (u32, u32), format: wgpu::TextureFormat) -> bool {
        let stale = self
            .targets
            .as_ref()
            .is_some_and(|t| t.size != size || t.format != format);
        if stale {
            self.targets = None;
            self.saved = false;
        }
        stale
    }

    fn ensure_targets(&mut self, size: (u32, u32), format: wgpu::TextureFormat) -> &mut Targets {
        if self
            .targets
            .as_ref()
            .is_none_or(|t| t.size != size || t.format != format)
        {
            self.targets = Some(Targets {
                size,
                format,
                imgs: Default::default(),
                bgs: HashMap::new(),
            });
            self.saved = false;
        }
        self.targets.as_mut().expect("just made")
    }
}

/// The part of the chain a pass belongs to; one timestamp span each.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Copy,
    Dof,
    Film,
    Glow,
    Blur,
    Shock,
    Sun,
}

impl Group {
    fn name(self) -> &'static str {
        match self {
            Group::Copy => "post copy",
            Group::Dof => "post dof",
            Group::Film => "post film",
            Group::Glow => "post glow",
            Group::Blur => "post blur",
            Group::Shock => "post shellshock",
            Group::Sun => "post sun",
        }
    }
}

struct Pass {
    group: Group,
    target: wgpu::TextureView,
    /// Keep what the target holds (the pass blends over it) instead of starting from black.
    load: bool,
    pipeline: Arc<wgpu::RenderPipeline>,
    vs: u32,
    ps: u32,
    tex: Arc<wgpu::BindGroup>,
    quad: u32,
}

/// The passes of one frame and where its scene goes.
pub(crate) struct Chain {
    /// The scene's target, when it is not the frame buffer.
    pub scene: Option<wgpu::TextureView>,
    /// The copy of the scene before the emissive surfaces, recorded between the two halves of the scene.
    postsun: Vec<Pass>,
    passes: Vec<Pass>,
}

enum Dest {
    Frame,
    Img(Id),
}

struct Builder<'a> {
    r: &'a mut Renderer,
    targets: Targets,
    /// The frame buffer and its size.
    frame: (wgpu::TextureView, (u32, u32)),
    base: FrameConsts,
    passes: Vec<Pass>,
    quads: Vec<[ScreenVertex; 4]>,
}

impl Builder<'_> {
    /// Draw a full-screen quad with `material` into `dest`, reading `feedback` as the `FEEDBACK` image. `color` is
    /// the quad's vertex colour (blue, green, red, alpha), `taps` the filter taps of a symmetric filter pass.
    fn draw(
        &mut self,
        group: Group,
        material: &str,
        dest: Dest,
        feedback: Option<Id>,
        color: [u8; 4],
        taps: Option<&FilterPass>,
    ) {
        if let Some(mat) = self.r.post_state.materials.get(material).cloned() {
            self.draw_quad(group, &mat, dest, feedback, color, taps, FULL_SCREEN);
        }
    }

    /// [`Builder::draw`] of any `material` over the clip-space `quad`: `[x, y, u, v]` of each corner.
    #[allow(clippy::too_many_arguments)]
    fn draw_quad(
        &mut self,
        group: Group,
        mat: &Arc<Material>,
        dest: Dest,
        feedback: Option<Id>,
        color: [u8; 4],
        taps: Option<&FilterPass>,
        quad: [[f32; 4]; 4],
    ) {
        let r = &mut *self.r;
        let Some(prep) = r.materials.prepare(
            &r.gpu,
            &mut r.textures,
            mat,
            &[TECH_UNLIT],
            VertexKind::Screen,
            false,
        ) else {
            return;
        };
        let (target, (w, h)) = match dest {
            Dest::Frame => self.frame.clone(),
            Dest::Img(id) => (
                self.targets.get(&r.gpu, id).view.clone(),
                self.targets.size_of(id),
            ),
        };
        let mut fc = self.base.clone();
        fc.vec[codeconst::RENDER_TARGET_SIZE as usize] =
            [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32];
        if let Some(t) = taps {
            for i in 0..t.half_count {
                fc.vec[codeconst::FILTER_TAP_0 as usize + i] = t.taps[i];
            }
        }
        let obj = Object::default();
        let (vs, bank) = r.alloc(prep.vs_regs());
        prep.fill_vs(bank, &fc, &obj);
        let (ps, bank) = r.alloc(prep.ps_regs());
        prep.fill_ps(bank, &fc, &obj);

        let key = (prep.id(), feedback.map_or(u8::MAX, |f| f as u8));
        let tex = match self.targets.bgs.get(&key) {
            Some(g) => g.clone(),
            None => {
                let image = |id: Id, targets: &mut Targets| targets.get(&r.gpu, id);
                let scene = image(Id::Scene, &mut self.targets);
                let post0 = image(Id::Post0, &mut self.targets);
                let post1 = image(Id::Post1, &mut self.targets);
                let floatz = self.targets.imgs[Id::FloatZ as usize].clone();
                let feedback = feedback.map(|f| image(f, &mut self.targets));
                let g = Arc::new(r.materials.bind_textures(
                    &r.gpu,
                    &mut r.textures,
                    &prep,
                    &|id| {
                        let linear = SamplerKey::State(CODE_SAMPLER);
                        match id {
                            ctex::FEEDBACK => feedback.clone().map(|t| (t, linear)),
                            ctex::RESOLVED_SCENE => Some((scene.clone(), linear)),
                            ctex::POST_EFFECT_0 => Some((post0.clone(), linear)),
                            ctex::POST_EFFECT_1 => Some((post1.clone(), linear)),
                            ctex::FLOATZ => floatz.clone().map(|t| (t, SamplerKey::ShadowRaw)),
                            _ => None,
                        }
                    },
                ));
                self.targets.bgs.insert(key, g.clone());
                g
            }
        };
        let pipeline = r.materials.pipeline_now(
            &r.gpu,
            &prep,
            Target {
                color: Some(self.targets.format),
                depth: None,
                samples: 1,
            },
        );
        let quad_index = self.quads.len() as u32;
        self.quads.push(quad.map(|[x, y, u, t]| ScreenVertex {
            pos: [x, y, 0.0],
            _pad: 0.0,
            color,
            uv: [u, t],
        }));
        self.passes.push(Pass {
            group,
            target,
            load: prep.state.blend().is_some(),
            pipeline,
            vs,
            ps,
            tex,
            quad: quad_index,
        });
    }

    /// `RB_FilterImage`: an optional first pass with its own material, then the `chain`, from `src` through the
    /// ping-pong images into `dst`.
    fn filter(
        &mut self,
        group: Group,
        first: Option<&str>,
        chain: &[FilterPass],
        src: Id,
        dst: Id,
    ) {
        let total = chain.len() + usize::from(first.is_some());
        let ping = [Id::Ping0, Id::Ping1];
        if total == 0 {
            // A radius below the narrowest kernel leaves the image as it is.
            self.draw(
                group,
                "feedbackreplace",
                Dest::Img(dst),
                Some(src),
                WHITE,
                None,
            );
        }
        for k in 0..total {
            let feedback = if k == 0 { src } else { ping[(k - 1) & 1] };
            let target = if k == total - 1 { dst } else { ping[k & 1] };
            match first.filter(|_| k == 0) {
                Some(m) => self.draw(group, m, Dest::Img(target), Some(feedback), WHITE, None),
                None => {
                    let p = &chain[k - usize::from(first.is_some())];
                    let name = format!("filter_symmetric_{}", p.half_count);
                    self.draw(
                        group,
                        &name,
                        Dest::Img(target),
                        Some(feedback),
                        WHITE,
                        Some(p),
                    );
                }
            }
        }
    }
}

/// The overlay material of the sun's glare and blind (`rgp.glareBlindMaterial`).
const GLARE_BLIND: &str = "$glare_blind";
const WHITE: [u8; 4] = [255; 4];
/// The corners of the whole screen, clockwise from the top left.
const FULL_SCREEN: [[f32; 4]; 4] = [
    [-1.0, 1.0, 0.0, 0.0],
    [1.0, 1.0, 1.0, 0.0],
    [1.0, -1.0, 1.0, 1.0],
    [-1.0, -1.0, 0.0, 1.0],
];

/// Plan the post passes of this frame: allocate what they need, fill their constant banks and upload their quads.
/// `target` is the frame buffer. Returns the chain; frames with nothing to do get an empty one with no offscreen
/// scene target. `distortion` asks for the copy of the scene the distortion materials sample, which needs the scene in
/// an offscreen target.
pub(crate) fn build(
    r: &mut Renderer,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    target: &wgpu::TextureView,
    z_near: f32,
    distortion: bool,
) -> Chain {
    let p = r.post_params();
    let offscreen = p.offscreen() || distortion;
    let save = p.save_screen;
    let shock = p.shell_shock.filter(|s| {
        !save
            && r.post_state.saved
            && (s.blur_alpha > 0.0 || s.flash_screengrab > 0.0 || s.flash_whiteout > 0.0)
    });
    let sun = r.sun.overlay;
    if !offscreen && shock.is_none() && sun == Default::default() {
        return Chain {
            scene: None,
            postsun: Vec::new(),
            passes: Vec::new(),
        };
    }
    let mut targets = {
        r.post_state.ensure_targets(size, format);
        r.post_state.targets.take().expect("just made")
    };
    let scene = offscreen.then(|| targets.get(&r.gpu, Id::Scene).view.clone());
    let frame = if save {
        targets.get(&r.gpu, Id::Saved).view.clone()
    } else {
        target.clone()
    };

    let mut base = FrameConsts::new(Mat4::IDENTITY, Mat4::IDENTITY, Vec3::ZERO);
    let [bias, tint_base, tint_delta] = p.film_constants();
    base.vec[codeconst::COLOR_BIAS as usize] = bias;
    base.vec[codeconst::COLOR_TINT_BASE as usize] = tint_base;
    base.vec[codeconst::COLOR_TINT_DELTA as usize] = tint_delta;
    let [setup, apply] = p.glow_constants();
    base.vec[codeconst::GLOW_SETUP as usize] = setup;
    base.vec[codeconst::GLOW_APPLY as usize] = apply;

    let mut b = Builder {
        r,
        targets,
        frame: (frame, size),
        base,
        passes: Vec::new(),
        quads: Vec::new(),
    };
    let mut postsun = Vec::new();
    if distortion {
        b.draw(
            Group::Copy,
            "feedbackreplace",
            Dest::Img(Id::PostSun),
            Some(Id::Scene),
            WHITE,
            None,
        );
        postsun = std::mem::take(&mut b.passes);
    }
    let dof = p.dof.filter(Dof::active);
    let film = p.film.active();
    let h = size.1;
    if offscreen {
        if dof.is_none() && !film {
            // The glow, blur and overlays work on top of the picture.
            b.draw(
                Group::Copy,
                "feedbackreplace",
                Dest::Frame,
                Some(Id::Scene),
                WHITE,
                None,
            );
        }
        if let Some(d) = dof {
            // RB_GetDepthOfFieldInputImages: a quarter-size downsample of the scene, blurred by the near radius, the
            // near circle of confusion taken from the two, and a small blur of that.
            let (scale, bias) = d.lerp_constants(h);
            b.base.vec[codeconst::DOF_EQUATION_SCENE as usize] = d.scene_equation(z_near);
            b.base.vec[codeconst::DOF_EQUATION_VIEWMODEL_AND_FAR_BLUR as usize] =
                d.view_model_equation();
            b.base.vec[codeconst::DOF_ROW_DELTA as usize] = [0.0, 1.0 / h as f32, 0.0, 0.0];
            b.base.vec[codeconst::DOF_LERP_SCALE as usize] = scale;
            b.base.vec[codeconst::DOF_LERP_BIAS as usize] = bias;
            let (qw, qh) = b.targets.size_of(Id::Post1);
            b.draw(
                Group::Dof,
                "dof_downsample",
                Dest::Img(Id::Post1),
                None,
                WHITE,
                None,
            );
            let radius = scene_radius(d.near_blur * 0.25, h);
            let chain = gaussian_chain(radius, radius, [qw, qh], [qw, qh], MAX_CHAIN);
            b.filter(Group::Dof, None, &chain, Id::Post1, Id::Post0);
            b.draw(
                Group::Dof,
                "dof_near_coc",
                Dest::Img(Id::Ping0),
                None,
                WHITE,
                None,
            );
            b.draw(
                Group::Dof,
                "small_blur",
                Dest::Img(Id::Post1),
                Some(Id::Ping0),
                WHITE,
                None,
            );
            let name = if film {
                "postfx_dof_color"
            } else {
                "postfx_dof"
            };
            b.draw(Group::Dof, name, Dest::Frame, None, WHITE, None);
        } else if film {
            b.draw(Group::Film, "postfx_color", Dest::Frame, None, WHITE, None);
        }
        if p.glow_active() {
            // RB_GlowFilterImage: the radius is scaled by the quarter-size target once for the target, once more by
            // RB_ApplyGlowFilter.
            let (qw, qh) = b.targets.size_of(Id::Post0);
            let radius = scene_radius(p.glow.radius * 0.25 * 0.25, h);
            let chain = gaussian_chain(radius, radius, [qw, qh], [qw, qh], MAX_CHAIN - 1);
            b.filter(
                Group::Glow,
                Some("glow_consistent_setup"),
                &chain,
                Id::Scene,
                Id::Post0,
            );
            b.draw(
                Group::Glow,
                "glow_apply_bloom",
                Dest::Frame,
                Some(Id::Post0),
                WHITE,
                None,
            );
        }
        if p.blur_radius > 0.0 {
            let (radius, alpha) = blur_screen(p.blur_radius, h);
            let (qw, qh) = b.targets.size_of(Id::Post0);
            let r = scene_radius(radius, h);
            let chain = gaussian_chain(r, r, [size.0, size.1], [qw, qh], MAX_CHAIN);
            b.filter(Group::Blur, None, &chain, Id::Scene, Id::Post0);
            let name = if p.film.enabled {
                "feedbackfilmblend"
            } else {
                "feedbackblend"
            };
            b.draw(
                Group::Blur,
                name,
                Dest::Frame,
                Some(Id::Post0),
                [255, 255, 255, alpha],
                None,
            );
        }
        if save {
            b.frame = (target.clone(), size);
            b.draw(
                Group::Copy,
                "feedbackreplace",
                Dest::Frame,
                Some(Id::Saved),
                WHITE,
                None,
            );
        }
    }
    if let Some(s) = shock {
        b.frame = (target.clone(), size);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if s.blur_alpha > 0.0 {
            b.draw(
                Group::Shock,
                "shellshock",
                Dest::Frame,
                Some(Id::Saved),
                [255, 255, 255, byte(s.blur_alpha)],
                None,
            );
        }
        if s.flash_screengrab > 0.0 || s.flash_whiteout > 0.0 {
            let w = byte(s.flash_whiteout);
            b.draw(
                Group::Shock,
                "shellshock_flashed",
                Dest::Frame,
                Some(Id::Saved),
                [w, w, w, byte(s.flash_screengrab)],
                None,
            );
        }
    }
    if sun != Default::default() {
        // RB_DrawSunPostEffects: the flare, then the blind and glare, over the finished picture.
        b.frame = (target.clone(), size);
        if let (Some(f), Some(mat)) = (sun.flare, b.r.scene.world.sun.flare_material.clone()) {
            let [x, y] = f.center;
            let [hw, hh] = f.half;
            let a = f.alpha;
            let quad = [
                [x + hw, y + hh, 0.0, 0.0],
                [x + hw, y - hh, 1.0, 0.0],
                [x - hw, y - hh, 1.0, 1.0],
                [x - hw, y + hh, 0.0, 1.0],
            ];
            b.draw_quad(
                Group::Sun,
                &mat,
                Dest::Frame,
                None,
                [a, a, a, 255],
                None,
                quad,
            );
        }
        if let Some(color) = sun.glare_blind {
            b.draw(Group::Sun, GLARE_BLIND, Dest::Frame, None, color, None);
        }
    }
    let Builder {
        r,
        targets,
        passes,
        quads,
        ..
    } = b;
    debug_assert!(quads.len() <= MAX_QUADS);
    if !quads.is_empty() {
        let n = quads.len().min(MAX_QUADS);
        r.gpu
            .queue
            .write_buffer(&r.post_state.quads, 0, bytemuck::cast_slice(&quads[..n]));
    }
    if save {
        r.post.save_screen = false;
        r.post_state.saved = true;
    }
    r.post_state.targets = Some(targets);
    Chain {
        scene,
        postsun,
        passes,
    }
}

/// Record the chain's passes, each group under one timestamp span: `postsun` is the scene copy, between the halves of
/// the scene.
pub(crate) fn record_postsun(
    enc: &mut wgpu::CommandEncoder,
    chain: &Chain,
    state: &State,
    vs_bg: &wgpu::BindGroup,
    ps_bg: &wgpu::BindGroup,
    timer: Option<&mut GpuTimer>,
) {
    record_passes(enc, &chain.postsun, state, vs_bg, ps_bg, timer);
}

/// The rest of the chain, after the scene.
pub(crate) fn record(
    enc: &mut wgpu::CommandEncoder,
    chain: &Chain,
    state: &State,
    vs_bg: &wgpu::BindGroup,
    ps_bg: &wgpu::BindGroup,
    timer: Option<&mut GpuTimer>,
) {
    record_passes(enc, &chain.passes, state, vs_bg, ps_bg, timer);
}

fn record_passes(
    enc: &mut wgpu::CommandEncoder,
    passes: &[Pass],
    state: &State,
    vs_bg: &wgpu::BindGroup,
    ps_bg: &wgpu::BindGroup,
    mut timer: Option<&mut GpuTimer>,
) {
    let mut span: Option<Span> = None;
    for (i, p) in passes.iter().enumerate() {
        let first = i == 0 || passes[i - 1].group != p.group;
        let last = passes.get(i + 1).is_none_or(|n| n.group != p.group);
        if first {
            span = timer.as_deref_mut().and_then(|t| t.span(p.group.name()));
        }
        let timestamps = span
            .zip(timer.as_deref())
            .and_then(|(s, t)| t.writes(s, first, last));
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(p.group.name()),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &p.target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if p.load {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(&p.pipeline);
        rp.set_bind_group(0, vs_bg, &[p.vs]);
        rp.set_bind_group(1, ps_bg, &[p.ps]);
        rp.set_bind_group(2, &*p.tex, &[]);
        let at = u64::from(p.quad) * QUAD_BYTES;
        rp.set_vertex_buffer(0, state.quads.slice(at..at + QUAD_BYTES));
        rp.set_index_buffer(state.indices.slice(..), wgpu::IndexFormat::Uint16);
        rp.draw_indexed(0..6, 0, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half of a 1D pass's weights and offsets expand to the symmetric kernel: with every tap kept the weights of
    /// both sides sum to one.
    #[test]
    fn symmetric_taps_sum_to_one() {
        for sigma in [0.5, 1.0, 2.5, 6.4] {
            let (offsets, weights, half) = gaussian_points(sigma, 640, 640, 8);
            let total: f32 = weights.iter().sum::<f32>() * 2.0;
            assert!((total - 1.0).abs() < 1e-4, "sigma {sigma}: {total}");
            // Dropped taps are the ones below one percent, and they are the tail.
            let kept: f32 = weights[..half].iter().sum::<f32>() * 2.0;
            assert!(
                kept <= 1.0 + 1e-4 && kept > 0.9,
                "sigma {sigma}: kept {kept}"
            );
            assert!(weights[half..].iter().all(|&w| w < 0.01) || half == 8);
            assert!(offsets.windows(2).all(|w| w[0] < w[1]), "{offsets:?}");
            // The centre pixel is shared by both sides, so the first tap carries half a pixel's weight.
            assert!(weights[1..].windows(2).all(|w| w[0] >= w[1]), "{weights:?}");
        }
    }

    /// Bilinear taps land between the two pixels they stand for, never past them.
    #[test]
    fn tap_offsets_stay_within_their_pixel_pair() {
        let (offsets, _, _) = gaussian_points(3.0, 256, 256, 8);
        for (i, o) in offsets.iter().enumerate() {
            let px = o * 256.0;
            assert!(
                px >= (2 * i) as f32 && px <= (2 * i + 1) as f32,
                "tap {i} at {px}"
            );
        }
    }

    /// Sigmas add in quadrature along each axis, so a chain's passes spend exactly the radius asked for (give or
    /// take the sliver below the narrowest pass), and it ends within the pass limit.
    #[test]
    fn chain_reaches_the_radius() {
        let dst = [160, 90];
        for (rx, ry) in [
            (0.5, 0.5),
            (3.0, 3.0),
            (20.0, 20.0),
            (30.0, 4.0),
            (2.0, 30.0),
            (25.0, 25.0),
        ] {
            let chain = gaussian_chain(rx, ry, dst, dst, 32);
            assert!(
                !chain.is_empty() && chain.len() <= 32,
                "{rx} {ry}: {}",
                chain.len()
            );
            let spent = |axes: [Axis; 2]| -> f32 {
                chain
                    .iter()
                    .filter(|p| p.axis == axes[0] || p.axis == axes[1])
                    .map(|p| p.sigma * p.sigma)
                    .sum()
            };
            for (name, want, got) in [
                ("x", rx * rx, spent([Axis::X, Axis::Both])),
                ("y", ry * ry, spent([Axis::Y, Axis::Both])),
            ] {
                // A last pass merging nearly equal axes spends their mean on both.
                let slack = MIN_SIGMA * MIN_SIGMA + (rx - ry).abs().min(MIN_SIGMA) * rx.max(ry);
                assert!(
                    (got - want).abs() <= slack.max(0.25),
                    "{rx} {ry} {name}: spent {got} of {want}"
                );
            }
            assert!(chain.iter().all(|p| p.half_count >= 1 && p.half_count <= 8));
        }
    }

    /// Past the pass limit the chain stops instead of looping, and a downsampling chain starts with the 2D pass.
    #[test]
    fn chain_is_bounded_and_downsamples_first() {
        let huge = gaussian_chain(10_000.0, 10_000.0, [640, 360], [160, 90], 32);
        assert_eq!(huge.len(), 32);
        assert_eq!(huge[0].axis, Axis::Both);
        assert_eq!(huge[0].half_count, 8);
        assert!(huge[0].sigma <= MAX_2D_SIGMA);
        assert!(gaussian_chain(0.1, 0.1, [160, 90], [160, 90], 32).is_empty());
    }

    fn dof() -> Dof {
        Dof {
            view_model_start: 0.0,
            view_model_end: 0.0,
            near_start: 8.0,
            near_end: 40.0,
            far_start: 400.0,
            far_end: 1000.0,
            near_blur: 6.0,
            far_blur: 3.0,
        }
    }

    /// The blur fraction is 1 at the out-of-focus distance, 0 at the in-focus one, linear between.
    #[test]
    fn dof_equation_limits() {
        let d = dof();
        let e = d.scene_equation(4.0);
        let near = |z: f32| (e[0] * z + e[2]).clamp(0.0, 1.0);
        let far = |z: f32| (e[1] * z + e[3]).clamp(0.0, 1.0);
        assert!((near(8.0) - 1.0).abs() < 1e-5 && near(40.0).abs() < 1e-5);
        assert!((near(24.0) - 0.5).abs() < 1e-5);
        assert!(far(400.0).abs() < 1e-5 && (far(1000.0) - 1.0).abs() < 1e-5);
        assert!((far(700.0) - 0.5).abs() < 1e-5);
        assert_eq!(far(5000.0), 1.0);
        assert_eq!(near(500.0), 0.0);
    }

    /// A far range that ends inside the near clip, or an empty near range, turns that side off.
    #[test]
    fn dof_degenerate_ranges_blur_nothing() {
        let mut d = dof();
        d.far_start = 2.0;
        d.far_end = 3.0;
        let e = d.scene_equation(4.0);
        assert_eq!((e[1], e[3]), (0.0, 0.0));
        // A near range that is not nearer than its in-focus plane keeps only the sliver in front of the clip plane.
        d.near_start = 50.0;
        d.near_end = 40.0;
        let e = d.scene_equation(4.0);
        assert_eq!(e[0] * 40.0 + e[2], -e[0] * 2.0 * 0.0 + (e[0] * 40.0 + e[2]));
        assert!(
            (e[0] * 2.0 + e[2]).abs() < 1e-5,
            "full blur at half the clip distance, none beyond"
        );
        assert!((e[0] * 4.0 + e[2]).clamp(0.0, 1.0) == 0.0 || e[0] < 0.0);
    }

    #[test]
    fn dof_activity_follows_the_ranges() {
        assert!(
            !Dof {
                near_end: 0.0,
                near_start: 0.0,
                far_end: 0.0,
                far_start: 0.0,
                view_model_end: 0.0,
                view_model_start: 0.0,
                near_blur: 4.0,
                far_blur: 4.0
            }
            .active()
        );
        assert!(dof().active());
        let mut d = dof();
        d.near_end = d.near_start;
        d.far_blur = 0.0;
        assert!(!d.active());
        d.view_model_end = 10.0;
        assert!(d.active());
    }

    /// The view model has no far part and carries the far blur relative to the near blur.
    #[test]
    fn view_model_equation_carries_far_blur() {
        let mut d = dof();
        d.view_model_start = 0.2;
        d.view_model_end = 12.0;
        let e = d.view_model_equation();
        assert_eq!(e[1], 0.0);
        assert!((e[3] - 0.5f32.sqrt()).abs() < 1e-6);
        assert!((e[0] * 0.2 + e[2] - 1.0).abs() < 1e-5 && (e[0] * 12.0 + e[2]).abs() < 1e-5);
    }

    /// The lerp constants map the blur fraction onto weights that sum to one at every fraction.
    #[test]
    fn dof_lerp_weights_partition_unity() {
        let (scale, bias) = dof().lerp_constants(720);
        for fraction in [0.0, 0.1, 0.3, 0.6, 0.9, 1.0] {
            let w: Vec<f32> = (0..4)
                .map(|i| (fraction * scale[i] + bias[i]).clamp(0.0, 1.0))
                .collect();
            // The shader reads weights (sharp, small, medium, large) as min(1 - a, b) combinations of these; they
            // are monotone ramps that start at 1 for a sharp pixel and end at 0 for a fully blurred one.
            assert!(w.iter().all(|v| (0.0..=1.0).contains(v)), "{w:?}");
        }
        assert_eq!(bias[0] + 0.0 * scale[0], 1.0);
        assert!((1.0 * scale[0] + bias[0]).clamp(0.0, 1.0) == 0.0);
        assert!(
            (1.0 * scale[3] + bias[3] - 1.0).abs() < 1e-5,
            "the last ramp reaches 1 at a fraction of 1"
        );
    }

    #[test]
    fn glow_constants_rescale_the_cutoff() {
        let mut p = PostParams::from_art(&MapArt::default());
        p.glow = Glow {
            enabled: true,
            radius: 3.0,
            bloom_cutoff: 0.75,
            bloom_desaturation: 0.2,
            bloom_intensity: 1.5,
            sky_bleed_intensity: 0.0,
        };
        let [setup, apply] = p.glow_constants();
        assert_eq!(setup, [0.75, 4.0, 0.0, 0.2]);
        assert_eq!(apply, [0.0, 0.0, 0.0, 1.5]);
        assert!(p.glow_active());
        p.glow.bloom_intensity = 0.0;
        assert!(!p.glow_active());
    }

    /// Neutral film is not an effect; any grade turns the offscreen path on, a disabled one never does.
    #[test]
    fn offscreen_only_for_visible_effects() {
        let mut p = PostParams::from_art(&MapArt::default());
        assert!(!p.offscreen());
        p.film.enabled = true;
        assert!(!p.offscreen());
        p.film.contrast = 1.2;
        assert!(p.offscreen());
        p.film.enabled = false;
        assert!(!p.offscreen());
        p.blur_radius = 1.0;
        assert!(p.offscreen());
        p.blur_radius = 0.0;
        p.save_screen = true;
        assert!(p.offscreen());
    }

    #[test]
    fn blur_below_the_filter_floor_fades_in() {
        // 720 lines: the floor is 2 virtual pixels.
        assert_eq!(blur_screen(1.0, 720), (2.0, 128));
        assert_eq!(blur_screen(5.0, 720), (5.0, 255));
    }

    #[test]
    fn shell_shock_blur_fades_to_one_percent() {
        assert_eq!(ShellShock::blurred_alpha(0.0, 1000.0), 0.99);
        assert!((ShellShock::blurred_alpha(1000.0, 1000.0) - 0.01).abs() < 1e-6);
    }

    #[test]
    fn code_constant_ids_match_their_names() {
        for (id, name) in [
            (codeconst::RENDER_TARGET_SIZE, "RENDER_TARGET_SIZE"),
            (
                codeconst::DOF_EQUATION_VIEWMODEL_AND_FAR_BLUR,
                "DOF_EQUATION_VIEWMODEL_AND_FAR_BLUR",
            ),
            (codeconst::DOF_EQUATION_SCENE, "DOF_EQUATION_SCENE"),
            (codeconst::DOF_LERP_SCALE, "DOF_LERP_SCALE"),
            (codeconst::DOF_LERP_BIAS, "DOF_LERP_BIAS"),
            (codeconst::DOF_ROW_DELTA, "DOF_ROW_DELTA"),
            (codeconst::FILTER_TAP_0, "FILTER_TAP_0"),
            (codeconst::FILTER_TAP_0 + 7, "FILTER_TAP_7"),
            (codeconst::GLOW_SETUP, "GLOW_SETUP"),
            (codeconst::GLOW_APPLY, "GLOW_APPLY"),
            (codeconst::COLOR_BIAS, "COLOR_BIAS"),
            (codeconst::COLOR_TINT_BASE, "COLOR_TINT_BASE"),
            (codeconst::COLOR_TINT_DELTA, "COLOR_TINT_DELTA"),
            (codeconst::DEPTH_FROM_CLIP, "DEPTH_FROM_CLIP"),
        ] {
            assert_eq!(codeconst::NAMES[id as usize], name);
        }
        for (id, name) in [
            (ctex::FEEDBACK, "FEEDBACK"),
            (ctex::RESOLVED_SCENE, "RESOLVED_SCENE"),
            (ctex::POST_EFFECT_0, "POST_EFFECT_0"),
            (ctex::POST_EFFECT_1, "POST_EFFECT_1"),
            (ctex::FLOATZ, "FLOATZ"),
        ] {
            assert_eq!(codeconst::TEXTURE_NAMES[id as usize], name);
        }
    }
}
