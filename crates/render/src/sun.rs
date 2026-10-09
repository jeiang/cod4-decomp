// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (gfx_d3d/rb_sky.cpp: RB_DrawSun, RB_DrawSunQuerySprite, RB_TessSunBillboard,
// RB_UpdateSunVisibilityWithoutQuery, RB_GetSunSampleRectRelativeArea, RB_DrawSunSprite, RB_DrawSunPostEffects,
// RB_DrawSunFlare, R_UpdateOverTime, RB_DrawBlindAndGlare, RB_CalcSunBlind; GPL-3.0, copyright the KisakCOD
// contributors and Activision).
//! The sun of a map: its sprite at infinity, the lens flare and the blind/glare overlay.
//!
//! How visible the sun is (`lastVisibility`, 0 to 1) comes from an occlusion query on a 16x16-pixel probe at the sun's
//! position, read back a few frames late without the CPU ever waiting; where the device cannot count samples (the GL
//! backend) it comes from the original's fallback, the screen-rect area and a sight trace into the map's collision.
//! The flare and the overlay fade towards what is visible over time ([`update_over_time`]); the renderer draws them at
//! the end of the post chain, the sprite with the world.

use crate::dynmesh::{DynMesh, DynVertex};
use crate::gpu::Gpu;
use assets::zone::gfx::Material;
use assets::zone::gfxworld::SunFlare;
use glam::{Mat4, Vec3, Vec4};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How far from the eye the sprite quad and the probe sit. The original puts them at infinity (`w = 0`); any distance
/// past the map's extent gives the same picture, and keeps the sprite an ordinary triangle list.
const SPRITE_DISTANCE: f32 = 65536.0;
/// Angular size of a sprite of `spriteSize` 1.
const SPRITE_SCALE: f32 = 0.001_311_093;
/// Side of the probe in pixels.
const PROBE_PIXELS: u32 = 16;
/// `CM_BoxSightTrace` length and contents mask of `RB_UpdateSunVisibilityWithoutQuery`.
const TRACE_LENGTH: f32 = 262_144.0;
const TRACE_MASK: i32 = 8195;
/// The probe is a grid of this many queries on a side: some devices (Metal) only say whether any sample passed, so the
/// share of blocks that did is the visibility there.
const GRID: usize = 4;
const QUERIES: usize = GRID * GRID;
/// Frames of readback in flight before a frame gives its query up.
const SLOTS: usize = 3;

/// The parts of [`SunFlare`] the effects read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fx {
    pub has_flare: bool,
    pub sprite_size: f32,
    pub flare_min_size: f32,
    pub flare_min_dot: f32,
    pub flare_max_size: f32,
    pub flare_max_dot: f32,
    pub flare_max_alpha: f32,
    pub flare_fade: (i32, i32),
    pub blind_min_dot: f32,
    pub blind_max_dot: f32,
    pub blind_max_darken: f32,
    pub blind_fade: (i32, i32),
    pub glare_min_dot: f32,
    pub glare_max_dot: f32,
    pub glare_max_lighten: f32,
    pub glare_fade: (i32, i32),
    /// Unit vector towards the sun.
    pub direction: Vec3,
}

impl Fx {
    pub fn new(s: &SunFlare) -> Fx {
        Fx {
            has_flare: s.flare_material.is_some(),
            sprite_size: s.sprite_size,
            flare_min_size: s.flare_min_size,
            flare_min_dot: s.flare_min_dot,
            flare_max_size: s.flare_max_size,
            flare_max_dot: s.flare_max_dot,
            flare_max_alpha: s.flare_max_alpha,
            flare_fade: (s.flare_fade_in_time, s.flare_fade_out_time),
            blind_min_dot: s.blind_min_dot,
            blind_max_dot: s.blind_max_dot,
            blind_max_darken: s.blind_max_darken,
            blind_fade: (s.blind_fade_in_time, s.blind_fade_out_time),
            glare_min_dot: s.glare_min_dot,
            glare_max_dot: s.glare_max_dot,
            glare_max_lighten: s.glare_max_lighten,
            glare_fade: (s.glare_fade_in_time, s.glare_fade_out_time),
            direction: Vec3::from(s.sun_fx_position),
        }
    }
}

