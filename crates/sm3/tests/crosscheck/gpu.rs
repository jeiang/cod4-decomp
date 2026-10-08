// SPDX-License-Identifier: GPL-3.0-only
//! The fixed synthetic frame: one quad, fixed constants and textures, offscreen targets, readback.
use crate::paths::{Dim, Kind, Prog};
use sm3::{Reflection, RegisterSet};
use std::collections::BTreeMap;
use wgpu::util::DeviceExt;

pub const SIZE: u32 = 32;
/// Every target is cleared to this; a pixel that still has it was never covered.
pub const CLEAR: [u8; 4] = [26, 51, 77, 102];

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: String,
    textures: BTreeMap<u8, wgpu::TextureView>,
    sampler: wgpu::Sampler,
    quad: wgpu::Buffer,
}

fn view_dim(d: Dim) -> wgpu::TextureViewDimension {
    match d {
        Dim::D2 => wgpu::TextureViewDimension::D2,
        Dim::D3 => wgpu::TextureViewDimension::D3,
        Dim::Cube => wgpu::TextureViewDimension::Cube,
    }
}

impl Gpu {
    /// `None` without a usable adapter.
    pub fn new() -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .map_err(|e| e.to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("sm3-crosscheck"),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        let mut textures = BTreeMap::new();
        for (key, dim, vd, size, layers) in [
            (0u8, wgpu::TextureDimension::D2, Dim::D2, [8, 8, 1], 1),
            (1, wgpu::TextureDimension::D3, Dim::D3, [4, 4, 4], 1),
            (2, wgpu::TextureDimension::D2, Dim::Cube, [4, 4, 1], 6),
        ] {
            let [w, h, d] = size;
            let mut data = vec![];
            for l in 0..layers {
                for z in 0..d {
                    for y in 0..h {
                        for x in 0..w {
                            // Smooth gradients so a sub-texel coordinate difference stays a small value difference.
                            let (fx, fy, fz) =
                                (x * 256 / w, y * 256 / h, (z + l) * 256 / (d * layers));
                            data.extend_from_slice(&[
                                (20 + fx * 200 / 256) as u8,
                                (30 + fy * 190 / 256) as u8,
                                (230 - (fx + fy) / 2 * 180 / 256 - fz / 4) as u8,
                                (250 - fx / 8 - fz / 4) as u8,
                            ]);
                        }
                    }
                }
            }
            let t = device.create_texture_with_data(
                &queue,
                &wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: if dim == wgpu::TextureDimension::D3 {
                            d
                        } else {
                            layers
                        },
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: dim,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &data,
            );
            textures.insert(
                key,
                t.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(view_dim(vd)),
                    ..Default::default()
                }),
            );
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let quad = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad"),
            size: 6 * 16 * 16,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Gpu {
            device,
            queue,
            adapter: format!("{} ({:?})", info.name, info.backend),
            textures,
            sampler,
            quad,
        })
    }
}

/// Value of the vertex attribute for D3D semantic (`usage`, `index`) at corner (`x`, `y`) of the quad.
fn attribute(usage: u32, index: u32, x: f32, y: f32) -> [f32; 4] {
    let (u, v) = ((x + 1.0) / 2.0, (y + 1.0) / 2.0);
    match usage {
        0 => [x, y, 0.5, 1.0],
        1 => [0.7, 0.3, 0.0, 0.0],
        2 => [0.0, 1.0, 0.0, 0.0],
        3 => [0.2673, 0.5345, 0.8018, 0.0],
        4 => [1.0, 0.0, 0.0, 0.0],
        5 => [u, v, 0.25 + 0.1 * index as f32, 1.0],
        6 => [0.8, 0.6, 0.0, 1.0],
        7 => [-0.6, 0.8, 0.0, 1.0],
        10 => [0.9 - 0.4 * u, 0.4 + 0.4 * v, 0.3 + 0.2 * u, 0.8],
        _ => [0.5, 0.5, 0.5, 1.0],
    }
}

/// Fixed value of float constant register `reg`: matrices (from the CTAB) are identity, the rest a deterministic
/// pattern in [0.2, 0.8].
fn constant(refl: &Reflection, stage_salt: u32, reg: u32) -> [f32; 4] {
    for e in &refl.ctab {
        let (lo, n) = (u32::from(e.register), u32::from(e.count));
        if e.set == RegisterSet::Float4 && matches!(e.class, 2 | 3) && (lo..lo + n).contains(&reg) {
            let mut row = [0.0; 4];
            row[((reg - lo) as usize).min(3)] = 1.0;
            return row;
        }
    }
    let mut v = [0.0; 4];
    for (k, c) in v.iter_mut().enumerate() {
        let h = (reg * 7 + k as u32 * 13 + stage_salt * 5 + 3) % 61;
        *c = 0.2 + 0.6 * h as f32 / 60.0;
    }
    v
}

