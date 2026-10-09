// SPDX-License-Identifier: GPL-3.0-only
//! The winit application: window, surface, render loop, fly camera, flythrough recording.
//!
//! The render loop runs as fast as the present mode allows and is independent of any simulation tick: the camera is a
//! function of wall-clock time.

use crate::Cli;
use crate::display::{self, hor_plus, view_fov, zoom_sensitivity};
use crate::flythrough;
use crate::gfx::Gfx;
use crate::input::{Input, InputFrame, buttons};
use crate::listen::{self, Listen};
use crate::loader::{self, Load};
use crate::models::Library;
use crate::netplay::NetPlay;
use crate::profile::Profiles;
use crate::serverlist;
use crate::session::{LevelChange, level_change, rotation};
use crate::shell::{Action, Shell};
use crate::showcase::Showcase;
use crate::ui::UiKey;
use crate::ui::loading::LoadingView;
use crate::video::Recorder;
use assets::vfs::Vfs;
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

/// Frames the surface size must hold before the recorder starts.
const RECORD_AFTER_STABLE: u32 = 10;
/// How long joining waits for the server to say which map it plays.
const JOIN_QUERY: Duration = Duration::from_secs(2);

/// The GPU, once the browser has handed it out (the adapter and device are asynchronous there).
struct Ready {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: Arc<Gpu>,
    note: Option<String>,
}

pub fn run(cli: Cli) -> Result<(), String> {
    let el = EventLoop::<Ready>::with_user_event()
        .build()
        .map_err(|e| format!("no display: {e}"))?;
    el.set_control_flow(ControlFlow::Poll);
    #[cfg(not(target_arch = "wasm32"))]
    if cli.list {
        let mut l = Lister { error: None };
        el.run_app(&mut l).map_err(|e| e.to_string())?;
        return l.error.map_or(Ok(()), Err);
    }
    #[cfg(target_arch = "wasm32")]
    crate::web::log("loading the map");
    let t = Instant::now();
    let map = if cli.menu_mode() {
        None
    } else {
        Some(
            MapData::load(&cli.install, &cli.map)
                .map_err(|e| format!("cannot load {}: {e}", cli.map))?,
        )
    };
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    #[cfg(target_arch = "wasm32")]
    crate::web::log(&format!("map loaded in {load_ms:.0} ms"));
    let mut v = Viewer {
        cli,
        map,
        load_ms,
        st: None,
        error: None,
        proxy: None,
        #[cfg(target_arch = "wasm32")]
        starting: false,
    };
    v.proxy = Some(el.create_proxy());
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        // The page owns the loop; there is no exit to return through.
        el.spawn_app(v);
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        el.run_app(&mut v).map_err(|e| e.to_string())?;
        if let Some(st) = v.st.as_mut()
            && let Err(e) = st.input.save()
        {
            eprintln!("cannot save the config: {e}");
        }
        if let Some(st) = v.st.as_mut() {
            if let Some(n) = st.net.as_mut() {
                n.disconnect();
            }
            if let Some(l) = st.listen.as_mut() {
                l.finish();
            }
        }
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
}

/// The GPU backends a `--backend` name allows: `auto` leaves wgpu's choice (every backend the platform has; on the
/// web WebGPU, and WebGL2 when the browser has no WebGPU); otherwise a comma list of wgpu backend names (`vulkan`,
/// `metal`, `dx12`, `gl` or `webgl`, `webgpu`).
fn instance_descriptor(name: &str) -> Result<wgpu::InstanceDescriptor, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    if name == "auto" {
        return Ok(desc);
    }
    let b = wgpu::Backends::from_comma_list(match name {
        "webgl" => "gl",
        n => n,
    });
    if b.is_empty() {
        return Err(format!("unknown --backend {name}"));
    }
    desc.backends = b;
    Ok(desc)
}