/// `R_UpdateOverTime`: move `current` towards `goal`, taking `fade_in_ms` to rise by one and `fade_out_ms` to fall by
/// one; a fade time of zero jumps.
pub fn update_over_time(
    current: f32,
    goal: f32,
    fade_in_ms: i32,
    fade_out_ms: i32,
    frame_ms: i32,
) -> f32 {
    if goal > current {
        if fade_in_ms <= 0 {
            return goal;
        }
        let next = current + frame_ms as f32 / fade_in_ms as f32;
        next.min(goal)
    } else if goal < current {
        if fade_out_ms <= 0 {
            return goal;
        }
        let next = current - frame_ms as f32 / fade_out_ms as f32;
        next.max(goal)
    } else {
        current
    }
}

/// Where `dot` lies between `min` and `max`, clamped to `[0, 1]`: zero up to `min`, one from `max`.
pub fn range_lerp(dot: f32, min: f32, max: f32) -> f32 {
    if min >= dot {
        0.0
    } else if max > dot {
        (dot - min) / (max - min)
    } else {
        1.0
    }
}

/// Milliseconds since the last frame. The first frame, and a clock that went backwards, count 10.
pub fn frame_ms(last: i32, now: i32) -> i32 {
    if last != 0 && last <= now {
        now - last
    } else {
        10
    }
}

/// `RB_GetSunSampleRectRelativeArea`: the share of a `rect_w` x `rect_h` pixel rectangle centred on the sun's screen
/// position that lies on the screen. `clip` is the sun direction (`w = 0`) in clip space; a sun behind the eye gives 0.
pub fn sample_rect_area(clip: Vec4, size: (u32, u32), rect_w: u32, rect_h: u32) -> f32 {
    if clip.w <= 0.0 {
        return 0.0;
    }
    let (w, h) = (size.0 as f32, size.1 as f32);
    let snap = |v: f32| v.round_ties_even() as i32;
    let left = snap((w * (clip.x / clip.w + 1.0) - rect_w as f32) * 0.5);
    let top = snap((h * (clip.y / clip.w + 1.0) - rect_h as f32) * 0.5);
    let right = (left + rect_w as i32).min(size.0 as i32);
    let bottom = (top + rect_h as i32).min(size.1 as i32);
    let (left, top) = (left.max(0), top.max(0));
    if right <= left || bottom <= top {
        return 0.0;
    }
    ((bottom - top) * (right - left)) as f32 / (rect_h * rect_w) as f32
}

/// `RB_UpdateSunVisibilityWithoutQuery`: the rectangle's area, none of it when `blocked` finds something solid
/// between the eye and the sun. The trace only runs when some of the rectangle is on screen.
pub fn fallback_visibility(area: f32, blocked: impl FnOnce() -> bool) -> f32 {
    if area == 0.0 || blocked() { 0.0 } else { area }
}

/// What `begin_frame` leaves for the post chain.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overlay {
    /// The flare: centre in clip space, half extent in clip space, and the vertex colour byte.
    pub flare: Option<Flare>,
    /// The full-screen glare and blind quad's colour (glare, glare, glare, blind) in bytes; `None` when both are zero.
    pub glare_blind: Option<[u8; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flare {
    pub center: [f32; 2],
    pub half: [f32; 2],
    pub alpha: u8,
}

/// The time-dependent state of a sun (`SunFlareDynamic`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fade {
    pub last_visibility: f32,
    pub last_dot: f32,
    pub flare_intensity: f32,
    pub current_blind: f32,
    pub current_glare: f32,
    /// Milliseconds of the last frame; zero before the first.
    pub last_time: i32,
}

