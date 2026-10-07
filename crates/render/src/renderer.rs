// SPDX-License-Identifier: GPL-3.0-or-later
//! The frame: cull, build the draw list, fill constant banks, record one pass.

use crate::codeconst::{self, FrameConsts, Object, tex as ctex};
use crate::cull::{self, Frustum};
use crate::gpu::Gpu;
use crate::material::{BANK_BYTES, Bank, Materials, Prepared, VertexKind};
use crate::scene::{Mesh, Scene, model_matrix};
use crate::texture::{Tex, TextureCache};
use assets::zone::gfx::Material;
use glam::{Mat4, Vec3, Vec4};
use sm3::SamplerDim;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const NEAR: f32 = 4.0;
/// The projection has no far plane worth the name: the sky shader drops the translation part of the matrix and so
/// lands at `z / w = r`, which must stay below one to survive clipping.
const DEPTH_SCALE: f32 = 0.99999;
/// Linear filtering, linear mips, clamped.
const CODE_SAMPLER: u8 = 0x72;
const TECH_UNLIT: usize = 4;
const TECH_EMISSIVE: usize = 5;
const TECH_LIT: usize = 7;
const TECH_LIT_SUN: usize = 8;
const LIT_SUN: [usize; 4] = [TECH_LIT_SUN, TECH_LIT, TECH_UNLIT, TECH_EMISSIVE];
const LIT: [usize; 3] = [TECH_LIT, TECH_UNLIT, TECH_EMISSIVE];

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

    /// View and projection matrices; the view maps game axes to the original's left-handed y-up camera space.
    pub fn matrices(&self, aspect: f32) -> (Mat4, Mat4) {
        let conv = Mat4::from_cols(
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(-1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::W,
        );
        let eye = conv.transform_point3(self.origin);
        let dir = conv.transform_vector3(self.forward());
        let f = dir.normalize();
        let s = Vec3::Y.cross(f).normalize();
        let u = f.cross(s);
        let look = Mat4::from_cols(
            Vec4::new(s.x, u.x, f.x, 0.0),
            Vec4::new(s.y, u.y, f.y, 0.0),
            Vec4::new(s.z, u.z, f.z, 0.0),
            Vec4::new(-s.dot(eye), -u.dot(eye), -f.dot(eye), 1.0),
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
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub cells: usize,
    pub surfaces: usize,
    pub models: usize,
    pub draws: usize,
    pub pipelines_missing: usize,
    pub cpu_ms: f64,
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
    sky: bool,
    order: (bool, u8, u32),
}

pub struct Renderer {
    pub gpu: Arc<Gpu>,
    pub scene: Scene,
    pub materials: Materials,
    pub textures: TextureCache,
    ring: Vec<Bank>,
    ring_buf: wgpu::Buffer,
    ring_cap: usize,
    vs_bg: wgpu::BindGroup,
    ps_bg: wgpu::BindGroup,
    bools: wgpu::Buffer,
    depth: Option<(wgpu::TextureView, (u32, u32))>,
    tex_bgs: HashMap<(u32, u8, u8), Arc<wgpu::BindGroup>>,
    pub clear: [f64; 3],
}

impl Renderer {
    pub fn new(
        gpu: Arc<Gpu>,
        scene: Scene,
        techsets: &[Arc<assets::zone::gfx::TechniqueSet>],
        textures: TextureCache,
    ) -> Renderer {
        let mut materials = Materials::new(&gpu);
        materials.add_techsets(techsets);
        let ring_cap = 1024;
        let ring_buf = new_ring(&gpu, ring_cap);
        let bools = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bool constants"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        let (vs_bg, ps_bg) = ring_groups(&gpu, &materials, &ring_buf, &bools);
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
        };
        r.prewarm();
        r
    }

    /// Translate and prepare every technique the map's surfaces can use, so frames do not hitch on first sight.
    fn prewarm(&mut self) {
        for m in self.scene.materials() {
            for kind in [VertexKind::World, VertexKind::Model] {
                self.prepare(&m, &LIT_SUN, kind);
                self.prepare(&m, &LIT, kind);
            }
        }
    }

    /// Build the pipelines for rendering into `format`, again to keep them off the frame path. Returns how many.
    pub fn warm(&mut self, format: wgpu::TextureFormat) -> usize {
        let mut n = 0;
        for m in self.scene.materials() {
            for kind in [VertexKind::World, VertexKind::Model] {
                for techs in [&LIT_SUN[..], &LIT[..]] {
                    if let Some(p) = self.prepare(&m, techs, kind) {
                        self.materials.pipeline(&self.gpu, &p, format, DEPTH_FORMAT);
                        n += 1;
                    }
                }
            }
        }
        n
    }

    /// The prepared pass for `techs` of `m`, for debugging tools.
    pub fn inspect(
        &mut self,
        m: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
    ) -> Option<Rc<Prepared>> {
        self.prepare(m, techs, kind)
    }

    fn prepare(
        &mut self,
        m: &Arc<Material>,
        techs: &[usize],
        kind: VertexKind,
    ) -> Option<Rc<Prepared>> {
        self.materials
            .prepare(&self.gpu, &mut self.textures, m, techs, kind)
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

    /// Code textures `p` samples, resolved for a surface with lightmap `lm` and reflection probe `probe`.
    fn tex_group(&mut self, p: &Rc<Prepared>, lm: u8, probe: u8) -> Arc<wgpu::BindGroup> {
        let key = (p.id(), lm, probe);
        if let Some(g) = self.tex_bgs.get(&key) {
            return g.clone();
        }
        let world = self.scene.world.clone();
        let mut codes: HashMap<u32, (Arc<Tex>, u8)> = HashMap::new();
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
                (ctex::MODEL_LIGHTING, _) => Some((self.scene.lighting_tex.clone(), 0xE2)),
                (ctex::WHITE, _) => Some((
                    self.textures.solid(&self.gpu, SamplerDim::D2, [255; 4]),
                    CODE_SAMPLER,
                )),
                (ctex::BLACK, _) => Some((
                    self.textures
                        .solid(&self.gpu, SamplerDim::D2, [0, 0, 0, 255]),
                    CODE_SAMPLER,
                )),
                (ctex::IDENTITY_NORMAL_MAP, _) => Some((
                    self.textures
                        .solid(&self.gpu, SamplerDim::D2, [128, 128, 255, 255]),
                    CODE_SAMPLER,
                )),
                (ctex::SKY, Some(i)) => self
                    .textures
                    .image(&self.gpu, &i)
                    .map(|t| (t, world.sky_sampler_state)),
                (_, Some(i)) => self
                    .textures
                    .image(&self.gpu, &i)
                    .map(|t| (t, CODE_SAMPLER)),
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

    fn alloc(&mut self) -> (u32, &mut Bank) {
        let at = self.ring.len();
        self.ring.push([[0.0; 4]; 256]);
        ((at as u64 * BANK_BYTES) as u32, &mut self.ring[at])
    }

    /// Draw the world seen from `view` into `target`.
    pub fn render(
        &mut self,
        view: &View,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> FrameStats {
        let t0 = std::time::Instant::now();
        self.ensure_depth(size);
        self.ring.clear();
        let (v, p) = view.matrices(size.0 as f32 / size.1 as f32);
        let mut frame = FrameConsts::new(v, p);
        frame.set_sun(self.scene.sun_dir, self.scene.sun_color, 1.0);
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

        let frustum = Frustum::from_clip(&(p * v));
        let vis = cull::visible(&self.scene.world, view.origin, &frustum);
        let mut stats = FrameStats {
            cells: vis.cells,
            surfaces: vis.surfaces.len(),
            models: 0,
            ..Default::default()
        };
        let mut draws: Vec<Draw> = Vec::new();
        let mut world_banks: HashMap<u32, (u32, u32)> = HashMap::new();

        // World surfaces.
        let world = self.scene.world.clone();
        let sun_index = world.sun_primary_light_index as u8;
        for &si in &vis.surfaces {
            let Some(surf) = world.dpvs.surfaces.get(si as usize) else {
                continue;
            };
            let Some(mat) = &surf.material else { continue };
            let techs: &[usize] = if surf.primary_light_index == sun_index {
                &LIT_SUN
            } else {
                &LIT
            };
            let Some(prep) = self.prepare(mat, techs, VertexKind::World) else {
                stats.pipelines_missing += 1;
                continue;
            };
            let (vs, ps) = *world_banks.entry(prep.id()).or_insert_with(|| {
                let obj = Object::default();
                let (vo, vb) = self.alloc();
                prep.fill_vs(vb, &frame, &obj);
                let (po, pb) = self.alloc();
                prep.fill_ps(pb, &frame, &obj);
                (vo, po)
            });
            let pipeline = self
                .materials
                .pipeline(&self.gpu, &prep, format, DEPTH_FORMAT);
            let tex_bg = self.tex_group(&prep, surf.lightmap_index, surf.reflection_probe_index);
            draws.push(Draw {
                sky: is_sky(mat),
                order: (false, mat.sort_key, si),
                prepared: prep,
                pipeline,
                tex_bg,
                mesh: self.scene.world_mesh.clone(),
                vs,
                ps,
                first_index: surf.base_index as u32,
                count: u32::from(surf.tri_count) * 3,
                base_vertex: surf.first_vertex,
            });
        }

        // Static models.
        for &mi in &vis.smodels {
            let Some(inst) = world.dpvs.smodel_draw_insts.get(mi as usize) else {
                continue;
            };
            let Some(model) = inst.model.clone() else {
                continue;
            };
            let origin = Vec3::from(inst.origin);
            let dist = origin.distance(view.origin);
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
            let techs: &[usize] = if inst.primary_light_index == sun_index {
                &LIT_SUN
            } else {
                &LIT
            };
            let mut counted = false;
            for s in 0..usize::from(info.surf_count) {
                let idx = usize::from(info.surf_index) + s;
                let Some(Some(mat)) = model.materials.get(idx) else {
                    continue;
                };
                let Some(prep) = self.prepare(mat, techs, VertexKind::Model) else {
                    continue;
                };
                let Some(mesh) = self.scene.model_mesh(&self.gpu, &model, idx) else {
                    continue;
                };
                let (vo, vb) = self.alloc();
                prep.fill_vs(vb, &frame, &obj);
                let (po, pb) = self.alloc();
                prep.fill_ps(pb, &frame, &obj);
                let pipeline = self
                    .materials
                    .pipeline(&self.gpu, &prep, format, DEPTH_FORMAT);
                let tex_bg = self.tex_group(&prep, 0, inst.reflection_probe_index);
                let tris = u32::from(model.surfs[idx].tri_count) * 3;
                draws.push(Draw {
                    sky: false,
                    order: (false, mat.sort_key, 1 << 31 | mi),
                    prepared: prep,
                    pipeline,
                    tex_bg,
                    mesh,
                    vs: vo,
                    ps: po,
                    first_index: 0,
                    count: tris,
                    base_vertex: 0,
                });
                counted = true;
            }
            stats.models += usize::from(counted);
        }
        draws.sort_by_key(|d| (!d.sky, d.order, d.prepared.id()));
        stats.draws = draws.len();

        self.upload_ring();
        let depth = &self.depth.as_ref().expect("depth").0;
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let (w, h) = (size.0 as f32, size.1 as f32);
            let mut cur_pipe: Option<*const wgpu::RenderPipeline> = None;
            let mut cur_tex: Option<*const wgpu::BindGroup> = None;
            let mut cur_mesh: Option<*const Mesh> = None;
            let mut sky_range = false;
            for d in &draws {
                if d.sky != sky_range {
                    sky_range = d.sky;
                    let depth = if sky_range { 1.0 } else { 0.0 };
                    rp.set_viewport(0.0, 0.0, w, h, depth, 1.0);
                }
                if cur_pipe != Some(Arc::as_ptr(&d.pipeline)) {
                    rp.set_pipeline(&d.pipeline);
                    cur_pipe = Some(Arc::as_ptr(&d.pipeline));
                }
                rp.set_bind_group(0, &self.vs_bg, &[d.vs]);
                rp.set_bind_group(1, &self.ps_bg, &[d.ps]);
                if cur_tex != Some(Arc::as_ptr(&d.tex_bg)) {
                    rp.set_bind_group(2, &*d.tex_bg, &[]);
                    cur_tex = Some(Arc::as_ptr(&d.tex_bg));
                }
                if cur_mesh != Some(Arc::as_ptr(&d.mesh)) {
                    rp.set_vertex_buffer(0, d.mesh.vb.slice(..));
                    rp.set_index_buffer(d.mesh.ib.slice(..), wgpu::IndexFormat::Uint16);
                    cur_mesh = Some(Arc::as_ptr(&d.mesh));
                }
                rp.draw_indexed(d.first_index..d.first_index + d.count, d.base_vertex, 0..1);
            }
        }
        self.gpu.queue.submit([enc.finish()]);
        stats.cpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
        stats
    }

    fn upload_ring(&mut self) {
        if self.ring.len() > self.ring_cap {
            self.ring_cap = self.ring.len().next_power_of_two();
            self.ring_buf = new_ring(&self.gpu, self.ring_cap);
            (self.vs_bg, self.ps_bg) =
                ring_groups(&self.gpu, &self.materials, &self.ring_buf, &self.bools);
        }
        self.gpu
            .queue
            .write_buffer(&self.ring_buf, 0, bytemuck::cast_slice(&self.ring));
    }
}

fn new_ring(gpu: &Gpu, banks: usize) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("constant banks"),
        size: banks as u64 * BANK_BYTES,
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