/// What the page's debug overlay shows: the graphics backend and where the frame time goes.
#[cfg(target_arch = "wasm32")]
fn overlay_values(st: &State) -> Value {
    let info = st.gpu.describe();
    let recent = &st.samples[st.samples.len().saturating_sub(60)..];
    let mean = |i: usize| recent.iter().map(|s| s[i]).sum::<f64>() / recent.len().max(1) as f64;
    let mut cpu: Vec<f64> = st.samples[st.samples.len().saturating_sub(120)..]
        .iter()
        .map(|s| s[0])
        .collect();
    cpu.sort_by(f64::total_cmp);
    let p99 = cpu.get((cpu.len() * 99 / 100).min(cpu.len().saturating_sub(1)));
    let cpu_tail = format!(
        "{:.1}/{:.1}",
        p99.copied().unwrap_or(0.0),
        cpu.last().copied().unwrap_or(0.0)
    );
    let mut warm: Vec<f64> = st.warm_ms[st.warm_ms.len().saturating_sub(120)..].to_vec();
    warm.sort_by(f64::total_cmp);
    let warm_tail = format!(
        "{:.1}/{:.1}",
        warm.get((warm.len() * 99 / 100).min(warm.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0.0),
        warm.last().copied().unwrap_or(0.0)
    );
    let sound = st
        .net
        .as_ref()
        .map(|n| n.sound())
        .or(st.menu_sound.as_ref())
        .map_or_else(|| "off".to_owned(), |s| s.overlay_line());
    json!({
        "sound": sound,
        "view (x y z, yaw, pitch)": format!(
            "{:.0} {:.0} {:.0}, {:.1}, {:.1}",
            st.pos.x, st.pos.y, st.pos.z, st.yaw.to_degrees(), st.pitch.to_degrees()
        ),
        "pointer": format!("grabbed {} locked {}", st.grabbed, crate::web::pointer_locked()),
        "canvas": format!("{}x{}", st.config.width, st.config.height),
        "fov_x": format!("{:.1}", st.fov_x.to_degrees()),
        "backend": format!("{} ({})", info.backend, info.name),
        "BC textures on the GPU": st.gpu.bc,
        "decoded texture MiB": st.renderer.as_ref().map_or(0, |r| r.textures.decoded_bytes() >> 20),
        "frames": st.samples.len(),
        "CPU ms/frame (last 60)": format!("{:.1}", mean(0)),
        "CPU ms p99/max (last 120)": cpu_tail,
        "frame interval ms (last 60)": format!("{:.1}", mean(1)),
        "warm_step ms p99/max (last 120)": warm_tail,
        "pipelines built": format!("{}/{}", st.warm.done, st.warm.total),
        "draws skipped (no pipeline)": st.pipelines_missing,
        "surfaces drawn": st.surfaces_drawn.last().copied().unwrap_or(0.0),
        "wasm memory MiB": crate::web::memory_bytes() >> 20,
        "net phase": st.net.as_ref().map_or("none", NetPlay::phase),
        "net snapshots": st.net.as_ref().map_or(0, NetPlay::snapshots),
    })
}

fn pick_present(
    requested: &str,
    caps: &[wgpu::PresentMode],
) -> (wgpu::PresentMode, Option<String>) {
    let want = match requested {
        "mailbox" => wgpu::PresentMode::Mailbox,
        "immediate" => wgpu::PresentMode::Immediate,
        // Vsync off without a mode asked for: the first the surface has that does not wait for the display.
        "uncapped" => [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
            .into_iter()
            .find(|m| caps.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo),
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

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
struct Lister {
    error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ApplicationHandler<Ready> for Lister {
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
    renderer: Option<Renderer>,
    shell: Option<Shell>,
    started: Instant,
    last_frame: Instant,
    last_t: f32,
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    /// Roll of the view, radians (the recoil kick of the own player).
    roll: f32,
    input: Input,
    grabbed: bool,
    /// Whether the system cursor is hidden now (see `sync_os_cursor`).
    os_cursor_hidden: bool,
    gate: crate::pointer::PointerGate,
    /// When the pointer was last asked for, and whether the browser has confirmed the lock since: the page grants a
    /// lock a moment after the request and drops it on its own (Escape, a lost activation).
    grab_at: Instant,
    lock_seen: bool,
    /// A menu was open at the last frame.
    menu_was_open: bool,
    samples: Vec<[f64; 3]>,
    /// GPU milliseconds per render call, once the timestamps come back (index = frame number - 1).
    gpu_ms: Vec<Option<f64>>,
    rss: u64,
    recorder: Option<Recorder>,
    /// The recorder starts once the frame size has held for [`RECORD_AFTER_STABLE`] frames: a compositor may still be
    /// resizing the window after the first frame.
    want_video: bool,
    rec_size: (u32, u32),
    rec_stable: u32,
    shot_taken: bool,
    /// Effect meshes in the last frame played on a server; the screenshot waits for some.
    fx_in_view: usize,
    shot_ok: bool,
    notes: Vec<String>,
    present_mode: wgpu::PresentMode,
    /// `r_aspectRatio`: the screen's shape, if the menu chose one.
    aspect: Option<f32>,
    /// The saved graphics settings are applied by the first frame.
    video_pending: bool,
    fov_x: f32,
    /// The held weapon's zoom field of view and how far the zoom has come.
    aim_zoom: Option<(f32, f32)>,
    /// A turret's or the intermission's field of view, which the weapon cannot change.
    fixed_fov: Option<f32>,
    surfaces_drawn: Vec<f64>,
    showcase: Option<Showcase>,
    net: Option<NetPlay>,
    /// The map the world, renderer and net state are for (empty in the menus).
    map_name: String,
    /// The server last joined, for `reconnect`.
    last_server: Option<String>,
    listen: Option<Listen>,
    tour: Option<flythrough::Tour>,
    ui_tour: Option<UiTour>,
    script: Option<UiScript>,
    /// A screenshot to take after the next paint (name without extension).
    shot_request: Option<String>,
    /// The sound system of the menus when no match is running (started by the first menu sound).
    menu_sound: Option<crate::sound::ClientSound>,
    /// The map being loaded in the background (the loading screen shows meanwhile).
    loading: Option<Load>,
    /// Frame intervals from the start of a map load until the player is in the world.
    load_gaps: Option<LoadGaps>,
    /// What the last load measured, for the report.
    load_report: Option<Value>,
    /// The next session answers the team and class menus by itself (a script's `start=` step).
    autojoin_next: bool,
    /// Milliseconds the last frame spent in the network frame, the renderer, getting the surface texture and presenting,
    /// to explain a slow gap.
    prev_cost: [f64; 4],
    /// A resize that arrived during a map load; see [`apply_resize`].
    pending_resize: Option<(u32, u32)>,
    /// When the page's debug overlay was last given its values.
    #[cfg(target_arch = "wasm32")]
    overlay_at: Instant,
    /// How many pipelines the background compile has built, and how many draws the last frame skipped for want of one.
    #[cfg(target_arch = "wasm32")]
    warm: render::Progress,
    #[cfg(target_arch = "wasm32")]
    pipelines_missing: usize,
    /// Milliseconds of each frame's [`Renderer::warm_step`].
    #[cfg(target_arch = "wasm32")]
    warm_ms: Vec<f64>,
}

/// The longest gap between two presented frames while a map loads.
struct LoadGaps {
    map: String,
    started: Instant,
    frames: u32,
    max_ms: f64,
    load_ms: Option<f64>,
    /// Gaps over 40 ms: how long, what the last frame spent where, and whether the loading screen was still up.
    slow: Vec<Value>,
}

/// The `--ui-tour` script: which menu is open and how many frames it has been shown.
struct UiTour {
    menus: Vec<&'static str>,
    index: usize,
    frames: u32,
    results: Vec<Value>,
}

/// Menus the tour opens: the front end, then the script menus a match opens.
const TOUR_MENUS: &[&str] = &[
    "main",
    "pc_join_unranked",
    "createserver",
    "main_options",
    "options_graphics",
    "main_controls",
    "options_look",
    "options_move",
    "team_marinesopfor",
    "class",
    "changeclass",
    "scoreboard",
    "popup_leavegame",
    "endofgame",
];

/// What a browser frame spends compiling pipelines (shader linking is serial there and would freeze the page for
/// seconds if done at once): the frames before every pipeline exists skip the draws that lack one.
#[cfg(target_arch = "wasm32")]
const WARM_BUDGET: Duration = Duration::from_millis(6);

/// Frames a tour menu is shown before its screenshot (fades and expressions settle).
const TOUR_FRAMES: u32 = 6;

/// `--ui-script`: steps that drive the menus like a player.
struct UiScript {
    /// The map a `maprotate` step started from.
    marker: Option<String>,
    steps: Vec<String>,
    index: usize,
    /// When the current step began and, for waits, how long it may take.
    began: Instant,
    results: Vec<Value>,
}

struct Viewer {
    cli: Cli,
    map: Option<MapData>,
    load_ms: f64,
    st: Option<State>,
    error: Option<String>,
    /// Where the GPU arrives on the web.
    proxy: Option<winit::event_loop::EventLoopProxy<Ready>>,
    /// The browser window is open and the GPU has been asked for.
    #[cfg(target_arch = "wasm32")]
    starting: bool,
}

impl Viewer {
    /// Window, surface and GPU, where the platform can wait for the adapter.
    #[cfg(not(target_arch = "wasm32"))]
    fn init(&mut self, el: &ActiveEventLoop) -> Result<State, String> {
        let (window, note) = display::create_window(el, &self.cli.request)?;
        let window = Arc::new(window);
        let instance = wgpu::Instance::new(instance_descriptor(&self.cli.backend)?);
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let mut gpu = Gpu::with_instance(instance, Some(&surface)).map_err(|e| e.to_string())?;
        gpu.bc &= !self.cli.no_bc;
        let gpu = Arc::new(gpu);
        self.finish_init(Ready {
            window,
            surface,
            gpu,
            note,
        })
    }

    /// The browser hands out the adapter and device asynchronously: open the window now and finish in
    /// [`ApplicationHandler::user_event`] when the GPU arrives.
    #[cfg(target_arch = "wasm32")]
    fn begin_init(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let (window, note) = display::create_window(el, &self.cli.request)?;
        let window = Arc::new(window);
        let proxy = self.proxy.clone().ok_or("no event loop proxy")?;
        let desc = instance_descriptor(&self.cli.backend)?;
        let no_bc = self.cli.no_bc;
        wasm_bindgen_futures::spawn_local(async move {
            let ready = async {
                let instance = wgpu::util::new_instance_with_webgpu_detection(desc).await;
                let surface = instance
                    .create_surface(window.clone())
                    .map_err(|e| e.to_string())?;
                let mut gpu = Gpu::with_instance_async(instance, Some(&surface))
                    .await
                    .map_err(|e| e.to_string())?;
                gpu.bc &= !no_bc;
                Ok::<_, String>(Ready {
                    window,
                    surface,
                    gpu: Arc::new(gpu),
                    note,
                })
            }
            .await;
            match ready {
                Ok(r) => {
                    let _ = proxy.send_event(r);
                }
                Err(e) => crate::web::fatal(&format!("no usable graphics: {e}")),
            }
        });
        Ok(())
    }

    fn finish_init(&mut self, ready: Ready) -> Result<State, String> {
        let Ready {
            window,
            surface,
            gpu,
            note,
        } = ready;
        let caps = surface.get_capabilities(&gpu.adapter);
        // The shaders write display-referred values: an 8-bit linear (non-sRGB) target, not an HDR one.
        let format = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]
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
        let vfs = Vfs::open_stock(&self.cli.install, 0)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        let renderer = match &self.map {
            Some(map) => {
                #[cfg(target_arch = "wasm32")]
                crate::web::log("building the scene");
                let scene = Scene::new(&gpu, map);
                #[cfg(target_arch = "wasm32")]
                crate::web::log("building the renderer");
                let mut r = Renderer::new(gpu.clone(), scene, map, TextureCache::new(Some(vfs), 0));
                r.settings = self.cli.settings;
                #[cfg(target_arch = "wasm32")]
                crate::web::log("compiling pipelines in the background");
                // The browser builds its pipelines a few milliseconds per frame, see `WARM_BUDGET`.
                #[cfg(not(target_arch = "wasm32"))]
                r.warm(config.format);
                #[cfg(target_arch = "wasm32")]
                crate::web::log("renderer ready");
                Some(r)
            }
            None => None,
        };
        let tour = match (&renderer, &self.map) {
            (Some(r), Some(map)) => {
                let w = &r.scene.world;
                Some(flythrough::Tour::new(
                    &map.spawn_points(),
                    (Vec3::from(w.mins), Vec3::from(w.maxs)),
                ))
            }
            _ => None,
        };
        let start = tour.as_ref().map_or_else(
            || flythrough::Tour::new(&[], (Vec3::splat(-1000.0), Vec3::splat(1000.0))).pose(0.0),
            |t| t.pose(0.0),
        );
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(out) = &self.cli.out {
            std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
        }
        let mut notes: Vec<String> = [note, present_note].into_iter().flatten().collect();
        let showcase = match self.cli.show_models {
            Some(n) => {
                let t = Instant::now();
                let mut lib = Library::load(&self.cli.install, &self.cli.map)?;
                let r = renderer.as_ref().ok_or("no map")?;
                let w = &r.scene.world;
                let collision = r
                    .scene
                    .collision
                    .clone()
                    .ok_or("the map has no collision data")?;
                let lit = |p: Vec3| {
                    let sight = &*collision as &dyn render::lightgrid::SightTrace;
                    render::lightgrid::light_grid_lookup(&w.light_grid, p.to_array(), Some(sight))
                        .entries
                        .iter()
                        .any(Option::is_some)
                };
                let s = Showcase::new(
                    &mut lib,
                    &*collision,
                    tour.as_ref().ok_or("no map")?,
                    "m16_gl_mp",
                    n,
                    &lit,
                )?;
                notes.push(format!(
                    "showcase: {n} players, models loaded in {:.0} ms",
                    t.elapsed().as_secs_f64() * 1000.0
                ));
                notes.extend(s.describe());
                Some(s)
            }
            None => None,
        };
        let (mut net, mut listen) = (None, None);
        if self.cli.netplay() {
            let t = Instant::now();
            let addr = match &self.cli.connect {
                Some(a) => serverlist::resolve(a)?,
                None => {
                    let l = listen::start(
                        &self.cli.install,
                        listen::Config {
                            map: self.cli.map.clone(),
                            bots: self.cli.bots,
                            gametype: self.cli.gametype.clone(),
                            rotation: None,
                            port: 0,
                            dvars: Vec::new(),
                        },
                    )?;
                    let a = l.addr;
                    notes.push(format!(
                        "listen server with {} bots up in {:.0} ms",
                        self.cli.bots,
                        t.elapsed().as_secs_f64() * 1000.0
                    ));
                    listen = Some(l);
                    a
                }
            };
            let lib = Library::load(&self.cli.install, &self.cli.map)?;
            let limits = Input::detached().pitch_limits();
            let mut n = NetPlay::connect(
                lib,
                self.map.as_ref().ok_or("no map")?,
                addr,
                &self.cli.name,
                limits,
                self.cli.autoplay,
                crate::sound::ClientSound::start(
                    &self.cli.install,
                    &self.cli.map,
                    !self.cli.no_sound,
                ),
            )?;
            n.set_autojoin(self.cli.autojoin || self.cli.autoplay);
            if let Some(name) = &self.cli.fx_demo {
                n.set_fx_demo(name.clone());
            }
            net = Some(n);
        }
        let install = server::content::Install::open(&self.cli.install)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        let mut input = Input::new(self.cli.config_dir.clone(), Some(&install.vfs));
        let mut shell = if self.cli.flythrough || self.cli.show_models.is_some() {
            None
        } else {
            Some(Shell::new(
                gpu.clone(),
                config.format,
                (config.width, config.height),
                &install,
                &mut input,
            )?)
        };
        if let (Some(sh), Some(n)) = (shell.as_mut(), net.as_ref()) {
            sh.ui.assets.add_weapon_icons(&n.weapon_defs());
        }
        let (profiles, stats) = Profiles::open(&install.root, input.config_dir(), "default");
        let (read, write) = profiles.config_paths();
        input.use_profile(read, write);
        if let Some(n) = net.as_mut() {
            n.set_profile(&stats);
        }
        if let Some(sh) = shell.as_mut() {
            sh.set_profiles(&mut input, profiles, stats);
            let (modes, rates) = display::video_modes(&window);
            sh.set_display(
                &mut input,
                modes,
                rates,
                (config.width, config.height),
                window.fullscreen().is_some(),
            );
        }
        if self.cli.menu_mode()
            && let Some(sh) = shell.as_mut()
        {
            sh.open(&mut input, "main");
        }
        // A match started from the command line is a match in progress: the HUD menus draw.
        if net.is_some()
            && let Some(sh) = shell.as_mut()
        {
            sh.st.in_game = true;
        }
        let want_video = self.cli.video && self.cli.flythrough;
        if want_video && !copy_src {
            notes.push("surface cannot be copied from; no video".into());
        }
        let want_video = want_video && copy_src;
        let aspect = config.width as f32 / config.height as f32;
        let now = Instant::now();
        if self.cli.fov_given {
            input.cvars.set("cg_fov", &self.cli.fov.to_string(), false);
        }
        let fov_x = hor_plus(cg_fov(&input), aspect);
        Ok(State {
            tour,
            window,
            surface,
            config,
            gpu,
            renderer,
            shell,
            started: now,
            last_frame: now,
            last_t: 0.0,
            pos: start.origin,
            yaw: start.yaw,
            pitch: start.pitch,
            roll: 0.0,
            input,
            grabbed: false,
            os_cursor_hidden: false,
            gate: Default::default(),
            grab_at: now,
            lock_seen: false,
            menu_was_open: false,
            samples: Vec::new(),
            gpu_ms: Vec::new(),
            rss: 0,
            recorder: None,
            want_video,
            rec_size: (0, 0),
            rec_stable: 0,
            shot_taken: false,
            fx_in_view: 0,
            shot_ok: false,
            notes,
            present_mode,
            aspect: None,
            video_pending: !self.cli.video_given && !self.cli.flythrough,
            fov_x,
            aim_zoom: None,
            fixed_fov: None,
            surfaces_drawn: Vec::new(),
            showcase,
            net,
            map_name: if self.cli.netplay() {
                self.cli.map.clone()
            } else {
                String::new()
            },
            last_server: self.cli.connect.clone(),
            listen,
            script: self.cli.ui_script.as_ref().map(|s| UiScript {
                marker: None,
                steps: s
                    .split(',')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect(),
                index: 0,
                began: Instant::now(),
                results: Vec::new(),
            }),
            shot_request: None,
            menu_sound: None,
            loading: None,
            load_gaps: None,
            load_report: None,
            autojoin_next: false,
            prev_cost: [0.0; 4],
            pending_resize: None,
            #[cfg(target_arch = "wasm32")]
            overlay_at: now,
            #[cfg(target_arch = "wasm32")]
            warm: render::Progress { done: 0, total: 0 },
            #[cfg(target_arch = "wasm32")]
            pipelines_missing: 0,
            #[cfg(target_arch = "wasm32")]
            warm_ms: Vec::new(),
            ui_tour: self.cli.ui_tour.as_ref().map(|_| UiTour {
                menus: TOUR_MENUS.to_vec(),
                index: 0,
                frames: 0,
                results: Vec::new(),
            }),
        })
    }
}

impl ApplicationHandler<Ready> for Viewer {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.st.is_some() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        match self.init(el) {
            Ok(s) => {
                s.window.request_redraw();
                self.st = Some(s);
            }
            Err(e) => {
                #[cfg(target_arch = "wasm32")]
                crate::web::fatal(&e);
                self.error = Some(e);
                el.exit();
            }
        }
        #[cfg(target_arch = "wasm32")]
        if !self.starting {
            self.starting = true;
            if let Err(e) = self.begin_init(el) {
                crate::web::fatal(&e);
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ready: Ready) {
        match self.finish_init(ready) {
            Ok(s) => {
                s.window.request_redraw();
                self.st = Some(s);
            }
            Err(e) => {
                #[cfg(target_arch = "wasm32")]
                crate::web::fatal(&e);
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let Some(st) = self.st.as_mut() else { return };
        if let WindowEvent::KeyboardInput { event, .. } = &ev
            && event.state == ElementState::Pressed
            && !event.repeat
            && event.physical_key == PhysicalKey::Code(KeyCode::Backquote)
            && let Some(sh) = st.shell.as_mut()
        {
            // `~` opens and closes the console wherever the player is; its key is never typed into it.
            sh.st.console.toggle();
            st.input.release_all();
            return;
        }
        let typing = st.shell.as_ref().is_some_and(|s| s.st.console.active());
        let captured = st.shell.as_ref().is_some_and(|s| s.ui.captures_input());
        if typing {
            if let (Some(sh), WindowEvent::KeyboardInput { event, .. }) = (st.shell.as_mut(), &ev)
                && event.state == ElementState::Pressed
            {
                for k in typed_keys(event) {
                    sh.console_key(&mut st.input, k);
                }
            }
        } else if captured {
            ui_event(st, &ev);
        } else {
            st.input.window_event(&ev);
        }
        if let WindowEvent::KeyboardInput { event, .. } = &ev
            && event.state == ElementState::Pressed
            && !event.repeat
            && event.physical_key == PhysicalKey::Code(KeyCode::F11)
        {
            toggle_fullscreen(&st.window);
        }
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) if s.width > 0 && s.height > 0 => {
                if st.loading.is_some() {
                    st.pending_resize = Some((s.width, s.height));
                } else {
                    apply_resize(st, (s.width, s.height));
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.cli.timed() && !captured && st.renderer.is_some() => {
                st.gate.click();
                grab_pointer(st);
            }
            WindowEvent::Focused(false) => release_pointer(st),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.frame(el) {
                    #[cfg(target_arch = "wasm32")]
                    crate::web::fatal(&e);
                    self.error = Some(e);
                    el.exit();
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, ev: DeviceEvent) {
        if let Some(st) = self.st.as_mut() {
            // A player typing is not looking around.
            if !st.shell.as_ref().is_some_and(|s| s.st.console.active()) {
                st.input.device_event(&ev);
            }
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
        if let Some(g) = st.load_gaps.as_mut() {
            g.frames += 1;
            g.max_ms = g.max_ms.max(interval);
            if interval > 40.0 && g.slow.len() < 16 {
                g.slow.push(json!({
                    "ms": interval.round(),
                    "loading_screen": st.loading.is_some(),
                    "net_ms": st.prev_cost[0].round(),
                    "render_ms": st.prev_cost[1].round(),
                    "acquire_ms": st.prev_cost[2].round(),
                    "present_ms": st.prev_cost[3].round(),
                }));
            }
        }
        if let Some(done) = st.loading.as_mut().and_then(Load::poll)
            && let Some(load) = st.loading.take()
        {
            let result = done.and_then(|l| finish_load(&self.cli, &mut self.map, st, &load, l));
            if let Err(e) = result {
                eprintln!("cannot load {}: {e}", load.map);
                end_session(&mut self.map, st);
            }
        }

        #[cfg(target_arch = "wasm32")]
        if st.grabbed {
            // The page owns the lock: Escape or a refused request ends it without a word to the window.
            if crate::web::pointer_locked() {
                st.lock_seen = true;
            } else if st.lock_seen || st.grab_at.elapsed() > Duration::from_millis(750) {
                // The browser took the lock back (Escape) or refused it (no user activation): like letting go on
                // purpose, a click takes it again; asking every frame would only be refused again.
                st.gate.let_go();
                release_pointer(st);
                // Escape ends the lock without reaching the page's key events: a chat line open then is closed, as
                // Escape would have closed it.
                if let Some(sh) = st.shell.as_mut()
                    && matches!(sh.st.console.mode, crate::console::Mode::Chat { .. })
                {
                    sh.st.console.close();
                }
            }
        }
        let menu_open = st.shell.as_ref().is_some_and(|s| s.ui.captures_input());
        if menu_open {
            // A locked cursor never moves, so the menu could not be clicked.
            release_pointer(st);
            // The menu takes the window's events: a button or key let go while it is open never reaches the input,
            // and a right click held when it opened would keep the sights up for good.
            if !st.menu_was_open {
                st.input.release_all();
            }
        }
        st.menu_was_open = menu_open;
        sync_os_cursor(st);
        // In a match the pointer is the game's whenever no menu wants it; a click is not required.
        let in_match = st.net.is_some()
            && st.renderer.is_some()
            && !self.cli.timed()
            && self.cli.ui_script.is_none();
        // The browser grants a lock only to a page the player just touched.
        #[cfg(target_arch = "wasm32")]
        let may_ask = crate::web::user_active();
        #[cfg(not(target_arch = "wasm32"))]
        let may_ask = true;
        if st
            .gate
            .wants_lock(in_match, menu_open, st.window.has_focus())
            && !st.grabbed
            && may_ask
        {
            grab_pointer(st);
        }
        if let Some(why) = st
            .net
            .as_ref()
            .and_then(NetPlay::dropped)
            .map(str::to_owned)
        {
            connection_lost(&mut self.map, st, &why);
        }
        let mut new_level = None;
        if let Some(sc) = st.showcase.as_mut() {
            let (p, y, pi) = sc.camera();
            (st.pos, st.yaw, st.pitch, st.roll) = (p, y, pi, 0.0);
            if let Some(r) = st.renderer.as_mut() {
                r.dynamic_models = sc.update(dt);
            }
        } else if st.loading.is_none()
            && let Some(net) = st.net.as_mut()
        {
            let f = if self.cli.autoplay {
                // The bot drives; only the grenade keys stay a script's (`throw=`).
                let keys = st.input.frame(dt).buttons;
                InputFrame {
                    buttons: keys & (buttons::FRAG | buttons::SMOKE),
                    ..InputFrame::default()
                }
            } else {
                let f = st.input.frame(dt);
                let typing = st.shell.as_ref().is_some_and(|s| s.st.console.active());
                if menu_open || typing {
                    InputFrame::default()
                } else {
                    f
                }
            };
            open_chat(st.shell.as_mut(), &mut st.input, &f);
            if f.quit() {
                el.exit();
            }
            if f.toggle_menu()
                && let Some(sh) = st.shell.as_mut()
            {
                let menu = st.input.cvar("g_scriptMainMenu").unwrap_or("").to_owned();
                sh.open(&mut st.input, &menu);
                if !sh.ui.captures_input() {
                    st.gate.let_go();
                }
            }
            if self.cli.autoplay && f.toggle_menu() {
                el.exit();
            }
            if let Some(why) = net.refused() {
                return Err(format!("the server refused the connection: {why}"));
            }
            #[cfg(target_arch = "wasm32")]
            if let Some(why) = crate::web::wire_failure() {
                return Err(format!("cannot reach the server: {why}"));
            }
            let t_net = Instant::now();
            net.set_volume(crate::sound::volume_of(&st.input.cvars));
            net.set_breath_volumes(
                st.input
                    .cvars
                    .with_prefix("bg_shock_volume_")
                    .into_iter()
                    .filter_map(|(n, v)| {
                        Some((
                            n.strip_prefix("bg_shock_volume_")?.to_owned(),
                            v.trim().parse().ok()?,
                        ))
                    })
                    .collect(),
            );
            net.set_footsteps(
                st.input
                    .cvar("cg_footsteps")
                    .is_none_or(|v| v.trim() != "0"),
            );
            if let Some(secs) = st
                .input
                .cvar("cl_timeout")
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
            {
                // The original's range is 0 to 3600 s; a timeout of 0 would drop the client at once.
                net.set_timeout(Duration::from_secs_f32(secs.clamp(1.0, 3600.0)));
            }
            let frame_out = net.frame(dt, &f);
            st.input.apply(&net.take_input_feedback());
            st.prev_cost[0] = t_net.elapsed().as_secs_f64() * 1000.0;
            if net.spawned()
                && let Some(g) = st.load_gaps.take()
            {
                st.load_report = Some(json!({
                    "map": g.map,
                    "load_ms": g.load_ms,
                    "to_spawn_ms": g.started.elapsed().as_secs_f64() * 1000.0,
                    "frames": g.frames,
                    "max_frame_gap_ms": g.max_ms,
                    "slow_gaps_ms": g.slow,
                }));
            }
            new_level = net.take_new_level();
            if let Some(sh) = st.shell.as_mut() {
                net.fill_live(&mut sh.st.live);
                sh.st.live.damage_in_scope = st
                    .input
                    .cvars
                    .get("cg_hudDamageIconInScope")
                    .is_some_and(|v| v.trim().parse::<f32>().is_ok_and(|v| v != 0.0));
                sh.st.live.descriptive_text = st
                    .input
                    .cvars
                    .get("cg_descriptiveText")
                    .is_none_or(|v| v.trim().parse::<f32>().is_ok_and(|v| v != 0.0));
                let aspect = st.config.height as f32 / st.config.width as f32;
                if let Some(r) = sh.st.live.reticle.as_mut() {
                    r.tan_half_fov_y = (st.fov_x * 0.5).tan() * aspect;
                }
                for ev in net.take_ui_events() {
                    sh.apply(&mut st.input, ev);
                }
                let now = sh.now_ms();
                net.fill_game_facts(&mut sh.st.game, now);
                let radar = crate::ownerdraw::radar_cfg(&st.input.cvars);
                sh.st.game.hud.step_radar(now, &radar);
                // The server's later stat changes (rank, unlocks) are the profile's. A new level's first burst is not.
                if new_level.is_none() {
                    for (i, v) in net.stat_changes() {
                        if let Some(s) =
                            usize::try_from(i).ok().and_then(|i| sh.st.stats.get_mut(i))
                        {
                            *s = v;
                        }
                    }
                }
                sh.tick(&mut st.input);
                let scores = f.held_other.iter().any(|c| c == "scores") || sh.st.scores_forced;
                if scores != sh.st.game.scoreboard {
                    sh.st.game.scoreboard = scores;
                    if scores {
                        sh.open(&mut st.input, "scoreboard");
                    } else {
                        sh.close_by_name(&mut st.input, "scoreboard");
                    }
                }
            }
            if f.toggle_menu() {
                release_pointer(st);
            }
            if f.toggle_fullscreen() {
                toggle_fullscreen(&st.window);
            }
            if let Some(nf) = frame_out {
                (st.pos, st.yaw, st.pitch, st.roll) = (nf.origin, nf.yaw, nf.pitch, nf.roll);
                st.aim_zoom = nf.sight.as_ref().map(|s| (s.zoom_fov, s.zoom));
                st.fixed_fov = nf.fixed_fov;
                let fov = view_fov_4_3(st);
                st.input
                    .set_fov_sensitivity_scale(zoom_sensitivity(fov, cg_fov(&st.input)));
                st.fx_in_view = nf.meshes.len();
                if let Some(r) = st.renderer.as_mut() {
                    r.dynamic_models = nf.models;
                    r.dynamic_meshes = nf.meshes;
                    if let Some((glow, film)) = nf.look.vision {
                        (r.post.glow, r.post.film) = (glow, film);
                    }
                    r.post.shell_shock = nf.look.shell_shock;
                    r.post.save_screen |= nf.look.save_screen;
                }
            }
        } else if st.loading.is_some() {
            let f = st.input.frame(dt);
            if f.quit() {
                el.exit();
            }
            if f.toggle_menu() {
                end_session(&mut self.map, st);
            }
        } else if self.cli.flythrough {
            if let Some(tour) = st.tour.as_ref() {
                let p = tour.pose(self.cli.fly_at.unwrap_or(t));
                (st.pos, st.yaw, st.pitch, st.roll) = (p.origin, p.yaw, p.pitch, 0.0);
            }
        } else {
            let f = st.input.frame(dt);
            if f.quit() {
                el.exit();
            }
            if f.toggle_menu() {
                release_pointer(st);
            }
            if f.toggle_fullscreen() {
                toggle_fullscreen(&st.window);
            }
            if st.renderer.is_some() && st.shell.is_none() {
                st.roll = 0.0;
                fly(st, &f, dt);
            }
        }
        if let Some(name) = new_level
            && let Err(e) = enter_level(&self.cli, &mut self.map, st, &name)
        {
            eprintln!("cannot enter {name}: {e}");
            end_session(&mut self.map, st);
        }
        if let Some(sh) = st.shell.as_mut()
            && let Err(e) = sh.st.profiles.save_if_changed(&sh.st.stats)
        {
            eprintln!("cannot save the profile: {e}");
        }
        if std::mem::take(&mut st.video_pending) && st.shell.is_some() {
            vid_restart(&self.cli, st);
        }
        if st.loading.is_none()
            && let Some(size) = st.pending_resize.take()
        {
            apply_resize(st, size);
        }
        let t_acquire = Instant::now();
        let frame = match st.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                if st.loading.is_none() {
                    reconfigure(st);
                }
                st.window.request_redraw();
                return Ok(());
            }
        };
        st.prev_cost[2] = t_acquire.elapsed().as_secs_f64() * 1000.0;
        let cpu_start = Instant::now();
        let aspect = st
            .aspect
            .unwrap_or(st.config.width as f32 / st.config.height.max(1) as f32);
        st.fov_x = hor_plus(cg_fov(&st.input), aspect);
        // Aiming zooms the world to the weapon's zoom field of view; the gun keeps the unzoomed one.
        let fov_x = hor_plus(view_fov_4_3(st), aspect);
        let view = View {
            origin: st.pos,
            yaw: st.yaw,
            pitch: st.pitch,
            roll: st.roll,
            fov_x,
            time: t,
        };
        let target = frame.texture.create_view(&Default::default());
        let size = (st.config.width, st.config.height);
        if let Some(r) = st.renderer.as_mut() {
            r.viewmodel_fov_x = Some(st.fov_x);
            #[cfg(target_arch = "wasm32")]
            {
                let t = Instant::now();
                st.warm = r.warm_step(st.config.format, WARM_BUDGET);
                st.warm_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let t_render = Instant::now();
            let stats = r.render(&view, &target, st.config.format, size);
            st.prev_cost[1] = t_render.elapsed().as_secs_f64() * 1000.0;
            #[cfg(target_arch = "wasm32")]
            {
                st.pipelines_missing = stats.pipelines_missing;
            }
            st.surfaces_drawn.push(stats.surfaces as f64);
            record_gpu(&mut st.gpu_ms, r.take_gpu_times());
        }
        if let (Some(sh), Some(l)) = (st.shell.as_mut(), st.loading.as_ref()) {
            let view = LoadingView {
                map: &l.map,
                note: l.note(),
                progress: l.progress(),
            };
            sh.paint_loading(&target, size, &view);
        } else if let Some(sh) = st.shell.as_mut() {
            let clear = st.renderer.is_none().then_some(wgpu::Color::BLACK);
            sh.st.live.clip = Some(view.clip_from_world(size.0 as f32 / size.1.max(1) as f32));
            sh.paint(&mut st.input, &target, size, clear);
        }
        if st.want_video && st.recorder.is_none() {
            let size = (frame.texture.width(), frame.texture.height());
            if size == st.rec_size {
                st.rec_stable += 1;
            } else {
                st.rec_size = size;
                st.rec_stable = 0;
            }
        }
        if st.want_video && st.recorder.is_none() && st.rec_stable >= RECORD_AFTER_STABLE {
            st.want_video = false;
            let path = self
                .cli
                .out
                .as_deref()
                .unwrap_or(Path::new("."))
                .join("flythrough.mp4");
            match Recorder::start(&path, st.rec_size, 720, 30) {
                Ok(r) => st.recorder = Some(r),
                Err(e) => st.notes.push(format!("video: {e}")),
            }
        }
        if let Some(r) = st.recorder.as_mut() {
            if (frame.texture.width(), frame.texture.height()) == st.rec_size {
                r.capture(
                    &st.gpu.device,
                    &st.gpu.queue,
                    &frame.texture,
                    Duration::from_secs_f32(t),
                );
            } else if !st
                .notes
                .iter()
                .any(|n| n.starts_with("window resized during"))
            {
                st.notes.push(
                    "window resized during the recording; frames at the new size are not recorded"
                        .into(),
                );
            }
        }
        if self.cli.screenshot
            && self.cli.timed()
            && !st.shot_taken
            && t >= self.cli.duration * 0.5
            && (st.net.is_none() || st.fx_in_view > 0 || t >= self.cli.duration * 0.8)
        {
            st.shot_taken = true;
            let out = self.cli.out.clone().unwrap_or_default();
            match save_png(
                &st.gpu,
                &frame.texture,
                st.config.format,
                &out.join("screenshot.png"),
            ) {
                Ok(_) => st.shot_ok = true,
                Err(e) => st.notes.push(format!("screenshot failed: {e}")),
            }
        }
        if let Some(t) = st.ui_tour.as_mut() {
            let dir = self.cli.ui_tour.clone().unwrap_or_default();
            let done = tour_step(
                st.shell.as_mut(),
                &mut st.input,
                t,
                &st.gpu,
                &frame.texture,
                st.config.format,
                &dir,
            );
            if done {
                let missing: Vec<String> = st
                    .shell
                    .as_ref()
                    .map(|s| s.missing().to_vec())
                    .unwrap_or_default();
                let report = json!({"status": "ok", "menus": t.results, "missing_images": missing});
                let _ = std::fs::write(
                    dir.join("ui.json"),
                    serde_json::to_vec_pretty(&report).unwrap_or_default(),
                );
                el.exit();
            }
        }
        if let Some(name) = st.shot_request.take() {
            let out = self.cli.out.clone().unwrap_or_default();
            let _ = std::fs::create_dir_all(&out);
            let lit = save_png(
                &st.gpu,
                &frame.texture,
                st.config.format,
                &out.join(format!("{name}.png")),
            );
            let ok = lit.is_ok();
            let hud = st.shell.as_ref().map(|s| s.st.game.hud.report());
            if let Some(sc) = st.script.as_mut() {
                sc.results.push(json!({"step": format!("shot={name}"), "ok": ok, "hud": hud,
                    "lit_fraction": lit.as_ref().ok(), "menus": st.shell.as_ref().map(|s| s.ui.open_menus().join(","))}));
                sc.index += 1;
                sc.began = Instant::now();
            }
        }
        if st.script.is_some() && script_step(st) {
            write_script_report(st, self.cli.out.clone().unwrap_or_default(), false);
            el.exit();
        }
        let t_present = Instant::now();
        st.gpu.queue.present(frame);
        st.prev_cost[3] = t_present.elapsed().as_secs_f64() * 1000.0;
        if let Some(m) = st.menu_sound.as_mut() {
            m.set_volume(crate::sound::volume_of(&st.input.cvars));
            m.frame([0.0; 3], 0.0, (interval / 1000.0) as f32);
        }
        let actions = st
            .shell
            .as_mut()
            .map(Shell::drain_actions)
            .unwrap_or_default();
        self.apply_actions(el, actions)?;
        let st = self.st.as_mut().ok_or("no window")?;
        let cpu_ms = cpu_start.elapsed().as_secs_f64() * 1000.0;
        if st.samples.len() % 30 == 0 {
            st.rss = rss();
        }
        st.samples.push([cpu_ms, interval, st.rss as f64]);
        #[cfg(target_arch = "wasm32")]
        if st.overlay_at.elapsed() >= Duration::from_millis(500) {
            st.overlay_at = Instant::now();
            crate::web::overlay(&overlay_values(st));
        }
        if self.cli.timed() && t >= self.cli.duration {
            self.finish(el)?;
            el.exit();
        } else {
            st.window.request_redraw();
        }
        Ok(())
    }

    fn apply_actions(&mut self, el: &ActiveEventLoop, actions: Vec<Action>) -> Result<(), String> {
        for a in actions {
            let st = self.st.as_mut().ok_or("no window")?;
            let a = match a {
                Action::Reconnect => match st.last_server.clone() {
                    Some(addr) => Action::Join(addr),
                    None => {
                        show_error(st, "There is no server to reconnect to.");
                        continue;
                    }
                },
                a => a,
            };
            match a {
                Action::Reconnect => {}
                Action::Quit => {
                    // A scripted run that reaches Quit has proved the exit path.
                    if st.script.is_some() {
                        write_script_report(st, self.cli.out.clone().unwrap_or_default(), true);
                    }
                    el.exit();
                }
                Action::StartServer { map, gametype } => {
                    if let Err(e) =
                        start_session(&self.cli, &mut self.map, st, &map, &gametype, None)
                    {
                        eprintln!("cannot start the server: {e}");
                        end_session(&mut self.map, st);
                    }
                }
                Action::Join(addr) => {
                    st.last_server = Some(addr.clone());
                    // The client loads what the server is playing, so it asks first.
                    let found = serverlist::resolve(&addr)
                        .and_then(|a| serverlist::query(a, JOIN_QUERY).map(|e| (a, e)));
                    match found {
                        Ok((a, e)) => {
                            if let Err(err) = start_session(
                                &self.cli,
                                &mut self.map,
                                st,
                                &e.map,
                                &e.gametype,
                                Some(a),
                            ) {
                                eprintln!("cannot join {addr}: {err}");
                                end_session(&mut self.map, st);
                            }
                        }
                        Err(err) => {
                            eprintln!("cannot join {addr}: {err}");
                            show_error(st, &err);
                        }
                    }
                }
                Action::Disconnect => end_session(&mut self.map, st),
                Action::MenuResponse { menu, response } => {
                    if let Some(n) = st.net.as_mut() {
                        n.send_command(&format!("menuresponse {menu} {response}"));
                    }
                }
                Action::VidRestart => vid_restart(&self.cli, st),
                Action::Console(line) => console_action(st, &line),
                // Menu sounds live in the zones of the front end (`code_post_gfx`), which the match's tables
                // do not load, so they have their own small sound system.
                Action::Sound(alias) => {
                    if !self.cli.no_sound {
                        st.menu_sound
                            .get_or_insert_with(|| {
                                crate::sound::ClientSound::start(&self.cli.install, "", true)
                            })
                            .play_ui(&alias);
                    }
                }
            }
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
        if let Some(r) = st.renderer.as_mut() {
            record_gpu(&mut st.gpu_ms, r.flush_gpu_times());
        }
        // The first interval is the time to the first frame, not a presentation interval.
        for (i, s) in st.samples.iter().enumerate().skip(1) {
            let gpu = st
                .gpu_ms
                .get(i)
                .copied()
                .flatten()
                .map_or_else(String::new, |g| format!("{g:.4}"));
            writeln!(csv, "{:.4},{gpu},{:.4},{}", s[0], s[1], s[2] as u64)
                .map_err(|e| e.to_string())?;
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
        let net = st.net.as_mut().map(|n| {
            let r = n.report();
            n.disconnect();
            r
        });
        let server = st.listen.as_mut().map(Listen::finish);
        let report = json!({
            "status": "ok",
            "net": net,
            "server": server,
            "gpu": gpu_json(&st.gpu),
            "settings": {
                "shadows": format!("{:?}", self.cli.settings.shadows).to_lowercase(),
                "fog": self.cli.settings.fog,
                "primary_lights": self.cli.settings.primary_lights,
            },
            "gpu_timestamps": st.renderer.as_ref().is_some_and(|r| r.timer.is_some()),
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
            "showcase": st.showcase.as_ref().map(Showcase::report),
            "notes": st.notes,
        });
        std::fs::write(
            out.join("client.json"),
            serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

/// Store finished GPU times (`(frame number, ms)`, numbered from 1) by frame index.
fn record_gpu(slots: &mut Vec<Option<f64>>, finished: Vec<(u64, f64)>) {
    for (frame, ms) in finished {
        let i = frame.saturating_sub(1) as usize;
        if slots.len() <= i {
            slots.resize(i + 1, None);
        }
        slots[i] = Some(ms);
    }
}

/// Writes `ui-script.json` and ends the script; `quit` when the run ended through the Quit command.
fn write_script_report(st: &mut State, out: PathBuf, quit: bool) {
    let Some(sc) = st.script.take() else { return };
    let ok = sc.results.iter().all(|r| r["ok"] == Value::Bool(true));
    let report = json!({
        "status": if ok { "ok" } else { "failed" },
        "quit": quit,
        "steps": sc.results,
        "missing_images": st.shell.as_ref().map(|s| s.missing().to_vec()),
        "hud_draw": st.shell.as_ref().map(|s| s.st.hud_stats.report()),
        "open_menus": st.shell.as_ref().map(|s| s.ui.open_menus().join(",")),
        "net": st.net.as_mut().map(NetPlay::report),
        "hud": st.shell.as_ref().map(|s| s.st.game.hud.report()),
        "map": st.map_name,
        "load": st.load_report,
        "objectives": {"plants": listen::objectives().0, "defuses": listen::objectives().1, "airstrikes": listen::objectives().2, "helicopters": listen::objectives().3, "heli_shots": listen::objectives().4},
    });
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(
        out.join("ui-script.json"),
        serde_json::to_vec_pretty(&report).unwrap_or_default(),
    );
}

/// One frame of the `--ui-script`; `true` once every step has run.
fn script_step(st: &mut State) -> bool {
    let Some(sc) = st.script.as_mut() else {
        return true;
    };
    let Some(step) = sc.steps.get(sc.index).cloned() else {
        return true;
    };
    let (key, arg) = step
        .split_once('=')
        .map_or((step.as_str(), ""), |(k, v)| (k, v));
    let waited = sc.began.elapsed().as_secs_f32();
    let done = |ok: bool, sc: &mut UiScript, note: String| {
        sc.results
            .push(json!({"step": step, "ok": ok, "note": note, "secs": waited}));
        sc.index += 1;
        sc.began = Instant::now();
    };
    match key {
        "click" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            let ok = sh.click(&mut st.input, arg);
            let open = sh.ui.open_menus().join(",");
            done(ok, sc, format!("open: {open}"));
        }
        // The same through the pointer, as a player's mouse does it.
        "mouse" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            let ok = sh.mouse_click(&mut st.input, arg);
            let open = sh.ui.open_menus().join(",");
            done(ok, sc, format!("open: {open}"));
        }
        // `see=Controls+Options` fails unless the top menu shows every one of the items; `nosee=` the reverse.
        "see" | "nosee" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            let wrong: Vec<&str> = arg
                .split('+')
                .filter(|w| sh.has_item(&mut st.input, w) != (key == "see"))
                .collect();
            let open = sh.ui.open_menus().join(",");
            done(
                wrong.is_empty(),
                sc,
                format!("{key} wrong: {wrong:?}; open: {open}"),
            );
        }
        "open" | "close" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            if key == "open" {
                sh.open(&mut st.input, arg);
            } else {
                sh.close_by_name(&mut st.input, arg);
            }
            done(true, sc, format!("open: {}", sh.ui.open_menus().join(",")));
        }
        "wait" => {
            if waited >= arg.parse::<f32>().unwrap_or(1.0) {
                done(true, sc, String::new());
            }
        }
        "menu" => {
            let (name, secs) = arg
                .split_once(':')
                .map_or((arg, 60.0), |(n, s)| (n, s.parse().unwrap_or(60.0)));
            let open = st.shell.as_ref().is_some_and(|s| s.ui.is_open(name));
            if open {
                done(true, sc, String::new());
            } else if waited > secs {
                let have = st
                    .shell
                    .as_ref()
                    .map(|s| s.ui.open_menus().join(","))
                    .unwrap_or_default();
                done(false, sc, format!("timed out; open: {have}"));
            }
        }
        "start" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            sh.st.actions.push(Action::StartServer {
                map: arg.to_owned(),
                gametype: "war".to_owned(),
            });
            st.autojoin_next = true;
            done(true, sc, String::new());
        }
        "ingame" => {
            let secs: f32 = arg.parse().unwrap_or(120.0);
            if st.net.as_ref().is_some_and(NetPlay::spawned) {
                done(true, sc, String::new());
            } else if waited > secs {
                done(false, sc, "timed out".into());
            }
        }
        // Waits for something the HUD shows: `killcam`, `dead`, `intermission` (the match is over), `feed` (a message
        // window has a line up), each for at most `=secs` seconds.
        "killcam" | "dead" | "intermission" | "feed" => {
            let live = st.shell.as_ref().map(|s| (&s.st.live, &s.st.feed));
            let reached = live.is_some_and(|(l, f)| match key {
                "killcam" => l.killcam,
                "dead" => l.dead,
                "intermission" => l.intermission,
                _ => (0..crate::hud::WINDOWS).any(|w| f.active(w, l.time)),
            });
            if reached {
                done(true, sc, String::new());
            } else if waited > arg.parse::<f32>().unwrap_or(120.0) {
                done(false, sc, "timed out".into());
            }
        }
        // Waits for the player to die and come back (`respawn=300`: at most that many seconds): once alive again with
        // health and out of the killcam, the low-health overlay must be off (`CG_Respawn` resets it).
        "respawn" => {
            let Some(sh) = st.shell.as_ref() else {
                return true;
            };
            let h = &sh.st.game.hud;
            if sc.marker.is_none() && h.pm_dead {
                sc.marker = Some("dead".into());
            }
            if sc.marker.is_some() && h.live && !h.pm_dead && !sh.st.live.killcam && h.health > 0 {
                let alpha = h.overlay.alpha(h.now);
                done(
                    alpha == 0.0,
                    sc,
                    format!("overlay alpha {alpha} after respawn"),
                );
            } else if waited > arg.parse::<f32>().unwrap_or(300.0) {
                done(false, sc, "timed out".into());
            }
        }
        // `scores=on` holds the scoreboard up as the Tab key would, `scores=off` lets go.
        "scores" => {
            if let Some(sh) = st.shell.as_mut() {
                sh.st.scores_forced = arg != "off";
            }
            done(true, sc, String::new());
        }
        // Escape as the menus get it (`key=escape`).
        "key" => {
            if let Some(sh) = st.shell.as_mut()
                && arg == "escape"
            {
                sh.key(&mut st.input, UiKey::Escape);
            }
            done(arg == "escape", sc, format!("key {arg}"));
        }
        // Holds a key down until the grenade count of the HUD drops (`throw=g:10`: the key, at most that many
        // seconds): the bind, the button and the server's throw as a player's key press goes through them.
        "throw" => {
            let (name, secs) = arg
                .split_once(':')
                .map_or((arg, 10.0), |(k, s)| (k, s.parse().unwrap_or(10.0)));
            let now = st.shell.as_ref().map_or((None, None), |s| {
                let h = &s.st.game.hud;
                (
                    h.frag.as_ref().map(|o| o.ammo),
                    h.second.as_ref().map(|o| o.ammo),
                )
            });
            match sc.marker.clone() {
                None => {
                    sc.marker = Some(format!("{now:?}"));
                    st.input.key(name, true);
                }
                Some(before) if before != format!("{now:?}") => {
                    st.input.key(name, false);
                    done(true, sc, format!("{before} -> {now:?}"));
                    sc.marker = None;
                }
                Some(before) if waited > secs => {
                    st.input.key(name, false);
                    done(false, sc, format!("no throw; counts stayed {before}"));
                    sc.marker = None;
                }
                Some(_) => {}
            }
        }
        // Waits until no menu is open: the game has the keyboard and mouse back (`nomenu=5`).
        "nomenu" => {
            let open = st.shell.as_ref().map(|s| s.ui.open_menus().join(","));
            if open.as_deref().is_none_or(str::is_empty) {
                done(true, sc, String::new());
            } else if waited > arg.parse::<f32>().unwrap_or(10.0) {
                done(false, sc, format!("timed out; open: {open:?}"));
            }
        }
        // Waits until the server says the player is on team `n` (1 axis, 2 allies, 3 spectator): `team=2:20`.
        // `team=save` remembers the team; `team=other:secs` waits for a different one.
        "team" | "weapon" => {
            let (want, secs) = arg
                .split_once(':')
                .map_or((arg, 20.0), |(n, s)| (n, s.parse().unwrap_or(20.0)));
            let saved = sc.marker.clone();
            let have = if key == "team" {
                st.shell
                    .as_ref()
                    .map_or(String::new(), |s| s.st.live.own_team.to_string())
            } else {
                st.net.as_ref().map(NetPlay::weapon).unwrap_or_default()
            };
            if key == "team" && want == "save" {
                sc.marker = Some(have.clone());
                done(true, sc, format!("team {have}"));
            } else if match (key, want) {
                ("team", "other") => saved.is_some_and(|t| t != have && have != "0"),
                ("team", _) => have == want,
                _ => have.contains(want),
            } {
                done(true, sc, format!("{key} {have}"));
            } else if waited > secs {
                done(false, sc, format!("timed out; {key} is {have:?}"));
            }
        }
        // Waits until a counter of the listen server reaches a minimum: `counter=helicopters:1:60` (the name, the
        // minimum and the seconds; names are plants, defuses, airstrikes, helicopters, heli_shots).
        "counter" => {
            let mut parts = arg.split(':');
            let name = parts.next().unwrap_or("");
            let min: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);
            let secs: f32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(30.0);
            let o = listen::objectives();
            let have = match name {
                "plants" => Some(o.0),
                "defuses" => Some(o.1),
                "airstrikes" => Some(o.2),
                "helicopters" => Some(o.3),
                "heli_shots" => Some(o.4),
                _ => None,
            };
            match have {
                None => done(false, sc, format!("unknown counter {name}")),
                Some(n) if n >= min => done(true, sc, format!("{name} {n}")),
                Some(n) if waited > secs => done(false, sc, format!("timed out; {name} is {n}")),
                Some(_) => {}
            }
        }
        "togglemenu" => {
            // Escape in a match: the server's script main menu.
            if let Some(sh) = st.shell.as_mut() {
                let menu = st.input.cvar("g_scriptMainMenu").unwrap_or("").to_owned();
                sh.open(&mut st.input, &menu);
            }
            done(true, sc, String::new());
        }
        "home" => {
            // Back at the front end: no match, the main menu open.
            let secs: f32 = arg.parse().unwrap_or(20.0);
            let home = st.net.is_none()
                && st.renderer.is_none()
                && st.shell.as_ref().is_some_and(|s| s.ui.is_open("main"));
            if home {
                done(true, sc, String::new());
            } else if waited > secs {
                let open = st.shell.as_ref().map(|s| s.ui.open_menus().join(","));
                done(
                    false,
                    sc,
                    format!(
                        "timed out; net {} renderer {} open {open:?}",
                        st.net.is_some(),
                        st.renderer.is_some()
                    ),
                );
            }
        }
        // A cvar's value (`cvaris=ui_netGametypeName dom`).
        "cvaris" => {
            let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
            let have = st.input.cvar(name).unwrap_or("").to_owned();
            done(have == want, sc, format!("{name} is {have:?}"));
        }
        // The rows a list shows (`rows=2 1`: the join menu's server list has one row), for the join menu's checks.
        "rows" => {
            let (feeder, want) = arg.split_once(' ').unwrap_or((arg, "0"));
            let have = st.shell.as_mut().map_or(0, |sh| {
                sh.feeder_rows(&mut st.input, feeder.parse().unwrap_or(0))
            });
            done(
                have.to_string() == want,
                sc,
                format!("feeder {feeder} has {have} rows"),
            );
        }
        // A server added to the favorites (`favorite=127.0.0.1:28999`), as the join menu's New Favorite does.
        "favorite" => {
            let added = serverlist::resolve(arg).map(|a| {
                if let Some(sh) = st.shell.as_mut() {
                    sh.st.servers.add_favorite(a);
                }
            });
            done(added.is_ok(), sc, format!("{added:?}"));
        }
        // A console line, as typed: `set=set scr_war_scorelimit 3`.
        "set" => {
            st.input.exec_line(arg);
            done(true, sc, String::new());
        }
        // A line typed at the console (`console=callvote map_restart`), as Enter sends it.
        "console" => {
            if let Some(sh) = st.shell.as_mut() {
                sh.console_line(&mut st.input, arg);
            }
            done(true, sc, String::new());
        }
        // A chat line typed into the field and sent with Enter, as T or Y and the keys do it
        // (`chat=hello`, `teamchat=hello`).
        "chat" | "teamchat" => {
            if let Some(sh) = st.shell.as_mut() {
                sh.st.console.open_chat(key == "teamchat");
                for c in arg.chars() {
                    sh.console_key(&mut st.input, UiKey::Char(c));
                }
                sh.console_key(&mut st.input, UiKey::Enter);
            }
            done(true, sc, String::new());
        }
        // Waits until a line with this text is in the console's output (`saw=hello:10`, at most that many seconds):
        // a chat line or a print the server sent.
        "saw" => {
            let (text, secs) = arg
                .rsplit_once(':')
                .map_or((arg, 10.0), |(t, s)| (t, s.parse().unwrap_or(10.0)));
            let seen = st
                .shell
                .as_ref()
                .is_some_and(|s| s.st.feed.console.iter().any(|l| l.contains(text)));
            if seen {
                done(true, sc, String::new());
            } else if waited > secs {
                done(false, sc, format!("timed out; no line with {text:?}"));
            }
        }
        // A line for the listen server's console, as typed there: `server=devtele human bombzone`.
        "server" => {
            listen::send(arg);
            done(true, sc, String::new());
        }
        // `vid_restart` as the graphics menu's Apply does it.
        "vidrestart" => {
            if let Some(sh) = st.shell.as_mut() {
                sh.st.actions.push(Action::VidRestart);
            }
            done(true, sc, String::new());
        }
        // What the graphics settings came to (`gfxis=aa 4`, `gfxis=specular 0`, `gfxis=aspect 1.78`,
        // `gfxis=fullscreen 0`, `gfxis=uncapped 1`): the renderer's and the surface's own state.
        "gfxis" => {
            let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
            let have = match name {
                "aa" => st
                    .renderer
                    .as_ref()
                    .map(|r| r.samples(st.config.format).to_string()),
                "specular" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.specular).to_string()),
                "dof" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.dof).to_string()),
                "glow" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.glow).to_string()),
                "shadows" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.shadows != render::ShadowMode::Off).to_string()),
                "aspect" => Some(format!("{:.2}", st.aspect.unwrap_or(0.0))),
                "fullscreen" => Some(u8::from(st.window.fullscreen().is_some()).to_string()),
                // 1 when the surface does not wait for the display, or has no mode that does not.
                "uncapped" => {
                    let caps = st.surface.get_capabilities(&st.gpu.adapter);
                    let free = caps
                        .present_modes
                        .iter()
                        .any(|m| *m != wgpu::PresentMode::Fifo);
                    Some(u8::from(st.present_mode != wgpu::PresentMode::Fifo || !free).to_string())
                }
                _ => None,
            };
            match have {
                Some(h) => done(h == want, sc, format!("{name} is {h}")),
                None => done(false, sc, format!("gfxis: unknown or unavailable {name:?}")),
            }
        }
        // A persistent stat, as the menus' `statset` would write it (`stat=2301 77`), and its check (`statis=2301 77`).
        "stat" | "statis" => {
            let mut it = arg.split_whitespace().map(|v| v.parse::<i32>().ok());
            let (i, v) = match (it.next(), it.next()) {
                (Some(Some(i)), Some(Some(v))) => (i, v),
                _ => (-1, 0),
            };
            let slot = st.shell.as_mut().and_then(|sh| {
                sh.st
                    .stats
                    .get_mut(usize::try_from(i).unwrap_or(usize::MAX))
            });
            match (key, slot) {
                ("stat", Some(s)) => {
                    *s = v;
                    done(true, sc, String::new());
                }
                ("statis", Some(s)) => {
                    let ok = *s == v;
                    done(ok, sc, format!("stat {i} is {s}"));
                }
                _ => done(
                    false,
                    sc,
                    format!("usage: {key}=<index> <value> (got {arg:?})"),
                ),
            }
        }
        // Joins a server like the menus' `connect` (`connect=127.0.0.1:28960`).
        "connect" => {
            if let Some(sh) = st.shell.as_mut() {
                sh.st.actions.push(Action::Join(arg.to_owned()));
            }
            done(true, sc, String::new());
        }
        // Waits until the client is in map `name` with a connection (`map=mp_crash:60`).
        "map" => {
            let (name, secs) = arg
                .split_once(':')
                .map_or((arg, 60.0), |(n, s)| (n, s.parse().unwrap_or(60.0)));
            if st.net.is_some() && st.map_name.eq_ignore_ascii_case(name) {
                done(true, sc, String::new());
            } else if waited > secs {
                done(false, sc, format!("timed out; map is {:?}", st.map_name));
            }
        }
        // Waits until the server has rotated to a different map than the one the step began in (`maprotate=300`).
        "maprotate" => {
            let from = sc.marker.get_or_insert_with(|| st.map_name.clone()).clone();
            let secs: f32 = arg.parse().unwrap_or(300.0);
            if st.net.is_some() && !st.map_name.is_empty() && st.map_name != from {
                sc.marker = None;
                done(true, sc, format!("{from} -> {}", st.map_name));
            } else if waited > secs {
                sc.marker = None;
                done(false, sc, format!("timed out; still in {from}"));
            }
        }
        "shot" => {
            if st.shot_request.is_none() {
                st.shot_request = Some(arg.to_owned());
            } else if waited > 5.0 {
                done(false, sc, "no screenshot".into());
            }
        }
        other => done(false, sc, format!("unknown step {other}")),
    }
    sc.index >= sc.steps.len()
}

