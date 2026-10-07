// SPDX-License-Identifier: GPL-3.0-or-later
//! The winit application: window, surface, render loop, fly camera, flythrough recording.
//!
//! The render loop runs as fast as the present mode allows and is independent of any simulation tick: the camera is a
//! function of wall-clock time.

use crate::Cli;
use crate::display::{self, hor_plus};
use crate::flythrough;
use crate::video::Recorder;
use assets::vfs::Vfs;
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

pub fn run(cli: Cli) -> Result<(), String> {
    let el = EventLoop::new().map_err(|e| format!("no display: {e}"))?;
    el.set_control_flow(ControlFlow::Poll);
    if cli.list {
        let mut l = Lister { error: None };
        el.run_app(&mut l).map_err(|e| e.to_string())?;
        return l.error.map_or(Ok(()), Err);
    }
    let t = Instant::now();
    let map = MapData::load(&cli.install, &cli.map)
        .map_err(|e| format!("cannot load {}: {e}", cli.map))?;
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let mut v = Viewer {
        cli,
        map,
        load_ms,
        st: None,
        error: None,
    };
    el.run_app(&mut v).map_err(|e| e.to_string())?;
    let out = v.cli.out.clone();
    if let (Some(e), Some(out)) = (&v.error, out) {
        let _ = std::fs::create_dir_all(&out);
        let _ = std::fs::write(
            out.join("client.json"),
            json!({"status": "error", "error": e}).to_string(),
        );
    }
    v.error.map_or(Ok(()), Err)
}

fn pick_present(
    requested: &str,
    caps: &[wgpu::PresentMode],
) -> (wgpu::PresentMode, Option<String>) {
    let want = match requested {
        "mailbox" => wgpu::PresentMode::Mailbox,
        "immediate" => wgpu::PresentMode::Immediate,
        _ => wgpu::PresentMode::Fifo,
    };
    if caps.contains(&want) {
        (want, None)
    } else {
        (
            wgpu::PresentMode::Fifo,
            Some(format!("present mode {requested} unavailable; using fifo")),
        )
    }
}

fn present_names(caps: &[wgpu::PresentMode]) -> Vec<String> {
    caps.iter()
        .map(|p| format!("{p:?}").to_lowercase())
        .collect()
}

fn gpu_json(g: &Gpu) -> Value {
    let i = g.describe();
    json!({
        "name": i.name, "backend": i.backend, "driver": i.driver, "device_type": i.device_type,
        "features": i.features,
        "limits": {"max_texture_dimension_2d": i.max_texture_dimension_2d, "max_bind_groups": i.max_bind_groups},
    })
}

struct Lister {
    error: Option<String>,
}

impl ApplicationHandler for Lister {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let result = (|| -> Result<Value, String> {
            let attrs = Window::default_attributes().with_visible(false);
            let window = Arc::new(el.create_window(attrs).map_err(|e| e.to_string())?);
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
            let gpu = Gpu::with_instance(instance, Some(&surface)).map_err(|e| e.to_string())?;
            let caps = surface.get_capabilities(&gpu.adapter);
            Ok(json!({
                "gpu": gpu_json(&gpu),
                "monitors": display::monitors_json(el),
                "present_modes": present_names(&caps.present_modes),
            }))
        })();
        match result {
            Ok(v) => println!("{v}"),
            Err(e) => self.error = Some(e),
        }
        el.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Arc<Gpu>,
    renderer: Renderer,
    started: Instant,
    last_frame: Instant,
    last_t: f32,
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    keys: HashSet<KeyCode>,
    grabbed: bool,
    samples: Vec<[f64; 3]>,
    rss: u64,
    recorder: Option<Recorder>,
    shot_taken: bool,
    shot_ok: bool,
    notes: Vec<String>,
    present_mode: wgpu::PresentMode,
    fov_x: f32,
    surfaces_drawn: Vec<f64>,
}

struct Viewer {
    cli: Cli,
    map: MapData,
    load_ms: f64,
    st: Option<State>,
    error: Option<String>,
}

