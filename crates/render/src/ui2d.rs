// SPDX-License-Identifier: GPL-3.0-or-later
//! Immediate-mode 2D layer for menus, HUD text and icons.
//!
//! Coordinates are target pixels with the origin at the top left and y growing down; the caller maps its own virtual
//! resolution to pixels. A frame is `begin`, any number of `quad`/`quad_rot`/`draw_text`/`scissor` calls, then `flush`,
//! which encodes one render pass and submits it. Consecutive quads with the same image and scissor share one draw.
//! Colours are straight (non-premultiplied) RGBA in 0..1, quantised to 8 bits per channel like the original's packed
//! vertex colours, and are multiplied with the texture sample. The colours are written as they are: pick a non-sRGB
//! target format, as D3D9 blended UI in gamma space.
//!
//! ## Materials
//!
//! [`Ui2d::image`] reduces a 2D `Material` to a texture, a blend mode and a sampler:
//!
//! * Texture: the first texture def with the colour-map semantic (0, the only one every stock `white`, `hud_*`,
//!   `compass_*`, `weapon_*`, `ui_*` and font material has), else the first image def. A cube image, an image that
//!   cannot be loaded, or a material that is only a reference stub (no texture defs: the `,name` placeholders in
//!   `ui_mp`/`common_mp`) becomes a 1x1 white image and its name is appended to [`Ui2d::missing`].
//! * Blend, from the first `state_bits` entry via [`StateBits::blend`]: no blend factors (e.g. `loadscreen_*`,
//!   `0x18128812`) is [`Blend::Opaque`]; a destination factor of One (the font glow materials, `0x19289125`:
//!   SrcAlpha/One) is [`Blend::Additive`]; everything else (SrcAlpha/InvSrcAlpha, `0x19285165`/`0x19289165`: `white`,
//!   fonts, icons) is [`Blend::Alpha`]. A material without state bits is alpha blended.
//! * Sampler, from the colour map's packed sampler-state byte: filter bits 0..2 (1 is point, anything else linear;
//!   the anisotropic levels are not used in 2D), mip filter bits 3..4 (0 none, 1 nearest, else linear), clamp u/v at
//!   bits 5/6 (clear means repeat: `white`'s byte is `0x01`; icons and fonts are clamped).
//!
//! ## Text
//!
//! Text follows the original's `DrawText2D`: the glyph table is indexed by letter, the pen advances by each glyph's
//! `dx`, `^0`..`^9` select a colour (and are skipped by [`text_width`]), `\n` starts a line `pixel_height` lower and
//! `\r` returns to the line start. The original shifts text by half a font pixel for D3D9's pixel-centre rule; that is
//! not reproduced since wgpu samples at pixel centres. Not implemented: the text cursor, the pulse/decay effects, the
//! inline hud-icon escape (`^` followed by byte 1 or 2) and rotated text.

use crate::gpu::Gpu;
use crate::state::StateBits;
use crate::texture::{self, Tex, TextureCache};
use assets::zone::gfx::{Material, TextureSource};
use assets::zone::text::{Font, Glyph};
use bytemuck::{Pod, Zeroable};
use sm3::SamplerDim;
use std::collections::HashMap;
use std::sync::Arc;

/// How an image combines with what is already in the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    Opaque,
    Alpha,
    Additive,
}

impl Blend {
    /// The mapping documented in the module comment.
    pub fn from_state(state: Option<StateBits>) -> Blend {
        use wgpu::BlendFactor::One;
        match state.map(StateBits::blend) {
            None => Blend::Alpha,
            Some(None) => Blend::Opaque,
            Some(Some(b)) if b.color.dst_factor == One => Blend::Additive,
            Some(Some(_)) => Blend::Alpha,
        }
    }
}

struct ImageData {
    bind_group: wgpu::BindGroup,
    blend: Blend,
    /// Keeps the texture alive; the bind group holds its view.
    _tex: Arc<Tex>,
}

/// A material reduced to what 2D drawing needs. Cheap to clone; equal when it is the same image object.
#[derive(Clone)]
pub struct UiImage(Arc<ImageData>);

impl UiImage {
    pub fn blend(&self) -> Blend {
        self.0.blend
    }
}