/// One frame of the tour; `true` when every menu has been shown.
fn tour_step(
    shell: Option<&mut Shell>,
    input: &mut Input,
    t: &mut UiTour,
    gpu: &Gpu,
    tex: &wgpu::Texture,
    format: wgpu::TextureFormat,
    dir: &Path,
) -> bool {
    let Some(sh) = shell else { return true };
    if t.frames == 0 {
        sh.close_all(input);
        match t.menus.get(t.index) {
            Some(m) => sh.open(input, m),
            None => return true,
        }
    }
    t.frames += 1;
    if t.frames < TOUR_FRAMES {
        return false;
    }
    let name = t.menus[t.index];
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("{name}.png"));
    let lit = match save_png(gpu, tex, format, &path) {
        Ok(f) => f,
        Err(e) => {
            t.results.push(json!({"menu": name, "error": e}));
            -1.0
        }
    };
    if lit >= 0.0 {
        t.results.push(json!({"menu": name, "open": sh.ui.is_open(name), "lit_fraction": lit, "screenshot": format!("{name}.png")}));
    }
    t.index += 1;
    t.frames = 0;
    t.index >= t.menus.len()
}

/// What a key press types or does in a text field: a named key, else the characters it produced.
fn typed_keys(event: &winit::event::KeyEvent) -> Vec<UiKey> {
    use winit::keyboard::{Key, NamedKey};
    let named = match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) => Some(UiKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(UiKey::Down),
        Key::Named(NamedKey::ArrowLeft) => Some(UiKey::Left),
        Key::Named(NamedKey::ArrowRight) => Some(UiKey::Right),
        Key::Named(NamedKey::Enter) => Some(UiKey::Enter),
        Key::Named(NamedKey::Escape) => Some(UiKey::Escape),
        Key::Named(NamedKey::Tab) => Some(UiKey::Tab),
        Key::Named(NamedKey::Backspace) => Some(UiKey::Backspace),
        Key::Named(NamedKey::Delete) => Some(UiKey::Delete),
        Key::Named(NamedKey::Home) => Some(UiKey::Home),
        Key::Named(NamedKey::End) => Some(UiKey::End),
        Key::Named(NamedKey::PageUp) => Some(UiKey::PageUp),
        Key::Named(NamedKey::PageDown) => Some(UiKey::PageDown),
        _ => None,
    };
    match (named, &event.text) {
        (Some(k), _) => vec![k],
        (None, Some(t)) => t.chars().map(UiKey::Char).collect(),
        (None, None) => Vec::new(),
    }
}

