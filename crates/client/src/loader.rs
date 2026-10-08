// SPDX-License-Identifier: GPL-3.0-or-later
//! Loading a map for a session in the background, so the window keeps presenting (and shows the loading screen)
//! instead of freezing for seconds.
//!
//! One worker thread decodes the map zone, uploads the world and builds the renderer with all its pipelines (spread
//! over every core), while sibling threads decode the client's content library and boot the listen server. The
//! window thread polls [`Load::poll`] once a frame.

use crate::listen::{self, Listen};
use crate::models::{Library, Team};
use assets::vfs::Vfs;
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, Settings, TextureCache};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

/// Where the session's server comes from.
pub enum Server {
    /// Boot a listen server of this process.
    Boot(listen::Config),
    /// Connect to this one.
    Join(SocketAddr),
    /// The connection stays: a level change on the server being played on.
    Keep,
}

/// What to load.
pub struct Request {
    pub install: PathBuf,
    pub map: String,
    pub server: Server,
    pub settings: Settings,
    pub format: wgpu::TextureFormat,
    /// The window's size in pixels, for the warm-up frame.
    pub size: (u32, u32),
}

/// Everything a session needs, built.
pub struct Loaded {
    pub data: MapData,
    pub renderer: Renderer,
    pub library: Library,
    /// The server to connect to and the listen server this load booted, unless [`Server::Keep`].
    pub server: Option<(SocketAddr, Option<Listen>)>,
    /// Milliseconds the load took.
    pub ms: f64,
}

/// A load in progress.
pub struct Load {
    pub map: String,
    /// A level change of a running session ([`Server::Keep`]), not the start of one.
    pub level_change: bool,
    rx: mpsc::Receiver<Result<Loaded, String>>,
    state: Arc<Progress>,
    cancel: Arc<AtomicBool>,
}

/// Progress as a fraction in millionths, and what is being done.
#[derive(Default)]
struct Progress {
    millionths: AtomicU32,
    step: AtomicU32,
}

const STEPS: [&str; 4] = [
    "Decoding the map",
    "Building the world",
    "Compiling shaders",
    "Starting the match",
];

impl Progress {
    fn set(&self, step: usize, fraction: f32) {
        self.step.store(step as u32, Ordering::Relaxed);
        self.millionths
            .store((fraction.clamp(0.0, 1.0) * 1e6) as u32, Ordering::Relaxed);
    }
}

impl Load {
    pub fn start(req: Request, gpu: Arc<Gpu>) -> Load {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let map = req.map.clone();
        let level_change = matches!(req.server, Server::Keep);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (st, c) = (state.clone(), cancel.clone());
            let failed = tx.clone();
            let spawned = std::thread::Builder::new()
                .name("map-load".into())
                .spawn(move || {
                    let _ = tx.send(run(&req, &gpu, &st, &c));
                });
            if let Err(e) = spawned {
                let _ = failed.send(Err(e.to_string()));
            }
        }
        // The browser has no threads: the load runs here, as it always did, and is ready at the first poll.
        #[cfg(target_arch = "wasm32")]
        let _ = tx.send(run(&req, &gpu, &state, &cancel));
        Load {
            map,
            level_change,
            rx,
            state,
            cancel,
        }
    }

    /// The result, once the load has finished.
    pub fn poll(&mut self) -> Option<Result<Loaded, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(e) => Some(Err(e.to_string())),
        }
    }

    pub fn progress(&self) -> f32 {
        self.state.millionths.load(Ordering::Relaxed) as f32 * 1e-6
    }

    /// What the loader is doing.
    pub fn note(&self) -> &'static str {
        STEPS[(self.state.step.load(Ordering::Relaxed) as usize).min(STEPS.len() - 1)]
    }
}

