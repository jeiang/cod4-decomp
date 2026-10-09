// SPDX-License-Identifier: GPL-3.0-only
//! Loading a map for a session in the background, so the window keeps presenting (and shows the loading screen)
//! instead of freezing for seconds.
//!
//! One worker thread decodes the map zone, uploads the world and builds the renderer with all its pipelines (spread
//! over every core), while sibling threads decode the client's content library and boot the listen server. The
//! window thread polls [`Load::poll`] once a frame.

use crate::listen::{self, Listen};
use crate::models::{Library, Team};
use assets::vfs::Vfs;
#[cfg(not(target_arch = "wasm32"))]
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, Settings, TextureCache};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
use std::time::Instant;

/// Where the session's server comes from.
pub enum Server {
    /// Boot a listen server of this process.
    Boot(listen::Config),
    /// Connect to this one.
    Join(SocketAddr),
    /// The connection stays: a level change on the server being played on.
    Keep,
    /// Play a recorded demo: there is no server, and the address handed back is only a stand-in the net layer names the
    /// peer by.
    #[cfg(not(target_arch = "wasm32"))]
    Demo,
}

/// What to load.
pub struct Request {
    pub install: PathBuf,
    pub map: String,
    pub server: Server,
    pub settings: Settings,
    /// Texture sizes dropped (`r_picmip`) and the most anisotropic filtering (`r_texFilterAnisoMax`).
    pub picmip: usize,
    pub aniso_max: u16,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the browser warms from its frames
    pub format: wgpu::TextureFormat,
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
    /// A join of someone else's match, not one this client hosts.
    pub joining: bool,
    #[cfg(not(target_arch = "wasm32"))]
    rx: mpsc::Receiver<Result<Loaded, String>>,
    state: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    #[cfg(target_arch = "wasm32")]
    steps: Steps,
}

