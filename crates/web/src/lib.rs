// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser entry of the renderer (spike for ticket #69): the user's picked install folder is read in place through
//! `File` objects (nothing is uploaded), one map is decoded and drawn on WebGPU, or WebGL2 when the browser has no
//! WebGPU.
//!
//! Runs in a dedicated worker: `FileReaderSync` is the only synchronous ranged read of a `File`, which is what the
//! [`ReadAt`] and `Read` interfaces of the `assets` crate need, and the canvas is an `OffscreenCanvas` transferred
//! from the page. The page's script drives [`Spike::frame`] and does the statistics.
#![cfg(target_arch = "wasm32")]

use assets::vfs::{Builder, ReadAt, SourceReader, Vfs};
use glam::Vec3;
use render::flythrough::{self, hor_plus};
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use web_time::Instant;

/// A browser `File`, read synchronously with `FileReaderSync`.
struct WebFile {
    file: web_sys::File,
    reader: web_sys::FileReaderSync,
    len: u64,
}

// SAFETY: this module only runs on wasm32-unknown-unknown without atomics, where the instance has exactly one thread,
// so the JS handles never cross threads. The bounds are what `ReadAt` demands of every source.
unsafe impl Send for WebFile {}
unsafe impl Sync for WebFile {}

impl WebFile {
    fn new(file: web_sys::File) -> Result<Self, JsValue> {
        Ok(Self {
            len: file.size() as u64,
            reader: web_sys::FileReaderSync::new()?,
            file,
        })
    }
}

impl ReadAt for WebFile {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.len)
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        let js = |e: JsValue| io::Error::other(format!("{e:?}"));
        let blob = self
            .file
            .slice_with_f64_and_f64(offset as f64, end as f64)
            .map_err(js)?;
        let bytes = self.reader.read_as_array_buffer(&blob).map_err(js)?;
        js_sys::Uint8Array::new(&bytes).copy_to(buf);
        Ok(())
    }
}

/// Load phases with wall time, reported to the page as they finish.
struct Phases<'a> {
    last: Instant,
    log: Vec<(String, f64)>,
    progress: &'a js_sys::Function,
}

impl Phases<'_> {
    fn mark(&mut self, name: &str) {
        let now = Instant::now();
        let ms = (now - self.last).as_secs_f64() * 1000.0;
        self.last = now;
        let _ = self.progress.call2(&JsValue::NULL, &name.into(), &ms.into());
        self.log.push((name.to_owned(), ms));
    }
}

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// wasm linear memory in bytes (it only grows, so this is the peak).
fn wasm_memory_bytes() -> f64 {
    (core::arch::wasm32::memory_size::<0>() * 65536) as f64
}

#[wasm_bindgen]
pub struct Spike {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Arc<Gpu>,
    renderer: Renderer,
    fov_x: f32,
    phases: Vec<(String, f64)>,
    backend: String,
    adapter: String,
    bc: bool,
    timestamps: bool,
    pipelines: usize,
    missing_textures: usize,
    last: FrameInfo,
}

#[derive(Default, Clone, Copy)]
struct FrameInfo {
    surfaces: usize,
    draws: usize,
    models: usize,
    pipelines_missing: usize,
    gpu_ms: f64,
}