impl Fade {
    /// `RB_DrawSunFlare` and `RB_CalcSunBlind` for one frame: the flare's alpha and size (640x480 pixels) when it
    /// draws, and the blind and glare amounts.
    pub fn advance(&mut self, fx: &Fx, ms: i32) -> (Option<(f32, f32)>, f32, f32) {
        let dot = self.last_dot;
        let flare = (fx.has_flare && fx.flare_min_dot < dot).then(|| {
            let lerp = range_lerp(dot, fx.flare_min_dot, fx.flare_max_dot);
            let alpha = lerp * fx.flare_max_alpha;
            let size = lerp * fx.flare_max_size + fx.flare_min_size;
            self.flare_intensity = update_over_time(
                self.flare_intensity,
                self.last_visibility,
                fx.flare_fade.0,
                fx.flare_fade.1,
                ms,
            );
            (self.flare_intensity * alpha, size)
        });
        let blind = if fx.blind_max_darken > 0.0 {
            let goal = range_lerp(dot, fx.blind_min_dot, fx.blind_max_dot) * self.last_visibility;
            self.current_blind = update_over_time(
                self.current_blind,
                goal,
                fx.blind_fade.0,
                fx.blind_fade.1,
                ms,
            );
            self.current_blind * fx.blind_max_darken
        } else {
            0.0
        };
        let glare = if fx.glare_max_lighten > 0.0 {
            let goal = range_lerp(dot, fx.glare_min_dot, fx.glare_max_dot) * self.last_visibility;
            self.current_glare = update_over_time(
                self.current_glare,
                goal,
                fx.glare_fade.0,
                fx.glare_fade.1,
                ms,
            );
            self.current_glare * fx.glare_max_lighten
        } else {
            0.0
        };
        (flare, blind, glare)
    }
}

/// `R_ConvertColorToBytes` of one channel.
fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
}

/// `RB_DrawSunSprite`: the corners of the sprite quad around `dir`, scaled to `distance`, in the original's vertex
/// order (texture coordinates `(0,0) (1,0) (1,1) (0,1)`).
pub fn sprite_corners(dir: Vec3, sprite_size: f32, distance: f32) -> [Vec3; 4] {
    let scale = sprite_size * SPRITE_SCALE;
    let perp = if dir.z * dir.z <= 0.99 {
        Vec3::new(dir.y, -dir.x, 0.0)
    } else {
        Vec3::X
    };
    let right = dir.cross(perp).normalize() * scale;
    let up = right.cross(dir);
    let (right_up, right_down) = (right + up, right - up);
    let at = |off: Vec3| (dir + off) * distance;
    [at(right_up), at(right_down), at(-right_up), at(-right_down)]
}

/// The sprite as a triangle list of `material`, the original's quad: vertices `0 1 2 3` with texture coordinates
/// `(0,0) (1,0) (1,1) (0,1)`, triangles `3 0 2` and `2 0 1`.
pub fn sprite_mesh(material: Arc<Material>, corners: [Vec3; 4]) -> DynMesh {
    let uv = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let v = |i: usize| DynVertex {
        pos: corners[i].to_array(),
        color: [255; 4],
        uv: uv[i],
        normal: [0.0, 0.0, 1.0],
        tangent: [1.0, 0.0, 0.0],
    };
    let mut mesh = DynMesh::new(material);
    mesh.verts.extend([v(3), v(0), v(2), v(2), v(0), v(1)]);
    mesh
}

/// What the camera sees of the sun: its position in clip space and the forward axis.
pub struct Cam {
    pub origin: Vec3,
    pub forward: Vec3,
    /// Projection times view (the view is a pure rotation): clip space of a direction.
    pub clip: Mat4,
    pub size: (u32, u32),
    /// The projection's near plane.
    pub near: f32,
    /// The frame's time in milliseconds.
    pub time_ms: i32,
}

/// The scene pass's targets, which the probe's pipeline must match.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProbeTarget {
    pub color: wgpu::TextureFormat,
    pub depth: wgpu::TextureFormat,
    pub samples: u32,
}

struct Slot {
    staging: wgpu::Buffer,
    ready: Arc<AtomicBool>,
    in_flight: bool,
    frame: u64,
    /// Samples a fully visible probe counts in that frame's pass.
    full: u32,
}

