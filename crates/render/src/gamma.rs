// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (gfx_d3d/r_init.cpp: R_CalcGammaRamp; GPL-3.0, copyright the KisakCOD contributors
// and Activision).
//! `r_gamma`: the whole picture (3D, HUD, menus, screenshots) goes through the ramp `out = in^(1 / r_gamma)`.
//!
//! The original loads that 256-entry ramp into the display (`R_CalcGammaRamp`); a window has no ramp to load, so the
//! frame is drawn into an offscreen image and a last full-screen pass applies the same curve while copying it to the
//! surface. At `r_gamma` 1 the curve is the identity and callers draw to the surface directly.

use crate::gpu::Gpu;

/// The dvar's range (`r_gamma`: 0.5 to 3, default 0.8).
pub const MIN: f32 = 0.5;
pub const MAX: f32 = 3.0;
pub const DEFAULT: f32 = 0.8;

/// The ramp's exponent for `r_gamma`, clamped to the dvar's range.
pub fn exponent(r_gamma: f32) -> f32 {
    if r_gamma.is_nan() {
        return 1.0 / DEFAULT;
    }
    1.0 / r_gamma.clamp(MIN, MAX)
}

/// Whether frames at `r_gamma` can skip the pass: the ramp is the identity (`R_CalcGammaRamp`'s `exponent == 1`).
pub fn is_identity(r_gamma: f32) -> bool {
    exponent(r_gamma) == 1.0
}

const SHADER: &str = "
struct Params { exponent: f32 }
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(pos.xy), 0);
    return vec4<f32>(pow(c.rgb, vec3<f32>(params.exponent)), c.a);
}
";

/// The offscreen frame and the pass that copies it through the ramp.
pub struct Gamma {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    format: wgpu::TextureFormat,
    frame: Option<Frame>,
}

struct Frame {
    size: (u32, u32),
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

impl Gamma {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Gamma {
        let d = &gpu.device;
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gamma"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gamma"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gamma"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gamma"),
            layout: Some(&pl),
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
                targets: &[Some(format.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let params = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gamma"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Gamma {
            pipeline,
            layout,
            params,
            format,
            frame: None,
        }
    }

    /// The image to draw the frame into, `size` pixels (re-made when the size changes).
    pub fn target(&mut self, gpu: &Gpu, size: (u32, u32)) -> &wgpu::TextureView {
        if self.frame.as_ref().is_none_or(|f| f.size != size) {
            let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("gamma frame"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("gamma"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.params.as_entire_binding(),
                    },
                ],
            });
            self.frame = Some(Frame { size, view, bind });
        }
        &self.frame.as_ref().expect("just made").view
    }

    /// Copy the frame drawn into [`Gamma::target`] to `dest` through the ramp of `r_gamma`.
    pub fn apply(&self, gpu: &Gpu, dest: &wgpu::TextureView, r_gamma: f32) {
        let Some(f) = &self.frame else { return };
        gpu.queue
            .write_buffer(&self.params, 0, &exponent(r_gamma).to_le_bytes());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("gamma"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dest,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &f.bind, &[]);
            rp.draw(0..3, 0..1);
        }
        gpu.queue.submit([enc.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gamma_darkens_the_midtones_and_one_is_the_identity() {
        let ramp = |i: f32, g: f32| i.powf(exponent(g));
        assert!(ramp(0.5, 0.8) < 0.5 && ramp(0.5, 1.0) == 0.5 && ramp(0.5, 1.5) > 0.5);
        assert_eq!((ramp(0.0, 0.8), ramp(1.0, 0.8)), (0.0, 1.0));
        assert!(is_identity(1.0) && !is_identity(0.8));
    }

    #[test]
    fn a_non_number_takes_the_default() {
        assert_eq!(exponent(f32::NAN), 1.0 / DEFAULT);
    }

    #[test]
    fn out_of_range_gamma_is_clamped_to_the_dvar_limits() {
        assert_eq!(exponent(0.01), 1.0 / MIN);
        assert_eq!(exponent(99.0), 1.0 / MAX);
    }
}