#[wasm_bindgen]
impl Spike {
    /// `paths[i]` (relative to the install root, `/`-separated) names `files[i]`. `backend` is `"auto"`, `"webgpu"` or
    /// `"webgl"`. `progress(name, ms)` is called as each load phase finishes.
    pub async fn load(
        canvas: web_sys::OffscreenCanvas,
        paths: js_sys::Array,
        files: js_sys::Array,
        map: String,
        backend: String,
        progress: js_sys::Function,
    ) -> Result<Spike, JsError> {
        console_error_panic_hook::set_once();
        let mut ph = Phases {
            last: Instant::now(),
            log: Vec::new(),
            progress: &progress,
        };

        let mut index: HashMap<String, Arc<WebFile>> = HashMap::new();
        for (p, f) in paths.iter().zip(files.iter()) {
            let path = p.as_string().ok_or_else(|| err("path is not a string"))?;
            let file = WebFile::new(f.unchecked_into()).map_err(|e| err(format!("{e:?}")))?;
            index.insert(path.to_ascii_lowercase(), Arc::new(file));
        }
        ph.mark(&format!("index {} files", index.len()));

        let vfs = open_vfs(&index)?;
        ph.mark("open IWDs (central directories)");

        let data = MapData::load_with(
            &map,
            |zone| {
                let name = format!("zone/english/{zone}.ff").to_ascii_lowercase();
                let file = index.get(&name).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, format!("{name} is not in the picked folder"))
                })?;
                Ok(SourceReader::new(file.clone()))
            },
            |zone| ph.mark(&format!("zone {zone}: read, inflate, decode")),
        )
        .map_err(err)?;
        // Not drawn from: the strings zone. Streamed and decoded to measure its cost on the same path.
        if let Some(file) = index.get("zone/english/localized_common_mp.ff") {
            let zone = assets::zone::Zone::open(SourceReader::new(file.clone())).map_err(err)?;
            let mut n = 0;
            zone.decode(&assets::zone::Consumer::Client, |_| n += 1)
                .map_err(err)?;
            ph.mark(&format!("zone localized_common_mp: read, inflate, decode ({n} assets)"));
        }

        let instance = new_instance(&backend).await;
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas.clone()))
            .map_err(err)?;
        let gpu = Arc::new(
            Gpu::with_instance_async(instance, Some(&surface))
                .await
                .map_err(err)?,
        );
        // Browsers report validation errors to nobody unless asked; the page shows what reaches the console.
        gpu.device.on_uncaptured_error(Arc::new(|e| {
            web_sys::console::error_1(&format!("wgpu: {e}").into());
        }));
        ph.mark("GPU adapter and device");

        let caps = surface.get_capabilities(&gpu.adapter);
        // The shaders write display-referred values: an 8-bit linear (non-sRGB) target.
        let format = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]
        .into_iter()
        .find(|f| caps.formats.contains(f))
        .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: canvas.width().max(1),
            height: canvas.height().max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&gpu.device, &config);

        let scene = Scene::new(&gpu, &data);
        ph.mark("scene: upload world and models");
        let mut renderer = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
        ph.mark("renderer: materials, shaders, textures");
        let pipelines = renderer.warm(config.format);
        ph.mark(&format!("renderer: {pipelines} pipelines"));

        let info = gpu.describe();
        let aspect = config.width as f32 / config.height as f32;
        let missing_textures = renderer.textures.failed.len();
        Ok(Spike {
            surface,
            config,
            gpu,
            renderer,
            fov_x: hor_plus(80.0, aspect),
            phases: ph.log,
            backend: info.backend,
            adapter: info.name,
            bc: info.features.iter().any(|f| f.contains("TEXTURE_COMPRESSION_BC")),
            timestamps: info.features.iter().any(|f| f.contains("TIMESTAMP_QUERY")),
            pipelines,
            missing_textures,
            last: FrameInfo::default(),
        })
    }

    /// Draw the flythrough at `t` seconds; returns the CPU milliseconds the call took.
    pub fn frame(&mut self, t: f64) -> Result<f64, JsError> {
        let start = Instant::now();
        let w = &self.renderer.scene.world;
        let p = flythrough::pose(t as f32, Vec3::from(w.mins), Vec3::from(w.maxs));
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(0.0);
            }
        };
        let view = View {
            origin: p.origin,
            yaw: p.yaw,
            pitch: p.pitch,
            fov_x: self.fov_x,
            time: t as f32,
        };
        let target = frame.texture.create_view(&Default::default());
        let stats = self.renderer.render(
            &view,
            &target,
            self.config.format,
            (self.config.width, self.config.height),
        );
        self.gpu.queue.present(frame);
        let gpu_ms = self
            .renderer
            .take_gpu_times()
            .last()
            .map_or(self.last.gpu_ms, |&(_, ms)| ms);
        self.last = FrameInfo {
            surfaces: stats.surfaces,
            draws: stats.draws,
            models: stats.models,
            pipelines_missing: stats.pipelines_missing,
            gpu_ms,
        };
        Ok(start.elapsed().as_secs_f64() * 1000.0)
    }

    /// Render the flythrough at `t` seconds into a texture and read it back as tightly packed RGBA8, `width * height * 4`
    /// bytes: proof of what was drawn for the page (a canvas handed to a worker cannot be read from the page).
    pub async fn capture(&mut self, t: f64) -> Result<Vec<u8>, JsError> {
        let (w, h) = (self.config.width, self.config.height);
        let dev = &self.gpu.device;
        let tex = dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let world = &self.renderer.scene.world;
        let p = flythrough::pose(t as f32, Vec3::from(world.mins), Vec3::from(world.maxs));
        let view = View { origin: p.origin, yaw: p.yaw, pitch: p.pitch, fov_x: self.fov_x, time: t as f32 };
        self.renderer.render(&view, &tex.create_view(&Default::default()), self.config.format, (w, h));
        let row = w * 4;
        assert_eq!(row % 256, 0, "the capture width must keep rows 256-byte aligned");
        let buf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = dev.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let mapped = js_sys::Promise::new(&mut |resolve, reject| {
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = match r {
                    Ok(()) => resolve.call0(&JsValue::NULL),
                    Err(e) => reject.call1(&JsValue::NULL, &e.to_string().into()),
                };
            });
        });
        wasm_bindgen_futures::JsFuture::from(mapped)
            .await
            .map_err(|e| err(format!("{e:?}")))?;
        let mut px = slice.get_mapped_range().map_err(err)?.to_vec();
        if self.config.format == wgpu::TextureFormat::Bgra8Unorm {
            px.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
        }
        Ok(px)
    }

    /// Facts about the run as JSON: backend, adapter, load phases, wasm memory, and the last frame's counters.
    pub fn report(&self) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "{{\"backend\":{:?},\"adapter\":{:?},\"bc_textures\":{},\"gpu_timestamps\":{},\"pipelines\":{},\
             \"textures_failed\":{},\"wasm_memory_bytes\":{},\"surfaces\":{},\"draws\":{},\"models\":{},\
             \"pipelines_missing\":{},\"gpu_ms\":{},\"phases\":[",
            self.backend,
            self.adapter,
            self.bc,
            self.timestamps,
            self.pipelines,
            self.missing_textures,
            wasm_memory_bytes(),
            self.last.surfaces,
            self.last.draws,
            self.last.models,
            self.last.pipelines_missing,
            self.last.gpu_ms,
        );
        for (i, (name, ms)) in self.phases.iter().enumerate() {
            let _ = write!(s, "{}[{name:?},{ms:.1}]", if i > 0 { "," } else { "" });
        }
        s.push_str("]}");
        s
    }
}