/// The occlusion query, its readback and the probe's pipeline.
struct Probe {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    verts: wgpu::Buffer,
    slots: Vec<Slot>,
    pipeline: Option<(ProbeTarget, wgpu::RenderPipeline)>,
    /// The slot this frame's query writes to.
    current: Option<usize>,
}

const PROBE_SHADER: &str = "
@vertex fn vs(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> { return vec4<f32>(p, 1.0); }
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }
";

impl Probe {
    fn new(gpu: &Gpu) -> Probe {
        let device = &gpu.device;
        Probe {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("sun probe"),
                ty: wgpu::QueryType::Occlusion,
                count: QUERIES as u32,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sun probe resolve"),
                size: 8 * QUERIES as u64,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            verts: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sun probe quad"),
                size: (6 * QUERIES * 12) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            slots: (0..SLOTS)
                .map(|_| Slot {
                    staging: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("sun probe staging"),
                        size: 8 * QUERIES as u64,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    ready: Arc::new(AtomicBool::new(false)),
                    in_flight: false,
                    frame: 0,
                    full: 1,
                })
                .collect(),
            pipeline: None,
            current: None,
        }
    }

    /// The visibility of the newest finished query, if any finished.
    fn collect(&mut self) -> Option<f32> {
        let mut done: Vec<usize> = (0..self.slots.len())
            .filter(|&i| self.slots[i].in_flight && self.slots[i].ready.load(Ordering::Acquire))
            .collect();
        done.sort_by_key(|&i| self.slots[i].frame);
        let mut latest = None;
        for i in done {
            let s = &mut self.slots[i];
            if let Ok(view) = s.staging.slice(..).get_mapped_range() {
                let counts: Vec<u64> = view
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|b| u64::from_le_bytes(*b))
                    .collect();
                latest = Some(visibility_of(&counts, s.full));
            }
            s.staging.unmap();
            s.ready.store(false, Ordering::Release);
            s.in_flight = false;
        }
        latest
    }

    fn pipeline(&mut self, gpu: &Gpu, t: ProbeTarget) {
        if self.pipeline.as_ref().is_some_and(|(k, _)| *k == t) {
            return;
        }
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sun probe"),
            source: wgpu::ShaderSource::Wgsl(PROBE_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sun probe"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x3];
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sun probe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attrs,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: t.color,
                    blend: None,
                    write_mask: wgpu::ColorWrites::empty(),
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: t.depth,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: t.samples.max(1),
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        self.pipeline = Some((t, p));
    }
}

/// A renderer's sun: fade state, the sprite's visibility source, and what the frame left for the post chain.
pub struct Sun {
    pub fade: Fade,
    pub overlay: Overlay,
    probe: Option<Probe>,
    hit_num: i32,
    frame: u64,
}

impl Sun {
    pub fn new(gpu: &Gpu) -> Sun {
        // The GL backend (WebGL2 in the browser) only has boolean occlusion queries.
        let counted = gpu.adapter.get_info().backend != wgpu::Backend::Gl;
        Sun {
            fade: Fade::default(),
            overlay: Overlay::default(),
            probe: counted.then(|| Probe::new(gpu)),
            hit_num: 0,
            frame: 0,
        }
    }