impl PartialEq for UiImage {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

fn color_bytes(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

/// One draw call: `count` vertices from `first` with one image and scissor.
#[derive(Clone, Debug, PartialEq)]
struct Batch<H> {
    image: H,
    scissor: Option<[u32; 4]>,
    first: u32,
    count: u32,
}

/// The GPU-free half of a frame: quads to vertices, merged into batches.
struct Batcher<H> {
    size: (u32, u32),
    vertices: Vec<Vertex>,
    batches: Vec<Batch<H>>,
    scissor: Option<[u32; 4]>,
}

impl<H: Clone + PartialEq> Batcher<H> {
    fn new() -> Self {
        Batcher {
            size: (1, 1),
            vertices: Vec::new(),
            batches: Vec::new(),
            scissor: None,
        }
    }

    fn begin(&mut self, size: (u32, u32)) {
        self.size = (size.0.max(1), size.1.max(1));
        self.vertices.clear();
        self.batches.clear();
        self.scissor = None;
    }

    /// `[x, y, w, h]` in pixels, rounded to whole pixels and clamped to the target; an empty result hides everything.
    fn set_scissor(&mut self, rect: Option<[f32; 4]>) {
        self.scissor = rect.map(|[x, y, w, h]| {
            let edge = |v: f32, max: u32| v.round().clamp(0.0, max as f32) as u32;
            let (x0, y0) = (edge(x, self.size.0), edge(y, self.size.1));
            let (x1, y1) = (edge(x + w, self.size.0), edge(y + h, self.size.1));
            [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)]
        });
    }

    fn quad(
        &mut self,
        image: &H,
        rect: [f32; 4],
        uv: [f32; 4],
        color: [f32; 4],
        rot: Option<(f32, [f32; 2])>,
    ) {
        if matches!(self.scissor, Some([_, _, 0, _] | [_, _, _, 0])) {
            return;
        }
        // A negative extent mirrors the picture inside the same box (`UI_DrawHandlePic`), it does not grow leftward.
        let [x, y, mut w, mut h] = rect;
        let [mut s0, mut t0, mut s1, mut t1] = uv;
        if w < 0.0 {
            w = -w;
            std::mem::swap(&mut s0, &mut s1);
        }
        if h < 0.0 {
            h = -h;
            std::mem::swap(&mut t0, &mut t1);
        }
        let mut p = [[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
        if let Some((angle, [px, py])) = rot {
            let (s, c) = angle.sin_cos();
            for q in &mut p {
                let (dx, dy) = (q[0] - px, q[1] - py);
                *q = [px + dx * c - dy * s, py + dy * c + dx * s];
            }
        }
        let t = [[s0, t0], [s1, t0], [s1, t1], [s0, t1]];
        let color = color_bytes(color);
        let v = |i: usize| Vertex {
            pos: p[i],
            uv: t[i],
            color,
        };
        let first = self.vertices.len() as u32;
        self.vertices.extend([v(0), v(1), v(2), v(0), v(2), v(3)]);
        match self.batches.last_mut() {
            Some(b) if b.image == *image && b.scissor == self.scissor => b.count += 6,
            _ => self.batches.push(Batch {
                image: image.clone(),
                scissor: self.scissor,
                first,
                count: 6,
            }),
        }
    }
}

const SHADER: &str = "
struct Globals { size: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> Out {
    var o: Out;
    o.pos = vec4<f32>(pos.x / g.size.x * 2.0 - 1.0, 1.0 - pos.y / g.size.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    o.color = color;
    return o;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    return in.color * textureSample(tex, samp, in.uv);
}
";

const INITIAL_VERTICES: u64 = 4096;

/// Sampler variants that matter in 2D, packed from the material's sampler-state byte.
fn sampler_key(state: u8) -> u8 {
    let nearest = state & 7 == 1;
    let mip = match (state >> 3) & 3 {
        0 => 0,
        1 => 1,
        _ => 2,
    };
    u8::from(nearest) | mip << 1 | ((state >> 5) & 3) << 3
}

fn make_sampler(device: &wgpu::Device, key: u8) -> wgpu::Sampler {
    let addr = |clamp: bool| {
        if clamp {
            wgpu::AddressMode::ClampToEdge
        } else {
            wgpu::AddressMode::Repeat
        }
    };
    let filter = if key & 1 != 0 {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    let mip = (key >> 1) & 3;
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ui2d"),
        address_mode_u: addr(key & 8 != 0),
        address_mode_v: addr(key & 16 != 0),
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: if mip == 2 {
            wgpu::MipmapFilterMode::Linear
        } else {
            wgpu::MipmapFilterMode::Nearest
        },
        lod_max_clamp: if mip == 0 { 0.0 } else { 32.0 },
        ..Default::default()
    })
}

pub struct Ui2d {
    gpu: Arc<Gpu>,
    pipelines: [wgpu::RenderPipeline; 3],
    globals: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    tex_layout: wgpu::BindGroupLayout,
    samplers: HashMap<u8, wgpu::Sampler>,
    images: HashMap<String, UiImage>,
    white: UiImage,
    vbuf: wgpu::Buffer,
    vbuf_vertices: u64,
    batcher: Batcher<UiImage>,
    /// Materials that fell back to the white image, once each, in the order they were asked for.
    pub missing: Vec<String>,
    /// Colours of `^8` (the viewer's team) and `^9` (the other team); the original takes them from the
    /// `g_TeamColor_*` dvars.
    pub team_colors: [[f32; 4]; 2],
}

fn blend_index(b: Blend) -> usize {
    match b {
        Blend::Opaque => 0,
        Blend::Alpha => 1,
        Blend::Additive => 2,
    }
}

fn vertex_buffer(device: &wgpu::Device, vertices: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ui2d vertices"),
        size: vertices * std::mem::size_of::<Vertex>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn build_image(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    samplers: &mut HashMap<u8, wgpu::Sampler>,
    tex: Arc<Tex>,
    blend: Blend,
    sampler_state: u8,
) -> UiImage {
    let key = sampler_key(sampler_state);
    let sampler = samplers
        .entry(key)
        .or_insert_with(|| make_sampler(device, key));
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ui2d image"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&tex.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    UiImage(Arc::new(ImageData {
        bind_group,
        blend,
        _tex: tex,
    }))
}

impl Ui2d {
    /// `format` is the colour format of every target later passed to [`Ui2d::flush`].
    pub fn new(gpu: Arc<Gpu>, format: wgpu::TextureFormat) -> Ui2d {
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui2d"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui2d globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui2d image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui2d"),
            bind_group_layouts: &[Some(&globals_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Unorm8x4];
        let pipeline = |blend: Option<wgpu::BlendState>, label| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attrs,
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let comp = |src, dst| wgpu::BlendComponent {
            src_factor: src,
            dst_factor: dst,
            operation: wgpu::BlendOperation::Add,
        };
        use wgpu::BlendFactor as F;
        let pipelines = [
            pipeline(None, "ui2d opaque"),
            pipeline(
                Some(wgpu::BlendState {
                    color: comp(F::SrcAlpha, F::OneMinusSrcAlpha),
                    alpha: comp(F::One, F::OneMinusSrcAlpha),
                }),
                "ui2d alpha",
            ),
            pipeline(
                Some(wgpu::BlendState {
                    color: comp(F::SrcAlpha, F::One),
                    alpha: comp(F::Zero, F::One),
                }),
                "ui2d additive",
            ),
        ];
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui2d globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui2d globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let vbuf = vertex_buffer(device, INITIAL_VERTICES);
        let white_tex = Arc::new(texture::upload(
            &gpu,
            "ui2d white",
            SamplerDim::D2,
            [1, 1, 1],
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            &[255; 4],
        ));
        let mut samplers = HashMap::new();
        let white = build_image(
            device,
            &tex_layout,
            &mut samplers,
            white_tex,
            Blend::Alpha,
            0xE2,
        );
        Ui2d {
            gpu: gpu.clone(),
            pipelines,
            globals,
            globals_bg,
            tex_layout,
            samplers,
            images: HashMap::new(),
            white,
            vbuf,
            vbuf_vertices: INITIAL_VERTICES,
            batcher: Batcher::new(),
            missing: Vec::new(),
            team_colors: [[1.0; 4]; 2],
        }
    }

    fn make_image(&mut self, tex: Arc<Tex>, blend: Blend, sampler_state: u8) -> UiImage {
        build_image(
            &self.gpu.device,
            &self.tex_layout,
            &mut self.samplers,
            tex,
            blend,
            sampler_state,
        )
    }

    /// A 1x1 opaque white image, alpha blended: solid fills take their colour from the quad.
    pub fn white(&self) -> UiImage {
        self.white.clone()
    }

    /// The material as a drawable image, cached by lower-cased material name.
    pub fn image(&mut self, cache: &mut TextureCache, mat: &Material) -> UiImage {
        let name = mat.name.as_deref().unwrap_or("").to_ascii_lowercase();
        if let Some(i) = self.images.get(&name) {
            return i.clone();
        }
        let def = mat
            .textures
            .iter()
            .filter(|t| matches!(t.source, TextureSource::Image(Some(_))))
            .find(|t| t.semantic == 0)
            .or_else(|| {
                mat.textures
                    .iter()
                    .find(|t| matches!(t.source, TextureSource::Image(Some(_))))
            });
        let loaded = def.and_then(|t| match &t.source {
            TextureSource::Image(Some(img)) => cache
                .image(&self.gpu, img)
                .filter(|tex| tex.dim == SamplerDim::D2)
                .map(|tex| (tex, t.sampler_state)),
            _ => None,
        });
        let blend = Blend::from_state(mat.state_bits.first().copied().map(StateBits));
        let image = match loaded {
            Some((tex, state)) => self.make_image(tex, blend, state),
            None => {
                self.missing.push(name.clone());
                let tex = self.white.0._tex.clone();
                self.make_image(tex, blend, 0xE2)
            }
        };
        self.images.insert(name, image.clone());
        image
    }

    /// A loose image (`images/<name>.iwi`) as the original's default 2D material: alpha blended, linear, clamped.
    /// Cached by lower-cased name; an image that cannot be loaded is white and recorded in [`Ui2d::missing`].
    pub fn image_named(&mut self, cache: &mut TextureCache, name: &str) -> UiImage {
        let key = name.trim_start_matches(',').to_ascii_lowercase();
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let tex = cache
            .named(&self.gpu, &key)
            .filter(|t| t.dim == SamplerDim::D2);
        let tex = tex.unwrap_or_else(|| {
            self.missing.push(key.clone());
            self.white.0._tex.clone()
        });
        let image = self.make_image(tex, Blend::Alpha, 0xE2);
        self.images.insert(key, image.clone());
        image
    }

    /// Starts a frame on a target of `size` pixels; drops whatever was queued and resets the scissor.
    pub fn begin(&mut self, size: (u32, u32)) {
        self.batcher.begin(size);
    }

    /// An axis-aligned image rectangle: `rect` is `[x, y, w, h]` and `uv` is `[s0, t0, s1, t1]`.
    pub fn quad(&mut self, img: &UiImage, rect: [f32; 4], uv: [f32; 4], color: [f32; 4]) {
        self.batcher.quad(img, rect, uv, color, None);
    }

    /// Like [`Ui2d::quad`], rotated by `angle_rad` about `pivot` (pixels). Positive angles turn clockwise on screen.
    pub fn quad_rot(
        &mut self,
        img: &UiImage,
        rect: [f32; 4],
        uv: [f32; 4],
        color: [f32; 4],
        angle_rad: f32,
        pivot: [f32; 2],
    ) {
        self.batcher
            .quad(img, rect, uv, color, Some((angle_rad, pivot)));
    }

    /// Clips later quads to `[x, y, w, h]` pixels; `None` removes the clip.
    pub fn scissor(&mut self, rect: Option<[f32; 4]>) {
        self.batcher.set_scissor(rect);
    }

    /// Text with the baseline at `y_baseline`; see the module comment for what is supported. `glow_img` is the
    /// font's glow material and only used when `style.glow` is set. `max_chars` of 0 means no limit.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        font: &Font,
        font_img: &UiImage,
        glow_img: Option<&UiImage>,
        text: &str,
        x: f32,
        y_baseline: f32,
        scale: f32,
        color: [f32; 4],
        style: TextStyle,
        max_chars: usize,
    ) {
        let b = &mut self.batcher;
        layout_text(
            font,
            text,
            [x, y_baseline],
            scale,
            color,
            style,
            max_chars,
            self.team_colors,
            |q| match (q.pass, glow_img) {
                (GlyphPass::Glow, Some(g)) => b.quad(g, q.rect, q.uv, q.color, None),
                (GlyphPass::Glow, None) => {}
                _ => b.quad(font_img, q.rect, q.uv, q.color, None),
            },
        );
    }

    /// Encodes everything queued since `begin` as one render pass on `target` (cleared to `clear`, or loaded) and
    /// submits it. The queue is empty afterwards. With nothing queued and no clear, nothing is submitted.
    pub fn flush(&mut self, target: &wgpu::TextureView, clear: Option<wgpu::Color>) {
        let b = &self.batcher;
        if b.batches.is_empty() && clear.is_none() {
            return;
        }
        let gpu = &self.gpu;
        let need = b.vertices.len() as u64;
        if need > self.vbuf_vertices {
            self.vbuf_vertices = need.max(self.vbuf_vertices * 2);
            self.vbuf = vertex_buffer(&gpu.device, self.vbuf_vertices);
        }
        gpu.queue
            .write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&b.vertices));
        let (w, h) = b.size;
        gpu.queue.write_buffer(
            &self.globals,
            0,
            bytemuck::cast_slice(&[w as f32, h as f32, 0.0, 0.0]),
        );
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ui2d"),
            });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui2d"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_bind_group(0, &self.globals_bg, &[]);
            rp.set_vertex_buffer(0, self.vbuf.slice(..));
            let mut scissor = None;
            rp.set_scissor_rect(0, 0, w, h);
            for batch in &b.batches {
                if batch.scissor != scissor {
                    scissor = batch.scissor;
                    let [x, y, sw, sh] = scissor.unwrap_or([0, 0, w, h]);
                    rp.set_scissor_rect(x, y, sw, sh);
                }
                let img = &batch.image.0;
                rp.set_pipeline(&self.pipelines[blend_index(img.blend)]);
                rp.set_bind_group(1, &img.bind_group, &[]);
                rp.draw(batch.first..batch.first + batch.count, 0..1);
            }
        }
        gpu.queue.submit([enc.finish()]);
        self.batcher.vertices.clear();
        self.batcher.batches.clear();
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Text