/// `chatmodepublic` and `chatmodeteam` (bound to T and Y) open the chat field; `toggleconsole` is the console's.
fn open_chat(shell: Option<&mut Shell>, input: &mut Input, f: &InputFrame) {
    let Some(sh) = shell else { return };
    for c in &f.pending_commands {
        match c.as_str() {
            "chatmodepublic" => sh.st.console.open_chat(false),
            "chatmodeteam" => sh.st.console.open_chat(true),
            "toggleconsole" => sh.st.console.toggle(),
            _ => continue,
        }
        input.release_all();
    }
}

/// A console line a menu, a bind or the console itself produced. What the server carries out goes to it (without the
/// shell's command loop, which the line may have come from); the rest is the input layer's.
/// The `rate` (bytes a second) and `snaps` (snapshots a second) the player's cvars ask the server for.
fn netplay_rates(input: &Input) -> (i32, i32) {
    let get = |n: &str, d: i32| {
        input
            .cvar(n)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(d)
    };
    (get("rate", 25_000), get("snaps", 30))
}

fn console_action(st: &mut State, line: &str) {
    for cmd in crate::input::config::split_commands(line) {
        if cmd[0].eq_ignore_ascii_case("rcon") {
            // `rcon <command>` with the password in `rconpassword`, answered as console output.
            let password = st.input.cvar("rconpassword").unwrap_or("").to_owned();
            if let Some(net) = st.net.as_mut() {
                net.send_rcon(&password, &cmd[1..].join(" "));
            } else if let Some(sh) = st.shell.as_mut() {
                sh.print_console("rcon: not connected to a server");
            }
        } else if cmd[0].eq_ignore_ascii_case("name") {
            // `name <text>`: the server reads the new name from the userinfo, with the rate and snapshot rate.
            let Some(new) = cmd.get(1) else {
                if let Some(sh) = st.shell.as_mut() {
                    let n = st.input.cvar("name").unwrap_or("").to_owned();
                    sh.print_console(&format!("\"name\" is \"{n}\""));
                }
                continue;
            };
            st.input.exec_line(&format!("set name \"{new}\""));
            let (rate, snaps) = netplay_rates(&st.input);
            if let Some(net) = st.net.as_mut() {
                net.send_command(&net::client::userinfo_command(new, rate, snaps));
            }
        } else if !crate::console::is_server_verb(&cmd[0]) {
            st.input.exec_line(&crate::input::config::join(&cmd));
        } else if let Some(net) = st.net.as_mut() {
            if let Some(wire) = crate::console::server_line(&cmd) {
                net.send_command(&wire);
            }
        } else if let Some(sh) = st.shell.as_mut() {
            sh.print_console(&format!("{}: not connected to a server", cmd[0]));
        }
    }
}