/// Where the browser's load is: each [`Load::poll`] does the next piece.
#[cfg(target_arch = "wasm32")]
enum Steps {
    Start(Arc<Request>, Arc<Gpu>),
    Decoded(Box<Work>),
    Built(Box<Work>),
    Finished,
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
        #[cfg(not(target_arch = "wasm32"))]
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let map = req.map.clone();
        let level_change = matches!(req.server, Server::Keep);
        let joining = matches!(req.server, Server::Join(_));
        let req = Arc::new(req);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (st, c) = (state.clone(), cancel.clone());
            let failed = tx.clone();
            let spawned = std::thread::Builder::new()
                .name("map-load".into())
                .spawn(move || {
                    let _ = tx.send(run(req, gpu, &st, &c));
                });
            if let Err(e) = spawned {
                let _ = failed.send(Err(e.to_string()));
            }
        }
        // The browser has no threads: the pieces run one per poll, the page repainting in between.
        Load {
            map,
            level_change,
            joining,
            #[cfg(not(target_arch = "wasm32"))]
            rx,
            state,
            cancel,
            #[cfg(target_arch = "wasm32")]
            steps: Steps::Start(req, gpu),
        }
    }

    /// The result, once the load has finished.
    pub fn poll(&mut self) -> Option<Result<Loaded, String>> {
        #[cfg(target_arch = "wasm32")]
        return self.step();
        #[cfg(not(target_arch = "wasm32"))]
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(e) => Some(Err(e.to_string())),
        }
    }

    /// Does the next piece of the browser's load.
    #[cfg(target_arch = "wasm32")]
    fn step(&mut self) -> Option<Result<Loaded, String>> {
        let state = &*self.state;
        let next = std::mem::replace(&mut self.steps, Steps::Finished);
        let r = match next {
            Steps::Start(req, gpu) => Work::new(req, gpu).and_then(|mut w| {
                w.decode(state)?;
                self.steps = Steps::Decoded(Box::new(w));
                Ok(None)
            }),
            Steps::Decoded(mut w) => w.build_world(state).map(|()| {
                self.steps = Steps::Built(w);
                None
            }),
            Steps::Built(mut w) => w.add_models().and_then(|()| w.finish(state)).map(Some),
            Steps::Finished => return None,
        };
        r.transpose()
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

/// A load in pieces: native runs them in a row on the loader thread, the browser one per [`Load::poll`] so the page
/// repaints (the loading screen) between them.
struct Work {
    req: Arc<Request>,
    gpu: Arc<Gpu>,
    started: Instant,
    booting: Option<listen::Booting>,
    data: Option<MapData>,
    library: Option<Library>,
    renderer: Option<Renderer>,
}

impl Work {
    fn new(req: Arc<Request>, gpu: Arc<Gpu>) -> Result<Self, String> {
        let booting = match &req.server {
            Server::Boot(cfg) => Some(listen::begin(&req.install, cfg.clone())?),
            _ => None,
        };
        Ok(Work {
            req,
            gpu,
            started: Instant::now(),
            booting,
            data: None,
            library: None,
            renderer: None,
        })
    }

    /// The map zone and the client's content decode (side by side, where there are threads).
    fn decode(&mut self, progress: &Progress) -> Result<(), String> {
        let (req, map) = (&*self.req, &self.req.map);
        progress.set(0, 0.0);
        #[cfg(not(target_arch = "wasm32"))]
        let (data, library) = std::thread::scope(|s| {
            let lib = s.spawn(|| Library::load(&req.install, map));
            let data =
                MapData::load(&req.install, map).map_err(|e| format!("cannot load {map}: {e}"));
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
        self.data = Some(data?);
        self.library = Some(library?);
        progress.set(1, 0.15);
        Ok(())
    }

    /// The scene (uploads) and the renderer.
    fn build_world(&mut self, progress: &Progress) -> Result<(), String> {
        let data = self.data.as_ref().ok_or("not decoded")?;
        let vfs = Vfs::open_stock(&self.req.install, 0)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        let scene = Scene::new(&self.gpu, data);
        let mut renderer = Renderer::new(
            self.gpu.clone(),
            scene,
            data,
            TextureCache::new(Some(vfs), self.req.picmip),
        );
        renderer.materials.aniso_max = self.req.aniso_max;
        renderer.settings = self.req.settings;
        self.renderer = Some(renderer);
        progress.set(2, 0.25);
        Ok(())
    }

    fn add_models(&mut self) -> Result<(), String> {
        let library = self.library.as_ref().ok_or("not decoded")?;
        let renderer = self.renderer.as_mut().ok_or("not built")?;
        add_match_models(renderer, library);
        Ok(())
    }

    /// Waits for the listen server and hands everything over.
    fn finish(self, progress: &Progress) -> Result<Loaded, String> {
        progress.set(3, 0.9);
        let server = match (&self.req.server, self.booting) {
            (Server::Join(addr), _) => Some((*addr, None)),
            #[cfg(not(target_arch = "wasm32"))]
            (Server::Demo, _) => Some((std::net::SocketAddr::from(([127, 0, 0, 1], 9)), None)),
            (_, Some(b)) => {
                let l = b.wait()?;
                Some((l.addr, Some(l)))
            }
            _ => None,
        };
        progress.set(3, 1.0);
        Ok(Loaded {
            data: self.data.ok_or("not decoded")?,
            renderer: self.renderer.ok_or("not built")?,
            library: self.library.ok_or("not decoded")?,
            server,
            ms: self.started.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

/// What a match draws that the map does not hold: every weapon's gun, hands and world model and the players get
/// their pipelines too.
pub fn add_match_models(renderer: &mut Renderer, library: &Library) {
    let mut models = Vec::new();
    for w in library.content.weapons() {
        models.extend(w.gun_models.iter().flatten().cloned());
        models.extend(w.world_models.iter().flatten().cloned());
        models.extend(w.hand_model.clone());
    }
    for team in [Team::Allies, Team::Axis] {
        if let Some(set) = library.team_models(team) {
            let names = std::iter::once(&set.body).chain(set.attach.iter().map(|(m, _)| m));
            models.extend(names.filter_map(|n| library.content.model(n).cloned()));
        }
    }
    // The scripts dress each player in a body and head of their own choosing (`_teams`): all the stock ones.
    for prefix in ["viewhands_", "body_mp_", "head_mp_"] {
        for name in library.content.model_names(prefix) {
            models.extend(library.content.model(name).cloned());
        }
    }
    // The props the map lets players knock about are not static models of the map.
    for d in library
        .content
        .clipmap()
        .map_or(&[][..], |c| &c.dyn_entities[..])
        .iter()
        .flat_map(|l| l.iter())
    {
        models.extend(d.model.clone());
    }
    // The script_models the map places (cars, props), by the `model` key of its entity string.
    if let Some(ents) = library.content.clipmap().and_then(|c| c.map_ents.as_ref()) {
        for name in entity_models(&ents.entity_string) {
            models.extend(library.content.model(&name).cloned());
        }
    }
    // What effects draw: clouds, sprites (quads) and models.
    let (mut clouds, mut sprites) = (Vec::new(), Vec::new());
    for d in library
        .content
        .effects()
        .iter()
        .flat_map(|e| e.elems.iter())
    {
        match &d.visuals {
            assets::zone::fx::FxVisuals::Materials(ms) => {
                let into = if d.elem_type == assets::zone::fx::elem::CLOUD {
                    &mut clouds
                } else {
                    &mut sprites
                };
                into.extend(ms.iter().flatten().cloned());
            }
            assets::zone::fx::FxVisuals::Models(ms) => models.extend(ms.iter().flatten().cloned()),
            assets::zone::fx::FxVisuals::Decals(ms) => {
                sprites.extend(ms.iter().flatten().flatten().cloned());
            }
            _ => {}
        }
    }
    renderer.warm_models(&models);
    renderer.warm_clouds(clouds);
    renderer.warm_sprites(sprites);
}

/// The values of the `model` keys of a map's entity string (`"model" "name"` lines).
fn entity_models(entities: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(entities).lines() {
        let mut q = line.split('"').skip(1).step_by(2);
        if let (Some("model"), Some(v)) = (q.next(), q.next())
            && !v.is_empty()
            && !v.starts_with('*')
            && !out.iter().any(|o| o == v)
        {
            out.push(v.to_owned());
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn run(
    req: Arc<Request>,
    gpu: Arc<Gpu>,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<Loaded, String> {
    let check = || {
        if cancel.load(Ordering::Relaxed) {
            Err("cancelled".to_owned())
        } else {
            Ok(())
        }
    };
    let mut w = Work::new(req.clone(), gpu.clone())?;
    w.decode(progress)?;
    check()?;
    w.build_world(progress)?;
    w.add_models()?;
    check()?;
    let renderer = w.renderer.as_mut().ok_or("not built")?;
    renderer.warm_progress(req.format, &|done, total| {
        progress.set(2, 0.25 + 0.65 * done as f32 / total.max(1) as f32);
    });
    check()?;
    progress.set(3, 0.88);
    draw_once(
        &gpu,
        w.renderer.as_mut().ok_or("not built")?,
        w.library.as_mut().ok_or("not decoded")?,
        w.data.as_ref().ok_or("not decoded")?,
        &req,
    );
    check()?;
    w.finish(progress)
}

/// The warm-up frame is small: the GPU is shared with the window's loading screen, and a full-size frame of a big map
/// keeps the queue busy long enough for the screen to stop presenting. Pipelines do not depend on the size.
#[cfg(not(target_arch = "wasm32"))]
const WARM_SIZE: (u32, u32) = (320, 180);

/// Renders one frame from a spawn point into an offscreen target: the first draw of a renderer builds its depth and
/// shadow targets, bind groups and post-process state, and the first sight of a player uploads its meshes: all of
/// it would otherwise stall the first frames the player sees.
#[cfg(not(target_arch = "wasm32"))]
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
            width: WARM_SIZE.0,
            height: WARM_SIZE.1,
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
        roll: 0.0,
        fov_x: 1.9,
        time: 0.0,
    };
    renderer.render(
        &view,
        &target.create_view(&Default::default()),
        req.format,
        WARM_SIZE,
    );
    // The frame's timestamp read-back would still be pending: a surface cannot be reconfigured (a window resize)
    // while the queue has work in flight, so wait for it here, off the window thread.
    renderer.flush_gpu_times();
}

#[cfg(test)]
mod tests {
    use super::entity_models;

    #[test]
    fn the_models_of_the_entity_string_are_listed_once_without_brush_models() {
        let ents = b"{\n\"classname\" \"script_model\"\n\"model\" \"vehicle_80s_hatch1_red\"\n}\n{\n\"model\" \"*12\"\n}\n{\n\"model\" \"vehicle_80s_hatch1_red\"\n\"modelscale\" \"2\"\n\"origin\" \"1 2 3\"\n}\0";
        assert_eq!(entity_models(ents), ["vehicle_80s_hatch1_red"]);
    }
}