/// Text decoration as the original menus select it with `textstyle`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextStyle {
    /// Drop-shadow offset in pixels, not scaled with the font: 0 none, 1 shadowed, 2 shadowed more. The shadow is
    /// black with the text's base alpha.
    pub shadow: u8,
    /// Every glyph advances by the width of an `o` instead of its own.
    pub monospace: bool,
    /// Glow colour `[r, g, b, a]` (the item's `glowcolor`); `None`, or an alpha of 0, draws no glow. The glow is
    /// four copies of the glyph, 1.75x as wide and 1.125x as tall, from the glow material, tinted by a tenth of
    /// this colour.
    pub glow: Option<[f32; 4]>,
}

impl TextStyle {
    pub const PLAIN: TextStyle = TextStyle {
        shadow: 0,
        monospace: false,
        glow: None,
    };

    /// The engine's `textStyle` numbers: 3 shadowed, 6 shadowed more, 128 monospace. The others (0 normal, 1 blinking
    /// which the menu code implements by skipping frames, ...) draw plain.
    pub fn from_menu_style(style: i32, glow: Option<[f32; 4]>) -> TextStyle {
        TextStyle {
            shadow: match style {
                3 => 1,
                6 => 2,
                _ => 0,
            },
            monospace: style == 128,
            glow,
        }
    }
}

