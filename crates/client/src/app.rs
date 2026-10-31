// SPDX-License-Identifier: GPL-3.0-only
//! The winit application: window, surface, render loop, fly camera, flythrough recording.
//!
//! The render loop runs as fast as the present mode allows and is independent of any simulation tick: the camera is a
//! function of wall-clock time.

use crate::Cli;
use crate::display::{self, hor_plus, view_fov, zoom_sensitivity};
use crate::flythrough;
use crate::gfx::{self, Gfx};
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
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
/// The profile a first start uses: the default, or in a browser the one it last switched to.
fn start_profile() -> String {
    #[cfg(target_arch = "wasm32")]
    return crate::profile::web_active_profile();
    #[cfg(not(target_arch = "wasm32"))]
    crate::profile::DEFAULT_PROFILE.to_owned()
}

/// How often a browser keeps changed settings (see `config_saved_at`).
#[cfg(target_arch = "wasm32")]
const CONFIG_SAVE_EVERY: Duration = Duration::from_secs(2);
/// How long joining waits for the server to say which map it plays.
const JOIN_QUERY: Duration = Duration::from_secs(2);

/// The GPU, once the browser has handed it out (the adapter and device are asynchronous there).
struct Ready {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: Arc<Gpu>,
    note: Option<String>,
}

pub fn run(
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))] mut cli: Cli,
) -> Result<(), String> {
    // A direct `--connect` runs the server's mod, as a join from the menus does. A browser has learned that before it
    // starts (`web_join`): it cannot wait for the server here.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(addr) = cli.connect.clone() {
        let e = serverlist::resolve(&addr).and_then(|a| serverlist::query(a, JOIN_QUERY))?;
        e.check_protocol()?;
        if !assets::paths::valid_game(&e.game) {
            return Err(format!("The server runs an invalid mod ({}).", e.game));
        }
        // What the server runs that the install lacks is fetched first, as a join from the menus does.
        #[cfg(not(target_arch = "wasm32"))]
        if crate::download::wanted(&cli.install, &e.game, &e.map) {
            let a = serverlist::resolve(&addr)?;
            crate::download::Download::start(&cli.install, a, &e.game, &e.map, &e.gametype)?
                .wait()?;
        }
        if e.game != cli.game {
            if !e.game.is_empty() && !assets::paths::game_dir(&cli.install, &e.game) {
                return Err(format!(
                    "You do not have the mod {} this server is running.",
                    e.game
                ));
            }
            cli.game = e.game;
        }
        cli.map = e.map;
    }
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
            MapData::load(&cli.install, &cli.game, &cli.map)
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
        gpu_log: GpuLog::default(),
        proxy: None,
        #[cfg(not(target_arch = "wasm32"))]
        start_singleplayer: None,
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
            tracing::error!(error = %e, "cannot save the config");
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
        if let (Some(e), Some(out)) = (&v.error, out)
            && let Err(err) = std::fs::create_dir_all(&out).and_then(|()| {
                std::fs::write(
                    out.join("client.json"),
                    json!({"status": "error", "error": e}).to_string(),
                )
            })
        {
            tracing::error!(dir = %out.display(), error = %err, "cannot write client.json");
        }
        let result = v.error.take().map_or(Ok(()), Err);
        let start_singleplayer = v
            .start_singleplayer
            .take()
            .map(|executable| (executable, v.cli.install.clone()));
        drop(v);
        result?;
        if let Some((executable, install)) = start_singleplayer {
            std::process::Command::new(executable)
                .current_dir(install)
                .spawn()
                .map_err(|e| format!("cannot start Single Player: {e}"))?;
        }
        Ok(())
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