/// The stock search path from the picked folder: the IWDs of `main` (the original loads `players` and `main_shared`
/// first, but an install has IWDs only under `main`).
fn open_vfs(index: &HashMap<String, Arc<WebFile>>) -> Result<Vfs, JsError> {
    let iwds = index
        .iter()
        .filter_map(|(path, file)| {
            let name = path.strip_prefix("main/")?;
            (!name.contains('/') && name.ends_with(".iwd"))
                .then(|| (name.to_owned(), Box::new(SharedFile(file.clone())) as Box<dyn ReadAt>))
        })
        .collect();
    let mut b = Builder::new(Path::new(""));
    b.add_iwds("main", None, iwds).map_err(err)?;
    b.finish(0).map_err(err)
}

/// `Box<dyn ReadAt>` over a file the zone readers also hold.
struct SharedFile(Arc<WebFile>);

impl ReadAt for SharedFile {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read_exact_at(offset, buf)
    }
}

async fn new_instance(backend: &str) -> wgpu::Instance {
    let backends = match backend {
        "webgpu" => wgpu::Backends::BROWSER_WEBGPU,
        "webgl" => wgpu::Backends::GL,
        _ => wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
    };
    let desc = wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    };
    wgpu::util::new_instance_with_webgpu_detection(desc).await
}