    /// Update the sun for this frame: the visibility, the fades and the overlay. Returns the world-space sprite quad to
    /// draw with the world, when the sun is in front of the camera.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_frame(
        &mut self,
        gpu: &Gpu,
        sun: &SunFlare,
        enabled: bool,
        cam: &Cam,
        sight: Option<&sim::cm::CollisionWorld>,
        target: ProbeTarget,
    ) -> Option<[Vec3; 4]> {
        self.frame += 1;
        self.overlay = Overlay::default();
        let measured = match self.probe.as_mut() {
            Some(p) => {
                p.current = None;
                let _ = gpu.device.poll(wgpu::PollType::Poll);
                p.collect()
            }
            None => None,
        };
        let ms = frame_ms(self.fade.last_time, cam.time_ms);
        self.fade.last_time = cam.time_ms;
        if !enabled || !sun.has_valid_data {
            return None;
        }
        let fx = Fx::new(sun);
        let dot = fx.direction.dot(cam.forward);
        self.fade.last_dot = dot;
        let clip = cam.clip * fx.direction.extend(0.0);
        if dot <= 0.0 {
            self.fade.last_visibility = 0.0;
        } else if let Some(p) = self.probe.as_mut() {
            if let Some(v) = measured {
                self.fade.last_visibility = v;
            }
            if let Some(i) = p.slots.iter().position(|s| !s.in_flight) {
                p.current = Some(i);
                p.slots[i].full = PROBE_PIXELS * PROBE_PIXELS * target.samples.max(1);
                p.pipeline(gpu, target);
                gpu.queue.write_buffer(
                    &p.verts,
                    0,
                    bytemuck::cast_slice(&probe_quads(clip, cam.near, cam.size)),
                );
            }
        } else {
            let area = sample_rect_area(clip, cam.size, PROBE_PIXELS, PROBE_PIXELS);
            self.fade.last_visibility = fallback_visibility(area, || {
                let Some(cm) = sight else { return false };
                let end = cam.origin + fx.direction * TRACE_LENGTH;
                self.hit_num = cm.sight_trace(
                    self.hit_num,
                    cam.origin.to_array(),
                    end.to_array(),
                    [0.0; 3],
                    [0.0; 3],
                    &sim::cm::ClipModel::World,
                    TRACE_MASK,
                );
                self.hit_num != 0
            });
        }
        let (flare, blind, glare) = self.fade.advance(&fx, ms);
        if let Some((alpha, size)) = flare.filter(|&(alpha, _)| clip.w > 0.0 && byte(alpha) > 0) {
            self.overlay.flare = Some(Flare {
                center: [clip.x / clip.w, clip.y / clip.w],
                half: [size / 640.0, size / 480.0],
                alpha: byte(alpha),
            });
        }
        let (g, b) = (byte(glare), byte(blind));
        if g > 0 || b > 0 {
            self.overlay.glare_blind = Some([g, g, g, b]);
        }
        (dot > 0.0).then(|| {
            sprite_corners(fx.direction, fx.sprite_size, SPRITE_DISTANCE).map(|c| c + cam.origin)
        })
    }

    /// The query set the scene pass must be created with, when this frame measures the sun.
    pub fn query_set(&self) -> Option<&wgpu::QuerySet> {
        self.probe
            .as_ref()
            .filter(|p| p.current.is_some())
            .map(|p| &p.set)
    }

    /// Draw the probe inside the scene pass (after the world, before the view model).
    pub fn draw_probe(&self, rp: &mut wgpu::RenderPass<'_>, size: (u32, u32)) {
        let Some(p) = self.probe.as_ref().filter(|p| p.current.is_some()) else {
            return;
        };
        let Some((_, pipeline)) = &p.pipeline else {
            return;
        };
        rp.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
        rp.set_pipeline(pipeline);
        rp.set_vertex_buffer(0, p.verts.slice(..));
        for k in 0..QUERIES as u32 {
            rp.begin_occlusion_query(k);
            rp.draw(6 * k..6 * k + 6, 0..1);
            rp.end_occlusion_query();
        }
    }

    /// Resolve this frame's query into its staging slot; call before finishing the encoder.
    pub fn resolve(&self, enc: &mut wgpu::CommandEncoder) {
        let Some((p, i)) = self.probe.as_ref().and_then(|p| p.current.map(|i| (p, i))) else {
            return;
        };
        enc.resolve_query_set(&p.set, 0..QUERIES as u32, &p.resolve, 0);
        enc.copy_buffer_to_buffer(&p.resolve, 0, &p.slots[i].staging, 0, 8 * QUERIES as u64);
    }

    /// Start mapping this frame's result; call after the queue submit.
    pub fn submitted(&mut self) {
        let frame = self.frame;
        let Some((p, i)) = self
            .probe
            .as_mut()
            .and_then(|p| p.current.take().map(|i| (p, i)))
        else {
            return;
        };
        let slot = &mut p.slots[i];
        slot.frame = frame;
        slot.in_flight = true;
        let ready = slot.ready.clone();
        slot.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                if r.is_ok() {
                    ready.store(true, Ordering::Release);
                }
            });
    }
}