/// `^0`..`^6` colours as 8-bit RGB; `^7` is the caller's colour and `^8`/`^9` the team colours.
const COLOR_TABLE: [[u8; 3]; 7] = [
    [0, 0, 0],
    [255, 92, 92],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [0, 255, 255],
    [255, 92, 255],
];

/// Offsets of the four glow copies, in pixels before font scaling.
const GLOW_OFFSETS: [[f32; 2]; 4] = [[-1.0, -1.0], [-1.0, 1.0], [1.0, -1.0], [1.0, 1.0]];

/// The font's glyph for `letter`: ASCII 0x20..=0x7F by index, the rest by binary search from index 96 on, and glyph
/// 14 (the period) for letters the font lacks. `None` only for a font too small to have that substitute.
pub fn find_glyph(font: &Font, letter: u32) -> Option<&Glyph> {
    let g: &[Glyph] = &font.glyphs;
    if (0x20..=0x7F).contains(&letter) {
        if let Some(x) = g
            .get(letter as usize - 32)
            .filter(|x| u32::from(x.letter) == letter)
        {
            return Some(x);
        }
    } else if let (Ok(l), Some(rest)) = (u16::try_from(letter), g.get(96..))
        && let Ok(i) = rest.binary_search_by_key(&l, |x| x.letter)
    {
        return Some(&rest[i]);
    }
    g.get(14)
}