impl Viewer {
    fn init(&mut self, el: &ActiveEventLoop) -> Result<State, String> {
        let (window, note) = display::create_window(el, &self.cli.request)?;
        let window = Arc::new(window);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let gpu =
            Arc::new(Gpu::with_instance(instance, Some(&surface)).map_err(|e| e.to_string())?);
        let caps = surface.get_capabilities(&gpu.adapter);
        // The shaders write display-referred values: an 8-bit linear (non-sRGB) target, not an HDR one.
        let format = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
            .into_iter()
            .find(|f| caps.formats.contains(f))
            .unwrap_or(caps.formats[0]);
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let copy_src = caps.usages.contains(wgpu::TextureUsages::COPY_SRC);
        if copy_src {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let (present_mode, present_note) = pick_present(&self.cli.present, &caps.present_modes);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&gpu.device, &config);
        let scene = Scene::new(&gpu, &self.map);
        let vfs = Vfs::open_stock(&self.cli.install, 0)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        let mut renderer = Renderer::new(
            gpu.clone(),
            scene,
            &self.map.techsets,
            TextureCache::new(Some(vfs), 0),
        );
        renderer.warm(config.format);
        let w = &renderer.scene.world;
        let (mins, maxs) = (Vec3::from(w.mins), Vec3::from(w.maxs));
        let start = flythrough::pose(0.0, mins, maxs);
        if let Some(out) = &self.cli.out {
            std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
        }
        let mut notes: Vec<String> = [note, present_note].into_iter().flatten().collect();
        let recorder = if self.cli.video && self.cli.flythrough {
            if copy_src {
                let path = self
                    .cli
                    .out
                    .as_deref()
                    .unwrap_or(Path::new("."))
                    .join("flythrough.mp4");
                std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
                    .map_err(|e| e.to_string())?;
                Some(
                    Recorder::start(&path, (config.width, config.height), 960, 30)
                        .map_err(|e| e.to_string())?,
                )
            } else {
                notes.push("surface cannot be copied from; no video".into());
                None
            }
        } else {
            None
        };
        let aspect = config.width as f32 / config.height as f32;
        let now = Instant::now();
        Ok(State {
            window,
            surface,
            config,
            gpu,
            renderer,
            started: now,
            last_frame: now,
            last_t: 0.0,
            pos: start.origin,
            yaw: start.yaw,
            pitch: start.pitch,
            keys: HashSet::new(),
            grabbed: false,
            samples: Vec::new(),
            rss: 0,
            recorder,
            shot_taken: false,
            shot_ok: false,
            notes,
            present_mode,
            fov_x: hor_plus(self.cli.fov, aspect),
            surfaces_drawn: Vec::new(),
        })
    }
}