/// Routes a window event to the menus (a menu is open and owns the keyboard and mouse).
fn ui_event(st: &mut State, ev: &WindowEvent) {
    let Some(sh) = st.shell.as_mut() else { return };
    // A bind item is waiting: the next key or button is the binding (Escape cancels, via the menus).
    if sh.ui.bind_pending() {
        let name = match ev {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => Some(crate::input::mouse_key_name(*button)),
            WindowEvent::MouseWheel { delta, .. } => {
                let y = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                (y != 0.0).then(|| if y > 0.0 { "mwheelup" } else { "mwheeldown" }.to_owned())
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                match event.physical_key {
                    winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape) => None,
                    winit::keyboard::PhysicalKey::Code(c) => crate::input::physical_key_name(c),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(n) = name {
            sh.bind_key(&mut st.input, &n);
            return;
        }
    }
    match ev {
        WindowEvent::CursorMoved { position, .. } => {
            sh.mouse_move(&mut st.input, position.x as f32, position.y as f32);
        }
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button,
            ..
        } => {
            let key = match button {
                MouseButton::Left => UiKey::Mouse1,
                MouseButton::Right => UiKey::Mouse2,
                _ => return,
            };
            sh.key(&mut st.input, key);
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let y = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            if y != 0.0 {
                sh.key(
                    &mut st.input,
                    if y > 0.0 {
                        UiKey::WheelUp
                    } else {
                        UiKey::WheelDown
                    },
                );
            }
        }
        WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
            for k in typed_keys(event) {
                sh.key(&mut st.input, k);
            }
        }
        _ => {}
    }
}