/// `scale` in menu units to the font-pixel multiplier: the original sizes fonts relative to 48 pixels.
pub fn font_scale_normalized(font: &Font, scale: f32) -> f32 {
    scale * 48.0 / font.pixel_height.max(1) as f32
}

/// Walks `text` the way the original's `R_TextWidth` and `DrawText2D` do and calls `f(glyph, pen_x, pen_y, colour)`
/// for each drawn letter, with the pen in unscaled font pixels relative to the start. `max_chars` counts drawn
/// letters (0: no limit).
fn walk(
    font: &Font,
    text: &str,
    max_chars: usize,
    base: [f32; 4],
    team: [[f32; 4]; 2],
    monospace: bool,
    mut f: impl FnMut(&Glyph, f32, f32, [f32; 4]),
) {
    let limit = if max_chars == 0 {
        usize::MAX
    } else {
        max_chars
    };
    let mono = find_glyph(font, 'o' as u32).map_or(0.0, |g| f32::from(g.dx));
    let (mut x, mut y, mut count) = (0.0f32, 0.0f32, 0);
    let mut color = base;
    let mut chars = text.chars().peekable();
    while count < limit {
        let Some(c) = chars.next() else { break };
        match c {
            '\n' => {
                x = 0.0;
                y += font.pixel_height as f32;
            }
            '\r' => x = 0.0,
            '^' if chars.peek().is_some_and(char::is_ascii_digit) => {
                let d = chars.next().map_or(7, |d| d as usize - '0' as usize);
                let rgb = match d {
                    0..=6 => COLOR_TABLE[d].map(|v| f32::from(v) / 255.0),
                    7 => [base[0], base[1], base[2]],
                    _ => {
                        let t = team[d - 8];
                        [t[0], t[1], t[2]]
                    }
                };
                color = [rgb[0], rgb[1], rgb[2], base[3]];
            }
            c => {
                if let Some(g) = find_glyph(font, c as u32) {
                    f(g, x, y, color);
                    x += if monospace { mono } else { f32::from(g.dx) };
                }
                count += 1;
            }
        }
    }
}

/// Width of the widest line of `text` in pixels at `scale` (menu units); colour escapes take no room.
pub fn text_width(font: &Font, text: &str, max_chars: usize, scale: f32) -> f32 {
    let mut width = 0.0f32;
    walk(
        font,
        text,
        max_chars,
        [0.0; 4],
        [[0.0; 4]; 2],
        false,
        |g, x, _, _| {
            width = width.max(x + f32::from(g.dx));
        },
    );
    width * font_scale_normalized(font, scale)
}

/// Height of one line in pixels at `scale` (menu units).
pub fn text_height(font: &Font, scale: f32) -> f32 {
    font.pixel_height as f32 * font_scale_normalized(font, scale)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphPass {
    Shadow,
    Face,
    Glow,
}

/// One glyph rectangle of laid-out text, in target pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphQuad {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub pass: GlyphPass,
}