impl ApplicationHandler for Viewer {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.st.is_some() {
            return;
        }
        match self.init(el) {
            Ok(s) => {
                s.window.request_redraw();
                self.st = Some(s);
            }
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let Some(st) = self.st.as_mut() else { return };
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) if s.width > 0 && s.height > 0 => {
                st.config.width = s.width;
                st.config.height = s.height;
                st.surface.configure(&st.gpu.device, &st.config);
                st.fov_x = hor_plus(self.cli.fov, s.width as f32 / s.height as f32);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(k),
                        state,
                        ..
                    },
                ..
            } => {
                if state == ElementState::Pressed {
                    if k == KeyCode::Escape {
                        el.exit();
                    }
                    st.keys.insert(k);
                } else {
                    st.keys.remove(&k);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.cli.flythrough => {
                st.grabbed = st
                    .window
                    .set_cursor_grab(CursorGrabMode::Locked)
                    .or_else(|_| st.window.set_cursor_grab(CursorGrabMode::Confined))
                    .is_ok();
                st.window.set_cursor_visible(!st.grabbed);
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.frame(el) {
                    self.error = Some(e);
                    el.exit();
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, ev: DeviceEvent) {
        if let (Some(st), DeviceEvent::MouseMotion { delta }) = (self.st.as_mut(), ev)
            && st.grabbed
        {
            st.yaw -= delta.0 as f32 * 0.0022;
            st.pitch = (st.pitch - delta.1 as f32 * 0.0022).clamp(-1.5, 1.5);
        }
    }
}

impl Viewer {
    fn frame(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let st = self.st.as_mut().ok_or("no window")?;
        let now = Instant::now();
        let t = st.started.elapsed().as_secs_f32();
        let interval = now.duration_since(st.last_frame).as_secs_f64() * 1000.0;
        st.last_frame = now;
        let dt = (t - st.last_t).min(0.1);
        st.last_t = t;

        if self.cli.flythrough {
            let w = &st.renderer.scene.world;
            let p = flythrough::pose(t, Vec3::from(w.mins), Vec3::from(w.maxs));
            (st.pos, st.yaw, st.pitch) = (p.origin, p.yaw, p.pitch);
        } else {
            fly(st, dt);
        }
        let frame = match st.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                st.surface.configure(&st.gpu.device, &st.config);
                st.window.request_redraw();
                return Ok(());
            }
        };
        let cpu_start = Instant::now();
        let view = View {
            origin: st.pos,
            yaw: st.yaw,
            pitch: st.pitch,
            fov_x: st.fov_x,
            time: t,
        };
        let target = frame.texture.create_view(&Default::default());
        let stats = st.renderer.render(
            &view,
            &target,
            st.config.format,
            (st.config.width, st.config.height),
        );
        st.surfaces_drawn.push(stats.surfaces as f64);
        if let Some(r) = st.recorder.as_mut() {
            r.capture(
                &st.gpu.device,
                &st.gpu.queue,
                &frame.texture,
                Duration::from_secs_f32(t),
            );
        }
        if self.cli.screenshot
            && self.cli.flythrough
            && !st.shot_taken
            && t >= self.cli.duration * 0.5
        {
            st.shot_taken = true;
            let out = self.cli.out.clone().unwrap_or_default();
            match save_png(
                &st.gpu,
                &frame.texture,
                st.config.format,
                &out.join("screenshot.png"),
            ) {
                Ok(()) => st.shot_ok = true,
                Err(e) => st.notes.push(format!("screenshot failed: {e}")),
            }
        }
        st.gpu.queue.present(frame);
        let cpu_ms = cpu_start.elapsed().as_secs_f64() * 1000.0;
        if st.samples.len() % 30 == 0 {
            st.rss = rss();
        }
        st.samples.push([cpu_ms, interval, st.rss as f64]);
        if self.cli.flythrough && t >= self.cli.duration {
            self.finish(el)?;
            el.exit();
        } else {
            st.window.request_redraw();
        }
        Ok(())
    }

    fn finish(&mut self, _: &ActiveEventLoop) -> Result<(), String> {
        let st = self.st.as_mut().ok_or("no window")?;
        let out = self.cli.out.clone().unwrap_or_default();
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        let mut csv = std::io::BufWriter::new(
            std::fs::File::create(out.join("frames.raw.csv")).map_err(|e| e.to_string())?,
        );
        writeln!(csv, "cpu_ms,gpu_ms,present_interval_ms,mem_bytes").map_err(|e| e.to_string())?;
        // The first interval is the time to the first frame, not a presentation interval.
        for s in st.samples.iter().skip(1) {
            writeln!(csv, "{:.4},,{:.4},{}", s[0], s[1], s[2] as u64).map_err(|e| e.to_string())?;
        }
        csv.flush().map_err(|e| e.to_string())?;
        let video = match st.recorder.take() {
            Some(r) => {
                let s = r.finish(&st.gpu.device).map_err(|e| e.to_string())?;
                json!({
                    "file": s.path.file_name().map(|f| f.to_string_lossy().into_owned()),
                    "frames": s.frames, "dropped": s.dropped, "bytes": s.bytes,
                    "width": s.width, "height": s.height, "fps": s.fps,
                })
            }
            None => Value::Null,
        };
        let mut drawn = st.surfaces_drawn.clone();
        drawn.sort_by(f64::total_cmp);
        let monitor_mhz = st
            .window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz());
        let report = json!({
            "status": "ok",
            "gpu": gpu_json(&st.gpu),
            "mode": {
                "width": st.config.width, "height": st.config.height,
                "refresh_mhz": monitor_mhz,
                "fullscreen": self.cli.request.kind.name(),
                "present_mode": format!("{:?}", st.present_mode).to_lowercase(),
            },
            "frames": st.samples.len(),
            "video": video,
            "screenshot": st.shot_ok.then_some("screenshot.png"),
            "zone_load_ms": self.load_ms,
            "world": {"surfaces_drawn_p50": drawn.get(drawn.len() / 2)},
            "notes": st.notes,
        });
        std::fs::write(
            out.join("client.json"),
            serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

fn fly(st: &mut State, dt: f32) {
    let speed = if st.keys.contains(&KeyCode::ShiftLeft) {
        1800.0
    } else {
        450.0
    } * dt;
    let fwd = Vec3::new(st.yaw.cos(), st.yaw.sin(), 0.0);
    let left = Vec3::new(-st.yaw.sin(), st.yaw.cos(), 0.0);
    for k in &st.keys {
        match k {
            KeyCode::KeyW => st.pos += fwd * speed,
            KeyCode::KeyS => st.pos -= fwd * speed,
            KeyCode::KeyA => st.pos += left * speed,
            KeyCode::KeyD => st.pos -= left * speed,
            KeyCode::Space => st.pos.z += speed,
            KeyCode::ControlLeft => st.pos.z -= speed,
            KeyCode::ArrowLeft => st.yaw += dt * 1.8,
            KeyCode::ArrowRight => st.yaw -= dt * 1.8,
            KeyCode::ArrowUp => st.pitch = (st.pitch + dt * 1.2).min(1.5),
            KeyCode::ArrowDown => st.pitch = (st.pitch - dt * 1.2).max(-1.5),
            _ => {}
        }
    }
}

fn rss() -> u64 {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let Ok(pid) = sysinfo::get_current_pid() else {
        return 0;
    };
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map_or(0, |p| p.memory())
}

/// Copy `tex` to the CPU and write it as an opaque PNG.
fn save_png(
    gpu: &Gpu,
    tex: &wgpu::Texture,
    format: wgpu::TextureFormat,
    path: &Path,
) -> Result<(), String> {
    let (w, h) = (tex.width(), tex.height());
    let bpr = (w * 4).next_multiple_of(256);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot"),
        size: u64::from(bpr * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    let data = buf
        .slice(..)
        .get_mapped_range()
        .map_err(|e| e.to_string())?;
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let row = &data[(y * bpr) as usize..(y * bpr + w * 4) as usize];
        for p in row.as_chunks::<4>().0 {
            rgba.extend_from_slice(&if bgra {
                [p[2], p[1], p[0], 255]
            } else {
                [p[0], p[1], p[2], 255]
            });
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut e = png::Encoder::new(file, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .and_then(|mut w| w.write_image_data(&rgba))
        .map_err(|e| e.to_string())
}