impl Drop for Load {
    /// An abandoned load stops at its next step and drops what it built.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn run(
    req: &Request,
    gpu: &Arc<Gpu>,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<Loaded, String> {
    let started = Instant::now();
    let check = || {
        if cancel.load(Ordering::Relaxed) {
            Err("cancelled".to_owned())
        } else {
            Ok(())
        }
    };
    let map = &req.map;
    let booting = match &req.server {
        Server::Boot(cfg) => Some(listen::begin(&req.install, cfg.clone())?),
        _ => None,
    };
    progress.set(0, 0.0);
    // The map zone and the client's content decode side by side.
    #[cfg(not(target_arch = "wasm32"))]
    let (data, library) = std::thread::scope(|s| {
        let lib = s.spawn(|| Library::load(&req.install, map));
        let data = MapData::load(&req.install, map).map_err(|e| format!("cannot load {map}: {e}"));
        (
            data,
            lib.join()
                .unwrap_or_else(|_| Err("content decode panicked".into())),
        )
    });
    #[cfg(target_arch = "wasm32")]
    let (data, library) = (
        MapData::load(&req.install, map).map_err(|e| format!("cannot load {map}: {e}")),
        Library::load(&req.install, map),
    );
    let (data, library) = (data?, library?);
    #[cfg(not(target_arch = "wasm32"))]
    let mut library = library;
    check()?;
    progress.set(1, 0.15);
    let vfs =
        Vfs::open_stock(&req.install, 0).map_err(|e| format!("cannot open the install: {e}"))?;
    let scene = Scene::new(gpu, &data);
    let mut renderer = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
    renderer.settings = req.settings;
    check()?;
    progress.set(2, 0.25);
    // The browser builds its pipelines a few milliseconds per frame instead, see the client's `WARM_BUDGET`.
    #[cfg(not(target_arch = "wasm32"))]
    renderer.warm_progress(req.format, &|done, total| {
        progress.set(2, 0.25 + 0.65 * done as f32 / total.max(1) as f32);
    });
    check()?;
    progress.set(3, 0.88);
    // Not in the browser, where it would stall the page for the frame it takes and its pipelines come over frames.
    #[cfg(not(target_arch = "wasm32"))]
    draw_once(gpu, &mut renderer, &mut library, &data, req);
    check()?;
    progress.set(3, 0.9);
    let server = match (&req.server, booting) {
        (Server::Join(addr), _) => Some((*addr, None)),
        (_, Some(b)) => {
            let l = b.wait()?;
            Some((l.addr, Some(l)))
        }
        _ => None,
    };
    progress.set(3, 1.0);
    Ok(Loaded {
        data,
        renderer,
        library,
        server,
        ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

/// Renders one frame from a spawn point into an offscreen target: the first draw of a renderer builds its depth and
/// shadow targets, bind groups and post-process state, and the first sight of a player uploads its meshes: all of
/// it would otherwise stall the first frames the player sees.
fn draw_once(
    gpu: &Gpu,
    renderer: &mut Renderer,
    library: &mut Library,
    data: &MapData,
    req: &Request,
) {
    let eye = data
        .spawn_points()
        .first()
        .map_or(Vec3::ZERO, |p| Vec3::from(*p) + Vec3::Z * 60.0);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("warm-up frame"),
        size: wgpu::Extent3d {
            width: req.size.0.max(1),
            height: req.size.1.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: req.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    // Both teams' players, so their meshes and materials are uploaded too.
    for (i, team) in [Team::Allies, Team::Axis].into_iter().enumerate() {
        let player = library
            .team_models(team)
            .and_then(|set| library.player(&set).ok());
        if let Some(mut p) = player {
            p.update(0.0, &Default::default());
            let at = [eye.x + 80.0 + 40.0 * i as f32, eye.y, eye.z - 60.0];
            renderer.dynamic_models.extend(p.instances(at));
        }
    }
    let view = render::View {
        origin: eye,
        yaw: 0.0,
        pitch: 0.0,
        fov_x: 1.9,
        time: 0.0,
    };
    renderer.render(
        &view,
        &target.create_view(&Default::default()),
        req.format,
        req.size,
    );
    // The frame's timestamp read-back would still be pending: a surface cannot be reconfigured (a window resize)
    // while the queue has work in flight, so wait for it here, off the window thread.
    renderer.flush_gpu_times();
}