/// Configures the surface again. wgpu refuses while the queue still has work to finish (a timestamp read-back that has
/// not come back is enough: the call panics), so the renderer's are waited for first.
fn reconfigure(st: &mut State) {
    if let Some(r) = st.renderer.as_mut() {
        record_gpu(&mut st.gpu_ms, r.flush_gpu_times());
    }
    st.surface.configure(&st.gpu.device, &st.config);
}

/// `vid_restart`: the window, the present mode and the aspect take the graphics settings now; a running match's
/// renderer takes the ones it can change live (shadows, specular, depth of field, glow, antialiasing), and the texture
/// size and filtering wait for the next map, which loads its textures again.
fn vid_restart(cli: &Cli, st: &mut State) {
    let g = Gfx::from_cvars(&st.input.cvars);
    if let Some(note) = display::apply(&st.window, &g.request(&cli.request)) {
        st.notes.push(note);
    }
    let caps = st.surface.get_capabilities(&st.gpu.adapter);
    let (mode, _) = pick_present(g.present(&cli.present), &caps.present_modes);
    let mut reconfigure_surface = mode != st.config.present_mode;
    st.config.present_mode = mode;
    st.present_mode = mode;
    if st.aspect != g.aspect {
        st.aspect = g.aspect;
        reconfigure_surface = true;
    }
    let (w, h) = (st.config.width, st.config.height);
    if let Some(r) = st.renderer.as_mut() {
        let before = r.samples(st.config.format);
        r.settings = g.settings(cli.settings);
        #[cfg(not(target_arch = "wasm32"))]
        if r.samples(st.config.format) != before && st.loading.is_none() {
            r.warm(st.config.format);
        }
    }
    if reconfigure_surface {
        // Applied by the next frame once no load is running (see `apply_resize`).
        st.pending_resize = Some((w, h));
    }
}

