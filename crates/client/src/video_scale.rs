// SPDX-License-Identifier: GPL-3.0-only
//! GPU-local encoded-channel bilinear scaling of completed presented images.

pub(super) struct GpuScale {
    source: wgpu::Texture,
    pub output: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
}

impl GpuScale {
    pub fn new(
        d: &wgpu::Device,
        src: (u32, u32),
        out: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Self {
        // Copy-compatible with sRGB sources, but interpolate already encoded channel values,
        // like the old CPU scaler, without applying another transfer function.
        let format = format.remove_srgb_suffix();
        let texture = |size: (u32, u32), usage| {
            d.create_texture(&wgpu::TextureDescriptor {
                label: Some("video scale"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let source = texture(
            src,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let output = texture(
            out,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let view = output.create_view(&Default::default());
        let source_view = source.create_view(&Default::default());
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video scale"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video scale"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            }],
        });
        let shader = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
    return vec4(p[i],0.,1.);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(image));
    let at = clamp(p.xy * vec2<f32>(size) / vec2<f32>($WIDTH.,$HEIGHT.) - .5,
                   vec2(0.),vec2<f32>(size-1));
    let a = vec2<i32>(floor(at));
    let b = min(a+1,size-1);
    let w = vec2<u32>(fract(at)*256.);
    let c00 = vec4<u32>(round(textureLoad(image,a,0)*255.));
    let c10 = vec4<u32>(round(textureLoad(image,vec2(b.x,a.y),0)*255.));
    let c01 = vec4<u32>(round(textureLoad(image,vec2(a.x,b.y),0)*255.));
    let c11 = vec4<u32>(round(textureLoad(image,b,0)*255.));
    let top = c00*(256u-w.x)+c10*w.x;
    let bot = c01*(256u-w.x)+c11*w.x;
    return vec4<f32>((top*(256u-w.y)+bot*w.y+32768u)>>vec4(16u))/255.;
}
"#
        .replace("$WIDTH", &out.0.to_string())
        .replace("$HEIGHT", &out.1.to_string());
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video scale"),
            source: wgpu::ShaderSource::Wgsl(shader.into()),
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video scale"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video scale"),
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
        Self {
            source,
            output,
            view,
            bind,
            pipeline,
        }
    }

    pub fn draw(&self, enc: &mut wgpu::CommandEncoder, source: &wgpu::Texture) {
        enc.copy_texture_to_texture(
            source.as_image_copy(),
            self.source.as_image_copy(),
            source.size(),
        );
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("video scale"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