/// The sun's visibility from the samples each probe block counted, `full` of them when everything is visible. A device
/// that counts gives the share of samples; one that only says whether any passed (no count above one) the share of
/// blocks that saw anything.
pub fn visibility_of(counts: &[u64], full: u32) -> f32 {
    if counts.is_empty() {
        return 0.0;
    }
    if counts.iter().any(|&c| c > 1) {
        (counts.iter().sum::<u64>() as f64 / f64::from(full.max(1))).min(1.0) as f32
    } else {
        counts.iter().filter(|&&c| c > 0).count() as f32 / counts.len() as f32
    }
}

/// The probe's triangles in clip space: 16 pixels square around the sun cut into a grid of blocks (two triangles
/// each), at the depth the projection (near plane `near`) gives the sprite.
fn probe_quads(clip: Vec4, near: f32, size: (u32, u32)) -> [[f32; 3]; 6 * QUERIES] {
    let (cx, cy) = (clip.x / clip.w, clip.y / clip.w);
    let (hw, hh) = (
        PROBE_PIXELS as f32 / size.0 as f32,
        PROBE_PIXELS as f32 / size.1 as f32,
    );
    // `clip.w` is the direction's depth along the view axis, `clip.z / clip.w` the projection's depth scale.
    let depth = clip.z / clip.w * (1.0 - near / (SPRITE_DISTANCE * clip.w));
    let mut out = [[0.0; 3]; 6 * QUERIES];
    let step = 2.0 / GRID as f32;
    for k in 0..QUERIES {
        let (i, j) = ((k % GRID) as f32, (k / GRID) as f32);
        let (x0, x1) = (
            cx + hw * (-1.0 + step * i),
            cx + hw * (-1.0 + step * (i + 1.0)),
        );
        let (y0, y1) = (
            cy + hh * (-1.0 + step * j),
            cy + hh * (-1.0 + step * (j + 1.0)),
        );
        let v = |x: f32, y: f32| [x, y, depth];
        out[6 * k..6 * k + 6].copy_from_slice(&[
            v(x0, y1),
            v(x1, y1),
            v(x0, y0),
            v(x0, y0),
            v(x1, y1),
            v(x1, y0),
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx() -> Fx {
        Fx {
            has_flare: true,
            sprite_size: 100.0,
            flare_min_size: 20.0,
            flare_min_dot: 0.5,
            flare_max_size: 60.0,
            flare_max_dot: 0.9,
            flare_max_alpha: 0.8,
            flare_fade: (200, 400),
            blind_min_dot: 0.7,
            blind_max_dot: 0.95,
            blind_max_darken: 0.6,
            blind_fade: (1000, 2000),
            glare_min_dot: 0.6,
            glare_max_dot: 0.9,
            glare_max_lighten: 0.5,
            glare_fade: (500, 500),
            direction: Vec3::X,
        }
    }

    #[test]
    fn fading_in_and_out_runs_at_the_configured_speed() {
        // One second to rise, two to fall.
        assert!((update_over_time(0.0, 1.0, 1000, 2000, 100) - 0.1).abs() < 1e-6);
        assert!((update_over_time(1.0, 0.0, 1000, 2000, 100) - 0.95).abs() < 1e-6);
    }

    #[test]
    fn fading_stops_at_the_goal_and_jumps_without_a_time() {
        assert_eq!(update_over_time(0.95, 1.0, 1000, 1000, 100), 1.0);
        assert_eq!(update_over_time(0.05, 0.0, 1000, 1000, 100), 0.0);
        assert_eq!(update_over_time(0.3, 0.3, 1000, 1000, 100), 0.3);
        assert_eq!(update_over_time(0.0, 0.7, 0, 1000, 5), 0.7);
        assert_eq!(update_over_time(0.9, 0.2, 1000, 0, 5), 0.2);
    }

    #[test]
    fn the_range_lerp_clamps_at_both_ends() {
        assert_eq!(range_lerp(0.4, 0.5, 0.9), 0.0);
        assert_eq!(range_lerp(0.5, 0.5, 0.9), 0.0);
        assert!((range_lerp(0.7, 0.5, 0.9) - 0.5).abs() < 1e-6);
        assert_eq!(range_lerp(0.9, 0.5, 0.9), 1.0);
        assert_eq!(range_lerp(1.0, 0.5, 0.9), 1.0);
    }

    #[test]
    fn the_first_frame_and_a_backwards_clock_take_ten_milliseconds() {
        assert_eq!(frame_ms(0, 5000), 10);
        assert_eq!(frame_ms(5000, 4000), 10);
        assert_eq!(frame_ms(5000, 5016), 16);
        assert_eq!(frame_ms(5000, 5000), 0);
    }

    #[test]
    fn the_sample_rect_is_clipped_by_the_screen() {
        let size = (640, 480);
        let at = |x: f32, y: f32| sample_rect_area(Vec4::new(x, y, 0.0, 1.0), size, 16, 16);
        assert_eq!(at(0.0, 0.0), 1.0);
        // Centred on the left edge: half of it is on the screen.
        assert!((at(-1.0, 0.0) - 0.5).abs() < 1e-6);
        // Centred on a corner: a quarter.
        assert!((at(1.0, 1.0) - 0.25).abs() < 1e-6);
        assert_eq!(at(-1.2, 0.0), 0.0);
        assert_eq!(
            sample_rect_area(Vec4::new(0.0, 0.0, 0.0, -1.0), size, 16, 16),
            0.0
        );
        assert_eq!(
            sample_rect_area(Vec4::new(0.0, 0.0, 0.0, 0.0), size, 16, 16),
            0.0
        );
    }

    #[test]
    fn the_sight_trace_only_runs_for_a_sun_on_screen_and_blocks_it() {
        assert_eq!(
            fallback_visibility(0.0, || panic!("traced a sun off the screen")),
            0.0
        );
        assert_eq!(fallback_visibility(0.5, || true), 0.0);
        assert_eq!(fallback_visibility(0.5, || false), 0.5);
    }

    #[test]
    fn nothing_blinds_when_the_maxima_are_zero() {
        let mut f = fx();
        f.blind_max_darken = 0.0;
        f.glare_max_lighten = 0.0;
        let mut s = Fade {
            last_dot: 1.0,
            last_visibility: 1.0,
            ..Default::default()
        };
        for _ in 0..500 {
            let (_, blind, glare) = s.advance(&f, 100);
            assert_eq!((blind, glare), (0.0, 0.0));
        }
        assert_eq!((s.current_blind, s.current_glare), (0.0, 0.0));
    }

    #[test]
    fn looking_at_a_visible_sun_blinds_up_to_the_maxima_and_a_hidden_one_fades_it_out() {
        let f = fx();
        let mut s = Fade {
            last_dot: 1.0,
            last_visibility: 1.0,
            ..Default::default()
        };
        let (mut blind, mut glare) = (0.0, 0.0);
        for _ in 0..100 {
            (_, blind, glare) = s.advance(&f, 100);
        }
        assert!((blind - 0.6).abs() < 1e-6 && (glare - 0.5).abs() < 1e-6);
        // One frame of 100 ms after the sun is blocked: blind falls by 100/2000, glare by 100/500.
        s.last_visibility = 0.0;
        let (_, blind, glare) = s.advance(&f, 100);
        assert!((blind - 0.95 * 0.6).abs() < 1e-6);
        assert!((glare - 0.8 * 0.5).abs() < 1e-6);
        for _ in 0..100 {
            s.advance(&f, 100);
        }
        assert_eq!((s.current_blind, s.current_glare), (0.0, 0.0));
    }

    #[test]
    fn turning_away_stops_the_blind_below_its_dot_range() {
        let f = fx();
        let mut s = Fade {
            last_dot: 0.65,
            last_visibility: 1.0,
            ..Default::default()
        };
        let (_, blind, glare) = s.advance(&f, 100);
        // Below the blind's minimum dot, inside the glare's range: only the glare rises.
        assert_eq!(blind, 0.0);
        assert!(glare > 0.0);
    }

    #[test]
    fn the_flare_fades_with_visibility_and_scales_with_the_angle() {
        let f = fx();
        let mut s = Fade {
            last_dot: 0.7,
            last_visibility: 1.0,
            ..Default::default()
        };
        // dot 0.7 is halfway through [0.5, 0.9]: alpha 0.4 and size 0.5 * 60 + 20, intensity up by 100/200.
        let (flare, _, _) = s.advance(&f, 100);
        let (alpha, size) = flare.expect("in range");
        assert!((alpha - 0.5 * 0.4).abs() < 1e-6);
        assert!((size - 50.0).abs() < 1e-5);
        // Below the minimum dot the flare is gone and its fade holds.
        s.last_dot = 0.4;
        let held = s.flare_intensity;
        assert!(s.advance(&f, 100).0.is_none());
        assert_eq!(s.flare_intensity, held);
        // Without a flare material it is never drawn.
        s.last_dot = 0.7;
        let mut none = f;
        none.has_flare = false;
        assert!(s.advance(&none, 100).0.is_none());
    }

    #[test]
    fn the_sprite_is_a_square_around_the_direction_of_the_given_size() {
        let dir = Vec3::new(0.6, 0.0, 0.8);
        let q = sprite_corners(dir, 100.0, 1.0);
        let centre = (q[0] + q[2]) * 0.5;
        assert!(centre.distance(dir) < 1e-6);
        assert!(((q[0] - q[2]).length() - 2.0 * 2f32.sqrt() * 100.0 * SPRITE_SCALE).abs() < 1e-5);
        // The edges are perpendicular to each other and to the direction.
        let (e1, e2) = (q[1] - q[0], q[3] - q[0]);
        assert!(e1.dot(e2).abs() < 1e-6);
        assert!(e1.dot(dir).abs() < 1e-6 && e2.dot(dir).abs() < 1e-6);
        // A sun straight up uses the other tangent frame and stays a square.
        let up = sprite_corners(Vec3::Z, 100.0, 1.0);
        assert!((up[1] - up[0]).length() > 0.0);
        assert!((up[0] - up[1]).length() - (up[1] - up[2]).length() < 1e-6);
    }

    #[test]
    fn probe_counts_become_a_visibility_with_or_without_real_counts() {
        // A counting device: the share of the samples.
        assert_eq!(visibility_of(&[16; 16], 256), 1.0);
        assert_eq!(
            visibility_of(&[16, 16, 8, 0, 0, 0, 0, 0], 256),
            40.0 / 256.0
        );
        assert_eq!(visibility_of(&[1000; 16], 256), 1.0);
        // A boolean device: the share of blocks that saw the sun.
        let mut b = [0u64; 16];
        b[..4].fill(1);
        assert_eq!(visibility_of(&b, 256), 0.25);
        assert_eq!(visibility_of(&[0; 16], 256), 0.0);
        assert_eq!(visibility_of(&[], 256), 0.0);
    }

    #[test]
    fn the_probe_blocks_tile_the_sixteen_pixel_square() {
        let clip = Vec4::new(0.0, 0.0, 0.99999, 1.0);
        let q = probe_quads(clip, 4.0, (640, 480));
        let xs: Vec<f32> = q.iter().map(|v| v[0]).collect();
        let ys: Vec<f32> = q.iter().map(|v| v[1]).collect();
        let (minx, maxx) = (
            xs.iter().cloned().fold(9.0, f32::min),
            xs.iter().cloned().fold(-9.0, f32::max),
        );
        let (miny, maxy) = (
            ys.iter().cloned().fold(9.0, f32::min),
            ys.iter().cloned().fold(-9.0, f32::max),
        );
        // 16 pixels of a 640x480 screen in clip space.
        assert!(((maxx - minx) * 320.0 - 16.0).abs() < 1e-3);
        assert!(((maxy - miny) * 240.0 - 16.0).abs() < 1e-3);
    }

    #[test]
    fn colours_round_to_bytes() {
        assert_eq!(byte(-1.0), 0);
        assert_eq!(byte(0.5), 128);
        assert_eq!(byte(2.0), 255);
    }
}