/// Follows a window resize: the surface, the field of view and the menus.
///
/// Never while a map loads: reconfiguring a surface fails (a panic) when another thread submits to the queue at the
/// same moment, and the loader does. The size is kept in `pending_resize` until the load is done.
/// `cg_fov` in degrees at 4:3, within the original's 1 to 160.
fn cg_fov(input: &Input) -> f32 {
    input.cvars.f32("cg_fov").clamp(1.0, 160.0)
}

/// The world's field of view in degrees at 4:3 (`CG_GetViewFov`).
fn view_fov_4_3(st: &State) -> f32 {
    let cv = &st.input.cvars;
    view_fov(
        cg_fov(&st.input),
        st.fixed_fov,
        st.aim_zoom,
        cv.f32("cg_fovscale"),
        cv.f32("cg_fovmin"),
    )
}

fn apply_resize(st: &mut State, (w, h): (u32, u32)) {
    st.config.width = w;
    st.config.height = h;
    reconfigure(st);
    st.fov_x = hor_plus(cg_fov(&st.input), st.aspect.unwrap_or(w as f32 / h as f32));
    if let Some(sh) = st.shell.as_mut() {
        sh.resize(w, h);
    }
}

/// Starts (`join` of `None`) or joins a match from the menus: begins loading the map in the background. The window
/// keeps drawing the loading screen; [`finish_load`] connects once the load is done.
/// For a join, `map` is what the server said it is playing.
fn start_session(
    cli: &Cli,
    map_slot: &mut Option<MapData>,
    st: &mut State,
    map: &str,
    gametype: &str,
    join: Option<std::net::SocketAddr>,
) -> Result<(), String> {
    end_session(map_slot, st);
    let server = match join {
        Some(a) => loader::Server::Join(a),
        None => loader::Server::Boot(listen_config(st, map, gametype, cli.bots)),
    };
    begin_load(cli, st, map, server);
    if let Some(sh) = st.shell.as_mut() {
        sh.close_all(&mut st.input);
    }
    Ok(())
}