/// Lays text out as glyph quads in drawing order: the face pass (each glyph's shadow, then the glyph), then, with a
/// glow colour, the glow pass. `origin` is the start of the first baseline.
#[allow(clippy::too_many_arguments)]
pub fn layout_text(
    font: &Font,
    text: &str,
    origin: [f32; 2],
    scale: f32,
    color: [f32; 4],
    style: TextStyle,
    max_chars: usize,
    team: [[f32; 4]; 2],
    mut emit: impl FnMut(GlyphQuad),
) {
    let s = font_scale_normalized(font, scale);
    let [ox, oy] = origin;
    let glyph_rect = |g: &Glyph, x: f32, y: f32| {
        [
            ox + (x + f32::from(g.x0)) * s,
            oy + (y * s) + f32::from(g.y0) * s,
            f32::from(g.pixel_width) * s,
            f32::from(g.pixel_height) * s,
        ]
    };
    let uv = |g: &Glyph| [g.s0, g.t0, g.s1, g.t1];
    walk(
        font,
        text,
        max_chars,
        color,
        team,
        style.monospace,
        |g, x, y, c| {
            if g.pixel_width == 0 || g.pixel_height == 0 {
                return;
            }
            let r = glyph_rect(g, x, y);
            if style.shadow > 0 {
                let o = f32::from(style.shadow);
                emit(GlyphQuad {
                    rect: [r[0] + o, r[1] + o, r[2], r[3]],
                    uv: uv(g),
                    color: [0.0, 0.0, 0.0, color[3]],
                    pass: GlyphPass::Shadow,
                });
            }
            emit(GlyphQuad {
                rect: r,
                uv: uv(g),
                color: c,
                pass: GlyphPass::Face,
            });
        },
    );
    let Some(glow) = style.glow.filter(|g| g[3] > 0.0) else {
        return;
    };
    walk(
        font,
        text,
        max_chars,
        color,
        team,
        style.monospace,
        |g, x, y, c| {
            if g.pixel_width == 0 || g.pixel_height == 0 {
                return;
            }
            let (pw, ph) = (f32::from(g.pixel_width), f32::from(g.pixel_height));
            let r = glyph_rect(g, x, y);
            let color = [glow[0] * 0.1, glow[1] * 0.1, glow[2] * 0.1, c[3]];
            for [dx, dy] in GLOW_OFFSETS {
                emit(GlyphQuad {
                    rect: [
                        r[0] - pw * 0.75 * 0.5 * s + dx * 2.0 * s,
                        r[1] - ph * 0.125 * 0.5 * s + dy * 2.0 * s,
                        pw * 1.75 * s,
                        ph * 1.125 * s,
                    ],
                    uv: uv(g),
                    color,
                    pass: GlyphPass::Glow,
                });
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(letter: u16, dx: u8) -> Glyph {
        Glyph {
            letter,
            x0: 1,
            y0: -8,
            dx,
            pixel_width: 6,
            pixel_height: 10,
            s0: f32::from(letter),
            t0: 0.0,
            s1: f32::from(letter) + 0.5,
            t1: 1.0,
        }
    }

    /// 0x20..=0x7F with dx = 5 (`o` is 7, `W` is 9, the period is 3), then letters 0xA1 and 0xE9 with dx 4.
    fn test_font() -> Font {
        let mut glyphs: Vec<Glyph> = (0x20..=0x7F)
            .map(|l| {
                let dx = match l {
                    0x6F => 7,
                    0x57 => 9,
                    0x2E => 3,
                    _ => 5,
                };
                glyph(l, dx)
            })
            .collect();
        glyphs.push(glyph(0xA1, 4));
        glyphs.push(glyph(0xE9, 4));
        Font {
            name: None,
            pixel_height: 24,
            material: None,
            glow_material: None,
            glyphs: glyphs.into(),
        }
    }

    const WHITE: [f32; 4] = [1.0; 4];

    fn lay(font: &Font, text: &str, style: TextStyle, max: usize) -> Vec<GlyphQuad> {
        let mut v = Vec::new();
        layout_text(
            font,
            text,
            [100.0, 50.0],
            0.5,
            WHITE,
            style,
            max,
            [[0.5; 4]; 2],
            |q| v.push(q),
        );
        v
    }

    #[test]
    fn normalized_scale_is_relative_to_48_pixels() {
        let f = test_font();
        assert_eq!(font_scale_normalized(&f, 1.0), 2.0);
        assert_eq!(text_height(&f, 0.5), 24.0);
    }

    #[test]
    fn width_sums_advances_skips_escapes_and_takes_the_widest_line() {
        let f = test_font();
        // scale 0.5 on a 24px font is a multiplier of 1.
        assert_eq!(text_width(&f, "abc", 0, 0.5), 15.0);
        assert_eq!(text_width(&f, "^1ab^7c", 0, 0.5), 15.0);
        assert_eq!(text_width(&f, "ab\nW", 0, 0.5), 10.0);
        assert_eq!(text_width(&f, "a\nWW\r\nb", 0, 0.5), 18.0);
        assert_eq!(text_width(&f, "abcd", 2, 0.5), 10.0);
        assert_eq!(text_width(&f, "", 0, 0.5), 0.0);
    }

    #[test]
    fn only_caret_digit_is_an_escape() {
        let f = test_font();
        // "^^1" draws a caret then escapes; "^a" and a trailing caret draw as letters.
        assert_eq!(text_width(&f, "^^1", 0, 0.5), 5.0);
        assert_eq!(text_width(&f, "^a", 0, 0.5), 10.0);
        assert_eq!(text_width(&f, "a^", 0, 0.5), 10.0);
    }

    #[test]
    fn glyph_lookup_indexes_ascii_searches_the_rest_and_substitutes() {
        let f = test_font();
        assert_eq!(find_glyph(&f, 'A' as u32).unwrap().letter, 'A' as u16);
        assert_eq!(find_glyph(&f, 0x7F).unwrap().letter, 0x7F);
        assert_eq!(find_glyph(&f, 0xE9).unwrap().letter, 0xE9);
        assert_eq!(find_glyph(&f, 0xA1).unwrap().letter, 0xA1);
        // missing: control character, a letter between stored ones, beyond u16
        for l in [0x01, 0xB0, 0x1_0000] {
            assert_eq!(find_glyph(&f, l).unwrap().letter, 0x2E);
        }
    }

    #[test]
    fn layout_places_glyphs_from_the_baseline() {
        let f = test_font();
        let q = lay(&f, "Wa", TextStyle::PLAIN, 0);
        assert_eq!(q.len(), 2);
        // multiplier 1: x + x0, baseline + y0, the glyph's size, uv from the glyph
        assert_eq!(q[0].rect, [101.0, 42.0, 6.0, 10.0]);
        assert_eq!(q[0].uv, [f32::from(b'W'), 0.0, f32::from(b'W') + 0.5, 1.0]);
        assert_eq!(q[1].rect, [101.0 + 9.0, 42.0, 6.0, 10.0]);
        assert!(
            q.iter()
                .all(|q| q.pass == GlyphPass::Face && q.color == WHITE)
        );
    }

    #[test]
    fn newline_drops_a_font_line_and_carriage_return_does_not() {
        let f = test_font();
        let q = lay(&f, "a\nb\rc", TextStyle::PLAIN, 0);
        assert_eq!(q.len(), 3);
        assert_eq!((q[1].rect[0], q[1].rect[1]), (101.0, 42.0 + 24.0));
        assert_eq!((q[2].rect[0], q[2].rect[1]), (101.0, 42.0 + 24.0));
    }

    #[test]
    fn escapes_recolour_keep_the_base_alpha_and_seven_restores() {
        let f = test_font();
        let mut v = Vec::new();
        layout_text(
            &f,
            "^1a^7b^8c^9d",
            [0.0; 2],
            0.5,
            [0.2, 0.4, 0.6, 0.5],
            TextStyle::PLAIN,
            0,
            [[0.1, 0.2, 0.3, 1.0], [0.9, 0.8, 0.7, 1.0]],
            |q| v.push(q.color),
        );
        assert_eq!(v[0], [1.0, 92.0 / 255.0, 92.0 / 255.0, 0.5]);
        assert_eq!(v[1], [0.2, 0.4, 0.6, 0.5]);
        assert_eq!(v[2], [0.1, 0.2, 0.3, 0.5]);
        assert_eq!(v[3], [0.9, 0.8, 0.7, 0.5]);
    }

    #[test]
    fn max_chars_counts_letters_not_escapes() {
        let f = test_font();
        assert_eq!(lay(&f, "^1abcd", TextStyle::PLAIN, 2).len(), 2);
    }

    #[test]
    fn shadow_precedes_each_glyph_offset_in_pixels_with_black_base_alpha() {
        let f = test_font();
        let style = TextStyle::from_menu_style(6, None);
        let mut v = Vec::new();
        layout_text(
            &f,
            "^1a",
            [0.0; 2],
            1.0,
            [1.0, 1.0, 1.0, 0.25],
            style,
            0,
            [[0.0; 4]; 2],
            |q| v.push(q),
        );
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].pass, GlyphPass::Shadow);
        assert_eq!(v[0].color, [0.0, 0.0, 0.0, 0.25]);
        // scale 1.0 on 24px is a multiplier of 2; the 2px shadow offset is not scaled
        assert_eq!(v[1].rect, [2.0, -16.0, 12.0, 20.0]);
        assert_eq!(v[0].rect, [4.0, -14.0, 12.0, 20.0]);
    }

    #[test]
    fn glow_follows_the_face_with_four_larger_dimmed_copies() {
        let f = test_font();
        let style = TextStyle {
            glow: Some([1.0, 0.5, 0.0, 0.3]),
            ..TextStyle::PLAIN
        };
        let q = lay(&f, "ab", style, 0);
        assert_eq!(q.len(), 2 + 8);
        assert!(q[..2].iter().all(|q| q.pass == GlyphPass::Face));
        let g = &q[2];
        assert_eq!(g.pass, GlyphPass::Glow);
        assert_eq!(g.color, [0.1, 0.05, 0.0, 1.0]);
        assert_eq!(&g.rect[2..], &[6.0 * 1.75, 10.0 * 1.125]);
        assert_eq!(
            lay(
                &f,
                "ab",
                TextStyle {
                    glow: Some([1.0, 1.0, 1.0, 0.0]),
                    ..style
                },
                0
            )
            .len(),
            2
        );
    }

    #[test]
    fn monospace_advances_by_the_width_of_o() {
        let f = test_font();
        let q = lay(&f, "iW.", TextStyle::from_menu_style(128, None), 0);
        assert_eq!(q[1].rect[0] - q[0].rect[0], 7.0);
        assert_eq!(q[2].rect[0] - q[1].rect[0], 7.0);
    }

    #[test]
    fn menu_styles_map_to_shadow_offsets() {
        let s = |n| TextStyle::from_menu_style(n, None).shadow;
        assert_eq!((s(0), s(1), s(3), s(6), s(128)), (0, 0, 1, 2, 0));
    }

    #[test]
    fn batches_merge_runs_and_split_on_image_or_scissor_changes() {
        let mut b = Batcher::<u32>::new();
        b.begin((100, 100));
        let q = |b: &mut Batcher<u32>, img| {
            b.quad(
                &img,
                [0.0, 0.0, 10.0, 10.0],
                [0.0, 0.0, 1.0, 1.0],
                WHITE,
                None,
            )
        };
        q(&mut b, 1);
        q(&mut b, 1);
        q(&mut b, 2);
        b.set_scissor(Some([10.4, 10.6, 20.0, 20.0]));
        q(&mut b, 2);
        q(&mut b, 2);
        b.set_scissor(None);
        q(&mut b, 2);
        let got: Vec<_> = b
            .batches
            .iter()
            .map(|x| (x.image, x.scissor, x.first, x.count))
            .collect();
        assert_eq!(
            got,
            [
                (1, None, 0, 12),
                (2, None, 12, 6),
                (2, Some([10, 11, 20, 20]), 18, 12),
                (2, None, 30, 6)
            ]
        );
        assert_eq!(b.vertices.len(), 36);
    }

    #[test]
    fn a_negative_extent_mirrors_the_picture_inside_the_same_box() {
        let mut b = Batcher::<u32>::new();
        b.begin((100, 100));
        b.quad(
            &1,
            [40.0, 10.0, -16.0, 8.0],
            [0.0, 0.0, 1.0, 1.0],
            WHITE,
            None,
        );
        let xs: Vec<f32> = b.vertices.iter().map(|v| v.pos[0]).collect();
        assert!(xs.iter().all(|&x| (40.0..=56.0).contains(&x)), "{xs:?}");
        // The left edge of the box shows the picture's right edge.
        assert_eq!(b.vertices[0].uv, [1.0, 0.0]);
        assert_eq!(b.vertices[1].uv, [0.0, 0.0]);
    }

    #[test]
    fn scissor_clamps_to_the_target_and_an_empty_one_hides_quads() {
        let mut b = Batcher::<u32>::new();
        b.begin((100, 50));
        b.set_scissor(Some([90.0, 40.0, 50.0, 50.0]));
        assert_eq!(b.scissor, Some([90, 40, 10, 10]));
        b.set_scissor(Some([200.0, 0.0, 5.0, 5.0]));
        b.quad(&1, [0.0; 4], [0.0; 4], WHITE, None);
        assert!(b.vertices.is_empty());
    }

    #[test]
    fn quad_vertices_are_two_triangles_in_pixels_with_8_bit_colour() {
        let mut b = Batcher::<u32>::new();
        b.begin((100, 100));
        b.quad(
            &1,
            [10.0, 20.0, 30.0, 40.0],
            [0.0, 0.25, 1.0, 0.75],
            [1.0, 0.5, 0.0, 2.0],
            None,
        );
        let p: Vec<_> = b.vertices.iter().map(|v| v.pos).collect();
        assert_eq!(
            p,
            [
                [10.0, 20.0],
                [40.0, 20.0],
                [40.0, 60.0],
                [10.0, 20.0],
                [40.0, 60.0],
                [10.0, 60.0]
            ]
        );
        assert_eq!(b.vertices[2].uv, [1.0, 0.75]);
        assert_eq!(b.vertices[0].color, [255, 128, 0, 255]);
    }

    #[test]
    fn rotation_turns_clockwise_about_the_pivot() {
        let mut b = Batcher::<u32>::new();
        b.begin((100, 100));
        // a quarter turn about the origin sends the point (10, 0) to (0, 10): clockwise with y down
        b.quad(
            &1,
            [10.0, 0.0, 10.0, 10.0],
            [0.0; 4],
            WHITE,
            Some((std::f32::consts::FRAC_PI_2, [0.0, 0.0])),
        );
        let p = b.vertices[0].pos;
        assert!(p[0].abs() < 1e-5 && (p[1] - 10.0).abs() < 1e-5, "{p:?}");
    }

    #[test]
    fn sampler_key_separates_filter_mip_and_clamp() {
        assert_eq!(sampler_key(0xE2), sampler_key(0xEB & !0x18 | 0x02));
        assert_ne!(sampler_key(0x01), sampler_key(0xE2));
        assert_ne!(sampler_key(0xE2), sampler_key(0x02));
        assert_ne!(sampler_key(0xE2 & !0x20), sampler_key(0xE2));
    }

    #[test]
    fn blend_follows_the_state_words() {
        let b = |w| Blend::from_state(Some(StateBits([w, 0xE00E_0002])));
        assert_eq!(b(0x1928_5165), Blend::Alpha);
        assert_eq!(b(0x1928_9165), Blend::Alpha);
        assert_eq!(b(0x1928_9125), Blend::Additive);
        assert_eq!(b(0x1812_8812), Blend::Opaque);
        assert_eq!(Blend::from_state(None), Blend::Alpha);
    }
}