/// Lets the world renderer go, keeping what it could not build for the run's report (a level change or a device loss
/// would lose the record with it).
fn drop_renderer(st: &mut State) {
    if let Some(r) = st.renderer.take() {
        st.material_failures.extend(
            r.materials
                .failures
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        st.image_failures.extend(r.textures.failed.iter().cloned());
    }
}

/// What the renderers of the run could not build, for `client.json` and `ui-script.json`: `material_failures` is
/// asserted empty by the harness (a material whose draws are skipped was invisible to every stage before, #375),
/// `image_failures` are reported.
fn render_report(st: &State) -> Value {
    let mut materials = st.material_failures.clone();
    let mut images = st.image_failures.clone();
    if let Some(r) = st.renderer.as_ref() {
        materials.extend(
            r.materials
                .failures
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        images.extend(r.textures.failed.iter().cloned());
    }
    json!({"material_failures": materials, "image_failures": images})
}

/// The `--list-display-modes` result the harness reads from stdout.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::print_stdout)]
fn print_list_json(v: &Value) {
    println!("{v}");
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
            Ok(v) => print_list_json(&v),
            Err(e) => self.error = Some(e),
        }
        el.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

struct State {
    window: Arc<Window>,
    /// Only ever `None` while it is being replaced: a window takes one surface at a time (Wayland refuses a second
    /// one's swapchain presenting with a fifo), so the old one is dropped before the new is made.
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    gpu: Arc<Gpu>,
    renderer: Option<Renderer>,
    /// The offscreen frame and pass of `r_gamma`.
    gamma: render::gamma::Gamma,
    shell: Option<Shell>,
    started: Instant,
    last_frame: Instant,
    /// When `pacereset` last ran and the frames drawn since (the harness's frame cap check).
    pace: (Instant, u32),
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
    /// Per frame, parallel to `samples`: where the milliseconds went (`frames.phases.csv`), see [`PHASES`].
    phases: Vec<[f32; 21]>,
    /// What the last render built on the frame path: pipelines, their ms, passes, their ms.
    late: [f32; 4],
    laps: [f32; 10],
    /// What frames built on the frame path: (frame, what).
    late_names: Vec<(usize, String)>,
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
    /// `screenshot` or `screenshotJpeg` ran: the next frame is written to the screenshots folder.
    screenshot: Option<crate::screenshot::Request>,
    /// Where the numbered screenshots go on from (the original's `lastNumber`).
    screenshot_next: u32,
    /// `shotd=`: the screenshot waits for a frame that copied the scene for distortion.
    shot_gate: bool,
    /// The last frame copied the scene for distortion materials.
    distortion_now: bool,
    /// Input events waiting for this frame's dispatch: the window's and the control channel's, in arrival order.
    events: Vec<crate::input::InputEvent>,
    /// The dev control channel (`--control`, `--control-stdio`); `None` unless a flag turned it on.
    control: Option<devctrl::Channel>,
    /// The channel's clock and step count per waiting request.
    control_waits: HashMap<u64, ControlWait>,
    /// A capture the channel asked for: (directory override, base64 in the reply), taken after the frame is presented.
    control_shot: Option<(Option<String>, bool)>,
    /// The capture that was taken, offered to its request on the next pass.
    control_shot_done: Option<Result<Value, String>>,
    /// The channel asked for the run to end (`quit`).
    control_quit: bool,
    /// Numbers the captures the channel takes (`control-<n>.png`).
    shot_serial: u32,
    /// `dlight=1`: a bright light a little way in front of the camera, for the harness.
    test_light: bool,
    /// Dynamic lights the last frame drew.
    lights_drawn: usize,
    /// The sound system of the menus when no match is running (started by the first menu sound).
    menu_sound: Option<crate::sound::ClientSound>,
    /// The map being loaded in the background (the loading screen shows meanwhile).
    loading: Option<Load>,
    /// A map the server plays being fetched from it before the join (the loading screen shows meanwhile).
    #[cfg(not(target_arch = "wasm32"))]
    download: Option<crate::download::Download>,
    /// The demo `playdemo` is loading the map of: played once the load is done.
    pending_demo: Option<PathBuf>,
    /// Where `record` writes demos and `playdemo` looks for them.
    demo_dir: PathBuf,
    /// The game mode id of the match being loaded (the loading screen names it); empty when unknown.
    load_gametype: String,
    /// Frame intervals from the start of a map load until the player is in the world.
    load_gaps: Option<LoadGaps>,
    /// What the last load measured, for the report.
    load_report: Option<Value>,
    /// The next session answers the team and class menus by itself (a script's `start=` step).
    autojoin_next: bool,
    /// The film block the last vision set gave, before the `r_brightness`/`r_contrast`/`r_desaturation` tweaks.
    film_base: Option<render::art::Film>,
    /// Effect counts at an `fxmark` step: `cgis=fx:<kind> +n` then reads how often the kind played since.
    fx_mark: HashMap<String, u64>,
    /// Milliseconds the last frame spent in the network frame, the renderer, getting the surface texture, presenting,
    /// the input and polls before the network frame, the late warm-up, and the UI painted between the renderer
    /// and the present (`PHASES`), to explain a slow gap.
    prev_cost: [f64; 7],
    /// Frames in a row the surface gave no texture although it was configured again; see [`recover_surface`].
    surface_failures: u32,
    /// How long a surface may time out or be occluded before a harness run gives up; `None` for a player's window,
    /// which a workspace switch or a cover can hide for any time.
    surface_stall_limit: Option<Duration>,
    /// How long the surface has only timed out or been occluded without the window being minimized, counted only
    /// while a frame is waiting for its image: a client that blocks for seconds is a stall of its own, priced
    /// elsewhere, and must not read as a surface that gave nothing.
    surface_stalled_ms: f64,
    /// The stalled surface was recreated once and the window is still giving no frame; the next expiry is the end.
    surface_retried: bool,
    /// A frame has been presented on the surface of the moment: before that the compositor has not mapped the window
    /// yet, and a timeout says nothing about a stall.
    presented: bool,
    /// The next resize takes a new surface from the window: the present mode changed, and a live swapchain is not
    /// handed over to another mode (see [`vid_restart`]).
    fresh_surface: bool,
    /// What the graphics device went through: losses and recoveries, and errors it reported.
    gpu_log: GpuLog,
    /// What the renderers of this run could not build, kept across level changes for the report: materials with no
    /// usable pass or a failed pipeline, by name and reason, and images that did not load. The harness asserts the
    /// first is empty (#375).
    material_failures: BTreeMap<String, String>,
    image_failures: BTreeSet<String>,
    /// A resize that arrived during a map load; see [`apply_resize`].
    pending_resize: Option<(u32, u32)>,
    /// When the page's debug overlay was last given its values.
    #[cfg(target_arch = "wasm32")]
    overlay_at: Instant,
    /// When the settings were last written to the page's storage: a browser has no exit to save them at.
    #[cfg(target_arch = "wasm32")]
    config_saved_at: Instant,
    /// The browser refused the settings once: they are not tried again, and the player has been told.
    #[cfg(target_arch = "wasm32")]
    config_failed: bool,
    /// How many pipelines the background compile has built, and how many draws the last frame skipped for want of one.
    #[cfg(target_arch = "wasm32")]
    warm: render::Progress,
    #[cfg(target_arch = "wasm32")]
    pipelines_missing: usize,
    /// Milliseconds of each frame's [`Renderer::warm_step`].
    #[cfg(target_arch = "wasm32")]
    warm_ms: Vec<f64>,
}

impl State {
    fn surface(&self) -> &wgpu::Surface<'static> {
        self.surface
            .as_ref()
            .expect("the surface is only absent while it is being replaced")
    }
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

/// What a native frame spends preparing the passes of models the server precached after the load
/// (`Renderer::warm_late`): a material's few passes fit in it, so the list is spread over a second or two of frames
/// rather than stalling the first one that sees it.
#[cfg(not(target_arch = "wasm32"))]
const WARM_LATE_BUDGET: Duration = Duration::from_millis(2);

/// Frames a tour menu is shown before its screenshot (fades and expressions settle).
const TOUR_FRAMES: u32 = 6;

/// A `throw` step in progress: the grenade counts of the HUD and the missiles seen when it pressed the key, and
/// whether it has let go yet.
struct Throw {
    before: (Option<i32>, Option<i32>),
    missiles: usize,
    released: bool,
}

impl Throw {
    /// Whether the grenade left the hand: the key was let go, a count dropped, a missile entity appeared and the
    /// player is alive. A frag that cooked off in the hand drops the count and kills the player; a count that
    /// changed with no missile is a pickup or a class change.
    fn thrown(&self, now: (Option<i32>, Option<i32>), missiles: usize, dead: bool) -> bool {
        self.released && now != self.before && missiles > self.missiles && !dead
    }
}

/// Whether the player's team `have` satisfies a `team=<want>` step: a number is that team; `same` the one
/// `team=save` kept; `other` the opposite side, which only a player on a side (1 or 2) can be on, so a spectator
/// (3) or an unassigned player (0) satisfies neither `other` nor a side.
fn team_matches(want: &str, have: &str, saved: Option<&str>) -> bool {
    let side = |t: &str| t == "1" || t == "2";
    match want {
        "other" => saved.is_some_and(|t| side(t) && side(have) && t != have),
        "same" => saved == Some(have),
        _ => have == want,
    }
}

#[cfg(test)]
mod step_tests {
    use super::*;

    #[test]
    fn the_other_team_is_the_other_side_not_the_spectators() {
        assert!(team_matches("other", "1", Some("2")));
        assert!(team_matches("other", "2", Some("1")));
        assert!(
            !team_matches("other", "3", Some("2")),
            "dying is not a side switch"
        );
        assert!(!team_matches("other", "0", Some("2")));
        assert!(!team_matches("other", "2", Some("2")));
        assert!(
            !team_matches("other", "1", Some("3")),
            "a saved spectator has no other side"
        );
        assert!(!team_matches("other", "1", None));
        assert!(team_matches("same", "2", Some("2")));
        assert!(!team_matches("same", "2", None));
        assert!(team_matches("2", "2", None));
    }

    #[test]
    fn a_throw_needs_the_release_a_count_drop_a_missile_and_a_live_player() {
        let t = Throw {
            before: (Some(1), Some(1)),
            missiles: 4,
            released: true,
        };
        assert!(t.thrown((Some(0), Some(1)), 5, false));
        assert!(
            !t.thrown((Some(0), Some(1)), 5, true),
            "cooked off in the hand"
        );
        assert!(
            !t.thrown((Some(0), Some(1)), 4, false),
            "no missile appeared"
        );
        assert!(
            !t.thrown((Some(1), Some(1)), 5, false),
            "someone else's grenade"
        );
        let held = Throw {
            released: false,
            ..t
        };
        assert!(
            !held.thrown((Some(0), Some(1)), 5, false),
            "still holding the key"
        );
    }
}

/// `--ui-script`: steps that drive the menus like a player. Each step that waits across frames keeps its own state
/// here: one shared field let `team=save` stand in for the map `maprotate` started from (#375).
struct UiScript {
    /// What `respawn` and `loc` carry between frames (the death seen; the hint count the step began from).
    marker: Option<String>,
    /// The team `team=save` remembered.
    team_saved: Option<String>,
    /// The map a `maprotate` step started from.
    rotate_from: Option<String>,
    /// The `throw` step under way.
    throw: Option<Throw>,
    /// What `cvar=save` and `round=next` remembered, by name.
    saved: HashMap<String, String>,
    /// A `killcam` or `dead` step has seen the player die: a later `respawn` step counts that death, since the
    /// player may already be back when it starts (the killcam is short and the steps before it take seconds).
    died: bool,
    steps: Vec<String>,
    index: usize,
    /// When the current step began and, for waits, how long it may take.
    began: Instant,
    results: Vec<Value>,
}

/// How long a `throw` step holds the key before letting go: long enough for the pin, short of any fuse.
const THROW_HOLD: f32 = 0.6;

/// A device that has run this long since its predecessor was lost starts the count of losses again.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const LOSS_FORGOTTEN: Duration = Duration::from_secs(5 * 60);

/// Device losses in a row one run recovers from before it gives up.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_DEVICE_LOSSES: usize = 3;

/// What the graphics device went through, for the run's report: the losses (each one was recovered from, or ended the
/// run) and the errors wgpu reported outside any scope, which the harness fails on.
#[derive(Default)]
struct GpuLog {
    losses: Vec<String>,
    errors: Vec<String>,
    /// Losses that count toward [`MAX_DEVICE_LOSSES`]: those since the device last ran for [`LOSS_FORGOTTEN`].
    recent: usize,
    last_loss: Option<Instant>,
}

impl GpuLog {
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    /// Notes that `gpu` was lost during `context`; an error once there have been more losses than a run recovers from.
    fn record_loss(&mut self, gpu: &Gpu, context: &str) -> Result<(), String> {
        let why = gpu.lost().unwrap_or_default();
        // What a device that is gone reported is the loss's own noise, not a second failure.
        drop(gpu.take_errors());
        if self.last_loss.is_some_and(|t| t.elapsed() > LOSS_FORGOTTEN) {
            self.recent = 0;
        }
        self.last_loss = Some(Instant::now());
        self.recent += 1;
        self.losses.push(format!("{context}: {why}"));
        if self.recent > MAX_DEVICE_LOSSES {
            return Err(format!(
                "the graphics device was lost {} times in a row: {}",
                self.recent,
                self.losses.join("; ")
            ));
        }
        tracing::warn!(context, why = %why, "recovering from the lost graphics device");
        Ok(())
    }

    /// The report's `gpu_health`, with what `gpu` reported since.
    fn report(&mut self, gpu: &Gpu) -> Value {
        self.errors.extend(gpu.take_errors());
        json!({"device_losses": self.losses, "errors": self.errors})
    }
}

/// A new instance, the window's surface on it and a device for both.
#[cfg(not(target_arch = "wasm32"))]
fn open_gpu(cli: &Cli, window: &Arc<Window>) -> Result<(wgpu::Surface<'static>, Arc<Gpu>), String> {
    let instance = wgpu::Instance::new(instance_descriptor(&cli.backend)?);
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| e.to_string())?;
    let mut gpu = Gpu::with_instance(instance, Some(&surface)).map_err(|e| e.to_string())?;
    gpu.bc &= !cli.no_bc;
    Ok((surface, Arc::new(gpu)))
}

struct Viewer {
    cli: Cli,
    map: Option<MapData>,
    load_ms: f64,
    st: Option<State>,
    error: Option<String>,
    /// Device losses and errors from before the state exists (the start-up retries); moved into it.
    gpu_log: GpuLog,
    /// Where the GPU arrives on the web.
    proxy: Option<winit::event_loop::EventLoopProxy<Ready>>,
    /// Launched only once, after the event loop and client state shut down.
    #[cfg(not(target_arch = "wasm32"))]
    start_singleplayer: Option<PathBuf>,
    /// The browser window is open and the GPU has been asked for.
    #[cfg(target_arch = "wasm32")]
    starting: bool,
}

impl Viewer {
    /// Window, surface and GPU, where the platform can wait for the adapter. A device that is lost while the
    /// surface is configured (the mode switch of exclusive fullscreen can do it) is made again on the same window.
    #[cfg(not(target_arch = "wasm32"))]
    fn init(&mut self, el: &ActiveEventLoop) -> Result<State, String> {
        let (window, note) = display::create_window(el, &self.cli.request)?;
        let window = Arc::new(window);
        loop {
            let (surface, gpu) = open_gpu(&self.cli, &window)?;
            let probe = gpu.clone();
            let ready = Ready {
                window: window.clone(),
                surface,
                gpu,
                note: note.clone(),
            };
            match self.finish_init(ready) {
                Err(e) if probe.lost().is_some() => self.gpu_log.record_loss(&probe, &e)?,
                result => return result,
            }
        }
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
                    if proxy.send_event(r).is_err() {
                        tracing::error!(
                            "the event loop is gone; the graphics cannot be handed over"
                        );
                    }
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
        if gpu.lost().is_some() {
            let why = gpu.explain("the window's surface could not be configured");
            tracing::error!(error = %why, "surface configuration failed");
            return Err(why);
        }
        let vfs = loader::open_vfs(&self.cli.install, &self.cli.game, &self.cli.map)?;
        // Native warms it again once the library is loaded (see below); the browser builds its pipelines by frames.
        #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
        let mut renderer = match &self.map {
            Some(map) => {
                #[cfg(target_arch = "wasm32")]
                crate::web::log("building the scene");
                let scene = Scene::new(&gpu, map)?;
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
                let mut lib = Library::load(&self.cli.install, &self.cli.game, &self.cli.map)?;
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
                            bot_count: 0,
                            gametype: self.cli.gametype.clone(),
                            rotation: None,
                            port: 0,
                            game: self.cli.game.clone(),
                            dvars: Vec::new(),
                            cheats: false,
                        },
                    )?;
                    let a = l.addr;
                    notes.push(format!(
                        "listen server with {} bots up in {:.0} ms",
                        self.cli.bots.unwrap_or(0),
                        t.elapsed().as_secs_f64() * 1000.0
                    ));
                    listen = Some(l);
                    a
                }
            };
            let lib = Library::load(&self.cli.install, &self.cli.game, &self.cli.map)?;
            // What the match draws that the map does not hold (players, weapons) gets its pipelines now, not at
            // first sight; what the scripts then dress players in is compiled beside the frames.
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(r) = renderer.as_mut() {
                crate::loader::add_match_models(r, &lib);
                r.warm(config.format);
                r.build_pipelines_in_background();
            }
            let limits = Input::detached().pitch_limits();
            let mut n = NetPlay::connect(
                lib,
                self.map.as_ref().ok_or("no map")?,
                addr,
                &self.cli.name,
                "",
                limits,
                self.cli.autoplay,
                crate::sound::ClientSound::start(
                    &self.cli.install,
                    &self.cli.game,
                    &self.cli.map,
                    !self.cli.no_sound,
                ),
            )?;
            n.set_autojoin(self.cli.autojoin || self.cli.autoplay);
            if let Some(name) = &self.cli.fx_demo {
                n.set_fx_demo(Some(name.clone()));
            }
            net = Some(n);
        }
        let install = server::content::Install::open_game(&self.cli.install, &self.cli.game)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        let mut input = Input::new(
            self.cli.config_dir.clone(),
            assets::vfs::Vfs::open(&self.cli.install, 0, &self.cli.game, None)
                .inspect_err(|e| {
                    tracing::warn!(game = %self.cli.game, error = %e, "cannot open the file system for the config");
                })
                .ok(),
        );
        input.cvars.set("fs_game", &self.cli.game, false);
        let demo_dir = input
            .config_dir()
            .unwrap_or_else(|| self.cli.install.clone())
            .join("demos");
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
        if let (Some(sh), Some(n)) = (shell.as_mut(), net.as_mut()) {
            sh.ui.assets.add_weapon_icons(&n.weapon_defs());
            n.set_tracer_material(sh.ui.assets.material("gfx_tracer").cloned());
        }
        let (profiles, stats) = match self
            .cli
            .config_dir
            .clone()
            .filter(|_| self.cli.harness_profile)
        {
            Some(dir) => {
                crate::profile::seed_harness_profile(&dir)
                    .map_err(|e| format!("cannot seed the harness profile: {e}"))?;
                Profiles::open_isolated(dir)
            }
            None => Profiles::open(&install.root, input.config_dir(), &start_profile()),
        };
        let (read, write) = profiles.config_paths();
        input.use_profile(read, write);
        // The profile is read after the native window exists: apply its archived position now, not only on vid_restart.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((x, y)) = Gfx::from_cvars(&input.cvars).position {
            window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        // A bare `--listen` started its server before the config was read: it learns `bot_count` now.
        if listen.is_some() {
            for line in saved_bot_count_lines(self.cli.bots, &input) {
                listen::send(&line);
            }
        }
        if let Some(n) = net.as_mut() {
            n.set_profile(&stats);
        }
        if let Some(sh) = shell.as_mut() {
            sh.set_profiles(&mut input, profiles, stats);
            let (modes, rates) =
                display::video_modes(&window, Gfx::from_cvars(&input.cvars).monitor);
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
        let gamma = render::gamma::Gamma::new(&gpu, config.format);
        Ok(State {
            tour,
            window,
            surface: Some(surface),
            config,
            gpu,
            renderer,
            gamma,
            shell,
            started: now,
            last_frame: now,
            pace: (now, 0),
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
            phases: Vec::new(),
            late: [0.0; 4],
            laps: [0.0; 10],
            late_names: Vec::new(),
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
                team_saved: None,
                rotate_from: None,
                throw: None,
                saved: HashMap::new(),
                died: false,
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
            screenshot: None,
            screenshot_next: 0,
            shot_gate: false,
            distortion_now: false,
            events: Vec::new(),
            control: crate::control::open(&self.cli),
            control_waits: HashMap::new(),
            control_shot: None,
            control_shot_done: None,
            control_quit: false,
            shot_serial: 0,
            test_light: false,
            lights_drawn: 0,
            menu_sound: None,
            loading: None,
            #[cfg(not(target_arch = "wasm32"))]
            download: None,
            pending_demo: None,
            demo_dir: demo_dir.clone(),
            load_gametype: String::new(),
            load_gaps: None,
            load_report: None,
            autojoin_next: false,
            fx_mark: HashMap::new(),
            film_base: None,
            prev_cost: [0.0; 7],
            surface_failures: 0,
            surface_stalled_ms: 0.0,
            surface_retried: false,
            presented: false,
            surface_stall_limit: (self.cli.timed() || self.cli.ui_script.is_some())
                .then_some(SURFACE_STALL),
            fresh_surface: false,
            gpu_log: std::mem::take(&mut self.gpu_log),
            material_failures: BTreeMap::new(),
            image_failures: BTreeSet::new(),
            pending_resize: None,
            #[cfg(target_arch = "wasm32")]
            overlay_at: now,
            #[cfg(target_arch = "wasm32")]
            config_saved_at: now,
            #[cfg(target_arch = "wasm32")]
            config_failed: false,
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
    /// The frame cap's timer ran out: draw the next frame (see [`next_frame`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn new_events(&mut self, _: &ActiveEventLoop, cause: winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. })
            && let Some(st) = self.st.as_ref()
        {
            st.window.request_redraw();
        }
    }

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
                tracing::error!(error = %e, "the window or graphics could not start");
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
                tracing::error!(error = %e, "the window or graphics could not start");
                #[cfg(target_arch = "wasm32")]
                crate::web::fatal(&e);
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let Some(st) = self.st.as_mut() else { return };
        // The event joins the frame's queue, where the control channel's events are too: both take the same route
        // (see [`dispatch_input`]), so an injected key press is the player's key press.
        if let Some(e) = translate(&ev) {
            st.events.push(e);
        }
        if let WindowEvent::KeyboardInput { event, .. } = &ev
            && event.state == ElementState::Pressed
            && !event.repeat
            && event.physical_key == PhysicalKey::Code(KeyCode::F11)
        {
            toggle_fullscreen(&st.window);
        }
        match ev {
            WindowEvent::CloseRequested => {
                tracing::info!("window close requested");
                el.exit();
            }
            WindowEvent::Resized(s) if s.width > 0 && s.height > 0 => {
                if st.loading.is_some() {
                    st.pending_resize = Some((s.width, s.height));
                } else {
                    if let Err(e) = apply_resize(st, (s.width, s.height)) {
                        tracing::error!(error = %e, "cannot resize the window's surface");
                        self.error = Some(e);
                        el.exit();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.cli.timed()
                && !st.shell.as_ref().is_some_and(|s| s.ui.captures_input())
                && st.renderer.is_some() =>
            {
                st.gate.click();
                grab_pointer(st);
            }
            WindowEvent::Focused(false) => release_pointer(st),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.frame(el) {
                    tracing::error!(error = %e, "the frame failed");
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

/// The columns of `frames.phases.csv`: network and prediction, surface acquire, render, present, then what the
/// render built on the frame path (pipelines and passes: count, ms each), the render's own laps, and last the
/// input and polls before the network frame, the late warm-up (`Renderer::warm_late`) and the UI painted between
/// the render and the present. With the acquire left out, the columns but the laps add up to `cpu_ms` of
/// `frames.raw.csv` but for the frame's tail (console lines, menu sound, actions).
const PHASES: &str = "net_ms,acquire_ms,render_ms,present_ms,late_pipelines,late_pipeline_ms,late_passes,late_pass_ms,r_setup,r_dynamic,r_sun_shadow,r_spot_shadow,r_main_draws,r_lights,r_floatz,r_post_upload,r_encode,r_submit,input_ms,warm_ms,ui_ms";

impl Viewer {
    /// The graphics device is lost and cannot be used again: a new one is made for the window, and everything that
    /// lived on the old one is built again. The menus and the surface at once; the world from its map (a match
    /// loads it again as for a level change, the server and the connection going on).
    #[cfg(not(target_arch = "wasm32"))]
    fn recover_device(&mut self) -> Result<(), String> {
        let st = self.st.as_mut().ok_or("no window")?;
        if st.showcase.is_some() {
            return Err(st
                .gpu
                .explain("the graphics device was lost, which --show-models cannot recover from"));
        }
        let old = st.gpu.clone();
        st.gpu_log.record_loss(&old, "while running")?;
        let window = st.window.clone();
        // The window takes one surface at a time.
        st.surface = None;
        // The new device must configure the surface the window has now: a mode that cannot be had is an error.
        let (surface, gpu) = loop {
            let (surface, gpu) = open_gpu(&self.cli, &window)?;
            let caps = surface.get_capabilities(&gpu.adapter);
            if !caps.formats.contains(&st.config.format)
                || !caps.present_modes.contains(&st.config.present_mode)
            {
                return Err(
                    "the new graphics device cannot present the way the old one did".into(),
                );
            }
            surface.configure(&gpu.device, &st.config);
            if gpu.lost().is_none() {
                break (surface, gpu);
            }
            st.gpu_log.record_loss(&gpu, "while recovering")?;
        };
        let format = st.config.format;
        // Everything on the old device goes before anything is built on the new one.
        drop_renderer(st);
        st.surface = Some(surface);
        st.gpu = gpu.clone();
        st.gamma = render::gamma::Gamma::new(&gpu, format);
        if let Some(r) = st.recorder.as_mut() {
            r.reset_gpu();
        }
        if let Some(sh) = st.shell.as_mut() {
            sh.replace_gpu(gpu.clone(), format, &self.cli.install)?;
        }
        st.surface_failures = 0;
        st.surface_stalled_ms = 0.0;
        st.surface_retried = false;
        st.presented = false;
        st.fresh_surface = false;
        if st.net.is_some() {
            let name = if st.map_name.is_empty() {
                self.cli.map.clone()
            } else {
                st.map_name.clone()
            };
            let gametype = st
                .input
                .cvars
                .get("g_gametype")
                .unwrap_or_default()
                .to_string();
            begin_load(&self.cli, st, &name, &gametype, loader::Server::Keep);
        } else if let Some(map) = &self.map {
            let vfs = loader::open_vfs(
                &self.cli.install,
                &game_of(&st.input),
                if st.map_name.is_empty() {
                    &self.cli.map
                } else {
                    &st.map_name
                },
            )?;
            let scene = Scene::new(&gpu, map)?;
            let gfx = Gfx::from_cvars(&st.input.cvars);
            let mut r = Renderer::new(
                gpu.clone(),
                scene,
                map,
                TextureCache::new(Some(vfs), gfx.picmip)
                    .with_map_picmips(gfx.picmip_bump, gfx.picmip_spec),
            );
            r.materials.aniso_max = gfx.aniso_max;
            r.materials.aniso_min = gfx.aniso_min;
            r.materials.mip_mode = gfx.mip_mode;
            r.settings = gfx.settings(self.cli.settings);
            r.warm(format);
            st.renderer = Some(r);
            st.film_base = None;
        }
        st.window.request_redraw();
        Ok(())
    }

    fn frame(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let _frame = tracing::debug_span!("frame").entered();
        let st = self.st.as_mut().ok_or("no window")?;
        let now = Instant::now();
        let t = st.started.elapsed().as_secs_f32();
        let interval = now.duration_since(st.last_frame).as_secs_f64() * 1000.0;
        st.last_frame = now;
        let dt = (t - st.last_t).min(0.1);
        st.last_t = t;
        st.pace.1 += 1;
        // The control channel, when a flag opened one: its requests are answered against this frame's state, and the
        // events it injects join the window's in the queue below.
        if st.control.is_some() {
            service_control(&self.cli, st);
            if std::mem::take(&mut st.control_quit) {
                el.exit();
            }
        }
        // This frame's input: the window's events and the channel's, in the order they arrived.
        for ev in std::mem::take(&mut st.events) {
            dispatch_input(st, &ev);
        }
        // The commands a `wait` held back run before this frame's input is read.
        match st.shell.as_mut() {
            Some(sh) => sh.run_due(&mut st.input),
            None => st.input.run_due(),
        }
        // What the listen server made of the host's console lines since the last frame (#366).
        host_console_replies(st);
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
                    "input_ms": st.prev_cost[4].round(),
                    "warm_ms": st.prev_cost[5].round(),
                    "ui_ms": st.prev_cost[6].round(),
                }));
            }
        }
        // The costs a frame may not reach (no network frame, nothing to warm) read as zero, not as the last frame's.
        for i in [0, 4, 5, 6] {
            st.prev_cost[i] = 0.0;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(done) = st.download.as_mut().and_then(|d| d.poll())
            && let Some(d) = st.download.take()
        {
            match done {
                Ok(()) => {
                    let (addr, gametype) = d.join.clone();
                    // The mod is here now, so the client can run it as the server does.
                    if let Err(why) = follow_server_game(&self.cli, &mut self.map, st, &d.game) {
                        show_error(st, &why);
                    } else {
                        start_session(
                            &self.cli,
                            &mut self.map,
                            st,
                            &d.map,
                            &gametype,
                            loader::Server::Join(addr),
                        );
                    }
                }
                Err(why) => {
                    tracing::error!(map = %d.map, error = %why, "cannot download the map");
                    show_error(st, &format!("Could not download from the server: {why}"));
                }
            }
        }
        if let Some(done) = st.loading.as_mut().and_then(Load::poll)
            && let Some(load) = st.loading.take()
        {
            let result = done.and_then(|l| finish_load(&self.cli, &mut self.map, st, &load, l));
            if let Err(e) = result {
                tracing::error!(map = %load.map, error = %e, "cannot load the map");
                // A level change on a lost device is loaded again on the new one; a session that was starting ends.
                #[cfg(not(target_arch = "wasm32"))]
                if st.net.is_some() && st.gpu.lost().is_some() {
                    return self.recover_device();
                }
                drop_session(&mut self.map, st, &e);
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
        // A refused join (a wrong password, a full server, a ban) goes back to the menus with the reason; a scripted
        // run has no one to read it and ends.
        if !self.cli.autoplay
            && let Some(why) = st
                .net
                .as_ref()
                .and_then(NetPlay::refused)
                .map(str::to_owned)
            && st.shell.is_some()
        {
            tracing::error!(reason = %why, "join refused");
            end_session(&mut self.map, st);
            if let Some(sh) = st.shell.as_mut() {
                sh.join_refused(&mut st.input, &why);
            }
        }
        if let Some(why) = st
            .net
            .as_ref()
            .and_then(NetPlay::dropped)
            .map(str::to_owned)
        {
            connection_lost(&mut self.map, st, &why);
        }
        // A bind item waits for a key: the pad's next button is the binding, as a key press is.
        st.input.capture_pad = st.shell.as_ref().is_some_and(|s| s.ui.bind_pending());
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
            screenshot_commands(st.shell.as_mut(), &f);
            shell_commands(st.shell.as_mut(), &mut st.input, &f);
            if let Some(sh) = st.shell.as_mut() {
                sh.st.actions.extend(own_commands(&f));
            }
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
            net.set_sound_mix(crate::sound::mix_of(&st.input.cvars));
            net.set_voice(st.input.cvar("cl_voice").is_none_or(|v| v.trim() != "0"));
            net.set_demo_paused(
                st.input
                    .cvar("cl_freezeDemo")
                    .is_some_and(|v| v.trim() != "0"),
            );
            if let Some(sh) = st.shell.as_ref() {
                net.set_muted(&sh.st.muted);
            }
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
            let cvar_f32 = |name: &str, default: f32| {
                st.input
                    .cvar(name)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                    .unwrap_or(default)
            };
            net.set_orbit(
                cvar_f32("cg_thirdPersonRange", 120.0),
                cvar_f32("cg_thirdPersonAngle", 0.0),
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
            // `CL_SystemInfoChanged`: the server's `sv_cheats` is the client's, and without cheats every cheat
            // dvar goes back to its reset value.
            if let Some(on) = net.take_cheat_state() {
                st.input.cvars.set_cheat_state(on);
            }
            net.set_switches(crate::netplay::Switches::read(&st.input.cvars));
            let tracer = |name: &str, default: f32| {
                st.input
                    .cvar(name)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                    .unwrap_or(default)
            };
            net.set_tracer_cvars(crate::tracer::Cvars {
                chance: tracer("cg_tracerchance", 0.2).clamp(0.0, 1.0),
                own_chance: tracer("cg_firstPersonTracerChance", 0.5).clamp(0.0, 1.0),
                scale: tracer("cg_tracerScale", 1.0).max(1.0),
                scale_min_dist: tracer("cg_tracerScaleMinDist", 5000.0).max(0.0),
                scale_dist_range: tracer("cg_tracerScaleDistRange", 25000.0).max(0.0),
            });
            net.set_projection(
                st.fov_x,
                st.aspect
                    .unwrap_or(st.config.width as f32 / st.config.height.max(1) as f32),
            );
            let frame_out = net.frame(dt, &f);
            if let (Some(note), Some(sh)) = (net.take_voice_note(), st.shell.as_mut()) {
                sh.print_console(&note);
            }
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
            // What the server precached beyond what the load warmed (planes, vehicles the scripts call in): its
            // passes are prepared a material per frame and its pipelines built beside the frames, not at first sight.
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(r) = st.renderer.as_mut() {
                let models = net.unwarmed_models();
                if !models.is_empty() {
                    r.warm_models(&models);
                    if let Some(sh) = st.shell.as_ref() {
                        r.warm_sprites(sh.ui.assets.material("gfx_tracer").cloned());
                    }
                }
                let t_warm = Instant::now();
                r.warm_late(st.config.format, WARM_LATE_BUDGET);
                st.prev_cost[5] = t_warm.elapsed().as_secs_f64() * 1000.0;
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
                // `CL_SubtitlePrint`: nothing with `cg_subtitles` off.
                let subtitles_on = st
                    .input
                    .cvars
                    .get("cg_subtitles")
                    .is_none_or(|v| v.trim() != "0");
                for s in net.take_subtitles() {
                    if subtitles_on {
                        sh.subtitle(&st.input, &s.key, s.ms);
                    }
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
                st.input.set_fov_sensitivity_scale(
                    zoom_sensitivity(fov, cg_fov(&st.input)) * nf.shock_sensitivity,
                );
                st.fx_in_view = nf.meshes.len();
                if let Some(r) = st.renderer.as_mut() {
                    r.dynamic_models = nf.models;
                    r.brush_models = nf.brush_models;
                    r.dynamic_meshes = nf.meshes;
                    r.dynamic_lights = nf
                        .lights
                        .iter()
                        .filter_map(|l| dynamic_light(l, &r.settings.spot))
                        .collect();
                    if st.test_light {
                        let fwd = View {
                            origin: nf.origin,
                            yaw: nf.yaw,
                            pitch: nf.pitch,
                            roll: 0.0,
                            fov_x: 1.0,
                            time: 0.0,
                        }
                        .forward();
                        r.dynamic_lights.extend(render::DynLight::omni(
                            nf.origin + fwd * 120.0,
                            300.0,
                            glam::Vec3::new(3.0, 2.5, 1.5),
                        ));
                    }
                    // `R_SetFilmInfo`: the vision set's film block, then (with cheats on) the `r_desaturation`,
                    // `r_contrast` and `r_brightness` tweaks on top of it (desaturation is applied twice, as stock
                    // does). The base is the look's each frame, so the tweaks never accumulate.
                    let cheats = st.input.cvars.bool("sv_cheats");
                    let base = nf
                        .look
                        .vision
                        .map(|(_, f)| f)
                        .or(st.film_base)
                        .unwrap_or(r.post.film);
                    st.film_base = Some(base);
                    if let Some((glow, _)) = nf.look.vision {
                        r.post.glow = glow;
                    }
                    r.post.film = crate::gfx::film(&st.input.cvars, base);
                    // Cheat dvars, read live as the original does: `r_fullbright` (`R_InitDrawMethod` on change)
                    // and `cg_drawShellshock` (`DrawShellshockBlend`).
                    r.settings.fullbright = cheats && st.input.cvars.bool("r_fullbright");
                    let shock_on = !cheats
                        || st
                            .input
                            .cvars
                            .get("cg_drawShellshock")
                            .is_none_or(|v| v.trim() != "0");
                    r.post.shell_shock = nf.look.shell_shock.filter(|_| shock_on);
                    r.post.dof = Some(nf.dof);
                    // The server's fog replaces the one the map's art script was read for, once it has set any.
                    if nf.fog_from_server {
                        r.art.fog = nf.fog;
                    }
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
        if let Some(n) = st.input.take_pad_press()
            && let Some(sh) = st.shell.as_mut()
        {
            sh.bind_key(&mut st.input, &n);
        }
        if let Some(name) = new_level
            && let Err(e) = enter_level(&self.cli, &mut self.map, st, &name)
        {
            tracing::error!(map = %name, error = %e, "cannot enter the level");
            drop_session(&mut self.map, st, &e);
        }
        if let Some(sh) = st.shell.as_mut()
            && let Err(e) = sh.st.profiles.save_if_changed(&sh.st.stats)
        {
            tracing::error!(error = %e, "cannot save the profile");
        }
        // A native client writes the config when it exits; a tab has no such moment, so changes are kept as they come.
        #[cfg(target_arch = "wasm32")]
        if !st.config_failed
            && st.input.unsaved()
            && st.config_saved_at.elapsed() >= CONFIG_SAVE_EVERY
        {
            st.config_saved_at = Instant::now();
            if let Err(e) = st.input.save() {
                tracing::error!(error = %e, "cannot save the config");
                st.config_failed = true;
                crate::web::notice(
                    "Your settings cannot be saved in this browser (storage is full or off).",
                );
            }
        }
        if std::mem::take(&mut st.video_pending) && st.shell.is_some() {
            vid_restart(&self.cli, st);
        }
        if st.loading.is_none()
            && let Some(size) = st.pending_resize.take()
        {
            apply_resize(st, size)?;
        }
        let t_acquire = Instant::now();
        // What the frame did before asking for its texture, beyond the network frame and the warm-up: the control
        // channel, the input, the due commands, the load and download polls.
        let pre_ms = t_acquire.duration_since(now).as_secs_f64() * 1000.0;
        st.prev_cost[4] = (pre_ms - st.prev_cost[0] - st.prev_cost[5]).max(0.0);
        let frame = match st.surface().get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                st.surface_failures = 0;
                st.surface_stalled_ms = 0.0;
                f
            }
            other => {
                let first_fail = st.surface_stalled_ms == 0.0;
                st.surface_stalled_ms += t_acquire.elapsed().as_secs_f64() * 1000.0;
                if let SurfaceAction::DeviceLost = recover_surface(st, &other, first_fail)? {
                    #[cfg(not(target_arch = "wasm32"))]
                    return self.recover_device();
                    #[cfg(target_arch = "wasm32")]
                    return Err(st.gpu.explain("cannot draw"));
                }
                st.window.request_redraw();
                return Ok(());
            }
        };
        st.prev_cost[2] = t_acquire.elapsed().as_secs_f64() * 1000.0;
        // `cpu_ms` is the frame's own work: everything but the wait for the surface texture.
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
        let surface_view = frame.texture.create_view(&Default::default());
        let size = (st.config.width, st.config.height);
        // `r_gamma` maps the whole picture, as the original's display ramp does: that ramp works only in full screen
        // and not with `r_ignorehwgamma`, so a window shows the picture as drawn. The frame goes offscreen and is
        // copied through the ramp at the end.
        let r_gamma = st
            .input
            .cvars
            .get("r_gamma")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(render::gamma::DEFAULT);
        let gamma = st.window.fullscreen().is_some()
            && !st.input.cvars.bool("r_ignorehwgamma")
            && !render::gamma::is_identity(r_gamma);
        let target = if gamma {
            st.gamma.target(&st.gpu, size).clone()
        } else {
            surface_view.clone()
        };
        // The menus up blur the world behind them (`UI_GetBlurRadius`, from the last painted frame).
        let menu_blur = match (&st.loading, &st.shell) {
            (None, Some(sh)) => sh.ui.blur_radius,
            _ => 0.0,
        };
        if let Some(r) = st.renderer.as_mut() {
            r.post.blur_radius = menu_blur;
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
            st.late = [
                stats.late_pipelines as f32,
                stats.late_pipeline_ms as f32,
                stats.late_passes as f32,
                stats.late_pass_ms as f32,
            ];
            st.laps = stats.laps;
            let n = st.samples.len();
            // The first frame holds what the load built; later ones are hitches. A cap keeps the report small.
            if n > 0 && st.late_names.len() < 256 {
                st.late_names
                    .extend(stats.late_names.iter().map(|w| (n, w.clone())));
            }
            st.surfaces_drawn.push(stats.surfaces as f64);
            st.lights_drawn = stats.lights;
            st.distortion_now = stats.distortion_copy;
            if let Some(sh) = st.shell.as_mut() {
                sh.st.fps_scene = crate::hud::fps::Scene {
                    prims: stats.draws + stats.shadow_draws + stats.light_draws,
                };
            }
            record_gpu(&mut st.gpu_ms, r.take_gpu_times());
        }
        // The UI and captures between the world and the present: menus, HUD, screenshots, the video recorder.
        let t_ui = Instant::now();
        #[cfg(not(target_arch = "wasm32"))]
        let downloading = st
            .download
            .as_ref()
            .map(|d| (d.map.clone(), d.note(), d.fraction()));
        #[cfg(target_arch = "wasm32")]
        let downloading: Option<(String, String, f32)> = None;
        if let (Some(sh), Some(l)) = (st.shell.as_mut(), st.loading.as_ref()) {
            let progress = l.progress();
            // The connection state shows once the level has loaded (`displayConnectionInfo`).
            let status = match (progress >= 1.0, l.joining) {
                (false, _) => "",
                (true, true) => "EXE_AWAITINGGAMESTATE",
                (true, false) => "EXE_AWAITINGHOST",
            };
            let view = LoadingView {
                map: &l.map,
                note: l.note(),
                progress,
                gametype: &st.load_gametype,
                status,
                now_ms: (t * 1000.0) as i32,
            };
            sh.paint_loading(&target, size, &view);
        } else if let (Some(sh), Some((map, note, progress))) = (st.shell.as_mut(), &downloading) {
            let view = LoadingView {
                map,
                note,
                progress: *progress,
                gametype: &st.load_gametype,
                status: "",
                now_ms: (t * 1000.0) as i32,
            };
            sh.paint_loading(&target, size, &view);
        } else if let Some(sh) = st.shell.as_mut() {
            let clear = st.renderer.is_none().then_some(wgpu::Color::BLACK);
            sh.st.live.clip = Some(view.clip_from_world(size.0 as f32 / size.1.max(1) as f32));
            sh.paint(&mut st.input, &target, size, clear);
        }
        if gamma {
            st.gamma.apply(&st.gpu, &surface_view, r_gamma);
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
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "cannot start the video recording");
                    st.notes.push(format!("video: {e}"));
                }
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
                Err(e) => {
                    tracing::error!(error = %e, "screenshot failed");
                    st.notes.push(format!("screenshot failed: {e}"));
                }
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
                if let Err(e) = std::fs::write(
                    dir.join("ui.json"),
                    serde_json::to_vec_pretty(&report).unwrap_or_default(),
                ) {
                    tracing::error!(error = %e, "cannot write ui.json");
                }
                el.exit();
            }
        }
        if let Some(req) = st.screenshot.take() {
            let said = take_screenshot(st, &frame.texture, &req);
            if let (Some(sh), Some(text)) = (st.shell.as_mut(), said) {
                sh.print_console(&text);
            }
        }
        let shot_ready = !st.shot_gate || st.distortion_now;
        if shot_ready && let Some(name) = st.shot_request.take() {
            st.shot_gate = false;
            let out = self.cli.out.clone().unwrap_or_default();
            if let Err(e) = std::fs::create_dir_all(&out) {
                tracing::error!(dir = %out.display(), error = %e, "cannot create the output directory");
            }
            let lit = save_png(
                &st.gpu,
                &frame.texture,
                st.config.format,
                &out.join(format!("{name}.png")),
            );
            if let Err(e) = &lit {
                tracing::error!(shot = %name, error = %e, "screenshot failed");
            }
            let ok = lit.is_ok();
            let hud = st.shell.as_ref().map(|s| s.st.game.hud.report());
            if let Some(sc) = st.script.as_mut() {
                sc.results.push(json!({"step": format!("shot={name}"), "ok": ok, "hud": hud,
                    "lit_fraction": lit.as_ref().ok(), "menus": st.shell.as_ref().map(|s| s.ui.open_menus().join(",")),
                    "weapon": st.net.as_ref().map(NetPlay::weapon),
                    "missiles": st.net.as_ref().map(NetPlay::missiles),
                    "explosions": st.net.as_ref().map(NetPlay::explosions)}));
                sc.index += 1;
                sc.began = Instant::now();
            }
        }
        if st.script.is_some() && script_step(st) {
            write_script_report(st, self.cli.out.clone().unwrap_or_default(), false);
            el.exit();
        }
        // A capture the channel asked for is taken from the frame that was just drawn; its request answers on the
        // next pass of the channel.
        if st.control_shot.is_some() {
            let shot = take_control_shot(&self.cli, st, &frame.texture);
            st.control_shot_done = Some(shot);
        }
        let t_present = Instant::now();
        st.prev_cost[6] = t_present.duration_since(t_ui).as_secs_f64() * 1000.0;
        st.gpu.queue.present(frame);
        st.presented = true;
        st.prev_cost[3] = t_present.elapsed().as_secs_f64() * 1000.0;
        if let Some(m) = st.menu_sound.as_mut() {
            m.set_volume(crate::sound::volume_of(&st.input.cvars));
            m.set_mix(crate::sound::mix_of(&st.input.cvars));
            m.frame([0.0; 3], 0.0, (interval / 1000.0) as f32);
        }
        // What commands said (a missing `exec` file, `dvarlist`) goes to the console.
        let said = st.input.take_messages();
        match st.shell.as_mut() {
            Some(sh) => said.iter().for_each(|l| sh.print_console(l)),
            None => said
                .iter()
                .for_each(|l| tracing::info!(text = %l, "command output")),
        }
        let actions = st
            .shell
            .as_mut()
            .map(Shell::drain_actions)
            .unwrap_or_default();
        self.apply_actions(el, actions)?;
        let st = self.st.as_mut().ok_or("no window")?;
        let cpu_ms = pre_ms + cpu_start.elapsed().as_secs_f64() * 1000.0;
        if st.samples.len() % 30 == 0 {
            st.rss = rss();
        }
        st.samples.push([cpu_ms, interval, st.rss as f64]);
        let c = st.prev_cost;
        let l = st.laps;
        let lap = |i: usize| {
            if i == 0 {
                l[0]
            } else {
                (l[i] - l[i - 1]).max(0.0)
            }
        };
        st.phases.push([
            c[0] as f32,
            c[2] as f32,
            c[1] as f32,
            c[3] as f32,
            st.late[0],
            st.late[1],
            st.late[2],
            st.late[3],
            lap(0),
            lap(1),
            lap(2),
            lap(3),
            lap(4),
            lap(5),
            lap(6),
            lap(7),
            lap(8),
            lap(9),
            c[4] as f32,
            c[5] as f32,
            c[6] as f32,
        ]);
        #[cfg(target_arch = "wasm32")]
        if st.overlay_at.elapsed() >= Duration::from_millis(500) {
            st.overlay_at = Instant::now();
            crate::web::overlay(&overlay_values(st));
        }
        if self.cli.timed() && t >= self.cli.duration {
            self.finish(el)?;
            el.exit();
        } else {
            next_frame(el, st);
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
                Action::StartServer {
                    map,
                    gametype,
                    cheats,
                } => {
                    // `SV_Map_f` looks for the map's zone before anything else (`DB_FileSize`, KisakCOD
                    // `server_mp/sv_ccmds_mp.cpp`): a map the install lacks starts no server and shows no loading
                    // screen. The original prints its `Can't find map` to the console; here it is the error popup's
                    // reason, so a menu player sees it too (#368). A match being played goes on, as there.
                    if assets::paths::zone_file(&self.cli.install, &game_of(&st.input), &map)
                        .is_none()
                    {
                        tracing::warn!(%map, "map not found: the install has no zone for it");
                        show_error(st, &format!("Can't find map \"{map}\"."));
                    } else {
                        let boot = loader::Server::Boot(listen_config(
                            st,
                            &map,
                            &gametype,
                            self.cli.bots,
                            cheats,
                        ));
                        start_session(&self.cli, &mut self.map, st, &map, &gametype, boot);
                    }
                }
                Action::Join(addr) => {
                    let _connect = tracing::info_span!("connect", %addr).entered();
                    tracing::info!("joining the server");
                    st.last_server = Some(addr.clone());
                    // The client loads what the server is playing, so it asks first.
                    let found = serverlist::resolve(&addr)
                        .and_then(|a| serverlist::query(a, JOIN_QUERY).map(|e| (a, e)));
                    match found {
                        Ok((_, e)) if e.lacks_password(st.input.cvar("password").unwrap_or("")) => {
                            // A passworded server and no password given: ask for one; the join is tried again from
                            // the menu once it is typed (`password_popmenu` edits the `password` dvar).
                            if let Some(sh) = st.shell.as_mut() {
                                sh.open(&mut st.input, "password_popmenu");
                            }
                        }
                        Ok((a, e)) => {
                            if let Err(why) = e.check_protocol() {
                                show_error(st, &why);
                                continue;
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            if crate::download::wanted(&self.cli.install, &e.game, &e.map) {
                                // The install may lack what the server runs (its mod, its map) or have other
                                // versions of it: ask the server for its files, if the player allows it.
                                let lacking =
                                    crate::download::lacks(&self.cli.install, &e.game, &e.map);
                                if st
                                    .input
                                    .cvar("cl_allowDownload")
                                    .is_some_and(|v| v.trim() == "0")
                                {
                                    if let Some(what) = lacking {
                                        show_error(
                                            st,
                                            &format!(
                                                "You do not have {what}, and downloading is turned off (cl_allowDownload)."
                                            ),
                                        );
                                        continue;
                                    }
                                } else {
                                    match crate::download::Download::start(
                                        &self.cli.install,
                                        a,
                                        &e.game,
                                        &e.map,
                                        &e.gametype,
                                    ) {
                                        Ok(d) => {
                                            end_session(&mut self.map, st);
                                            st.download = Some(d);
                                        }
                                        Err(err) => show_error(st, &err),
                                    }
                                    continue;
                                }
                            }
                            // The server's mod is the client's too (`CL_SystemInfoChanged` sets `fs_game`).
                            if let Err(why) =
                                follow_server_game(&self.cli, &mut self.map, st, &e.game)
                            {
                                show_error(st, &why);
                                continue;
                            }
                            start_session(
                                &self.cli,
                                &mut self.map,
                                st,
                                &e.map,
                                &e.gametype,
                                loader::Server::Join(a),
                            );
                        }
                        Err(err) => {
                            tracing::error!(%addr, error = %err, "cannot join");
                            show_error(st, &err);
                        }
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                Action::StartSingleplayer => {
                    match crate::shell::singleplayer_executable(
                        &self.cli.install,
                        cfg!(target_os = "windows"),
                    ) {
                        Ok(executable) => match std::fs::canonicalize(executable) {
                            Ok(executable) => {
                                self.start_singleplayer = Some(executable);
                                if st.script.is_some() {
                                    write_script_report(
                                        st,
                                        self.cli.out.clone().unwrap_or_default(),
                                        true,
                                    );
                                }
                                el.exit();
                            }
                            Err(err) => show_error(
                                st,
                                &format!("Could not locate Single Player executable: {err}"),
                            ),
                        },
                        Err(message) => show_error(st, &message),
                    }
                }
                #[cfg(target_arch = "wasm32")]
                Action::StartSingleplayer => {
                    if let Err(message) =
                        crate::shell::singleplayer_executable(&self.cli.install, false)
                    {
                        show_error(st, &message);
                    }
                }
                Action::PlayDemo(name) => {
                    if let Err(e) = play_demo(&self.cli, &mut self.map, st, &name) {
                        tracing::error!(demo = %name, error = %e, "cannot play the demo");
                        show_error(st, &format!("Cannot play the demo {name}: {e}"));
                    }
                }
                Action::Disconnect => {
                    tracing::info!("disconnect requested");
                    end_session(&mut self.map, st);
                    // An `fs_game` changed during the match applies now.
                    restart_files(&self.cli, st);
                }
                Action::MenuResponse { menu, response } => {
                    if let Some(n) = st.net.as_mut() {
                        n.send_command(&format!("menuresponse {menu} {response}"));
                    }
                }
                Action::Screenshot(r) => st.screenshot = Some(r),
                Action::VidRestart => {
                    tracing::info!("vid_restart requested");
                    vid_restart(&self.cli, st);
                    restart_files(&self.cli, st);
                }
                Action::Console(line) => console_action(st, &line),
                Action::CloseMenu(name) => {
                    if let Some(sh) = st.shell.as_mut() {
                        sh.close_by_name(&mut st.input, &name);
                    }
                }
                // The front end uses a separate sound system with MP, SP and localized code zones.
                // Match sound tables are loaded per map.
                Action::SoundLoop(alias) if !self.cli.no_sound => {
                    st.menu_sound
                        .get_or_insert_with(|| {
                            crate::sound::ClientSound::start(
                                &self.cli.install,
                                &game_of(&st.input),
                                "",
                                true,
                            )
                        })
                        .menu_loop(&alias);
                }
                Action::SoundLoop(_) => {}
                Action::SoundLoopStop(alias) => {
                    if let Some(s) = st.menu_sound.as_mut() {
                        s.menu_loop_stop(&alias);
                    }
                }
                Action::Sound(alias) => {
                    if !self.cli.no_sound {
                        st.menu_sound
                            .get_or_insert_with(|| {
                                crate::sound::ClientSound::start(
                                    &self.cli.install,
                                    &game_of(&st.input),
                                    "",
                                    true,
                                )
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
        // Where each frame's time went, to tell a hitch's cause: same rows as `frames.raw.csv` (the first dropped).
        let mut ph = std::io::BufWriter::new(
            std::fs::File::create(out.join("frames.phases.csv")).map_err(|e| e.to_string())?,
        );
        writeln!(ph, "{PHASES}").map_err(|e| e.to_string())?;
        for p in st.phases.iter().skip(1) {
            let row: Vec<String> = p.iter().map(|v| format!("{v:.3}")).collect();
            writeln!(ph, "{}", row.join(",")).map_err(|e| e.to_string())?;
        }
        ph.flush().map_err(|e| e.to_string())?;
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
                "fullscreen": display::actual_kind(&st.window).name(),
                "fullscreen_requested": self.cli.request.kind.name(),
                "present_mode": format!("{:?}", st.present_mode).to_lowercase(),
            },
            "frames": st.samples.len(),
            "late_builds": st.late_names.iter().map(|(n, w)| format!("{n} {w}")).collect::<Vec<_>>(),
            "video": video,
            "screenshot": st.shot_ok.then_some("screenshot.png"),
            "zone_load_ms": self.load_ms,
            "world": {"surfaces_drawn_p50": drawn.get(drawn.len() / 2)},
            "showcase": st.showcase.as_ref().map(Showcase::report),
            "notes": st.notes,
            "gpu_health": st.gpu_log.report(&st.gpu),
            "render": render_report(st),
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
    let gpu_health = st.gpu_log.report(&st.gpu);
    let report = json!({
        "gpu_health": gpu_health,
        "render": render_report(st),
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
    if let Err(e) = std::fs::create_dir_all(&out).and_then(|()| {
        std::fs::write(
            out.join("ui-script.json"),
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        )
    }) {
        tracing::error!(dir = %out.display(), error = %e, "cannot write ui-script.json");
    }
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
        // A listen server on the map, joined with the team and class answered by itself (`start=mp_crash`);
        // `devmap=mp_crash` is the same with cheats on, as the console's `devmap`.
        "start" | "devmap" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            sh.st.actions.push(Action::StartServer {
                map: arg.to_owned(),
                gametype: "war".to_owned(),
                cheats: key == "devmap",
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
                sc.died |= matches!(key, "killcam" | "dead");
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
            if sc.marker.is_none() && (h.pm_dead || sc.died) {
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
        // `loc=MESSAGE_KEY:HINT_KEY`: puts a bare-key message (as `iprintlnbold(&"KEY")` sends it) through the print,
        // announcement and chat paths and holds a use-trigger hint of the second key until it has been drawn. Fails if
        // either shows as a key, or the hint was never drawn.
        "loc" => {
            let Some(sh) = st.shell.as_mut() else {
                return true;
            };
            let (msg, hint) = arg.split_once(':').unwrap_or((arg, ""));
            let drawn = |sh: &Shell| sh.st.game.hud.drawn.get(&72).copied().unwrap_or(0);
            if sc.marker.is_none() {
                sc.marker = Some(drawn(sh).to_string());
                use net::ui::{PrintKind, UiEvent};
                for ev in [
                    UiEvent::Print {
                        kind: PrintKind::Bold,
                        text: msg.to_owned(),
                    },
                    UiEvent::Announce {
                        text: msg.to_owned(),
                    },
                    UiEvent::Chat {
                        team: false,
                        client: 0,
                        tag: net::ui::chat_tag::NORMAL,
                        text: msg.to_owned(),
                    },
                ] {
                    sh.apply(&mut st.input, ev);
                }
            }
            let before: u32 = sc
                .marker
                .as_deref()
                .and_then(|m| m.parse().ok())
                .unwrap_or(0);
            let h = &mut sh.st.game.hud;
            // Dated a second ahead: the hint lasts 100 ms, and a slow frame must not outrun it.
            (h.cursor_hint, h.cursor_hint_time) = (2, h.now + 1000);
            h.cursor_hint_text = hint.to_owned();
            let hint_drawn = drawn(sh) > before;
            if hint_drawn || waited > 10.0 {
                let shown = crate::hud::localize(&sh.ui.assets, msg);
                let h = &sh.st.game.hud;
                let unresolved = &sh.st.hud_stats.unresolved;
                let ok = shown != msg
                    && hint_drawn
                    && unresolved.is_empty()
                    && h.unresolved_text.is_empty();
                done(
                    ok,
                    sc,
                    format!(
                        "message {shown:?}; hint drawn {hint_drawn}; unresolved {unresolved:?} {:?}",
                        h.unresolved_text
                    ),
                );
                sc.marker = None;
            }
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
        // Presses and releases a key as a player's hand does (`holdkey=mouse2` aims down the sights). A key the
        // config binds to nothing does nothing, so it fails the step instead of reporting a press that went nowhere.
        "holdkey" | "releasekey" => {
            let bound = st.input.bound(arg).map(str::to_owned);
            match bound {
                Some(cmd) => {
                    st.input.key(arg, key == "holdkey");
                    done(true, sc, format!("{key} {arg} ({cmd})"));
                }
                None => done(false, sc, format!("{key} {arg}: the key is not bound")),
            }
        }
        // Throws a grenade as a player does (`throw=g:10`: the key, at most that many seconds): presses the key,
        // lets go after [`THROW_HOLD`], then waits until the HUD's grenade count dropped, a missile entity appeared
        // and the player is still alive. Holding until the count dropped let a frag cook off in the hand and count
        // as a throw (#375).
        "throw" => {
            let (name, secs) = arg
                .split_once(':')
                .map_or((arg, 10.0), |(k, s)| (k, s.parse().unwrap_or(10.0)));
            let (now, dead) = st.shell.as_ref().map_or(((None, None), true), |s| {
                let h = &s.st.game.hud;
                (
                    (
                        h.frag.as_ref().map(|o| o.ammo),
                        h.second.as_ref().map(|o| o.ammo),
                    ),
                    h.pm_dead || h.health <= 0,
                )
            });
            let missiles = st.net.as_ref().map_or(0, NetPlay::missiles_appeared);
            match sc.throw.as_mut() {
                None => {
                    if st.input.bound(name).is_none() {
                        done(false, sc, format!("{name} is not bound"));
                    } else {
                        sc.throw = Some(Throw {
                            before: now,
                            missiles,
                            released: false,
                        });
                        st.input.key(name, true);
                    }
                }
                Some(t) if !t.released && waited >= THROW_HOLD => {
                    t.released = true;
                    st.input.key(name, false);
                }
                Some(t) => {
                    let thrown = t.thrown(now, missiles, dead);
                    if thrown {
                        let note = format!(
                            "{:?} -> {now:?}, {} new missile(s)",
                            t.before,
                            missiles - t.missiles
                        );
                        sc.throw = None;
                        done(true, sc, note);
                    } else if waited > secs {
                        st.input.key(name, false);
                        let note = format!(
                            "no throw: counts {:?} -> {now:?}, {} new missile(s), dead {dead}",
                            t.before,
                            missiles - t.missiles
                        );
                        sc.throw = None;
                        done(false, sc, note);
                    }
                }
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
        // `team=save` remembers the team; `team=other:secs` waits for the other side (a spectator or an unassigned
        // player is on neither, so dying does not satisfy it, #375), `team=same:secs` for the remembered one.
        // `weapon=<name>:secs` waits for the weapon in hand whose name starts with `<name>` (`m16_gl` is the rifle,
        // `gl_m16` its launcher raised; a part anywhere in the name matched the rifle for the launcher, #375).
        "team" | "weapon" => {
            let (want, secs) = arg
                .split_once(':')
                .map_or((arg, 20.0), |(n, s)| (n, s.parse().unwrap_or(20.0)));
            let saved = sc.team_saved.clone();
            let have = if key == "team" {
                st.shell
                    .as_ref()
                    .map_or(String::new(), |s| s.st.live.own_team.to_string())
            } else {
                st.net.as_ref().map(NetPlay::weapon).unwrap_or_default()
            };
            let matched = if key == "team" {
                team_matches(want, &have, saved.as_deref())
            } else {
                have.starts_with(want)
            };
            if key == "team" && want == "save" {
                sc.team_saved = Some(have.clone());
                done(true, sc, format!("team {have}"));
            } else if matched {
                done(true, sc, format!("{key} {have}"));
            } else if waited > secs {
                done(false, sc, format!("timed out; {key} is {have:?}"));
            }
        }
        // A dvar the server set on the client (`setclientdvar`), remembered and compared across rounds:
        // `cvar=save cg_objectiveText` keeps its value; `cvar=same cg_objectiveText:5` and `cvar=other ...:5` wait up
        // to the seconds for the value to be the remembered one, or a different one.
        "cvar" => {
            let (what, rest) = arg.split_once(' ').unwrap_or((arg, ""));
            let (name, secs) = rest
                .split_once(':')
                .map_or((rest, 5.0), |(n, s)| (n, s.parse().unwrap_or(5.0)));
            let have = st.input.cvar(name).unwrap_or("").to_owned();
            let saved = sc.saved.get(name).cloned();
            match what {
                "save" => {
                    sc.saved.insert(name.to_owned(), have.clone());
                    done(true, sc, format!("{name} is {have:?}"));
                }
                "same" | "other" => {
                    let ok = saved
                        .as_ref()
                        .is_some_and(|s| (*s == have) == (what == "same"));
                    if ok {
                        done(true, sc, format!("{name} is {have:?}"));
                    } else if waited > secs {
                        done(
                            false,
                            sc,
                            format!("timed out; {name} is {have:?}, saved {saved:?}"),
                        );
                    }
                }
                _ => done(false, sc, format!("unknown cvar step {what:?}")),
            }
        }
        // Waits for the next level to begin (a round restart or a map change) since the step started (`round=next:150`):
        // the player is then out of the world again until the server spawns them (`ingame=` waits for that).
        "round" => {
            let (_, secs) = arg
                .split_once(':')
                .map_or((arg, 150.0), |(n, s)| (n, s.parse().unwrap_or(150.0)));
            let have = st.net.as_ref().map_or(0, NetPlay::levels);
            let from = sc
                .saved
                .entry("round".into())
                .or_insert_with(|| have.to_string())
                .parse::<u32>()
                .unwrap_or(have);
            if have > from {
                sc.saved.remove("round");
                done(true, sc, format!("level {from} -> {have}"));
            } else if waited > secs {
                sc.saved.remove("round");
                done(false, sc, format!("timed out; still in level {have}"));
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
        // A cvar's value against a localized string's (`cvarloc=customclass3 CLASS_CUSTOM3`).
        "cvarloc" => {
            let (name, key) = arg.split_once(' ').unwrap_or((arg, ""));
            let want = st
                .shell
                .as_ref()
                .and_then(|s| s.ui.assets.translate_ref(key))
                .map(|t| t.to_string())
                .unwrap_or_default();
            let have = st.input.cvar(name).unwrap_or("").to_owned();
            done(
                !want.is_empty() && have == want,
                sc,
                format!("{name} is {have:?}, {key} is {want:?}"),
            );
        }
        // The view pitch in degrees (negative looks down), within a range (`pitchis=-90 -20`).
        "pitchis" => {
            let mut it = arg
                .split_whitespace()
                .map(|v| v.parse::<f32>().unwrap_or(0.0));
            let (lo, hi) = (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0));
            let pitch = st.pitch.to_degrees();
            done(
                (lo..=hi).contains(&pitch),
                sc,
                format!("pitch {pitch:.1} (want {lo}..{hi})"),
            );
        }
        // A character key to the menus, as a key press reaches an open menu (`uikey=1`).
        "uikey" => {
            if let (Some(sh), Some(c)) = (st.shell.as_mut(), arg.chars().next()) {
                sh.key(&mut st.input, UiKey::Char(c));
            }
            done(true, sc, format!("key {arg}"));
        }
        // A cvar's value (`cvaris=ui_netGametypeName dom`).
        "cvaris" => {
            let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
            let have = st.input.cvar(name).unwrap_or("").to_owned();
            done(have == want, sc, format!("{name} is {have:?}"));
        }
        // The rows a list shows: `rows=2 1` is exactly one, `rows=2 +1` at least one; a list may take up to the
        // seconds after the colon (`rows=2 +1:15`, default 5) to get there, as a refresh takes its time.
        "rows" => {
            let (feeder, rest) = arg.split_once(' ').unwrap_or((arg, "0"));
            let (want, secs) = rest
                .split_once(':')
                .map_or((rest, 5.0), |(w, s)| (w, s.parse().unwrap_or(5.0)));
            let have = st.shell.as_mut().map_or(0, |sh| {
                sh.feeder_rows(&mut st.input, feeder.parse().unwrap_or(0))
            });
            let ok = match want.strip_prefix('+') {
                Some(n) => have >= n.parse().unwrap_or(1),
                None => have.to_string() == want,
            };
            if ok || waited > secs {
                done(ok, sc, format!("feeder {feeder} has {have} rows"));
            }
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
        // Waits until the scoreboard lists at least this many players (`players=13:30`, at most that many seconds):
        // the server's own roster, not what is in view. It holds the scoreboard up meanwhile and lets go after.
        "players" => {
            let (n, secs) = arg
                .split_once(':')
                .map_or((arg, 10.0), |(n, s)| (n, s.parse().unwrap_or(10.0)));
            let want = n.parse::<usize>().unwrap_or(usize::MAX);
            if let Some(sh) = st.shell.as_mut() {
                if sh.st.live.scores.len() >= want {
                    sh.st.scores_forced = false;
                    done(true, sc, String::new());
                } else if waited > secs {
                    sh.st.scores_forced = false;
                    let have = sh.st.live.scores.len();
                    done(
                        false,
                        sc,
                        format!("timed out; {have} of {want} players listed"),
                    );
                } else {
                    sh.st.scores_forced = true;
                }
            }
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
        // As `saw=`, but not a typed line's echo (`] …`): what the server or the engine answered
        // (`sawreply=zz_word:5`).
        "sawreply" => {
            let (text, secs) = arg
                .rsplit_once(':')
                .map_or((arg, 10.0), |(t, s)| (t, s.parse().unwrap_or(10.0)));
            let seen = st.shell.as_ref().is_some_and(|s| {
                s.st.feed
                    .console
                    .iter()
                    .any(|l| l.contains(text) && !l.starts_with("] "))
            });
            if seen {
                done(true, sc, String::new());
            } else if waited > secs {
                done(false, sc, format!("timed out; no reply line with {text:?}"));
            }
        }
        // Assert an output line is absent now, after the scenario waited for the command to finish.
        "notsaw" => {
            let seen = st
                .shell
                .as_ref()
                .is_some_and(|s| s.st.feed.console.iter().any(|l| l.contains(arg)));
            done(!seen, sc, format!("console contains {arg:?}: {seen}"));
        }
        // A line for the listen server's console, as typed there: `server=devtele human bombzone`.
        "server" => {
            listen::send(arg);
            done(true, sc, String::new());
        }
        // The graphics device is destroyed under the client, as a driver reset does it: the next frame must find it lost
        // and make a new one (`losedevice`); `renderer=secs` then waits for the world to be drawable again.
        "losedevice" => {
            st.gpu.device.destroy();
            done(true, sc, String::new());
        }
        "renderer" => {
            if st.renderer.is_some() {
                done(true, sc, String::new());
            } else if waited > arg.parse::<f32>().unwrap_or(60.0) {
                done(false, sc, "no renderer".into());
            }
        }
        // Counts the frames from here, for `gfxis=pace 1`.
        "pacereset" => {
            st.pace = (Instant::now(), 0);
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
                // Whether the frame is blurred by depth of field: the scripts' or the sights' range, and `r_dof_enable`.
                "blur" => st.renderer.as_ref().map(|r| {
                    u8::from(r.settings.dof && r.post.dof.is_some_and(|d| d.active())).to_string()
                }),
                "glow" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.glow).to_string()),
                "shadows" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.shadows != render::ShadowMode::Off).to_string()),
                // The sampler filtering the materials were made with: `r_texFilterAnisoMin` and `Max`, and
                // `r_texFilterMipMode` as its enum index (`gfxis=aniso 2 4`, `gfxis=mipmode 2`).
                "aniso" => st
                    .renderer
                    .as_ref()
                    .map(|r| format!("{} {}", r.materials.aniso_min, r.materials.aniso_max)),
                "mipmode" => st
                    .renderer
                    .as_ref()
                    .map(|r| (r.materials.mip_mode as u8).to_string()),
                // How many dynamic lights the last frame drew.
                "dlights" => Some(st.lights_drawn.to_string()),
                "aspect" => Some(format!("{:.2}", st.aspect.unwrap_or(0.0))),
                "fullscreen" => Some(u8::from(st.window.fullscreen().is_some()).to_string()),
                // 1 when the surface does not wait for the display, or has no mode that does not.
                "uncapped" => {
                    let caps = st
                        .surface
                        .as_ref()
                        .map(|s| s.get_capabilities(&st.gpu.adapter))
                        .unwrap_or_default();
                    let free = caps
                        .present_modes
                        .iter()
                        .any(|m| *m != wgpu::PresentMode::Fifo);
                    Some(u8::from(st.present_mode != wgpu::PresentMode::Fifo || !free).to_string())
                }
                "zfeather" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.z_feather).to_string()),
                "water" => st
                    .renderer
                    .as_ref()
                    .map(|r| u8::from(r.settings.draw_water).to_string()),
                // The picmips of colour, normal and specular maps the match's textures were loaded with.
                "picmip" => st.renderer.as_ref().map(|r| {
                    let [c, b, s] = r.textures.picmips();
                    format!("{c} {b} {s}")
                }),
                // 1 when the frames since `pacereset` came no faster than `com_maxfps` allows (a fifth of slack). The
                // check needs a present mode that does not wait for the display (`r_vsync 0`): it fails when the
                // display paces the frames although the surface offers a mode that would not.
                "pace" => {
                    let (since, frames) = st.pace;
                    let free = st.surface.as_ref().is_some_and(|s| {
                        s.get_capabilities(&st.gpu.adapter)
                            .present_modes
                            .iter()
                            .any(|m| !display_paced(*m))
                    });
                    Some(
                        match gfx::frame_gap(&st.input.cvars, display_paced(st.present_mode)) {
                            Some(gap) => {
                                let each = since.elapsed().as_secs_f64() / f64::from(frames.max(1));
                                u8::from(frames >= 2 && each >= gap.as_secs_f64() * 0.8)
                            }
                            // Fifo is all the surface has: the display paces, the cap cannot be shown.
                            None => u8::from(!free),
                        }
                        .to_string(),
                    )
                }
                _ => None,
            };
            // `gfxis=aa max`: the most samples the adapter offers for the scene's colour and depth, whatever was asked.
            let want = if (name, want) == ("aa", "max") {
                let beyond_spec = st
                    .gpu
                    .device
                    .features()
                    .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
                let ok = |f: wgpu::TextureFormat, n: u32| {
                    (n <= 4 || beyond_spec)
                        && st
                            .gpu
                            .adapter
                            .get_texture_format_features(f)
                            .flags
                            .sample_count_supported(n)
                };
                [16u32, 8, 4, 2]
                    .into_iter()
                    .find(|&n| ok(st.config.format, n) && ok(wgpu::TextureFormat::Depth32Float, n))
                    .unwrap_or(1)
                    .to_string()
            } else {
                want.to_owned()
            };
            // `gfxis=dlights 1+`: at least that many, for a count the scene's own lights add to.
            match have {
                Some(h) => {
                    let ok = match want.strip_suffix('+') {
                        Some(min) => match (h.parse::<u64>(), min.parse::<u64>()) {
                            (Ok(h), Ok(min)) => h >= min,
                            _ => false,
                        },
                        None => h == want,
                    };
                    done(ok, sc, format!("{name} is {h}"));
                }
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
        // The step starts only once a map is loaded: with none there is nothing to rotate away from.
        "maprotate" => {
            let secs: f32 = arg.parse().unwrap_or(300.0);
            if sc.rotate_from.is_none() && !st.map_name.is_empty() {
                sc.rotate_from = Some(st.map_name.clone());
            }
            match sc.rotate_from.clone() {
                Some(from)
                    if st.net.is_some() && !st.map_name.is_empty() && st.map_name != from =>
                {
                    sc.rotate_from = None;
                    done(true, sc, format!("{from} -> {}", st.map_name));
                }
                Some(from) if waited > secs => {
                    sc.rotate_from = None;
                    done(false, sc, format!("timed out; still in {from}"));
                }
                None if waited > secs => done(false, sc, "timed out; no map was loaded".into()),
                _ => {}
            }
        }
        // `dlight=1` adds a bright light in front of the camera each frame, `dlight=0` removes it.
        "dlight" => {
            st.test_light = arg.trim() != "0";
            done(true, sc, String::new());
        }
        // `shotd=<name>`: the screenshot of the first frame that copied the scene for a distortion material.
        "shotd" => {
            if st.shot_request.is_none() {
                st.shot_gate = true;
                st.shot_request = Some(arg.to_owned());
            } else if waited > 10.0 {
                st.shot_gate = false;
                st.shot_request = None;
                done(false, sc, "no frame drew a distortion material".into());
            }
        }
        // `fxdemo=<effect>` plays the effect in front of the player every 1.5 s, `fxdemo=off` stops it.
        "fxdemo" => {
            if let Some(n) = st.net.as_mut() {
                n.set_fx_demo((arg != "off").then(|| arg.to_owned()));
            }
            done(st.net.is_some(), sc, "fx demo".into());
        }
        // What the cgame dvars came to last frame (`cgis=gun 0`, `cgis=hud2d 0`, `cgis=fps 1`,
        // `cgis=fullbright 1`, `cgis=shock 1` for the shellshock blend, `cgis=subtitle 1` for a subtitle up) or
        // how often an effect kind played (`cgis=fx:shell_eject 0`, `cgis=fx:bullet_impact_noblood 1`): what was
        // drawn or played, not the dvar's value.
        "cgis" => {
            let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
            let sh = st.shell.as_ref();
            let have =
                match name {
                    "gun" => st.net.as_ref().map(|n| u8::from(n.gun_drawn()).to_string()),
                    "hud2d" => sh.map(|s| u8::from(s.st.ownerdraws_painted > 0).to_string()),
                    "fps" => sh.map(|s| u8::from(s.st.fps_drawn.is_some()).to_string()),
                    "fpstext" => sh.map(|s| s.st.fps_drawn.clone().unwrap_or_default()),
                    "fullbright" => st
                        .renderer
                        .as_ref()
                        .map(|r| u8::from(r.settings.fullbright).to_string()),
                    "shock" => st
                        .renderer
                        .as_ref()
                        .map(|r| u8::from(r.post.shell_shock.is_some()).to_string()),
                    // Console-driven checks of the centre print duration, the killcam builders and film output.
                    "centertime" => sh.map(|s| s.st.feed.center_ms.to_string()),
                    "heli_camera" => st.net.as_ref().map(|n| n.killcam_sample(true)),
                    "air_camera" => st.net.as_ref().map(|n| n.killcam_sample(false)),
                    "brightness" => st.renderer.as_ref().zip(st.film_base).map(|(r, base)| {
                        format!("{:.2}", r.post.film.brightness - base.brightness)
                    }),
                    "film_sample" => st.film_base.map(|mut base| {
                        base.desaturation = 0.5;
                        base.contrast = 1.0;
                        base.brightness = 0.0;
                        let film = crate::gfx::film(&st.input.cvars, base);
                        format!(
                            "{:.2} {:.2} {:.2}",
                            film.brightness, film.contrast, film.desaturation
                        )
                    }),
                    "crosshair_alpha" => sh
                        .and_then(|s| s.st.live.reticle.as_ref())
                        .map(|r| format!("{:.2}", crate::crosshair::layout(r).0)),
                    "window_position" => {
                        // Wayland deliberately offers no global position; there assert the request that reaches
                        // display::apply. Platforms exposing outer_position must report the actual placement.
                        let p = st
                            .window
                            .outer_position()
                            .ok()
                            .map(|p| (p.x, p.y))
                            .or_else(|| {
                                Gfx::from_cvars(&st.input.cvars)
                                    .request(&display::Request::default())
                                    .position
                            });
                        p.map(|(x, y)| format!("{x} {y}"))
                    }
                    "subtitle" => sh.map(|s| {
                        u8::from(s.st.feed.active(crate::hud::SUBTITLE, s.st.live.time)).to_string()
                    }),
                    _ => name.strip_prefix("fx:").and_then(|kind| {
                        st.net.as_ref().map(|n| {
                            let played = n.fx_played(kind);
                            match want.strip_prefix('+') {
                                Some(n_str) => {
                                    let target = n_str.parse::<u64>().unwrap_or(u64::MAX);
                                    let at = st.fx_mark.get(kind).copied().unwrap_or(0);
                                    let delta = played.saturating_sub(at);
                                    if delta == target {
                                        "ok".to_owned()
                                    } else {
                                        format!("+{delta}")
                                    }
                                }
                                None => played.to_string(),
                            }
                        })
                    }),
                };
            match have {
                Some(h) => done(
                    if want.starts_with('+') {
                        h == "ok"
                    } else {
                        h == want
                    },
                    sc,
                    format!("{name} is {h}"),
                ),
                None => done(false, sc, format!("cgis: unknown or unavailable {name:?}")),
            }
        }
        // `fxmark` remembers every effect count; `cgis=fx:<kind> +n` afterwards compares what played since it
        // (exactly n, so a wrong effect kind fails as much as a missing one).
        "fxmark" => {
            if let Some(n) = st.net.as_ref() {
                st.fx_mark = n.fx_counts();
            }
            done(st.net.is_some(), sc, "fx marked".into());
        }
        // A bullet of the weapon in hand strikes a player 64 units ahead on the next frame (`hitflesh`): the flesh
        // impact effect, or the no-blood one with `cg_blood` off.
        "hitflesh" => {
            if let Some(n) = st.net.as_mut() {
                n.inject_flesh_hit();
            }
            done(st.net.is_some(), sc, "flesh hit".into());
        }
        // A `shellshock` of the default kind for `shock=<ms>` ms, as a nearby explosion's; `subtitle=<key>` is a
        // started sound's subtitle (`CL_SubtitlePrint`), both as the server or the sound engine would hand them on.
        "shock" => {
            if let Some(n) = st.net.as_mut() {
                n.inject_shock(arg.parse().unwrap_or(5000));
            }
            done(st.net.is_some(), sc, "shellshock".into());
        }
        "subtitle" => {
            if let Some(n) = st.net.as_mut() {
                n.inject_subtitle(arg, 2000);
            }
            done(st.net.is_some(), sc, "subtitle".into());
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
        // The tour has no match, so the scoreboard would draw only its header: hold it up over synthetic rows.
        sh.st.scores_forced = t.menus.get(t.index) == Some(&"scoreboard");
        sh.st.live.own_team = 2;
        sh.st.live.active = sh.st.scores_forced;
        sh.st.in_game = sh.st.scores_forced;
        sh.st.live.scores = if sh.st.scores_forced {
            (0..12)
                .map(|i| crate::hud::ScoreLine {
                    client: i,
                    name: format!("Player{i}"),
                    team: if i % 2 == 0 { 2 } else { 1 },
                    score: 100 - i32::from(i) * 7,
                    kills: 10 - i32::from(i) / 2,
                    deaths: i32::from(i) / 3,
                    ping: 30 + i32::from(i),
                    ..Default::default()
                })
                .collect()
        } else {
            Vec::new()
        };
        match t.menus.get(t.index) {
            Some(m) => sh.open(input, m),
            None => return true,
        }
        // The join menu's tab highlight is set by `setitemcolor <group> backcolor`: the tour sets one and checks that
        // the item took it (reported as `setitemcolor`).
        if t.menus.get(t.index) == Some(&"pc_join_unranked") {
            sh.run_menu_script(
                input,
                "pc_join_unranked",
                "setitemcolor grpTabs backcolor 1 0 0 1",
            );
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
        let mut entry = json!({"menu": name, "open": sh.ui.is_open(name), "lit_fraction": lit, "screenshot": format!("{name}.png")});
        if name == "pc_join_unranked" {
            let colors = sh.ui.item_back_colors(name, "grpTabs");
            entry["setitemcolor"] =
                json!(!colors.is_empty() && colors.iter().all(|c| *c == [1.0, 0.0, 0.0, 1.0]));
        }
        t.results.push(entry);
    }
    t.index += 1;
    t.frames = 0;
    t.index >= t.menus.len()
}

/// What a key press types or does in a text field: a named key, else the characters it produced.
fn typed_ui_keys(ev: &crate::input::InputEvent) -> Vec<UiKey> {
    use winit::keyboard::{Key, NamedKey};
    match ev {
        crate::input::InputEvent::Text(t) => t.chars().map(UiKey::Char).collect(),
        crate::input::InputEvent::Key {
            logical,
            text,
            state: ElementState::Pressed,
            ..
        } => {
            let named = match logical {
                Some(Key::Named(NamedKey::ArrowUp)) => Some(UiKey::Up),
                Some(Key::Named(NamedKey::ArrowDown)) => Some(UiKey::Down),
                Some(Key::Named(NamedKey::ArrowLeft)) => Some(UiKey::Left),
                Some(Key::Named(NamedKey::ArrowRight)) => Some(UiKey::Right),
                Some(Key::Named(NamedKey::Enter)) => Some(UiKey::Enter),
                Some(Key::Named(NamedKey::Escape)) => Some(UiKey::Escape),
                Some(Key::Named(NamedKey::Tab)) => Some(UiKey::Tab),
                Some(Key::Named(NamedKey::Backspace)) => Some(UiKey::Backspace),
                Some(Key::Named(NamedKey::Delete)) => Some(UiKey::Delete),
                Some(Key::Named(NamedKey::Insert)) => Some(UiKey::Insert),
                Some(Key::Named(NamedKey::Home)) => Some(UiKey::Home),
                Some(Key::Named(NamedKey::End)) => Some(UiKey::End),
                Some(Key::Named(NamedKey::PageUp)) => Some(UiKey::PageUp),
                Some(Key::Named(NamedKey::PageDown)) => Some(UiKey::PageDown),
                _ => None,
            };
            match (named, text) {
                (Some(k), _) => vec![k],
                (None, Some(t)) => t.chars().map(UiKey::Char).collect(),
                (None, None) => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// The window's event as the engine's own, for the queue: the fields the engine reads, and no more.
fn translate(ev: &WindowEvent) -> Option<crate::input::InputEvent> {
    use crate::input::InputEvent;
    use winit::event::MouseScrollDelta;
    Some(match ev {
        WindowEvent::KeyboardInput { event, .. } => {
            let PhysicalKey::Code(code) = event.physical_key else {
                return None;
            };
            InputEvent::Key {
                code,
                logical: Some(event.logical_key.clone()),
                text: event.text.as_ref().map(|t| t.to_string()),
                state: event.state,
                repeat: event.repeat,
            }
        }
        WindowEvent::MouseInput { state, button, .. } => InputEvent::Button {
            button: *button,
            state: *state,
        },
        WindowEvent::MouseWheel { delta, .. } => {
            let y = match delta {
                MouseScrollDelta::LineDelta(_, y) => *y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            if y == 0.0 {
                return None;
            }
            InputEvent::Wheel { y }
        }
        WindowEvent::CursorMoved { position, .. } => InputEvent::Cursor {
            x: position.x as f32,
            y: position.y as f32,
        },
        WindowEvent::Focused(f) => InputEvent::Focus(*f),
        _ => return None,
    })
}

/// Routes one input event the way the player's own would go: the console's key, then the console's typing, the
/// menus, or the game. The window's events and the control channel's both come through here.
fn dispatch_input(st: &mut State, ev: &crate::input::InputEvent) {
    use crate::input::InputEvent;
    if let InputEvent::Key {
        code: KeyCode::Backquote,
        state: ElementState::Pressed,
        repeat: false,
        ..
    } = ev
        && let Some(sh) = st.shell.as_mut()
    {
        // `~` opens and closes the console wherever the player is; its key is never typed into it.
        sh.st.console.toggle();
        st.input.release_all();
        return;
    }
    let typing = st.shell.as_ref().is_some_and(|s| s.st.console.active());
    let captured = st.shell.as_ref().is_some_and(|s| s.ui.captures_input());
    // The scoreboard held up by `+scores` takes the scroll keys and nothing else (`CL_KeyEvent`: a key down that
    // `Scoreboard_HandleInput` scrolls with goes no further); every other key stays the game's, so the bind's release
    // reaches the bind table and lets the scoreboard go.
    let scoreboard = st.shell.as_ref().is_some_and(|s| s.ui.scoreboard_only());
    if typing {
        if let Some(sh) = st.shell.as_mut() {
            for k in typed_ui_keys(ev) {
                sh.console_key(&mut st.input, k);
            }
        }
    } else if captured {
        ui_event(st, ev);
    } else {
        if scoreboard && let Some(sh) = st.shell.as_mut() {
            let keys = match ev {
                InputEvent::Wheel { y } => vec![if *y > 0.0 {
                    UiKey::WheelUp
                } else {
                    UiKey::WheelDown
                }],
                InputEvent::Key {
                    state: ElementState::Pressed,
                    ..
                } => typed_ui_keys(ev),
                _ => Vec::new(),
            };
            let mut scrolled = false;
            for k in keys {
                scrolled |= sh.st.scroll_scores(&sh.ui, &k);
            }
            if scrolled {
                return;
            }
        }
        st.input.event(ev);
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

/// `screenshot` and `screenshotJpeg` from a bind (F12) or the console.
fn screenshot_commands(shell: Option<&mut Shell>, f: &InputFrame) {
    let Some(sh) = shell else { return };
    for c in &f.pending_commands {
        for cmd in crate::input::config::split_commands(c) {
            let format = match cmd[0].to_ascii_lowercase().as_str() {
                "screenshot" => crate::screenshot::Format::Tga,
                "screenshotjpeg" => crate::screenshot::Format::Jpeg,
                _ => continue,
            };
            if let Some(r) = crate::screenshot::Request::parse(format, &cmd[1..]) {
                sh.st.actions.push(Action::Screenshot(r));
            }
        }
    }
}

/// The app's own commands (`bots`, `rcon`, `name`, `record`, `cmd`, ...) and every word nobody runs, from a bind or
/// a config line: `console_action` carries them out or forwards them (the input layer only hands them over).
fn own_commands(f: &InputFrame) -> Vec<Action> {
    f.pending_commands
        .iter()
        .flat_map(|c| crate::input::config::split_commands(c))
        .filter(|cmd| crate::console::is_own_verb(&cmd[0]) || !crate::console::is_command(&cmd[0]))
        .map(|cmd| Action::Console(crate::input::config::join(&cmd)))
        .collect()
}

#[cfg(test)]
mod own_command_tests {
    use super::*;

    /// A bound word nothing knows is the app's to forward or call unknown; the input layer's and the net play's
    /// own commands are not.
    #[test]
    fn unknown_words_are_console_actions_and_known_ones_are_not() {
        let mut input = Input::detached();
        input.exec_line("zz_unknown_word a b; weapnext; record d1; set zz_cv 1; zz_cv 2");
        let lines: Vec<String> = own_commands(&input.frame(0.01))
            .into_iter()
            .map(|a| match a {
                Action::Console(l) => l,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(lines, ["zz_unknown_word a b", "record d1"]);
        assert_eq!(input.cvar("zz_cv"), Some("2"));
    }
}

/// The shell's commands (`setfromdvar`, `vid_restart`, `setFromLocString`, ...) from a bind: run through its
/// command loop, as a menu's would be.
fn shell_commands(shell: Option<&mut Shell>, input: &mut Input, f: &InputFrame) {
    let Some(sh) = shell else { return };
    for cmd in f
        .pending_commands
        .iter()
        .flat_map(|c| crate::input::config::split_commands(c))
        .filter(|cmd| crate::console::is_shell_verb(&cmd[0]))
    {
        sh.exec_line(input, &crate::input::config::join(&cmd));
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
    (get("rate", 90_000), get("snaps", 30))
}

/// The `bot_count` setting (archived): the bots a menu-started server plays with on every map.
fn bot_count(input: &Input) -> usize {
    input
        .cvar("bot_count")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(64)
}

/// What a listen server started before the config was read is told of `bot_count`: nothing when `--bots` was given.
fn saved_bot_count_lines(bots_flag: Option<usize>, input: &Input) -> Vec<String> {
    let n = bot_count(input);
    if bots_flag.is_some() || n == 0 {
        return Vec::new();
    }
    vec![format!("set bot_count {n}"), format!("bots {n}")]
}

fn console_action(st: &mut State, line: &str) {
    for cmd in crate::input::config::split_commands(line) {
        let _command = tracing::debug_span!("command", name = %cmd[0]).entered();
        let verb = cmd[0].to_ascii_lowercase();
        match verb.as_str() {
            "bots" => {
                // `bots <n>` brings the listen server's bots to n at once (the command is the listen server's console's).
                match (
                    cmd.get(1).and_then(|n| n.parse::<usize>().ok()),
                    st.listen.is_some(),
                ) {
                    (Some(n), true) => {
                        // The count is the player's setting too: the next map start keeps it.
                        st.input.exec_line(&format!("seta bot_count {}", n.min(64)));
                        listen::send(&format!("bots {n}"));
                    }
                    (None, true) => say(st, "usage: bots <count>"),
                    (_, false) => say(st, "bots: only on a server started from this client"),
                }
            }
            "rcon" => {
                // `rcon <command>` with the password in `rconpassword`, answered as console output.
                let password = st.input.cvar("rconpassword").unwrap_or("").to_owned();
                if let Some(net) = st.net.as_mut() {
                    net.send_rcon(&password, &cmd[1..].join(" "));
                } else if let Some(sh) = st.shell.as_mut() {
                    sh.print_console("rcon: not connected to a server");
                }
            }
            "name" => {
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
            }
            "record" => record_command(st, cmd.get(1).map(String::as_str)),
            "stoprecord" => {
                let stopped = st.net.as_mut().is_some_and(NetPlay::stop_record);
                say(
                    st,
                    if stopped {
                        "Stopped demo."
                    } else {
                        "Not recording a demo."
                    },
                );
            }
            // `CL_ForwardToServer_f`: the words after `cmd` go to the server as one reliable command.
            "cmd" => match st.net.as_mut().filter(|n| n.in_match()) {
                Some(net) => {
                    if cmd.len() > 1 {
                        net.send_command(&cmd[1..].join(" "));
                    }
                }
                None => say(st, "Not connected to a server."),
            },
            // `CG_QuickMessage_f`: a player in the match gets the quick message menu (`UI_Popup`: every other menu
            // closes first, and the cursor starts at the middle of the screen).
            "mp_quickmessage" => {
                if st.net.as_ref().is_some_and(NetPlay::is_player)
                    && let Some(sh) = st.shell.as_mut()
                {
                    sh.close_all(&mut st.input);
                    sh.open(&mut st.input, "quickmessage");
                    sh.center_cursor(&mut st.input);
                }
            }
            "centerview" => {
                if let Some(net) = st.net.as_mut() {
                    net.center_view();
                }
            }
            v if crate::console::is_server_verb(v) => {
                if let Some(net) = st.net.as_mut() {
                    if let Some(wire) = crate::console::server_line(&cmd) {
                        net.send_command(&wire);
                    }
                } else if let Some(sh) = st.shell.as_mut() {
                    sh.print_console(&format!("{}: not connected to a server", cmd[0]));
                }
            }
            v if crate::console::is_command(v) => {
                st.input.exec_line(&crate::input::config::join(&cmd));
            }
            // `Cmd_ExecuteSingleCommand` past the client's commands: on a listen server the host's console runs the
            // server's commands and variables too (`status`, `kick`, `map_restart`; one command table in the
            // original), and what the server knows neither is forwarded as a client command
            // (`CL_ForwardCommandToServer`) once it says so (`host_console_replies`). Without a server of this
            // process the word is forwarded at once, or is unknown; a `+` word always is.
            v => {
                if v.starts_with('-') {
                    continue;
                }
                if !v.starts_with('+') && st.listen.is_some() {
                    let line = crate::input::config::join(&cmd);
                    if listen::host_line(&line) {
                        continue;
                    }
                }
                forward_to_server(st, &cmd);
            }
        }
    }
}

/// `CL_ForwardCommandToServer`: a word nothing here knows goes to the server while connected (which answers an
/// unknown one); otherwise, or for a `+` word, it is unknown.
fn forward_to_server(st: &mut State, cmd: &[String]) {
    let Some(v) = cmd.first() else { return };
    match st
        .net
        .as_mut()
        .filter(|n| !v.starts_with('+') && n.in_match())
    {
        Some(net) => net.send_command(&crate::input::config::join(cmd)),
        None => {
            crate::console::note_unknown(v);
            say(st, &format!("Unknown command \"{v}\""));
        }
    }
}

/// The listen server's answers to what the host typed: its output goes to the console, and a line it did not know
/// is forwarded to it as a client command, as to any server (#366).
fn host_console_replies(st: &mut State) {
    if st.listen.is_none() {
        return;
    }
    for reply in listen::host_replies() {
        if reply.forward {
            if let Some(cmd) = crate::input::config::split_commands(&reply.line).first() {
                forward_to_server(st, cmd);
            }
        } else {
            for line in reply.output.lines() {
                say(st, line);
            }
        }
    }
}

/// Routes a window event to the menus (a menu is open and owns the keyboard and mouse).
fn ui_event(st: &mut State, ev: &crate::input::InputEvent) {
    use crate::input::InputEvent;
    use winit::event::ElementState;
    let Some(sh) = st.shell.as_mut() else { return };
    // A bind item is waiting: the next key or button is the binding (Escape cancels, via the menus).
    if sh.ui.bind_pending() {
        let name = match ev {
            InputEvent::Button {
                state: ElementState::Pressed,
                button,
            } => Some(crate::input::mouse_key_name(*button)),
            InputEvent::Wheel { y } => {
                Some(if *y > 0.0 { "mwheelup" } else { "mwheeldown" }.to_owned())
            }
            InputEvent::Key {
                code,
                state: ElementState::Pressed,
                repeat: false,
                ..
            } => match code {
                KeyCode::Escape => None,
                c => crate::input::physical_key_name(*c),
            },
            _ => None,
        };
        if let Some(n) = name {
            sh.bind_key(&mut st.input, &n);
            return;
        }
    }
    match ev {
        InputEvent::Cursor { x, y } => {
            sh.mouse_move(&mut st.input, *x, *y);
        }
        InputEvent::Button {
            state: ElementState::Pressed,
            button,
        } => {
            let key = match button {
                MouseButton::Left => UiKey::Mouse1,
                MouseButton::Right => UiKey::Mouse2,
                _ => return,
            };
            sh.key(&mut st.input, key);
        }
        InputEvent::Button {
            state: ElementState::Released,
            button: MouseButton::Left | MouseButton::Right,
        } => sh.mouse_up(),
        InputEvent::Wheel { y } => {
            sh.key(
                &mut st.input,
                if *y > 0.0 {
                    UiKey::WheelUp
                } else {
                    UiKey::WheelDown
                },
            );
        }
        InputEvent::Key {
            state: ElementState::Pressed,
            ..
        }
        | InputEvent::Text(_) => {
            for k in typed_ui_keys(ev) {
                sh.key(&mut st.input, k);
            }
        }
        _ => {}
    }
}

/// Frames in a row a configured surface may give no texture before the run stops with an error.
const SURFACE_RETRIES: u32 = 120;
/// How long a surface may only time out, or be occluded in a window that is not minimized, before a timed or scripted
/// run stops.
const SURFACE_STALL: Duration = Duration::from_secs(10);

/// What a frame without a texture needs next.
enum SurfaceAction {
    /// The frame is skipped; the next one tries again.
    Retry,
    /// The device is gone: [`Viewer::recover_device`].
    DeviceLost,
}

/// What to do when the surface gave no texture. A timeout, or an occluded (minimized) window, only skips the frame;
/// outdated or invalid state is configured again; a lost surface is taken again from the window. A lost device is
/// for the caller to replace; a surface that stays unusable ends the run with the reason.
fn recover_surface(
    st: &mut State,
    outcome: &wgpu::CurrentSurfaceTexture,
    first_fail: bool,
) -> Result<SurfaceAction, String> {
    use wgpu::CurrentSurfaceTexture as C;
    // A load runs on the device: configuring the surface under it fails, and its end shows what the device is.
    if st.loading.is_some() {
        return Ok(SurfaceAction::Retry);
    }
    if st.gpu.lost().is_some() {
        return Ok(SurfaceAction::DeviceLost);
    }
    if matches!(outcome, C::Timeout | C::Occluded) {
        if matches!(outcome, C::Occluded) && st.window.is_minimized() == Some(true) {
            st.surface_stalled_ms = 0.0;
            return Ok(SurfaceAction::Retry);
        }
        if let Some(limit) = st.surface_stall_limit {
            if first_fail && !st.presented {
                // Nothing has reached the screen yet: a compositor that has not mapped the window hands no image
                // back, and asking for it is the only thing a client can do about it (a no-op once it is mapped;
                // `set_visible` is itself Unsupported on Wayland and Web, so `request_redraw` is what acts there). A
                // window on a workspace the compositor is not showing is not a client's to move (`focus_window` is
                // unsupported on Wayland), which is why the error below names the monitor. A player's window is left
                // alone: `surface_stall_limit` is `None` for it, since they can switch desktops for as long as they
                // like.
                st.window.set_visible(true);
                st.window.request_redraw();
            }
            if st.surface_stalled_ms > limit.as_secs_f64() * 1000.0 {
                // A swapchain can keep timing out in a way a reconfiguration fixes: a compositor that stopped handing
                // its images back, or a window that was re-created under it. Give it one new surface and the same time
                // again before the run ends, and say what the window was when it does.
                if !st.surface_retried && st.window.is_minimized() != Some(true) {
                    st.surface_retried = true;
                    st.surface_stalled_ms = 0.0;
                    tracing::info!(
                        outcome = ?outcome,
                        size = ?st.window.inner_size(),
                        scale = st.window.scale_factor(),
                        fullscreen = ?st.window.fullscreen(),
                        "the window's surface gave no frame; making the surface again"
                    );
                    fresh_surface(st)?;
                    return Ok(SurfaceAction::Retry);
                }
                let size = st.window.inner_size();
                let scale = st.window.scale_factor();
                let monitor = st
                    .window
                    .current_monitor()
                    .map(|m| format!("{:?}", m.name()))
                    .unwrap_or_else(|| "none".into());
                let presented = if st.presented {
                    "handed no image back"
                } else {
                    "never presented a frame"
                };
                return Err(st.gpu.explain(&format!(
                    "the window's surface gave no frame in {:.0} s of waiting ({outcome:?}): the window is {}x{} \
                     at scale {scale} ({:?}) on monitor {monitor}, not minimized, and its compositor {presented}",
                    st.surface_stalled_ms / 1000.0,
                    size.width,
                    size.height,
                    st.window.fullscreen()
                )));
            }
        }
        return Ok(SurfaceAction::Retry);
    }
    st.surface_failures += 1;
    tracing::warn!(outcome = ?outcome, failures = st.surface_failures, "the window's surface gave no frame");
    if st.surface_failures > SURFACE_RETRIES {
        return Err(st
            .gpu
            .explain(&format!("the window's surface gave no frame ({outcome:?})")));
    }
    if matches!(outcome, C::Lost) {
        fresh_surface(st)?;
    } else {
        reconfigure(st);
    }
    Ok(SurfaceAction::Retry)
}

/// Takes a new surface from the window and configures it: the old one, with its swapchain, goes. wgpu refuses a
/// reconfiguration while the queue has work to finish, so the renderer's timestamps are waited for first.
fn fresh_surface(st: &mut State) -> Result<(), String> {
    if let Some(r) = st.renderer.as_mut() {
        record_gpu(&mut st.gpu_ms, r.flush_gpu_times());
    }
    st.surface = None;
    let surface = st
        .gpu
        .instance
        .create_surface(st.window.clone())
        .map_err(|e| {
            tracing::error!(error = %e, "cannot make the window's surface again");
            format!("cannot make the window's surface again: {e}")
        })?;
    surface.configure(&st.gpu.device, &st.config);
    st.surface = Some(surface);
    Ok(())
}

/// Configures the surface again. wgpu refuses while the queue still has work to finish (a timestamp read-back that has
/// not come back is enough: the call panics), so the renderer's are waited for first.
fn reconfigure(st: &mut State) {
    if let Some(r) = st.renderer.as_mut() {
        record_gpu(&mut st.gpu_ms, r.flush_gpu_times());
    }
    st.surface().configure(&st.gpu.device, &st.config);
}

/// What the renderer draws for a light an effect added: colour is bytes there, and an element with a direction is a
/// spot light.
fn dynamic_light(l: &fx::Light, spot: &render::SpotParams) -> Option<render::DynLight> {
    let color = glam::Vec3::from(l.color.map(|c| f32::from(c) / 255.0));
    match l.dir {
        Some(dir) => render::DynLight::spot(l.origin, dir, l.radius, color, spot),
        None => render::DynLight::omni(l.origin, l.radius, color),
    }
}

/// `FS_Restart` after `fs_game` changed: the menus run the mod now in the cvar (not while a match runs: it applies at the
/// next restart). A mod that cannot be opened is dropped from the cvar.
fn restart_files(cli: &Cli, st: &mut State) {
    let Some(sh) = st.shell.as_mut() else { return };
    if st.net.is_some() || !sh.game_changed(&st.input) {
        return;
    }
    let game = game_of(&st.input);
    let size = (st.config.width, st.config.height);
    let restarted = server::content::Install::open_game(&cli.install, &game)
        .map_err(|e| format!("cannot open the install: {e}"))
        .and_then(|install| sh.restart_files(&install, size, &mut st.input));
    if let Err(why) = restarted {
        tracing::warn!(%game, error = %why, "cannot restart the file system");
        st.input.cvars.set("fs_game", &sh.st.fs_game, false);
        show_error(st, &why);
        return;
    }
    tracing::info!(%game, "file system restarted");
    // The menu sounds are read again, with the mod's.
    st.menu_sound = None;
}

/// Before a join: the client runs the mod the server runs. Disconnects and restarts the file system when they differ;
/// the mod must be installed (its folder under `mods/`; a join fetches it first, see [`crate::download`]).
fn follow_server_game(
    cli: &Cli,
    map_slot: &mut Option<MapData>,
    st: &mut State,
    game: &str,
) -> Result<(), String> {
    if game == game_of(&st.input) {
        return Ok(());
    }
    if !assets::paths::valid_game(game) {
        return Err(format!("The server runs an invalid mod ({game})."));
    }
    if !game.is_empty() && !assets::paths::game_dir(&cli.install, game) {
        return Err(format!(
            "You do not have the mod {game} this server is running."
        ));
    }
    end_session(map_slot, st);
    st.input.cvars.set("fs_game", game, false);
    restart_files(cli, st);
    if game_of(&st.input) == game {
        Ok(())
    } else {
        Err(format!("Cannot run the mod {game}."))
    }
}

/// `fs_game`, the mod the player runs: what `fs_game` holds when it is a mod folder, else none.
pub fn game_of(input: &Input) -> String {
    let game = input.cvars.get("fs_game").unwrap_or_default();
    if assets::paths::valid_game(game) {
        game.to_owned()
    } else {
        String::new()
    }
}

/// Whether the present mode waits for the display, which then paces the frames (no `com_maxfps` cap on top).
fn display_paced(mode: wgpu::PresentMode) -> bool {
    matches!(
        mode,
        wgpu::PresentMode::Fifo | wgpu::PresentMode::FifoRelaxed
    )
}

/// Asks for the next frame: at once, or once `com_maxfps` allows it (`Com_Frame`'s `minMsec`, counted from the start
/// of the frame just drawn), by waiting on the event loop rather than spinning. The browser paces its own frames.
fn next_frame(el: &ActiveEventLoop, st: &State) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let due = gfx::frame_gap(&st.input.cvars, display_paced(st.present_mode))
            .map(|gap| st.last_frame + gap);
        match due.filter(|d| Instant::now() < *d) {
            Some(due) => return el.set_control_flow(ControlFlow::WaitUntil(due)),
            None => el.set_control_flow(ControlFlow::Poll),
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = el;
    st.window.request_redraw();
}

/// `vid_restart`: the window, the present mode and the aspect take the graphics settings now; a running match's
/// renderer takes the ones it can change live (shadows, specular, depth of field, glow, antialiasing), and the texture
/// size and filtering wait for the next map, which loads its textures again.
fn vid_restart(cli: &Cli, st: &mut State) {
    let _span = tracing::info_span!("vid_restart").entered();
    let g = Gfx::from_cvars(&st.input.cvars);
    if let Some(note) = display::apply(&st.window, &g.request(&cli.request)) {
        st.notes.push(note);
    }
    let (modes, rates) = display::video_modes(&st.window, g.monitor);
    if let Some(sh) = st.shell.as_mut() {
        if !modes.is_empty() {
            sh.st.video_modes = modes;
        }
        if !rates.is_empty() {
            sh.st.refresh_rates = rates;
        }
    }
    let caps = st.surface().get_capabilities(&st.gpu.adapter);
    let (mode, _) = pick_present(g.present(&cli.present), &caps.present_modes);
    let mut reconfigure_surface = mode != st.config.present_mode;
    // The swapchain is not handed over to another present mode: the surface is taken again (see `apply_resize`).
    st.fresh_surface |= reconfigure_surface;
    st.config.present_mode = mode;
    st.present_mode = mode;
    if st.aspect != g.aspect {
        st.aspect = g.aspect;
        reconfigure_surface = true;
    }
    let (w, h) = (st.config.width, st.config.height);
    if let Some(r) = st.renderer.as_mut() {
        #[cfg(not(target_arch = "wasm32"))]
        let before = r.samples(st.config.format);
        r.settings = g.settings(cli.settings);
        #[cfg(not(target_arch = "wasm32"))]
        if r.samples(st.config.format) != before && st.loading.is_none() {
            r.warm(st.config.format);
        }
    }
    tracing::info!(present_mode = ?mode, resurface = reconfigure_surface, "vid_restart applied");
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

fn apply_resize(st: &mut State, (w, h): (u32, u32)) -> Result<(), String> {
    let resized = (w, h) != (st.config.width, st.config.height);
    st.config.width = w;
    st.config.height = h;
    if std::mem::take(&mut st.fresh_surface) {
        fresh_surface(st)?;
    } else if resized {
        reconfigure(st);
    }
    st.fov_x = hor_plus(cg_fov(&st.input), st.aspect.unwrap_or(w as f32 / h as f32));
    if let Some(sh) = st.shell.as_mut() {
        sh.resize(w, h);
    }
    Ok(())
}

/// Starts or joins a match from the menus: begins loading the map in the background. The window keeps drawing the
/// loading screen; [`finish_load`] connects once the load is done. For a join, `map` is what the server said it is
/// playing.
fn start_session(
    cli: &Cli,
    map_slot: &mut Option<MapData>,
    st: &mut State,
    map: &str,
    gametype: &str,
    server: loader::Server,
) {
    end_session(map_slot, st);
    begin_load(cli, st, map, gametype, server);
    if let Some(sh) = st.shell.as_mut() {
        sh.close_all(&mut st.input);
    }
}

/// Starts the background load of `map` and the measuring of its frame gaps.
fn begin_load(cli: &Cli, st: &mut State, map: &str, gametype: &str, server: loader::Server) {
    // A warning that repeats per load logs again for this one.
    logging::new_window();
    tracing::info!(%map, "map load started");
    let gfx = Gfx::from_cvars(&st.input.cvars);
    let req = loader::Request {
        install: cli.install.clone(),
        game: game_of(&st.input),
        map: map.to_owned(),
        server,
        settings: gfx.settings(cli.settings),
        picmip: gfx.picmip,
        picmip_bump: gfx.picmip_bump,
        picmip_spec: gfx.picmip_spec,
        aniso_max: gfx.aniso_max,
        aniso_min: gfx.aniso_min,
        mip_mode: gfx.mip_mode,
        format: st.config.format,
    };
    gametype.clone_into(&mut st.load_gametype);
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
    let _span = tracing::info_span!("map_load", %map).entered();
    tracing::info!(load_ms = ms, level_change = load.level_change, "map loaded");
    let sound =
        crate::sound::ClientSound::start(&cli.install, &game_of(&st.input), map, !cli.no_sound);
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
            net.set_tracer_material(sh.ui.assets.material("gfx_tracer").cloned());
        }
    } else {
        let (addr, listen) = server.ok_or("no server to connect to")?;
        let limits = st.input.pitch_limits();
        // The `password` userinfo dvar: what a passworded server is given.
        let password = st.input.cvar("password").unwrap_or("").to_owned();
        let mut net = NetPlay::connect(
            library, &data, addr, &cli.name, &password, limits, false, sound,
        )?;
        if let Some(path) = st.pending_demo.take() {
            net.play_demo(&path)?;
        }
        net.set_autojoin(std::mem::take(&mut st.autojoin_next));
        let (rate, snaps) = netplay_rates(&st.input);
        net.set_userinfo(&cli.name, rate, snaps);
        if let Some(sh) = st.shell.as_ref() {
            net.set_profile(&sh.st.stats);
            net.set_tracer_material(sh.ui.assets.material("gfx_tracer").cloned());
        }
        st.net = Some(net);
        st.listen = listen;
        if let Some(sh) = st.shell.as_mut() {
            sh.st.in_game = true;
            sh.close_all(&mut st.input);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let mut renderer = renderer;
    #[cfg(not(target_arch = "wasm32"))]
    renderer.build_pipelines_in_background();
    st.renderer = Some(renderer);
    st.film_base = None;
    *map_slot = Some(data);
    st.map_name = map.to_owned();
    // The menus' map title and minimap look the map up by this (the original sets it from the server's info).
    st.input.cvars.set("mapname", map, false);
    Ok(())
}

/// The menu-started server: the chosen map then the other stock maps in turn, the dvars the player set (time and score
/// limits), on a standard port when one is free so the LAN list finds it.
fn listen_config(
    st: &State,
    map: &str,
    gametype: &str,
    bots: Option<usize>,
    cheats: bool,
) -> listen::Config {
    let maps: Vec<String> = st
        .shell
        .as_ref()
        .map(|s| s.st.maps.iter().map(|m| m.name.clone()).collect())
        .unwrap_or_default();
    listen::Config {
        map: map.to_owned(),
        bots,
        bot_count: bot_count(&st.input),
        gametype: Some(gametype.to_owned()),
        rotation: Some(rotation(gametype, &maps, map)),
        port: listen::free_standard_port(),
        game: game_of(&st.input),
        dvars: st.input.cvars.with_prefix("scr_"),
        cheats,
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
            drop_renderer(st);
            *map_slot = None;
            let gametype = st
                .input
                .cvars
                .get("g_gametype")
                .unwrap_or_default()
                .to_string();
            begin_load(cli, st, name, &gametype, loader::Server::Keep);
        }
    }
    Ok(())
}

/// A line for the console.
fn say(st: &mut State, text: &str) {
    if let Some(sh) = st.shell.as_mut() {
        sh.print_console(text);
    }
}

/// The file of demo `name` in `dir`: a plain name, so it cannot reach outside the folder.
fn demo_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("a demo name is letters, digits, _ and -".into());
    }
    Ok(dir.join(format!("{name}.c4edemo")))
}

/// `record [name]`: starts writing the match being played to a demo.
fn record_command(st: &mut State, name: Option<&str>) {
    let name = name.map_or_else(
        || {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            format!("demo_{secs}")
        },
        str::to_owned,
    );
    let result = demo_file(&st.demo_dir, &name).and_then(|path| {
        std::fs::create_dir_all(&st.demo_dir).map_err(|e| e.to_string())?;
        st.net
            .as_mut()
            .ok_or("not connected to a server".to_owned())?
            .start_record(&path)
            .map(|()| path)
    });
    match result {
        Ok(path) => say(st, &format!("recording to {}.", path.display())),
        Err(e) => {
            tracing::warn!(demo = %name, error = %e, "record: cannot start the demo");
            say(st, &format!("record: {e}"));
        }
    }
}

/// `playdemo <name>`: ends the session, loads the map the demo was recorded on and plays the demo there.
fn play_demo(
    cli: &Cli,
    map_slot: &mut Option<MapData>,
    st: &mut State,
    name: &str,
) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (cli, map_slot, st, name);
        Err("demos are not available in the browser".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = demo_file(&st.demo_dir, name)?;
        let map = net::demo::map_of(&path).map_err(|e| e.to_string())?;
        end_session(map_slot, st);
        st.pending_demo = Some(path);
        begin_load(cli, st, &map, "", loader::Server::Demo);
        if let Some(sh) = st.shell.as_mut() {
            sh.close_all(&mut st.input);
        }
        Ok(())
    }
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
    // A demo that ran out is no failure: the menu, without an error over it.
    if why == net::client::DEMO_ENDED {
        tracing::info!("the demo ended");
        end_session(map_slot, st);
    } else {
        tracing::error!(reason = %why, "connection lost");
        drop_session(map_slot, st, why);
    }
}

/// `Com_Error(ERR_DROP, why)`: the session ends and the main menu comes back with the error popup over it, as
/// `Com_ErrorCleanup` sets `com_errorMessage` and `UI_SetActiveMenu(UIMENU_MAIN)` then opens `error_popmenu`
/// (KisakCOD `qcommon/common.cpp` `Com_ErrorCleanup`, `ui_mp/ui_main_mp.cpp` `UI_SetActiveMenu`). Every failed start,
/// load, level change or lost connection ends here, so the player is told why and `reconnect` has a server to go to.
fn drop_session(map_slot: &mut Option<MapData>, st: &mut State, why: &str) {
    end_session(map_slot, st);
    show_error(st, why);
}

/// Back to the main menu: drops the connection, the server and the world.
fn end_session(map_slot: &mut Option<MapData>, st: &mut State) {
    if st.net.is_some() {
        tracing::info!(map = %st.map_name, "session ended");
    }
    if let Some(n) = st.net.as_mut() {
        n.disconnect();
    }
    st.net = None;
    st.input.set_fov_sensitivity_scale(1.0);
    st.roll = 0.0;
    st.loading = None;
    #[cfg(not(target_arch = "wasm32"))]
    {
        st.download = None;
    }
    st.pending_demo = None;
    st.load_gaps = None;
    if let Some(l) = st.listen.as_mut() {
        l.finish();
    }
    st.listen = None;
    drop_renderer(st);
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
/// What `screenshot` and `screenshotJpeg` write: the frame, to the screenshots folder of the user's folder. The
/// line to print, none for `silent`.
fn take_screenshot(
    st: &mut State,
    tex: &wgpu::Texture,
    req: &crate::screenshot::Request,
) -> Option<String> {
    let fail = || (!req.silent).then(|| "ScreenShot: Couldn't create a file".to_owned());
    let Some(dir) = st.input.config_dir().map(|d| d.join("screenshots")) else {
        tracing::error!("screenshot: there is no config folder to write to");
        return fail();
    };
    let Some(path) = crate::screenshot::target(&dir, req, &mut st.screenshot_next) else {
        tracing::error!(dir = %dir.display(), "screenshot: no free file name");
        return fail();
    };
    let rgba = match read_rgba(&st.gpu, tex, st.config.format) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "screenshot: cannot read the frame back");
            return fail();
        }
    };
    let rgb = rgba
        .1
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let pic = crate::screenshot::Picture {
        width: tex.width(),
        height: tex.height(),
        rgb,
    };
    if let Err(e) = crate::screenshot::write(&path, req.format, &pic) {
        tracing::error!(path = %path.display(), error = %e, "screenshot: cannot write the file");
        return fail();
    }
    let shown = path
        .file_name()
        .map(|n| format!("screenshots/{}", n.to_string_lossy()))?;
    (!req.silent).then(|| format!("Wrote {shown}"))
}

fn save_png(
    gpu: &Gpu,
    tex: &wgpu::Texture,
    format: wgpu::TextureFormat,
    path: &Path,
) -> Result<f64, String> {
    let (w, h) = (tex.width(), tex.height());
    let (lit, rgba) = read_rgba(gpu, tex, format)?;
    let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut e = png::Encoder::new(file, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .and_then(|mut w| w.write_image_data(&rgba))
        .map_err(|e| e.to_string())?;
    Ok(lit)
}

/// The frame's pixels as opaque RGBA, with the share of them that is not near black.
fn read_rgba(
    gpu: &Gpu,
    tex: &wgpu::Texture,
    format: wgpu::TextureFormat,
) -> Result<(f64, Vec<u8>), String> {
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
    Ok((lit as f64 / f64::from(w * h), rgba))
}

#[cfg(test)]
mod gpu_log_tests {
    use super::*;

    /// Needs an adapter: skips without.
    #[test]
    fn losses_are_recovered_from_a_few_times_then_end_the_run() {
        let _one_device = crate::GPU_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(gpu) = Gpu::new(None) else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        gpu.device.destroy();
        let mut log = GpuLog::default();
        for _ in 0..MAX_DEVICE_LOSSES {
            log.record_loss(&gpu, "test").expect("recoverable");
        }
        assert!(log.record_loss(&gpu, "test").is_err());
        let health = log.report(&gpu);
        assert_eq!(
            health["device_losses"].as_array().map(Vec::len),
            Some(MAX_DEVICE_LOSSES + 1)
        );
    }
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

#[cfg(test)]
mod bot_count_tests {
    use super::*;

    #[test]
    fn a_bound_bots_command_becomes_a_console_action() {
        let mut input = Input::detached();
        input.exec_line("bind x \"bots 12\"");
        input.key("x", true);
        let lines: Vec<_> = own_commands(&input.frame(0.01))
            .into_iter()
            .filter_map(|a| match a {
                Action::Console(l) => Some(l),
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["bots 12"]);
    }

    #[test]
    fn a_saved_bot_count_reaches_a_bare_listen_server_and_the_flag_wins() {
        let mut input = Input::detached();
        input.exec_line("seta bot_count 4");
        assert_eq!(
            saved_bot_count_lines(None, &input),
            ["set bot_count 4", "bots 4"]
        );
        assert!(saved_bot_count_lines(Some(0), &input).is_empty());
        input.exec_line("seta bot_count 0");
        assert!(saved_bot_count_lines(None, &input).is_empty());
    }
}

// ---- the dev control channel --------------------------------------------------------------------------------
//
// The channel answers requests against the running game; see `crate::control` and `docs/control-channel.md`. Its
// requests reach the same code the player's own actions do: `key` and `text` go into the frame's input queue, `exec`
// into the console, a capture takes the frame that was just drawn.

/// The clock and step count of one request the channel is holding.
struct ControlWait {
    at: Instant,
    steps: u64,
}

/// Answers this frame's channel requests. Injected events land in `st.events`, so they are dispatched with the
/// window's on this same frame.
fn service_control(cli: &Cli, st: &mut State) {
    let Some(mut ch) = st.control.take() else {
        return;
    };
    let answered = {
        let mut host = ControlHost { cli, st };
        ch.service(&mut host)
    };
    if answered > 0 {
        tracing::debug!(answered, "control: requests answered");
    }
    let front = ch.front().map(|r| r.seq);
    st.control = Some(ch);
    match front {
        Some(min) => st.control_waits.retain(|s, _| *s >= min),
        None => st.control_waits.clear(),
    }
}

/// The game behind the channel's requests: the app's own state.
struct ControlHost<'a> {
    cli: &'a Cli,
    st: &'a mut State,
}

impl devctrl::Handler for ControlHost<'_> {
    fn handle(&mut self, req: &devctrl::Request) -> devctrl::Outcome {
        crate::control::handle(self, req)
    }
}

impl crate::control::Control for ControlHost<'_> {
    fn frames(&self) -> u64 {
        self.st.samples.len() as u64
    }

    fn waited(&mut self, seq: u64) -> f64 {
        self.st
            .control_waits
            .entry(seq)
            .or_insert_with(|| ControlWait {
                at: Instant::now(),
                steps: 0,
            })
            .at
            .elapsed()
            .as_secs_f64()
    }

    fn step_left(&mut self, seq: u64, frames: u64) -> u64 {
        let w = self
            .st
            .control_waits
            .entry(seq)
            .or_insert_with(|| ControlWait {
                at: Instant::now(),
                steps: 0,
            });
        w.steps += 1;
        frames.saturating_sub(w.steps)
    }

    fn state(&mut self, what: Option<&str>) -> Value {
        let parts: Vec<&str> = match what {
            Some(w) => vec![w],
            None => crate::control::PARTS.to_vec(),
        };
        state_document(self.cli, self.st, &parts)
    }

    fn binds_of(&self, command: &str) -> Vec<String> {
        self.st
            .input
            .binding_keys(command)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn cvar(&self, name: &str) -> Option<String> {
        self.st.input.cvar(name).map(str::to_owned)
    }

    fn set_cvar(&mut self, name: &str, value: &str) -> Result<(), String> {
        let line =
            crate::input::config::join(&["set".to_owned(), name.to_owned(), value.to_owned()]);
        let before = self.st.input.cvar(name).map(str::to_owned);
        self.st.input.exec_line(&line);
        let after = self.st.input.cvar(name).map(str::to_owned);
        if before == after && after.as_deref() != Some(value) {
            return Err(format!("{name} did not take the value"));
        }
        Ok(())
    }

    fn inject(&mut self, ev: crate::input::InputEvent) {
        self.st.events.push(ev);
    }

    fn exec(&mut self, line: &str) {
        match self.st.shell.as_mut() {
            Some(sh) => sh.console_line(&mut self.st.input, line),
            None => self.st.input.exec_line(line),
        }
    }

    fn capture(&mut self, dir: Option<&str>, base64: bool) -> Option<Result<Value, String>> {
        if let Some(done) = self.st.control_shot_done.take() {
            return Some(done);
        }
        self.st.control_shot = Some((dir.map(str::to_owned), base64));
        None
    }

    fn view(
        &mut self,
        origin: Option<[f32; 3]>,
        angles: Option<[f32; 3]>,
    ) -> Result<Value, String> {
        if let Some(o) = origin {
            self.st.pos = Vec3::new(o[0], o[1], o[2]);
        }
        if let Some(a) = angles {
            self.st.pitch = a[0].to_radians();
            self.st.yaw = a[1].to_radians();
            self.st.roll = a[2].to_radians();
        }
        let in_match = self.st.net.is_some();
        Ok(json!({
            "origin": [self.st.pos.x, self.st.pos.y, self.st.pos.z],
            "angles": [self.st.pitch.to_degrees(), self.st.yaw.to_degrees(), self.st.roll.to_degrees()],
            // In a match the server's position and the player's own aim replace this on the next frame.
            "held": !in_match,
        }))
    }

    fn server(
        &mut self,
        seq: u64,
        req: &serde_json::Map<String, Value>,
    ) -> Option<Result<Value, String>> {
        // Only a client that runs a listen server has one to ask: a `--connect` client answers at once, so a
        // request cannot wedge the queue waiting for a server that will never drain it.
        if self.st.listen.is_none() {
            return Some(Err("this client runs no listen server".to_owned()));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::listen::request(seq, req)
        }
        // The browser hosts no server yet (#378's Phase 5 step 2 makes hosting say so).
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (seq, req);
            Some(Err("the browser hosts no server".to_owned()))
        }
    }

    fn quit(&mut self) {
        self.st.control_quit = true;
    }
}

/// The state document: the game's real state, section by section. Only the sections asked for are built, so a `wait`
/// on `player.health` does not build the entity list every frame.
fn state_document(cli: &Cli, st: &mut State, parts: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    for part in parts {
        let v = match *part {
            "client" => json!({
                "map": if st.map_name.is_empty() { cli.map.clone() } else { st.map_name.clone() },
                "frames": st.samples.len(),
                "seconds": st.started.elapsed().as_secs_f64(),
                "size": [st.config.width, st.config.height],
                "loading": st.loading.is_some(),
                "in_world": st.renderer.is_some(),
                "out": cli.out.as_ref().map(|p| p.display().to_string()),
                "backends": cli.backend.clone(),
                "present": cli.present.clone(),
                // Whether mouse look can work: the window has the focus and the pointer is locked, or why not.
                "focused": st.window.has_focus(),
                "pointer_locked": st.grabbed,
                "look_gate": st.input.look_gate(),
            }),
            "menu" => menu_state(st),
            "hud" => hud_state(st),
            "console" => json!({
                "open": st.shell.as_ref().is_some_and(|sh| sh.st.console.active()),
                "field": st.shell.as_ref().map(|sh| sh.st.console.field()),
                "lines": st.shell.as_ref().map(|sh| sh.st.feed.console.clone()),
                // The lines as one text, for a `contains` on a line's part (`console.text`).
                "text": st.shell.as_ref().map(|sh| devctrl::text_lines(&sh.st.feed.console)),
                "messages": st.input.messages(),
            }),
            "cvars" => {
                let mut m = serde_json::Map::new();
                for (k, v) in st.input.all_cvars() {
                    m.insert(k, Value::String(v));
                }
                Value::Object(m)
            }
            "binds" => {
                let mut m = serde_json::Map::new();
                for (k, v) in st.input.all_binds() {
                    m.insert(k.to_owned(), Value::String(v.to_owned()));
                }
                Value::Object(m)
            }
            "view" => json!({
                "origin": [st.pos.x, st.pos.y, st.pos.z],
                "pitch": st.pitch.to_degrees(),
                "yaw": st.yaw.to_degrees(),
                "roll": st.roll.to_degrees(),
                "drawn_frames": st.samples.len(),
            }),
            "draw" => json!({
                "surfaces": st.surfaces_drawn.last().copied(),
                "lights": st.lights_drawn,
                "distortion_copy": st.distortion_now,
                "late": st.late,
            }),
            "render" => render_report(st),
            other => match st.net.as_mut() {
                Some(net) => net.control_state(other),
                None => Value::Null,
            },
        };
        m.insert((*part).to_owned(), v);
    }
    Value::Object(m)
}

/// The menus on the stack, the item that has the focus, and every item of each open menu with the rectangle a click
/// is tested against.
fn menu_state(st: &mut State) -> Value {
    let Some(sh) = st.shell.as_mut() else {
        return Value::Null;
    };
    let open: Vec<String> = sh.ui.open_menus().iter().map(|m| (*m).to_owned()).collect();
    let focused = sh
        .ui
        .focused_report()
        .map(|(menu, item, rect)| json!({"menu": menu, "item": item, "rect": rect}));
    let mut items = Vec::new();
    for menu in &open {
        for it in sh.menu_items(&mut st.input, menu) {
            items.push(json!({
                "menu": menu,
                "name": it.name,
                "text": it.text,
                "visible": it.visible,
                "rect": it.rect,
            }));
        }
    }
    json!({"open": open, "focused": focused, "items": items})
}

/// What the HUD drew and counted since the run began (`stats`, the counters `hud_draw` of `ui-script.json` has),
/// what it shows now (`report`, `live`), and the game facts the scoreboard reads.
fn hud_state(st: &mut State) -> Value {
    let Some(sh) = st.shell.as_ref() else {
        return Value::Null;
    };
    let g = &sh.st.game;
    let live = &sh.st.live;
    json!({
        "report": sh.st.game.hud.report(),
        "stats": sh.st.hud_stats.report(),
        "live": {
            "killcam": live.killcam,
            "dead": live.dead,
            "intermission": live.intermission,
            "following": live.following.is_some(),
            // A message window has a line up.
            "feed": (0..crate::hud::WINDOWS).any(|w| sh.st.feed.active(w, live.time)),
            "players_listed": live.scores.len(),
            "own_team": live.own_team,
        },
        "missing_images": sh.missing(),
        "scoreboard_shown": g.scoreboard,
        "scores_top": sh.st.scores_top,
        "dead": g.dead,
        "intermission": g.intermission,
        "team": g.team,
        "gametype": g.gametype,
        "time_left": g.time_left,
        "allies_score": g.allies_score,
        "axis_score": g.axis_score,
        "score": g.score,
        "kills": g.kills,
        "deaths": g.deaths,
        "ping": g.ping,
        "clip_ammo": g.clip_ammo,
        "open_menus": sh.ui.open_menus(),
    })
}

/// Takes the capture the channel asked for from the frame just drawn and answers its request.
fn take_control_shot(cli: &Cli, st: &mut State, tex: &wgpu::Texture) -> Result<Value, String> {
    let (dir, inline) = st.control_shot.take().unwrap_or_default();
    let dir = dir
        .map(PathBuf::from)
        .or_else(|| cli.out.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    st.shot_serial += 1;
    let path = dir.join(format!("control-{}.png", st.shot_serial));
    let lit = save_png(&st.gpu, tex, st.config.format, &path)?;
    let mut reply = json!({
        "path": path.display().to_string(),
        "width": tex.width(),
        "height": tex.height(),
        "lit_fraction": lit,
    });
    if inline {
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read the capture: {e}"))?;
        reply["png_base64"] = Value::String(devctrl::base64(&bytes));
    }
    Ok(reply)
}