/// Starts the background load of `map` and the measuring of its frame gaps.
fn begin_load(cli: &Cli, st: &mut State, map: &str, server: loader::Server) {
    let gfx = Gfx::from_cvars(&st.input.cvars);
    let req = loader::Request {
        install: cli.install.clone(),
        map: map.to_owned(),
        server,
        settings: gfx.settings(cli.settings),
        picmip: gfx.picmip,
        aniso_max: gfx.aniso_max,
        format: st.config.format,
    };
    st.loading = Some(Load::start(req, st.gpu.clone()));
    st.load_gaps = Some(LoadGaps {
        map: map.to_owned(),
        started: Instant::now(),
        frames: 0,
        max_ms: 0.0,
        load_ms: None,
        slow: Vec::new(),
    });
}

/// The background load is done: a started session connects to its server and shows the world; a level change hands the
/// new world to the running session.
fn finish_load(
    cli: &Cli,
    map_slot: &mut Option<MapData>,
    st: &mut State,
    load: &Load,
    loaded: loader::Loaded,
) -> Result<(), String> {
    let loader::Loaded {
        data,
        renderer,
        library,
        server,
        ms,
    } = loaded;
    let map = &load.map;
    let sound = crate::sound::ClientSound::start(&cli.install, map, !cli.no_sound);
    if let Some(g) = st.load_gaps.as_mut() {
        g.load_ms = Some(ms);
    }
    if let Some(sh) = st.shell.as_mut() {
        sh.ui.assets.add_weapon_icons(&library.content.weapons());
    }
    if load.level_change {
        let net = st.net.as_mut().ok_or("the session ended during the load")?;
        net.new_level(Some((library, &data, sound)))?;
        if let Some(sh) = st.shell.as_ref() {
            net.upload_stats(&sh.st.stats);
        }
    } else {
        let (addr, listen) = server.ok_or("no server to connect to")?;
        let limits = st.input.pitch_limits();
        let mut net = NetPlay::connect(library, &data, addr, &cli.name, limits, false, sound)?;
        net.set_autojoin(std::mem::take(&mut st.autojoin_next));
        let (rate, snaps) = netplay_rates(&st.input);
        net.set_userinfo(&cli.name, rate, snaps);
        if let Some(sh) = st.shell.as_ref() {
            net.set_profile(&sh.st.stats);
        }
        st.net = Some(net);
        st.listen = listen;
        if let Some(sh) = st.shell.as_mut() {
            sh.st.in_game = true;
            sh.close_all(&mut st.input);
        }
    }
    st.renderer = Some(renderer);
    *map_slot = Some(data);
    st.map_name = map.to_owned();
    // The menus' map title and minimap look the map up by this (the original sets it from the server's info).
    st.input.cvars.set("mapname", map, false);
    Ok(())
}

/// The menu-started server: the chosen map then the other stock maps in turn, the dvars the player set (time and score
/// limits), on a standard port when one is free so the LAN list finds it.
fn listen_config(st: &State, map: &str, gametype: &str, bots: usize) -> listen::Config {
    let maps: Vec<String> = st
        .shell
        .as_ref()
        .map(|s| s.st.maps.iter().map(|m| m.name.clone()).collect())
        .unwrap_or_default();
    listen::Config {
        map: map.to_owned(),
        bots,
        gametype: Some(gametype.to_owned()),
        rotation: Some(rotation(gametype, &maps, map)),
        port: listen::free_standard_port(),
        dvars: st.input.cvars.with_prefix("scr_"),
    }
}

/// The server announced level `name`: if it is another map, begins loading it in the background (the old world and
/// renderer go first; [`finish_load`] hands over the new one) and goes back to the menus the server opens.
fn enter_level(
    cli: &Cli,
    map_slot: &mut Option<MapData>,
    st: &mut State,
    name: &str,
) -> Result<(), String> {
    match level_change(&st.map_name, name) {
        LevelChange::Same => {
            let Some(net) = st.net.as_mut() else {
                return Ok(());
            };
            net.new_level(None)?;
            if let Some(sh) = st.shell.as_ref() {
                net.upload_stats(&sh.st.stats);
            }
        }
        LevelChange::Load => {
            st.renderer = None;
            *map_slot = None;
            begin_load(cli, st, name, loader::Server::Keep);
        }
    }
    Ok(())
}

/// The error screen: `message` over the menu.
fn show_error(st: &mut State, message: &str) {
    st.input.cvars.set("com_errorMessage", message, false);
    if let Some(sh) = st.shell.as_mut() {
        sh.open(&mut st.input, "error_popmenu");
    }
}

/// The server kicked us or went silent: back to the main menu with the reason up (`reconnect` rejoins).
fn connection_lost(map_slot: &mut Option<MapData>, st: &mut State, why: &str) {
    eprintln!("connection lost: {why}");
    end_session(map_slot, st);
    show_error(st, why);
}

/// Back to the main menu: drops the connection, the server and the world.
fn end_session(map_slot: &mut Option<MapData>, st: &mut State) {
    if let Some(n) = st.net.as_mut() {
        n.disconnect();
    }
    st.net = None;
    st.input.set_fov_sensitivity_scale(1.0);
    st.roll = 0.0;
    st.loading = None;
    st.load_gaps = None;
    if let Some(l) = st.listen.as_mut() {
        l.finish();
    }
    st.listen = None;
    st.renderer = None;
    *map_slot = None;
    st.map_name.clear();
    release_pointer(st);
    if let Some(sh) = st.shell.as_mut() {
        sh.st.in_game = false;
        sh.st.game.hud.live = false;
        sh.close_all(&mut st.input);
        sh.open(&mut st.input, "main");
    }
}

/// Lock the cursor to the window (a click). `captured` follows what the OS actually granted.
fn grab_pointer(st: &mut State) {
    st.grab_at = Instant::now();
    st.lock_seen = false;
    st.grabbed = st
        .window
        .set_cursor_grab(CursorGrabMode::Locked)
        .or_else(|_| st.window.set_cursor_grab(CursorGrabMode::Confined))
        .is_ok();
    sync_os_cursor(st);
    st.input.set_captured(st.grabbed);
}

/// Hides the system cursor while the game draws its own (a menu with a pointer open) or has the pointer locked, shows
/// it otherwise. The original does the same; two cursors on screen, offset from each other, was the alternative.
fn sync_os_cursor(st: &mut State) {
    let drawn = st
        .shell
        .as_ref()
        .is_some_and(|s| s.ui.cursor_visible && s.ui.captures_input());
    let hide = st.grabbed || drawn;
    if hide != st.os_cursor_hidden {
        st.window.set_cursor_visible(!hide);
        st.os_cursor_hidden = hide;
    }
}

/// The fullscreen state after a toggle: windowed becomes borderless on the current monitor and back.
fn next_fullscreen(current: Option<Fullscreen>) -> Option<Fullscreen> {
    current.is_none().then_some(Fullscreen::Borderless(None))
}

/// `togglefullscreen` and F11. In the browser this is the Fullscreen API, which only a user gesture may start, so
/// it runs from the key event's own handler; the canvas follows with a `Resized`.
fn toggle_fullscreen(window: &Window) {
    window.set_fullscreen(next_fullscreen(window.fullscreen()));
}

/// Give the cursor back: Escape (`togglemenu`), the menu, or the window losing focus (winit leaves a macOS lock on
/// across focus changes, which freezes the cursor over other apps). A click takes it again. Never quits.
fn release_pointer(st: &mut State) {
    if st.grabbed {
        let _ = st.window.set_cursor_grab(CursorGrabMode::None);
        st.grabbed = false;
    }
    sync_os_cursor(st);
    st.input.set_captured(false);
}

/// Free-fly camera driven by the player input. Fly-cam pitch is positive up; input pitch is positive down.
fn fly(st: &mut State, f: &InputFrame, dt: f32) {
    let speed = if f.buttons & buttons::SPRINT != 0 {
        1800.0
    } else {
        450.0
    } * dt;
    let fwd = Vec3::new(st.yaw.cos(), st.yaw.sin(), 0.0);
    let left = Vec3::new(-st.yaw.sin(), st.yaw.cos(), 0.0);
    st.pos += (fwd * f.move_forward - left * f.move_right) * speed;
    st.pos.z += f.up * speed;
    st.yaw += f.look_delta_yaw.to_radians();
    st.pitch = (st.pitch - f.look_delta_pitch.to_radians()).clamp(-1.5, 1.5);
}

/// Memory the process holds, bytes: on the web the wasm linear memory (it only grows).
#[cfg(target_arch = "wasm32")]
fn rss() -> u64 {
    (core::arch::wasm32::memory_size::<0>() * 65536) as u64
}

#[cfg(not(target_arch = "wasm32"))]
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
) -> Result<f64, String> {
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
    let mut lit = 0u64;
    for y in 0..h {
        let row = &data[(y * bpr) as usize..(y * bpr + w * 4) as usize];
        for p in row.as_chunks::<4>().0 {
            lit += u64::from(u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) > 24);
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
        .map_err(|e| e.to_string())?;
    Ok(lit as f64 / f64::from(w * h))
}

#[cfg(test)]
mod fullscreen_tests {
    use super::*;

    #[test]
    fn the_toggle_alternates_windowed_and_borderless() {
        let on = next_fullscreen(None);
        assert!(matches!(on, Some(Fullscreen::Borderless(None))));
        assert!(next_fullscreen(on).is_none());
    }
}