pub struct Frame {
    /// Color targets, each `SIZE * SIZE * 4` bytes.
    pub targets: Vec<Vec<u8>>,
}

/// Render the frame with `vs` and `ps`. `vr`/`pr` are the reflections of the original blobs (CTAB for constants).
pub fn render(
    gpu: &Gpu,
    vs: &Prog,
    ps: &Prog,
    vr: &Reflection,
    pr: &Reflection,
) -> Result<Frame, String> {
    let dev = &gpu.device;
    let scope_v = dev.push_error_scope(wgpu::ErrorFilter::Validation);
    let scope_i = dev.push_error_scope(wgpu::ErrorFilter::Internal);

    // Bind group layouts: union of both stages' resources, keyed (group, binding).
    struct Entry<'a> {
        res: &'a crate::paths::Res,
        stage: wgpu::ShaderStages,
        prog: &'a Prog,
        refl: &'a Reflection,
        salt: u32,
    }
    let mut entries: BTreeMap<(u32, u32), Entry> = BTreeMap::new();
    for (prog, refl, stage, salt) in [
        (vs, vr, wgpu::ShaderStages::VERTEX, 0),
        (ps, pr, wgpu::ShaderStages::FRAGMENT, 1),
    ] {
        for r in &prog.res {
            match entries.get_mut(&(r.group, r.binding)) {
                Some(e) => {
                    let same = matches!(
                        (&e.res.kind, &r.kind),
                        (Kind::Texture { dim: a, reg: ra }, Kind::Texture { dim: b, reg: rb }) if a == b && ra == rb
                    ) || matches!(
                        (&e.res.kind, &r.kind),
                        (Kind::Sampler, Kind::Sampler)
                    );
                    if !same {
                        return Err(format!(
                            "bind conflict at group {} binding {}",
                            r.group, r.binding
                        ));
                    }
                    e.stage |= stage;
                }
                None => {
                    entries.insert(
                        (r.group, r.binding),
                        Entry {
                            res: r,
                            stage,
                            prog,
                            refl,
                            salt,
                        },
                    );
                }
            }
        }
    }
    let ngroups = entries.keys().map(|k| k.0 + 1).max().unwrap_or(0);
    let mut layouts = vec![];
    let mut groups = vec![];
    for g in 0..ngroups {
        let es: Vec<_> = entries.iter().filter(|(k, _)| k.0 == g).collect();
        let lay: Vec<wgpu::BindGroupLayoutEntry> = es
            .iter()
            .map(|((_, b), e)| wgpu::BindGroupLayoutEntry {
                binding: *b,
                visibility: e.stage,
                ty: match &e.res.kind {
                    Kind::Float { .. } | Kind::Zero { .. } => wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    Kind::Texture { dim, .. } => wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: view_dim(*dim),
                        multisampled: false,
                    },
                    Kind::Sampler => {
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
                    }
                },
                count: None,
            })
            .collect();
        let layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &lay,
        });
        let mut buffers = vec![];
        for (_, e) in &es {
            buffers.push(match &e.res.kind {
                Kind::Float { vec4s, regmap } => {
                    let mut data = vec![0f32; *vec4s as usize * 4];
                    for reg in 0..256u32 {
                        let idx = match regmap {
                            Some(m) => match m.get(&reg) {
                                Some(i) => *i,
                                None => continue,
                            },
                            None => reg,
                        } as usize;
                        if idx < *vec4s as usize {
                            data[idx * 4..idx * 4 + 4]
                                .copy_from_slice(&constant(e.refl, e.salt, reg));
                        }
                    }
                    Some(dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: &f32_bytes(&data),
                        usage: wgpu::BufferUsages::UNIFORM,
                    }))
                }
                Kind::Zero { bytes } => {
                    Some(dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: &vec![0u8; (*bytes as usize).max(16)],
                        usage: wgpu::BufferUsages::UNIFORM,
                    }))
                }
                _ => None,
            });
            let _ = e.prog;
        }
        let bg_entries: Vec<wgpu::BindGroupEntry> = es
            .iter()
            .zip(&buffers)
            .map(|(((_, b), e), buf)| wgpu::BindGroupEntry {
                binding: *b,
                resource: match (&e.res.kind, buf) {
                    (Kind::Texture { dim, .. }, _) => wgpu::BindingResource::TextureView(
                        &gpu.textures[&match dim {
                            Dim::D2 => 0,
                            Dim::D3 => 1,
                            Dim::Cube => 2,
                        }],
                    ),
                    (Kind::Sampler, _) => wgpu::BindingResource::Sampler(&gpu.sampler),
                    (_, Some(buf)) => buf.as_entire_binding(),
                    _ => unreachable!(),
                },
            })
            .collect();
        groups.push(dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &bg_entries,
        }));
        layouts.push(layout);
    }
    let layout_refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
    let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &layout_refs,
        immediate_size: 0,
    });

    // Vertex data: Float32x4 per attribute, two triangles covering the target.
    let corners = [
        (-1.0, -1.0),
        (1.0, -1.0),
        (1.0, 1.0),
        (-1.0, -1.0),
        (1.0, 1.0),
        (-1.0, 1.0),
    ];
    let mut vdata: Vec<f32> = vec![];
    for (x, y) in corners {
        for (_, usage, index) in &vs.vin {
            vdata.extend_from_slice(&attribute(*usage, *index, x, y));
        }
    }
    gpu.queue.write_buffer(&gpu.quad, 0, &f32_bytes(&vdata));
    let attrs: Vec<wgpu::VertexAttribute> = vs
        .vin
        .iter()
        .enumerate()
        .map(|(i, (loc, _, _))| wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: i as u64 * 16,
            shader_location: *loc,
        })
        .collect();
    let vsm = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(vs.wgsl.as_str().into()),
    });
    let psm = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ps"),
        source: wgpu::ShaderSource::Wgsl(ps.wgsl.as_str().into()),
    });
    let ntargets = ps.targets.max(1) as usize;
    let fmt = wgpu::TextureFormat::Rgba8Unorm;
    let target_states: Vec<Option<wgpu::ColorTargetState>> =
        (0..ntargets).map(|_| Some(fmt.into())).collect();
    let stride = vs.vin.len() as u64 * 16;
    let pipeline = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &vsm,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: stride,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attrs,
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &psm,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            targets: &target_states,
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let ext = wgpu::Extent3d {
        width: SIZE,
        height: SIZE,
        depth_or_array_layers: 1,
    };
    let texes: Vec<wgpu::Texture> = (0..ntargets)
        .map(|_| {
            dev.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: ext,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        })
        .collect();
    let views: Vec<wgpu::TextureView> = texes
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let clear = wgpu::Color {
        r: f64::from(CLEAR[0]) / 255.0,
        g: f64::from(CLEAR[1]) / 255.0,
        b: f64::from(CLEAR[2]) / 255.0,
        a: f64::from(CLEAR[3]) / 255.0,
    };
    let mut enc = dev.create_command_encoder(&Default::default());
    {
        let atts: Vec<Option<wgpu::RenderPassColorAttachment>> = views
            .iter()
            .map(|v| {
                Some(wgpu::RenderPassColorAttachment {
                    view: v,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &atts,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(&pipeline);
        for (i, g) in groups.iter().enumerate() {
            rp.set_bind_group(i as u32, g, &[]);
        }
        rp.set_vertex_buffer(0, gpu.quad.slice(..stride * 6));
        rp.draw(0..6, 0..1);
    }
    let bpr = SIZE * 4;
    let padded = bpr.div_ceil(256) * 256;
    let bufs: Vec<wgpu::Buffer> = texes
        .iter()
        .map(|t| {
            let b = dev.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: u64::from(padded * SIZE),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: t,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &b,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(SIZE),
                    },
                },
                ext,
            );
            b
        })
        .collect();
    gpu.queue.submit([enc.finish()]);
    for b in &bufs {
        b.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    }
    dev.poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    if let Some(e) = pollster::block_on(scope_i.pop()) {
        return Err(format!("internal: {e}"));
    }
    if let Some(e) = pollster::block_on(scope_v.pop()) {
        return Err(format!(
            "validation: {}",
            e.to_string()
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    let targets = bufs
        .iter()
        .map(|b| {
            let m = b.slice(..).get_mapped_range().unwrap();
            let mut out = Vec::with_capacity((bpr * SIZE) as usize);
            for y in 0..SIZE {
                out.extend_from_slice(&m[(y * padded) as usize..(y * padded + bpr) as usize]);
            }
            out
        })
        .collect();
    Ok(Frame { targets })
}

fn f32_bytes(f: &[f32]) -> Vec<u8> {
    f.iter().flat_map(|v| v.to_ne_bytes()).collect()
}
