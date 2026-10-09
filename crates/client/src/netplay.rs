// SPDX-License-Identifier: GPL-3.0-only
//! Playing on a server: the connection, the usercmds built from the player's input, prediction of the player's own
//! movement, and the models of everyone else drawn from the interpolated snapshots.
//!
//! Every render frame the network is read; about every 8 ms (the original's `cl_maxpackets` ceiling) the input becomes
//! one usercmd stamped with the client's estimate of the server clock. The player's body is the newest snapshot's
//! player state with the unacknowledged commands replayed on top ([`net::predict`]); other players are drawn
//! [`net::view::INTERP_DELAY_MS`] behind the server clock between the two snapshots around that moment, which is also
//! the moment the server rewinds them to when it judges this client's shots.

mod corpses;
mod hud;
mod melee;
mod names;

use crate::crosshair::Reticle;
use crate::damage::{DamageHud, DamageView};
use crate::effects::Effects;
use crate::events::{ClientEvent, Events};
use crate::helicopter::Rotors;
use crate::input::{Feedback, InputFrame, Seen, buttons, scan_own};
use crate::kick::Kick;
use crate::look::{Look, LookOut, cap_turn};
use crate::models::{Library, Player, PlayerModelSet, Team};
use crate::props::{Launches, Props};
use crate::sound::{ClientSound, Who};
use crate::viewmodel::{Sight, ViewModel, ViewTags};
use crate::wire::Wire;
use glam::Vec3;
use net::client::NetClient;
use net::entity::{EntityState, etype};
use net::predict::{Env, PlayerBoxes, Predictor};
use render::ModelInstance;
use serde_json::{Value, json};
use server::netsv::eflags;
use server::playeranim::{LegsWire, PlayerPoseInput, TorsoWire};
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::{ANGLE_UNIT, Params, PlayerState, PmType, UserCmd, pmf};
use sim::weapon::fire::aim_spread_degrees;
use sim::weapon::gun::ViewFx;
use sim::weapon::{PlayerWeapons, WeaponParams, WeaponTable};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;
use web_time::Instant;

/// The shortest interval between two usercmds.
const CMD_INTERVAL: Duration = Duration::from_millis(8);
/// How often the scoreboard asks for fresh rows while it is up (`UpdateScores`).
const SCORES_INTERVAL: Duration = Duration::from_secs(2);
/// How long a player model is kept after the snapshots stop mentioning it.
const GONE_AFTER: Duration = Duration::from_millis(500);

/// Whether `spawn` starts a life the client has not seen yet (the first snapshot counts), remembering it in `last`.
fn new_life(last: &mut Option<u16>, spawn: u16) -> bool {
    last.replace(spawn) != Some(spawn)
}

fn brush_surfaces(world: &assets::zone::gfxworld::GfxWorld) -> Vec<bool> {
    world.models.iter().map(|m| m.surface_count > 0).collect()
}

/// Alive in the world: the weapon switching of the original (`pm_type < PM_DEAD`) leaves spectators and the dead alone.
fn alive(ps: &PlayerState) -> bool {
    matches!(
        ps.pm_type,
        PmType::Normal | PmType::NormalLinked | PmType::LastStand
    )
}

/// The field of view the original forces for a turret user (55) and for the intermission (90), whatever the weapon
/// (`CG_GetViewFov`).
fn fixed_fov(ps: &PlayerState) -> Option<f32> {
    if ps.e_flags & sim::pm::ef::TURRET_ACTIVE != 0 {
        Some(55.0)
    } else if ps.pm_type == PmType::Intermission {
        Some(90.0)
    } else {
        None
    }
}

/// What the render loop draws this frame.
pub struct NetFrame {
    /// Eye position.
    pub origin: Vec3,
    /// Radians, counter-clockwise from +x.
    pub yaw: f32,
    /// Radians, positive up.
    pub pitch: f32,
    /// Radians, clockwise: the roll of the recoil kick.
    pub roll: f32,
    pub models: Vec<ModelInstance>,
    /// The map's inline models (doors, lifts, crates) where their entities are.
    pub brush_models: Vec<render::BrushInstance>,
    /// The aim zoom and scope overlay of the held weapon.
    pub sight: Option<Sight>,
    /// The field of view the world is drawn with regardless of the weapon: a turret's or the intermission's.
    pub fixed_fov: Option<f32>,
    /// Effect sprites and decals.
    pub meshes: Vec<render::DynMesh>,
    /// The lights the effects add: muzzle flashes, explosions, fire.
    pub lights: Vec<fx::Light>,
    /// Happenings new this frame; see [`crate::events`].
    #[expect(dead_code, reason = "read by effects, audio and the interface")]
    pub events: Vec<ClientEvent>,
    /// Server console commands other than the effect names the event layer takes.
    #[expect(
        dead_code,
        reason = "read by the interface once it has server commands to act on"
    )]
    pub commands: Vec<String>,
    /// Vision set and shell shock for the picture.
    pub look: LookOut,
    /// The blur of the picture.
    pub dof: render::Dof,
    /// The fog the server set, blended from the one before; `None` for none. Only meant once `fog_from_server`:
    /// until the scripts set any, the map's own art fog stands.
    pub fog: Option<render::art::Fog>,
    pub fog_from_server: bool,
}

struct Remote {
    player: Player,
    /// The body model the player is drawn with.
    body: String,
    /// World model of the weapon the player holds.
    weapon: Option<String>,
    seen: Instant,
}

/// The spread of how fast the eye moved: median, 99th percentile and the share of frames faster than twice the median
/// (the walking player's speed is steady; a view that jerks is not).
fn eye_speed_summary(speeds: &[f32]) -> Value {
    let moving: Vec<f32> = speeds.iter().copied().filter(|v| *v > 20.0).collect();
    if moving.len() < 20 {
        return Value::Null;
    }
    let mut v = moving.clone();
    v.sort_by(f32::total_cmp);
    let at = |q: f32| v[((v.len() - 1) as f32 * q) as usize];
    let median = at(0.5);
    let fast = moving.iter().filter(|s| **s > 2.0 * median).count();
    json!({
        "frames": moving.len(),
        "median": median,
        "p99": at(0.99),
        "max": at(1.0),
        "over_twice_median": fast as f32 / moving.len() as f32,
    })
}

/// Numbers the run reports.
#[derive(Default)]
struct Counters {
    cmds: u64,
    shots: u64,
    /// The most the view kicked up, and the most the pitch of a sent cmd differed from the player's own aim, degrees.
    max_kick_up: f32,
    /// The gun: the speed shots gave its recoil spring, the most its offset and its turn-lag reached, degrees, and
    /// how often the recoil came back to rest. `gun_springs` is false for a weapon that cannot aim (no spring).
    gun_speed_given: f32,
    max_gun_recoil: f32,
    max_gun_sway: f32,
    gun_recoil_settled: u64,
    gun_recoil_live: bool,
    gun_springs: bool,
    max_kick_in_cmd: f32,
    /// The camera layer: frames the prediction began a stair step on (the eye was to ease, not jump), the frames of
    /// those where the drawn eye still jumped, the largest offsets of the drawn eye from the logical one (units, the
    /// lean sideways, the landing dip, the stair smoothing), the largest turn of the view (degrees), and the frames
    /// the hands' animated camera moved the view.
    stair_frames: u64,
    stair_snaps: u64,
    /// The last snap's logical jump, drawn jump, smoothing offset and frame time, to tell why.
    stair_last_snap: [f32; 4],
    steps_seen: u64,
    view_lean_max: f32,
    lean_frames: u64,
    view_dip_max: f32,
    view_step_max: f32,
    view_bob_max: f32,
    view_turn_max: f32,
    camera_tag_frames: u64,
    /// Frames drawn from behind the dead player's body, the farthest the camera hung from the head there (units),
    /// frames a kill cam hung the camera on the entity that killed, and frames the picture was blurred.
    death_view_frames: u64,
    death_view_range_max: f32,
    kill_cam_frames: u64,
    dof_frames: u64,
    last_render_z: Option<f32>,
    /// How many times the view kick came back to rest after a kick.
    kick_settled: u64,
    kicked: bool,
    /// How often the player state's `damage_event` changed while alive, the value last seen, and the most the red flash and the number of wedges showed.
    damage_events: u64,
    damage_event_seen: u8,
    max_damage_flash: f32,
    max_damage_wedges: usize,
    predictions: u64,
    max_players_seen: usize,
    /// The most other players drawn in one frame, and why the others could not be built (a model or animation the
    /// zones lack): a player the snapshot has but the picture does not.
    max_players_drawn: usize,
    player_faults: std::collections::BTreeSet<String>,
    /// Names of the torso clips (`pt_*`) seen playing on other players: fire, reload, melee, throw, pullout, flinch.
    torso_clips: std::collections::BTreeSet<&'static str>,
    /// Frames in which another player was reloading, swinging the knife or throwing a grenade: weapon handling that
    /// always has a torso clip, so a run with some must have shown one.
    remote_weapon_frames: u64,
    spawned: bool,
    start: Option<[f32; 3]>,
    end: [f32; 3],
    path: f32,
    weapon: String,
    frames_with_viewmodel: u64,
    target_frames: u64,
    in_range_frames: u64,
    unknown_weapon: std::collections::BTreeMap<String, String>,
    /// `script_model` entities in the newest snapshot, how many of them were drawn, and the names of those that could
    /// not be (a model the zones lack).
    script_models_seen: usize,
    script_models_drawn: usize,
    /// `BRUSH` entities with surfaces in the newest snapshot (each is handed to the renderer), and the most at once.
    brush_models_seen: usize,
    brush_models_max: usize,
    script_models_unloaded: std::collections::BTreeSet<String>,
    /// Every model a `script_model` was drawn with at some frame.
    script_models_names: std::collections::BTreeSet<String>,
    /// `VEHICLE` entities in the interpolated view of the newest frame and how many of them were drawn.
    vehicles_seen: usize,
    vehicles_drawn: usize,
    /// Models the zones lack for a vehicle, and the most vehicles drawn at once.
    vehicles_unloaded: std::collections::BTreeSet<String>,
    vehicles_max: usize,
    projectiles_max: usize,
    /// Events received, by kind.
    events: std::collections::BTreeMap<&'static str, u64>,
    fx_quads_max: usize,
    fx_tracers_max: usize,
    fx_decals_max: usize,
    /// Ragdolls made for corpses seen, and the most corpses drawn at once.
    ragdolls: u64,
    corpses_max: usize,
    fx_live_max: usize,
    /// The most script looping effects playing at once, and the strongest camera shake and sway felt.
    looped_fx_max: usize,
    shake_max: f32,
    sway_max: f32,
    /// How fast the eye moved between drawn frames (units per second) while the player walked; the spread of these
    /// is what a jerky view is made of.
    eye_speeds: Vec<f32>,
}

/// How far one degree of mouse turn moves the map pick, as a fraction of the map.
const LOC_CURSOR_SPEED: f32 = 0.004;

pub struct NetPlay {
    net: NetClient<Wire>,
    /// Voice chat: the microphone, the other players' voices, `cl_voice`, and the players the person muted.
    talker: crate::voice::Talker,
    hearing: crate::voice::Hearing,
    voice_on: bool,
    muted: HashSet<u16>,
    lib: Library,
    weapons: WeaponTable,
    params: Params,
    /// The `MOVEMENT` configstring [`Self::params`] was made from.
    params_info: String,
    boxes: PlayerBoxes,
    /// Per inline model of the map: whether it has surfaces to draw (a clip brush has none).
    brush_surfaces: Vec<bool>,
    pred: Predictor,
    /// Pitch (positive down) and yaw (positive left) in degrees, as the player has turned.
    angles: [f32; 2],
    /// The kick of the player's own shots; kept apart from `angles`, the player's aim.
    kick: Kick,
    /// The view's hit kick and scope sway, the same the server aims shots with.
    fx: ViewFx,
    /// The screen's answer to the hits the player takes, and what the HUD draws of it this frame.
    damage: DamageView,
    damage_hud: DamageHud,
    pitch_limits: (f32, f32),
    cmd_time: i32,
    last_cmd: Instant,
    want_weapon: Option<u16>,
    /// The last primary weapon held (`weaponLatestPrimaryIdx`): where cycling from an item or offhand returns to.
    latest_primary: u16,
    /// The spawn count last seen on the player's own state: a new life drops the weapon asked for.
    last_spawn: Option<u16>,
    /// The weapon held before an action slot picked another, for the slot's second press.
    before_slot: Option<u16>,
    /// A night-vision slot was pressed: the next command carries the button.
    nv_press: bool,
    /// The own player's event counter last acted on, and what the input layer is yet to be told of it.
    own_events: Seen,
    /// The own events the latest frame's prediction delivered, for the HUD's hints.
    own_new: Vec<(u8, u8)>,
    feedback: Feedback,
    /// Where the map pick of a location selection points, 0..1 across and down the map.
    pub loc_cursor: [f32; 2],
    remotes: HashMap<u16, Remote>,
    /// The bodies the server's corpse entities are drawn as, by entity number.
    corpses: HashMap<u16, corpses::Body>,
    /// The impulse of each recent death by client number, until the body is made.
    pushes: HashMap<u16, [f32; 3]>,
    vm: Option<((u16, u16), ViewModel)>,
    /// The view model put away for the off-hand weapon on show (or back for the weapon in hand), so a throw does not
    /// build one each time.
    vm_spare: Option<((u16, u16), ViewModel)>,
    /// What aiming did to the view in the last frame.
    sight: Option<Sight>,
    c: Counters,
    auto: Option<Auto>,
    auto_join: Option<net::ui::AutoJoin>,
    /// Events for the menu runtime (a person's menus drain them; autoplay answers them itself).
    ui_events: Vec<net::ui::UiEvent>,
    last_eye: Option<Vec3>,
    /// The melee aim assist: the tangents of the half angles of the view, the lunge the next command carries (yaw,
    /// distance), whether the swing is winding up, and the view's pull onto its target.
    tan_half_fov: [f32; 2],
    melee_charge: (f32, u8),
    meleeing: bool,
    automelee: crate::automelee::AutoMelee,
    /// The camera layer over the logical eye: stair smoothing, bob, lean, landing dip, scoped sway.
    camera: crate::camera::Camera,
    /// `cg_thirdPersonRange` and `cg_thirdPersonAngle`.
    orbit: crate::camera::Orbit,
    /// The blur of aiming down the sights, easing between frames.
    ads_dof: crate::dof::AdsDof,
    /// The fog the server set.
    fog: crate::fog::FogState,
    /// The other players as the last frame's view saw them, for overhead names and head icons.
    scan: crate::hud::NameScan,
    /// The state the HUD shows (predicted, or the followed player's) and the view yaw in degrees, from the last frame.
    hud_view: Option<(PlayerState, f32)>,
    /// The crosshair of the held weapon from the last frame; `None` when dead, watching another player or the weapon
    /// has no reticle. The HUD draws it; the field of view is the app's to fill in.
    reticle: Option<Reticle>,
    sound: ClientSound,
    events: Events,
    /// The client's estimate of the server clock at the last frame, for the HUD.
    live_time: i32,
    kill_icons: HashMap<String, crate::hud::KillIcon>,
    scores_asked: Option<Instant>,
    server_addr: String,
    effects: Effects,
    tracer_cvars: crate::tracer::Cvars,
    /// The weapon rules the client plays by (`player_meleeRange` and the like): the server's defaults, which no
    /// cvar changes yet.
    weapon_params: WeaponParams,
    tracer_material: Option<std::sync::Arc<assets::zone::gfx::Material>>,
    /// Earthquakes shaking the view.
    shakes: sim::shake::CameraShakes,
    props: Props,
    /// Ragdoll hits of this frame, heard with the props'.
    ragdoll_hits: Vec<crate::props::Collision>,
    launches: Launches,
    look: Look,
    /// What the shell shock holds the view to (`CL_CapTurnRate`, the mouse scale): pitch and yaw degrees per second.
    max_turn: [f32; 2],
    shock_sensitivity: f32,
    /// The effect `--fx-demo` plays, and when it last did.
    fx_demo: Option<(String, Option<i32>)>,
    /// The server announced a level the app has not acted on yet.
    new_level: Option<String>,
    stat_sync: crate::profile::StatSync,
    /// The rotor rig of each helicopter model drawn (`None` when the model or its animation is missing), and the
    /// seconds the rotors have turned.
    rotors: HashMap<String, Option<Rotors>>,
    rotor_clock: f32,
    /// `drawvehicles 0` leaves the vehicles out of the picture (the harness compares a view with and without them).
    draw_vehicles: bool,
    /// The server's `delta_angles` of the last snapshot: when it turns the player's view (a spawn, a teleport), the
    /// commands' angles turn with it.
    last_delta: Option<[f32; 3]>,
}

/// Silence from the server after which the player is warned (the original's `CG_DrawDisconnect` waits for the
/// 128th unacknowledged command, about two seconds).
const INTERRUPTED_MS: u64 = 2000;

impl NetPlay {
    pub fn connect(
        lib: Library,
        map: &render::MapData,
        server: SocketAddr,
        name: &str,
        pitch_limits: (f32, f32),
        autoplay: bool,
        sound: ClientSound,
    ) -> Result<Self, String> {
        let clipmap = map.clipmap.clone().ok_or("the map has no collision data")?;
        let world = map.world.clone();
        let t = Wire::open(server)?;
        #[cfg(not(target_arch = "wasm32"))]
        let qport = (std::process::id() & 0xffff) as u16;
        // No process id in a browser: any value the server has not seen from this address will do.
        #[cfg(target_arch = "wasm32")]
        let qport = (js_sys::Math::random() * 65536.0) as u16;
        let weapons =
            WeaponTable::new(&lib.content.weapons()).map_err(|e| format!("weapon table: {e:?}"))?;
        Ok(Self {
            net: NetClient::new(t, server, name, "", qport),
            talker: crate::voice::Talker::default(),
            hearing: crate::voice::Hearing::default(),
            voice_on: true,
            muted: HashSet::new(),
            weapons,
            params: Params {
                mantle_anims: lib.content.mantle_anims(),
                ..Params::default()
            },
            params_info: String::new(),
            boxes: PlayerBoxes::new(clipmap),
            brush_surfaces: brush_surfaces(&map.world),
            pred: Predictor::default(),
            angles: [0.0; 2],
            kick: Kick::default(),
            fx: ViewFx::default(),
            damage: DamageView::default(),
            damage_hud: DamageHud::default(),
            pitch_limits,
            cmd_time: 0,
            last_cmd: Instant::now(),
            want_weapon: None,
            latest_primary: 0,
            last_spawn: None,
            before_slot: None,
            nv_press: false,
            own_events: Seen::default(),
            own_new: Vec::new(),
            feedback: Feedback::default(),
            loc_cursor: [0.5; 2],
            remotes: HashMap::new(),
            corpses: HashMap::new(),
            pushes: HashMap::new(),
            vm: None,
            vm_spare: None,
            sight: None,
            c: Counters::default(),
            auto: autoplay.then(Auto::default),
            auto_join: autoplay.then(net::ui::AutoJoin::default),
            ui_events: Vec::new(),
            last_eye: None,
            tan_half_fov: [0.84, 0.63],
            melee_charge: (0.0, 0),
            meleeing: false,
            automelee: crate::automelee::AutoMelee::default(),
            camera: crate::camera::Camera::default(),
            orbit: crate::camera::Orbit::default(),
            ads_dof: crate::dof::AdsDof::default(),
            fog: crate::fog::FogState::default(),
            scan: crate::hud::NameScan::default(),
            hud_view: None,
            reticle: None,
            sound,
            props: Props::new(
                lib.content
                    .clipmap()
                    .map_or(&[][..], |c| &c.dyn_entities[..]),
            ),
            launches: Launches::default(),
            ragdoll_hits: Vec::new(),
            look: Look::new((map.art.glow, map.art.film)),
            max_turn: [0.0; 2],
            shock_sensitivity: 1.0,
            effects: Effects::new(&lib.content, world),
            tracer_cvars: crate::tracer::Cvars::default(),
            weapon_params: WeaponParams::default(),
            tracer_material: None,
            shakes: Default::default(),
            lib,
            events: Events::default(),
            live_time: 0,
            kill_icons: HashMap::new(),
            scores_asked: None,
            server_addr: server.to_string(),
            fx_demo: None,
            new_level: None,
            stat_sync: crate::profile::StatSync::default(),
            rotors: HashMap::new(),
            rotor_clock: 0.0,
            draw_vehicles: true,
            last_delta: None,
        })
    }

    /// Plays effect `name` in front of the player every 1.5 s of server time; `None` stops.
    pub fn set_fx_demo(&mut self, name: Option<String>) {
        self.fx_demo = name.map(|n| (n, None));
    }

    pub fn refused(&self) -> Option<&str> {
        self.net.refused()
    }

    /// Why the server ended the connection (a kick) or went silent: the message the player is shown.
    pub fn dropped(&self) -> Option<&str> {
        self.net.dropped()
    }

    /// `cl_timeout`: the silence the server may keep.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.net.set_timeout(timeout);
    }

    /// Answers the team and class menus with defaults (what a person's menus do when they pick the first choices).
    pub fn set_autojoin(&mut self, on: bool) {
        self.auto_join = on.then(net::ui::AutoJoin::default);
    }

    /// The name of the weapon in hand, as the view model last drew it.
    pub fn weapon(&self) -> String {
        self.c.weapon.clone()
    }

    /// Where the join stands, for the browser overlay: "connecting", "connected" (snapshots arrive) or "spawned".
    #[cfg(target_arch = "wasm32")]
    pub fn phase(&self) -> &'static str {
        if self.c.spawned {
            "spawned"
        } else if self.net.connected() {
            "connected"
        } else {
            "connecting"
        }
    }

    /// Snapshots received so far.
    #[cfg(target_arch = "wasm32")]
    pub fn snapshots(&self) -> u64 {
        self.net.stats().map_or(0, |s| s.packets_in)
    }

    /// The server has put the player in the world (alive at least once).
    pub fn spawned(&self) -> bool {
        self.c.spawned
    }

    /// The server's UI events since the last call, in order (open a menu, set a dvar, print, ...).
    pub fn take_ui_events(&mut self) -> Vec<net::ui::UiEvent> {
        let events = std::mem::take(&mut self.ui_events);
        crate::hud::fill::note_kill_icons(&events, &self.lib.content, &mut self.kill_icons);
        events
    }

    /// Copies what the server told the UI into `live` for this frame's drawing, and asks for fresh scoreboard rows
    /// every couple of seconds while the scoreboard is up (`live.scores_wanted`).
    pub fn fill_live(&mut self, live: &mut crate::hud::LiveUi) {
        crate::hud::fill::fill(&mut self.net, live, self.live_time, self.last_eye);
        live.scan.clone_from(&self.scan);
        live.scope = self.sight.as_ref().and_then(|s| s.overlay.clone());
        live.flashed = self.look.flashbanged(self.live_time);
        live.night_vision = self.look.night_vision();
        live.reticle.clone_from(&self.reticle);
        live.damage.clone_from(&self.damage_hud);
        if live.kill_icons.len() != self.kill_icons.len() {
            live.kill_icons.clone_from(&self.kill_icons);
        }
        live.server_addr.clone_from(&self.server_addr);
        live.interrupted = self.net.silent_ms() > INTERRUPTED_MS;
        if !live.active || !live.scores_wanted {
            self.scores_asked = None;
        } else if self
            .scores_asked
            .is_none_or(|t| t.elapsed() >= SCORES_INTERVAL)
        {
            self.scores_asked = Some(Instant::now());
            self.net.request_scores();
        }
    }

    /// The material the tracers are drawn with (`gfx_tracer`, from the UI zones), kept across levels.
    pub fn set_tracer_material(&mut self, m: Option<std::sync::Arc<assets::zone::gfx::Material>>) {
        self.effects.set_tracer_material(m.clone());
        self.tracer_material = m;
    }

    /// What the player has set about tracers (`cg_tracerchance` and the like).
    pub fn set_tracer_cvars(&mut self, c: crate::tracer::Cvars) {
        self.tracer_cvars = c;
    }

    /// Every weapon definition of the loaded content.
    pub fn weapon_defs(&self) -> Vec<std::sync::Arc<assets::zone::weapon::WeaponDef>> {
        self.lib.content.weapons()
    }

    /// The map the server announced since the last call: the app then calls [`Self::new_level`].
    pub fn take_new_level(&mut self) -> Option<String> {
        self.new_level.take()
    }

    /// Starts over for a new level: the old one's prediction, models, events and clock are meaningless. With `world`
    /// (another map was loaded) the collision, models and sound are replaced too. The connection stays.
    pub fn new_level(
        &mut self,
        world: Option<(Library, &render::MapData, ClientSound)>,
    ) -> Result<(), String> {
        if let Some((lib, map, sound)) = world {
            self.weapons = WeaponTable::new(&lib.content.weapons())
                .map_err(|e| format!("weapon table: {e:?}"))?;
            let clipmap = map.clipmap.clone().ok_or("the map has no collision data")?;
            self.effects = Effects::new(&lib.content, map.world.clone());
            self.effects
                .set_tracer_material(self.tracer_material.clone());
            self.params.mantle_anims = lib.content.mantle_anims();
            self.lib = lib;
            self.boxes = PlayerBoxes::new(clipmap);
            self.brush_surfaces = brush_surfaces(&map.world);
            self.sound = sound;
        }
        {
            let sound = &mut self.sound;
            self.hearing.close_all(|p| sound.close_voice_pipe(p));
        }
        self.pred = Predictor::default();
        self.cmd_time = 0;
        self.want_weapon = None;
        self.latest_primary = 0;
        self.last_spawn = None;
        self.remotes.clear();
        self.corpses.clear();
        self.vm = None;
        self.vm_spare = None;
        self.events = Events::default();
        self.own_events = Seen::default();
        self.own_new.clear();
        self.shakes.clear();
        self.last_eye = None;
        self.camera.reset();
        self.ads_dof = crate::dof::AdsDof::default();
        self.fog.reset();
        self.scan = crate::hud::NameScan::default();
        self.c.spawned = false;
        self.c.start = None;
        Ok(())
    }

    /// The name, rate and snapshot rate the server hears as soon as the connection is up.
    pub fn set_userinfo(&mut self, name: &str, rate: i32, snaps: i32) {
        self.net
            .set_userinfo(net::client::userinfo_command(name, rate, snaps));
    }

    /// The profile that goes to the server as soon as the connection is up, before the person begins: the scripts
    /// read the rank and the classes when they begin.
    pub fn set_profile(&mut self, stats: &[i32]) {
        self.net.set_profile(crate::profile::upload_commands(stats));
    }

    /// Hands the server the profile's stats (it fabricates stand-ins when a person joins) and takes what it says now
    /// as already known, so only later changes flow back ([`Self::stat_changes`]).
    pub fn upload_stats(&mut self, stats: &[i32]) {
        for c in crate::profile::upload_commands(stats) {
            self.net.command(&c);
        }
        if let Some(ui) = self.net.ui() {
            self.stat_sync.baseline(ui.stats());
        }
    }

    /// Stat values the server changed since the last call.
    pub fn stat_changes(&mut self) -> Vec<(i32, i32)> {
        match self.net.ui() {
            Some(ui) => self.stat_sync.changes(ui.stats()),
            None => Vec::new(),
        }
    }

    /// Sends a client command line to the server (`menuresponse <menu> <response>`).
    pub fn send_command(&mut self, cmd: &str) {
        self.net.command(cmd);
    }

    /// `rcon` as the console types it.
    pub fn send_rcon(&mut self, password: &str, command: &str) {
        self.net.rcon(password, command);
    }

    pub fn disconnect(&mut self) {
        self.net.disconnect();
    }

    /// `cl_voice`: whether holding the talk key sends the microphone.
    pub fn set_voice(&mut self, on: bool) {
        self.voice_on = on;
    }

    /// The players the person muted (the mute menu): their voices are not played, and the server is told so it does
    /// not even send them.
    pub fn set_muted(&mut self, muted: &HashSet<u16>) {
        if *muted == self.muted {
            return;
        }
        for n in muted.difference(&self.muted) {
            self.net.command(&format!("mute {n}"));
        }
        for n in self.muted.difference(muted) {
            self.net.command(&format!("unmute {n}"));
        }
        self.muted.clone_from(muted);
    }

    /// Why the microphone could not be used, once.
    pub fn take_voice_note(&mut self) -> Option<String> {
        self.talker.note.take()
    }

    /// One frame of voice chat: the microphone's frames go to the server while the talk key is `down`, the voices the
    /// server relayed are played.
    fn voice_step(&mut self, down: bool) {
        let now = Instant::now();
        for f in self.talker.frames(down, self.voice_on, now) {
            self.net.send_voice(f);
        }
        let sound = &mut self.sound;
        for v in self.net.take_voice() {
            let gain = if self.muted.contains(&u16::from(v.speaker)) {
                0.0
            } else {
                1.0
            };
            if gain > 0.0 {
                self.hearing.feed(&v, gain, now, || sound.open_voice_pipe());
            }
        }
        self.hearing.tick(now, |p| sound.close_voice_pipe(p));
    }

    /// Writes what this connection receives to `path` (`record`).
    pub fn start_record(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.net.start_record(path)
    }

    /// Ends the recording (`stoprecord`); `false` when none was running.
    pub fn stop_record(&mut self) -> bool {
        self.net.stop_record()
    }

    /// Plays the demo at `path` instead of a server's match (`playdemo`).
    pub fn play_demo(&mut self, path: &std::path::Path) -> Result<(), String> {
        let reader = net::demo::DemoReader::open(path).map_err(|e| e.to_string())?;
        self.net.play_demo(reader)
    }

    /// `cl_freezeDemo`: holds a playing demo where it is.
    pub fn set_demo_paused(&mut self, paused: bool) {
        self.net.pause_demo(paused);
    }

    /// `snd_volume`, from the cvar store each frame.
    pub fn set_volume(&mut self, volume: f32) {
        self.sound.set_volume(volume);
    }

    /// `cg_footsteps`, from the cvar store each frame.
    /// The `bg_shock_volume_<channel>` cvars: what a held breath dips the channels to.
    pub fn set_breath_volumes(&mut self, volumes: Vec<(String, f32)>) {
        self.sound.set_breath_volumes(volumes);
    }

    pub fn set_footsteps(&mut self, on: bool) {
        self.sound.set_footsteps(on);
    }

    /// The camera's horizontal field of view in radians and aspect ratio (width over height), which the effects cull
    /// what they spawn and draw against.
    pub fn set_projection(&mut self, fov_x: f32, aspect: f32) {
        self.effects.set_projection(fov_x, aspect);
        // The aim assist's view: the tangents of the half angles.
        let tan_x = (fov_x * 0.5).tan();
        self.tan_half_fov = [tan_x, tan_x / aspect.max(f32::EPSILON)];
    }

    /// `cg_thirdPersonRange` and `cg_thirdPersonAngle`, from the cvar store each frame.
    pub fn set_orbit(&mut self, range: f32, angle: f32) {
        self.orbit = crate::camera::Orbit { range, angle };
    }

    /// What the player state says the input layer must change (forced stance, ADS reset, frozen); take it each
    /// frame and give it to `Input::apply`.
    pub fn take_input_feedback(&mut self) -> Feedback {
        self.feedback.take()
    }

    /// Takes the server's movement tunables (`jump_height`, `friction`, ...) as its `MOVEMENT` configstring gives
    /// them, so the prediction moves as the server does.
    fn follow_movement_dvars(&mut self) {
        let Some(info) = self.net.ui_ref().map(|u| u.config(net::ui::cs::MOVEMENT)) else {
            return;
        };
        if info != self.params_info {
            self.params = Params {
                mantle_anims: self.lib.content.mantle_anims(),
                ..Params::from_info(info)
            };
            self.params_info = info.to_owned();
        }
    }

    /// Runs one render frame of play. `None` until the server has given the player a body.
    pub fn frame(&mut self, dt: f32, input: &InputFrame) -> Option<NetFrame> {
        self.net.pump(Duration::ZERO);
        self.voice_step(input.held_other.iter().any(|c| c == "talk"));
        self.answer_menus();
        let now_ms = self.net.now_ms();
        let st = self.net.snaps.server_time(now_ms);
        let own = self.net.latest().map(|s| s.own());
        let (Some(st), Some(own)) = (st, own) else {
            self.net.send();
            return None;
        };
        self.live_time = st;
        if let Some(fog) = self.net.ui_ref().map(|u| u.config(net::ui::cs::FOGVARS)) {
            self.fog.follow(fog, st);
        }
        if let Some(snap) = self.net.latest() {
            follow_server_turn(
                &mut self.angles,
                &mut self.last_delta,
                snap.ps.delta_angles,
                snap.follow.is_some(),
            );
        }
        // Autoplay replaces the player's input with a bot's.
        let auto_input;
        let input = if self.auto.is_some() {
            // The effect demo stands still so the effect stays in front of the camera.
            auto_input = if self.fx_demo.is_some() {
                InputFrame::default()
            } else {
                let mut f = self.autoplay(dt, st, own);
                f.buttons |= input.buttons;
                f
            };
            &auto_input
        } else {
            input
        };

        let picking = self.net.latest().is_some_and(|s| s.ps.loc_selection != 0);
        if picking {
            // The mouse moves the point on the map, not the view.
            self.loc_cursor[0] =
                (self.loc_cursor[0] - input.cursor_yaw * LOC_CURSOR_SPEED).clamp(0.0, 1.0);
            self.loc_cursor[1] =
                (self.loc_cursor[1] + input.cursor_pitch * LOC_CURSOR_SPEED).clamp(0.0, 1.0);
        } else {
            self.loc_cursor = [0.5; 2];
            let scale = self.shock_sensitivity;
            self.angles[1] += cap_turn(input.look_delta_yaw * scale, self.max_turn[1], dt);
            self.angles[0] = (self.angles[0]
                + cap_turn(input.look_delta_pitch * scale, self.max_turn[0], dt))
            .clamp(self.pitch_limits.0, self.pitch_limits.1);
        }
        for c in &input.pending_commands {
            self.command(c);
        }
        // Only a living player who is not watching another has a swing to assist.
        if self
            .net
            .latest()
            .is_some_and(|s| s.follow.is_none() && alive(&s.ps))
        {
            self.melee_assist(st, own, dt, input.buttons & buttons::MELEE != 0);
        } else {
            self.melee_charge = (0.0, 0);
            self.meleeing = false;
            self.automelee = crate::automelee::AutoMelee::default();
        }
        if self.last_cmd.elapsed() >= CMD_INTERVAL || self.c.cmds == 0 {
            self.last_cmd = Instant::now();
            self.send_cmd(input, st);
        } else {
            self.net.send();
        }

        let snap = self.net.latest()?.clone();
        if snap.follow.is_some() || self.net.playing_demo() {
            // Watching another player (a killcam or a followed spectator) or a recording: the snapshot's
            // player state is what was, so nothing is predicted; draw the view as it came.
            self.pred.resync_events();
            self.own_new.clear();
            let ps = snap.ps.clone();
            self.kick.clear();
            // The followed player's hits turn the view and show on the screen as the player's own would.
            self.damage
                .look(&ps, st, [ps.viewangles[0], ps.viewangles[1]]);
            let gun = self.weapons.info(sim::pm::viewmodel_weapon(&ps)).gun;
            self.fx.observe(&ps);
            let hit_view = self.fx.damage_kick(&ps, &gun);
            self.damage_hud = self.damage.hud(st, ps.viewangles[1]);
            let eye = Vec3::new(
                ps.origin[0],
                ps.origin[1],
                ps.origin[2] + ps.view_height_current,
            );
            self.last_eye = Some(eye);
            self.hud_view = Some((ps.clone(), ps.viewangles[1]));
            self.reticle = None;
            // The thing that killed, if the scripts named one and it can be seen: the camera hangs on it, and
            // every body is drawn, the followed player's too.
            self.boxes.sync(&snap);
            let kill = self.kill_cam(st, &snap);
            let mut models = self.remote_players(dt, st, ps.client_num, kill.is_some());
            self.scan_names(
                st,
                ps.client_num,
                eye,
                (ps.viewangles[0], ps.viewangles[1]),
                false,
            );
            models.extend(self.script_models(dt, st, ps.client_num));
            let brush_models = self.brush_models(&snap, st);
            models.extend(self.items(&snap));
            models.extend(self.vehicles(dt, st, ps.client_num));
            let def = self
                .lib
                .content
                .weapon(self.weapons.name(sim::pm::viewmodel_weapon(&ps)))
                .cloned();
            let aim = [ps.viewangles[0], ps.viewangles[1]];
            let (seen, render, kill_fov, view_model) = if let Some((k, _)) = &kill {
                (k.angles, k.origin, Some(k.fov), None)
            } else {
                // The watched player's view bobs and leans as their own does (`CG_OffsetFirstPersonView`).
                let cam = self.camera.follow(&crate::camera::Frame {
                    ps: &ps,
                    now: st,
                    weapon: def.as_deref().map(crate::camera::WeaponView::from),
                    aim,
                    step: 0.0,
                    params: &self.params,
                    events: &[],
                });
                let mut seen = [
                    aim[0] + cam.angles[0],
                    aim[1] + cam.angles[1],
                    cam.angles[2],
                ];
                let mut render = eye + Vec3::from(cam.offset);
                let mut at = ps.origin;
                for (a, o) in at.iter_mut().zip(cam.offset) {
                    *a += o;
                }
                models.extend(self.view_model(dt, &ps, at, seen, false));
                render += self.hands_camera(&ps, &mut seen);
                let range = crate::dof::view_model(
                    &ps,
                    def.as_deref().map(|d| (d.ads_dof_start, d.ads_dof_end)),
                );
                (seen, render, None, Some(range))
            };
            let (yaw, pitch) = (seen[1].to_radians(), -seen[0].to_radians());
            let dof = match &kill {
                Some((_, dof)) => *dof,
                None => self.scene_dof(&snap.ps, &ps, view_model, render, (yaw, pitch), dt),
            };
            self.c.kill_cam_frames += u64::from(kill.is_some());
            self.c.dof_frames += u64::from(dof.active());
            let (events, commands) = self.take_events(&snap);
            self.look
                .goggles(ps.weapon_flags & sim::pm::wf::NIGHTVISION != 0, st);
            let look = self.look.frame(st);
            self.shock_effects(&look);
            // The effects the followed player's guns and the map's events start are as visible to a watcher.
            let drawn = self.fx_frame(
                dt,
                st,
                ps.client_num,
                &events,
                render,
                (yaw, pitch, seen[2].to_radians()),
            );
            models.extend(self.props.instances());
            models.extend(drawn.models);
            return Some(NetFrame {
                origin: render,
                yaw: yaw + (look.kick[1] + drawn.sway[1]).to_radians(),
                pitch: pitch + (look.kick[0] - drawn.sway[0] - hit_view[0]).to_radians(),
                roll: seen[2].to_radians() + (drawn.sway[2] + hit_view[1]).to_radians(),
                models,
                brush_models,
                sight: kill_fov.is_none().then(|| self.sight.clone()).flatten(),
                fixed_fov: kill_fov.or_else(|| fixed_fov(&ps)),
                meshes: drawn.meshes,
                lights: drawn.lights,
                events,
                commands,
                look,
                dof,
                fog: self.fog.at(st),
                fog_from_server: self.fog.announced(),
            });
        }
        self.follow_movement_dvars();
        self.boxes.sync(&snap);
        let env = Env {
            world: self.boxes.world(),
            weapons: &self.weapons,
            params: &self.params,
            time: st,
        };
        let p = self.pred.predict(&snap, &env);
        self.c.predictions += 1;
        let ps = p.ps;
        self.meleeing = ps.weapon_state == sim::pm::weapon_state::MELEE_INIT;
        let events = p.events;
        self.own_new.clone_from(&events);
        let err = self.pred.error_at(st);
        let feet = [
            ps.origin[0] + err[0],
            ps.origin[1] + err[1],
            ps.origin[2] + err[2],
        ];
        let dead = matches!(ps.pm_type, PmType::Dead | PmType::DeadLinked);
        let def = self
            .lib
            .content
            .weapon(self.weapons.name(sim::pm::viewmodel_weapon(&ps)));
        let ads_dof = def.map(|d| (d.ads_dof_start, d.ads_dof_end));
        if dead {
            self.kick.clear();
        } else {
            self.kick.shots(
                &events,
                &ps,
                self.weapons.info(sim::pm::viewmodel_weapon(&ps)),
            );
            self.kick.step(
                dt,
                ps.weapon_pos_frac,
                def.map(|d| [d.hip_view_kick_center_speed, d.ads_view_kick_center_speed]),
            );
        }
        let gun = self.weapons.info(sim::pm::viewmodel_weapon(&ps)).gun;
        if dead {
            self.fx.clear();
        } else {
            // A hit shows in the snapshot's state at the time the server's next command saw it.
            self.fx.observe(&snap.ps);
            self.fx.step(&ps, &gun);
        }
        // A hit turns the camera but not the aim, and the HUD shows it.
        let hit_view = if dead {
            self.damage.clear();
            [0.0; 2]
        } else {
            let view = [
                self.angles[0] + self.kick.angles()[0],
                self.angles[1] + self.kick.angles()[1],
            ];
            self.damage.look(&ps, st, view);
            self.fx.damage_kick(&ps, &gun)
        };
        self.damage_hud = if dead {
            DamageHud::default()
        } else {
            self.damage.hud(st, self.angles[1] + self.kick.angles()[1])
        };
        // A respawn starts the count over; only a change the player lived to see is a hit.
        self.c.damage_events += u64::from(!dead && ps.damage_event != self.c.damage_event_seen);
        self.c.damage_event_seen = ps.damage_event;
        self.c.max_damage_flash = self.c.max_damage_flash.max(self.damage_hud.flash);
        self.c.max_damage_wedges = self.c.max_damage_wedges.max(self.damage_hud.wedges.len());
        let (was_kicked, kick) = (self.c.kicked, self.kick.angles());
        self.c.kicked = kick != [0.0; 3];
        self.c.kick_settled += u64::from(was_kicked && !self.c.kicked);
        self.c.max_kick_up = self.c.max_kick_up.max(-kick[0]);
        // Alive in the world: a spectator (before the team and class are chosen) has a view but no body.
        self.c.spawned |= matches!(
            ps.pm_type,
            PmType::Normal | PmType::NormalLinked | PmType::LastStand
        );
        if self.c.start.is_none() {
            self.c.start = Some(feet);
        } else {
            let d = Vec3::from(feet) - Vec3::from(self.c.end);
            // A respawn is not walking.
            if d.length() < 64.0 {
                self.c.path += d.length();
            }
        }
        self.c.end = feet;
        // The logical eye is where shots start, sound is heard and the harness measures; the camera draws from the
        // render eye, which the stair smoothing, bob, lean and landing move.
        let eye = Vec3::new(feet[0], feet[1], feet[2] + ps.view_height_current);
        // The scope's sway moves the view like the kick does, but only the kick goes to the server with the aim: the
        // server adds the same sway itself.
        let idle = if dead {
            [0.0; 2]
        } else {
            self.fx.idle(&ps, &gun)
        };
        let aim = [
            self.angles[0] + kick[0] + idle[0],
            self.angles[1] + kick[1] + idle[1],
        ];
        let cam = self.camera.view(&crate::camera::Frame {
            ps: &ps,
            events: &events,
            now: st,
            weapon: def.map(|d| crate::camera::WeaponView::from(&**d)),
            aim,
            step: self.pred.step_offset(st),
            params: &self.params,
        });
        self.camera_metrics(&cam, eye, dt, dead, ps.leanf != 0.0 && !dead);
        let yaw_deg = if dead {
            ps.viewangles[1]
        } else {
            self.angles[1]
        };
        self.last_eye = Some(eye);
        self.hud_view = Some((ps.clone(), yaw_deg));
        self.reticle = if dead { None } else { self.reticle_of(&ps) };
        // The hands hang off the render eye, with the camera's angles on the aim.
        let mut seen = [
            aim[0] + cam.angles[0],
            aim[1] + cam.angles[1],
            cam.angles[2],
        ];
        let mut render = eye + Vec3::from(cam.offset);
        let mut hands = Vec::new();
        let mut view_model = None;
        if dead {
            // The dead see their body from behind (`CG_OffsetThirdPersonView`), turned the way the blow came from.
            let head = render;
            (seen, render) = self.death_view(&ps, head);
            self.c.death_view_frames += 1;
            self.c.death_view_range_max = self.c.death_view_range_max.max(head.distance(render));
        } else {
            let mut at = feet;
            for (a, o) in at.iter_mut().zip(cam.offset) {
                *a += o;
            }
            let index = sim::pm::viewmodel_weapon(&ps);
            let clip_empty = index != 0 && p.inv.clip(&self.weapons, index) == 0;
            hands = self.view_model(dt, &ps, at, seen, clip_empty);
            render += self.hands_camera(&ps, &mut seen);
            view_model = Some(crate::dof::view_model(&ps, ads_dof));
        }
        // The listener hears from the drawn eye (`SND_SetListener(.., refdef.vieworg, ..)`).
        self.hear(dt, render, &ps, &events, &snap);
        if new_life(&mut self.last_spawn, ps.spawn_count) {
            // `CG_Respawn`: the weapon in hand is the selected one.
            self.want_weapon = None;
            self.note_latest_primary(ps.weapon as u16);
        }
        let mut fb = scan_own(&mut self.own_events, &ps, &events);
        if std::mem::take(&mut fb.out_of_ammo) {
            self.out_of_ammo_change(&ps, &p.inv);
        }
        self.feedback.merge(fb);

        let (events, commands) = self.take_events(&snap);
        let look = self.look.frame(st);
        self.shock_effects(&look);
        let mut models = self.remote_players(dt, st, own, dead);
        let view = if dead {
            (0.0, ps.viewangles[1])
        } else {
            (aim[0], aim[1])
        };
        self.scan_names(st, own, eye, view, dead);
        models.extend(self.script_models(dt, st, own));
        let brush_models = self.brush_models(&snap, st);
        models.extend(self.items(&snap));
        models.extend(self.vehicles(dt, st, own));
        models.extend(hands);
        let (yaw, pitch) = (seen[1].to_radians(), -(seen[0] + hit_view[0]).to_radians());
        let roll = (seen[2] + kick[2] + hit_view[1]).to_radians();
        let dof = self.scene_dof(&snap.ps, &ps, view_model, render, (yaw, pitch), dt);
        self.c.dof_frames += u64::from(dof.active());
        let drawn = self.fx_frame(dt, st, own, &events, render, (yaw, pitch, roll));
        models.extend(self.props.instances());
        models.extend(drawn.models);
        Some(NetFrame {
            origin: render,
            yaw: yaw + (look.kick[1] + drawn.sway[1]).to_radians(),
            pitch: pitch + (look.kick[0] - drawn.sway[0]).to_radians(),
            roll: roll + drawn.sway[2].to_radians(),
            models,
            brush_models,
            sight: self.sight.clone(),
            fixed_fov: fixed_fov(&ps),
            meshes: drawn.meshes,
            lights: drawn.lights,
            events,
            commands,
            look,
            dof,
            fog: self.fog.at(st),
            fog_from_server: self.fog.announced(),
        })
    }

    /// `CG_OffsetThirdPersonView` for a dead player: the angles (pitch positive down, yaw, roll) and the place of
    /// the camera hung behind the head at `head`, the world's walls in its way.
    fn death_view(&self, ps: &PlayerState, head: Vec3) -> ([f32; 3], Vec3) {
        let world = self.boxes.world();
        let hull = [crate::camera::ORBIT_HULL; 3];
        let trace = |a: Vec3, b: Vec3| {
            world
                .trace(
                    a.to_array(),
                    b.to_array(),
                    hull.map(|h| -h),
                    hull,
                    ps.client_num,
                    crate::camera::ORBIT_MASK,
                )
                .fraction
        };
        let dead_yaw = (ps.dead_yaw != sim::pm::DEAD_YAW_UNSET).then_some(ps.dead_yaw as f32);
        let (angles, at) = crate::camera::third_person(
            head,
            [ps.viewangles[0], ps.viewangles[1], 0.0],
            dead_yaw,
            self.orbit,
            &trace,
        );
        (angles, at)
    }

    /// The camera a kill cam puts on the entity the scripts named as the killer, and the blur it is seen through
    /// (`CG_HelicopterKillCamEnabled`, `CG_AirstrikeKillCamEnabled`): `None` outside a kill cam and while the entity
    /// or the victim is not in view.
    fn kill_cam(
        &self,
        st: i32,
        snap: &net::snapshot::Snapshot,
    ) -> Option<(crate::camera::KillCam, render::Dof)> {
        let follow = snap.follow.filter(|f| f.archive_ms > 0)?;
        let number = follow.entity?;
        snap.entity(number)?;
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, None);
        let find = |n: u16| ents.iter().find(|e| e.number == n);
        let (killer, victim) = (find(number)?, find(follow.own)?);
        let target = Vec3::from(victim.origin);
        if killer.etype == etype::VEHICLE {
            let k = crate::camera::helicopter_kill_cam(Vec3::from(killer.origin), target)?;
            let dof = crate::dof::helicopter_kill_cam(k.distance, crate::camera::HELI_DIST);
            Some((k, dof))
        } else {
            let k =
                crate::camera::airstrike_kill_cam(Vec3::from(killer.origin), killer.angles, target);
            Some((k, crate::dof::airstrike_kill_cam(k.distance)))
        }
    }

    /// `CG_UpdateSceneDepthOfField`: the blur of the picture seen from `eye` looking along `(yaw, pitch)` radians.
    /// `snap` is the player state the server sent (the scripts' blur), `ps` the one drawn (the aim); `view_model` is
    /// the range the first-person weapon blurs over, `None` when it is not drawn.
    fn scene_dof(
        &mut self,
        snap: &PlayerState,
        ps: &PlayerState,
        view_model: Option<(f32, f32)>,
        eye: Vec3,
        (yaw, pitch): (f32, f32),
        dt: f32,
    ) -> render::Dof {
        let mut dof = crate::dof::scripted(snap).unwrap_or_else(|| {
            let aimed = ps.weapon_pos_frac != 0.0 || ps.pm_type >= PmType::Dead;
            let mut focus = crate::dof::ADS_TRACE;
            if aimed {
                let (sy, cy) = yaw.sin_cos();
                let (sp, cp) = pitch.sin_cos();
                let to = eye + Vec3::new(cp * cy, cp * sy, sp) * focus;
                focus *= self
                    .boxes
                    .world()
                    .trace(
                        eye.to_array(),
                        to.to_array(),
                        [0.0; 3],
                        [0.0; 3],
                        ps.client_num,
                        contents::MASK_SHOT,
                    )
                    .fraction;
            }
            self.ads_dof.update(ps, focus, dt)
        });
        (dof.view_model_start, dof.view_model_end) = view_model.unwrap_or_default();
        dof
    }

    /// Plays what the props did this frame (bullet impacts, destroy effects) and every body hit loud enough to hear.
    fn prop_effects(&mut self) {
        let mut h = self.props.take();
        let (weapons, content) = (&self.weapons, &self.lib.content);
        let def = |w: u16| {
            weapons
                .get(w)
                .and_then(|i| content.weapon(&i.name))
                .cloned()
        };
        for e in &h.impacts {
            self.effects.event(e, &def);
        }
        self.sound.world_events(&h.impacts, &def, &|_| None);
        for (fx, frame) in &h.fx {
            self.effects.play_frame("dynent_destroy", fx, *frame);
        }
        for c in self.effects.take_collisions() {
            h.collisions
                .push(crate::props::heard(self.boxes.world(), c.prefix, &c.impact));
        }
        h.collisions.append(&mut self.ragdoll_hits);
        self.sound.collisions(&h.collisions);
    }

    /// Plays what `events` start and advances the effects and the props to `st`; returns what to draw from `eye`
    /// looking along `(yaw, pitch, roll)` radians. `own` is whose gun the first-person flash comes from.
    fn fx_frame(
        &mut self,
        dt: f32,
        st: i32,
        own: u16,
        events: &[ClientEvent],
        eye: Vec3,
        (yaw, pitch, roll): (f32, f32, f32),
    ) -> crate::effects::Drawn {
        self.effects.set_tracer_cvars(self.tracer_cvars);
        self.effects
            .set_view(own, self.vm.as_ref().and_then(|(_, v)| v.tags()));
        for e in events {
            if let ClientEvent::Earthquake { origin, quake } = e {
                self.shakes.start(
                    st,
                    eye.to_array(),
                    quake.scale,
                    quake.duration_ms,
                    *origin,
                    quake.radius,
                );
            }
            if let ClientEvent::PlayerDeath { client, push, .. } = e {
                self.pushes.insert(*client, *push);
            }
            let weapons = &self.weapons;
            let content = &self.lib.content;
            let def = |w: u16| {
                weapons
                    .get(w)
                    .and_then(|i| content.weapon(&i.name))
                    .cloned()
            };
            self.props.event(e, &def, self.boxes.world());
            corpses::blast(&mut self.corpses, e, &def);
            self.effects.event(e, &def);
        }
        if let Some((name, last)) = &mut self.fx_demo
            && last.is_none_or(|l| st.wrapping_sub(l) >= 1500)
        {
            *last = Some(st);
            let (yaw, pitch) = (self.angles[1].to_radians(), -self.angles[0].to_radians());
            self.effects.demo(name, eye, yaw, pitch);
        }
        let flying: Vec<_> = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own))
            .into_iter()
            .filter(|e| e.etype == etype::MISSILE)
            .collect();
        let missiles: Vec<_> = flying
            .iter()
            .map(|e| {
                (
                    e.number,
                    Vec3::from(e.origin),
                    Vec3::from(e.velocity),
                    e.weapon,
                )
            })
            .collect();
        let (weapons, content) = (&self.weapons, &self.lib.content);
        self.effects.missiles(&missiles, &|w| {
            weapons
                .get(w)
                .and_then(|i| content.weapon(&i.name))
                .cloned()
        });
        let projectiles = self.projectiles(&flying, st);
        if let Some(snap) = self.net.latest() {
            self.effects.world_fx(&snap.entities, &self.events, eye, st);
        }
        self.effects.update(st, self.boxes.world());
        for s in self.effects.take_sounds() {
            self.sound.play_world(&s.alias, s.origin.to_array());
        }
        self.props.update(dt, self.boxes.world());
        self.prop_effects();
        let mut drawn = self.effects.draw(eye, yaw, pitch, roll);
        drawn.models.extend(projectiles);
        drawn.sway = self.shakes.sway(st, eye.to_array());
        self.c.shake_max = self
            .c
            .shake_max
            .max(self.shakes.strength(st, eye.to_array()));
        self.c.sway_max = self
            .c
            .sway_max
            .max(drawn.sway.iter().fold(0.0, |m, a| m.max(a.abs())));
        self.c.looped_fx_max = self.c.looped_fx_max.max(self.effects.looped_fx());
        self.c.fx_quads_max = self.c.fx_quads_max.max(drawn.quads);
        self.c.fx_tracers_max = self.c.fx_tracers_max.max(drawn.tracers);
        self.c.fx_decals_max = self.c.fx_decals_max.max(drawn.decals);
        self.c.fx_live_max = self.c.fx_live_max.max(self.effects.live_elems());
        drawn
    }

    /// The crosshair of the weapon in `ps`: the spread the server would shoot with right now.
    fn reticle_of(&self, ps: &PlayerState) -> Option<Reticle> {
        let index = sim::pm::viewmodel_weapon(ps);
        let weapon = self.lib.content.weapon(self.weapons.name(index))?.clone();
        let spread_deg = aim_spread_degrees(self.weapons.info(index), ps, &self.weapon_params);
        Some(Reticle {
            weapon,
            spread_deg,
            spread_scale: ps.aim_spread_scale,
            ads: ps.weapon_pos_frac,
            tan_half_fov_y: 0.0,
        })
    }

    /// The events new in `snap` and the server commands nothing here consumed.
    fn take_events(&mut self, snap: &net::Snapshot) -> (Vec<ClientEvent>, Vec<String>) {
        let now = snap.server_time;
        let (look, content) = (&mut self.look, &self.lib.content);
        let file = |n: &str| {
            content.rawfile(n).map(|b| {
                let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
                String::from_utf8_lossy(&b[..end]).into_owned()
            })
        };
        let sound = &mut self.sound;
        self.net.commands.retain(|c| {
            !channel_volume_command(c, sound, &file)
                && !net::ui::ServerCmd::parse(c).is_some_and(|c| look.command(&c, now, &file))
        });
        let commands = self.events.take_commands(&mut self.net.commands);
        let events = self.events.scan(snap);
        for e in &events {
            *self.c.events.entry(e.kind()).or_default() += 1;
        }
        let (weapons, lib) = (&self.weapons, &self.lib);
        self.sound.world_events(
            &events,
            &|w| {
                let info = weapons.get(w)?;
                lib.content.weapon(&info.name).cloned()
            },
            &|client| {
                snap.entities
                    .iter()
                    .find(|e| e.etype == etype::PLAYER && e.client == client)
                    .map(|e| [e.origin[0], e.origin[1], e.origin[2] + 60.0])
            },
        );
        (events, commands)
    }

    /// The shell shock's hold on the view and its sounds for this frame.
    fn shock_effects(&mut self, look: &LookOut) {
        self.max_turn = look.max_turn;
        self.shock_sensitivity = look.sensitivity;
        self.sound.shock(&look.sound);
    }

    /// Feeds the sound system: the listener, the server's sound commands, and the own player's events.
    fn hear(
        &mut self,
        dt: f32,
        eye: Vec3,
        ps: &PlayerState,
        events: &[(u8, u8)],
        snap: &net::Snapshot,
    ) {
        let yaw = self.angles[1].to_radians();
        self.sound.frame(eye.to_array(), yaw, dt);
        let loops: Vec<_> = snap
            .entities
            .iter()
            .filter(|e| e.loop_sound != 0)
            .map(|e| (e.number, e.loop_sound, e.origin))
            .collect();
        self.sound.entity_loops(&loops);
        // Only the sound commands; the rest belong to other systems.
        let (mine, rest): (Vec<String>, Vec<String>) = std::mem::take(&mut self.net.commands)
            .into_iter()
            .partition(|l| ClientSound::is_command(l));
        self.net.commands = rest;
        for line in mine {
            self.sound.command(&line);
        }
        self.sound.hold_breath(
            (dt * 1000.0) as i32,
            ps.weapon_flags & sim::pm::wf::HOLD_BREATH != 0,
            (WeaponParams::default().breath_hold_time * 1000.0) as i32,
        );
        let def = self.lib.content.weapon(self.weapons.name(ps.weapon as u16));
        self.sound.own_events(
            &Who {
                own: true,
                entity: ps.client_num,
                origin: eye.to_array(),
                weapon: def.map(|d| &**d),
                weapon_of: &|w| self.lib.content.weapon(self.weapons.name(w)).cloned(),
                quiet: ps.perks & sim::pm::PERK_QUIETER != 0,
                turret: ps.e_flags & sim::pm::ef::TURRET_ACTIVE != 0,
            },
            events,
        );
    }

    /// `+actionslot N`: what the player state says slot N does (`CG_ActionSlotDown_f`).
    fn action_slot(&mut self, arg: &str) {
        use sim::pm::action_slot as at;
        let Some(slot) = arg
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|s| (1..=4).contains(s))
        else {
            return;
        };
        let Some(snap) = self.net.latest() else {
            return;
        };
        let (kind, param) = (
            snap.ps.action_slot_type[slot - 1],
            snap.ps.action_slot_param[slot - 1],
        );
        let inv = PlayerWeapons::from_words(&snap.inv);
        let cur = self.want_weapon.unwrap_or_else(|| inv.selected());
        match kind {
            at::WEAPON if inv.has(param) => {
                if cur == param {
                    self.want_weapon = self.before_slot.filter(|w| inv.has(*w));
                } else {
                    self.before_slot = Some(cur);
                    self.select_weapon(param);
                }
            }
            at::ALT_MODE => {
                let alt = self.weapons.info(cur).alt_weapon;
                if alt != 0 && inv.has(alt) {
                    self.select_weapon(alt);
                }
            }
            at::NIGHT_VISION => self.nv_press = true,
            _ => {}
        }
    }

    fn command(&mut self, cmd: &str) {
        let words = crate::input::config::split_commands(cmd).into_iter().next();
        if let Some(w) = words.filter(|w| crate::console::is_server_verb(&w[0])) {
            if let Some(line) = crate::console::server_line(&w) {
                self.net.command(&line);
            }
            return;
        }
        if let Some(arg) = cmd.strip_prefix("actionslot ") {
            self.action_slot(arg);
            return;
        }
        if let Some(arg) = cmd.strip_prefix("drawvehicles ") {
            self.draw_vehicles = arg.trim() != "0";
            return;
        }
        let forward = match cmd {
            "weapnext" => true,
            "weapprev" => false,
            _ => return,
        };
        let Some(snap) = self.net.latest() else {
            return;
        };
        // `WeaponCycleAllowed`, `CycleWeapPrimary`: not while dead, frozen or with the weapons disabled.
        let ps = &snap.ps;
        if !alive(ps)
            || ps.pm_flags & pmf::FROZEN != 0
            || ps.weapon_flags & sim::pm::wf::DISABLED != 0
        {
            return;
        }
        let inv = PlayerWeapons::from_words(&snap.inv);
        let cur = self.selected_weapon(&inv, ps);
        if let Some(w) = inv.cycle_primary(&self.weapons, cur, self.latest_primary, forward, false)
        {
            self.select_weapon(w);
        }
    }

    /// The weapon the player has asked for, else the one the scripts did, else the one in hand.
    fn selected_weapon(&self, inv: &PlayerWeapons, ps: &PlayerState) -> u16 {
        self.want_weapon.unwrap_or(match inv.selected() {
            0 => ps.weapon as u16,
            w => w,
        })
    }

    fn select_weapon(&mut self, w: u16) {
        self.want_weapon = Some(w);
        self.note_latest_primary(w);
    }

    fn note_latest_primary(&mut self, w: u16) {
        let primary = PlayerWeapons::latest_primary_of(&self.weapons, w);
        if primary != 0 {
            self.latest_primary = primary;
        }
    }

    /// `CG_OutOfAmmoChange`: the weapon in hand ran dry, so raise another unless it is one that stays up when empty.
    fn out_of_ammo_change(&mut self, ps: &PlayerState, inv: &PlayerWeapons) {
        let held = ps.weapon as u16;
        let stays = self
            .lib
            .content
            .weapon(self.weapons.name(held))
            .is_some_and(|d| d.cancel_auto_holster_when_empty != 0);
        if let Some(w) =
            inv.out_of_ammo_target(&self.weapons, alive(ps), held, stays, self.latest_primary)
        {
            self.select_weapon(w);
        }
    }

    fn send_cmd(&mut self, input: &InputFrame, st: i32) {
        let Some(snap) = self.net.latest() else {
            return;
        };
        self.cmd_time = (self.cmd_time + 1).max(st);
        // A weapon the player asked for is asked for until it is held; after that the scripts decide.
        if self.want_weapon == Some(snap.ps.weapon as u16) {
            self.want_weapon = None;
        }
        let delta = snap.ps.delta_angles;
        let pack = |deg: f32, d: f32| (((deg - d) / ANGLE_UNIT).round() as i32) & 0xffff;
        let weapon = self
            .want_weapon
            .unwrap_or_else(|| PlayerWeapons::from_words(&snap.inv).selected());
        let axis = |v: f32| (v.clamp(-1.0, 1.0) * 127.0) as i8;
        let nv = std::mem::take(&mut self.nv_press);
        let mut buttons = input.buttons as i32 | if nv { sim::pm::button::NIGHTVISION } else { 0 };
        let mut selected_location = [0i8; 2];
        if snap.ps.loc_selection != 0 {
            use sim::pm::button as b;
            let stance = buttons & (b::PRONE | b::CROUCH | b::TEMP_STANCE);
            buttons = stance | b::LOC_SELECTING;
            if input.pressed & buttons::ATTACK != 0 {
                buttons |= b::LOC_CONFIRM;
                let at = |v: f32| ((v * 255.0 + 0.5).floor() as i32 - 128) as i8;
                selected_location = [at(self.loc_cursor[0]), at(self.loc_cursor[1])];
            } else if input.pressed & (buttons::MELEE | buttons::RELOAD) != 0 {
                buttons |= b::LOC_CANCEL;
            }
        }
        // The kick of the player's shots goes to the server with the aim (`CL_FinishMove`); the scope's sway only
        // moves the camera.
        let kick = self.kick.angles();
        let cmd = UserCmd {
            selected_location,
            server_time: self.cmd_time,
            buttons,
            angles: [
                pack(self.angles[0] + kick[0], delta[0]),
                pack(self.angles[1] + kick[1], delta[1]),
                pack(kick[2], delta[2]),
            ],
            weapon: weapon as u8,
            forwardmove: axis(input.move_forward),
            rightmove: axis(input.move_right),
            melee_charge_yaw: self.melee_charge.0,
            melee_charge_dist: self.melee_charge.1,
            ..UserCmd::default()
        };
        if input.buttons & buttons::ATTACK != 0 {
            self.c.shots += 1;
        }
        let sent =
            (cmd.angles[0].wrapping_sub(pack(self.angles[0], delta[0])) as i16).unsigned_abs();
        self.c.max_kick_in_cmd = self.c.max_kick_in_cmd.max(f32::from(sent) * ANGLE_UNIT);
        self.c.cmds += 1;
        self.pred.push(cmd);
        self.net.send_cmd(cmd);
    }

    /// The hands' own camera (reloads, sprints and draws shake it; `CG_ApplyViewAnimation`): moves `seen`, the view
    /// angles, onto the tag's and returns how far it moves the eye. Nothing for a spectator, a turret or the
    /// intermission.
    fn hands_camera(&mut self, ps: &PlayerState, seen: &mut [f32; 3]) -> Vec3 {
        let tag = crate::camera::takes_hands_camera(ps)
            .then(|| self.vm.as_ref().and_then(|(_, v)| v.tags()))
            .flatten()
            .and_then(|t| t.camera);
        let Some(tag) = tag else { return Vec3::ZERO };
        for (a, t) in seen.iter_mut().zip(tag.angles) {
            *a += sim::pm::math::angle_delta(t, *a);
        }
        self.c.camera_tag_frames += 1;
        Vec3::from(tag.offset)
    }

    /// Records what the camera layer did this frame; `eye` is the logical eye.
    fn camera_metrics(
        &mut self,
        cam: &crate::camera::View,
        eye: Vec3,
        dt: f32,
        dead: bool,
        leaning: bool,
    ) {
        let c = &mut self.c;
        // The eye's height with only the stair smoothing applied: bob, a landing's dip and a lean move it too, and
        // would blur whether the smoothing did its part.
        let render_z = eye.z + cam.step;
        let steps = self.pred.steps;
        // A frame that began a stair step: the logical eye jumped by the step, the drawn one must not.
        if steps != c.steps_seen {
            c.steps_seen = steps;
            // A long frame stamps its step well before `now`, so the smoothing has already run part of its way.
            if let (Some(last), Some(drawn)) = (self.last_eye, c.last_render_z)
                && dt > 0.0
                && dt < 0.03
                && !dead
            {
                let jump = (eye.z - last.z).abs();
                // A step the smoothing is full for (`cg_viewZSmoothingMax`) cannot be eased whole, by design.
                // Only a jump the step explains: walking up a slope moves the eye too, and nothing eases that.
                let taken = self.pred.step_taken.abs();
                if (3.0..24.0).contains(&jump)
                    && taken >= jump * 0.8
                    && cam.step.abs() < net::predict::STEP_MAX - 0.01
                {
                    c.stair_frames += 1;
                    let drawn_jump = (render_z - drawn).abs();
                    if drawn_jump > jump * 0.6 {
                        c.stair_snaps += 1;
                        c.stair_last_snap = [jump, drawn_jump, cam.step, dt];
                    }
                }
            }
        }
        // How fast the eye moved, with the stairs eased as they are drawn: a step the smoothing absorbs is not a jerk.
        if let (Some(last), Some(drawn)) = (self.last_eye, c.last_render_z)
            && dt > 0.0
            && dt < 0.05
            && !dead
        {
            let moved = Vec3::new(eye.x - last.x, eye.y - last.y, render_z - drawn).length();
            // A respawn or a teleport is not walking.
            if moved < 64.0 {
                c.eye_speeds.push(moved / dt);
            }
        }
        c.last_render_z = Some(render_z);
        c.lean_frames += u64::from(leaning);
        c.view_lean_max = c.view_lean_max.max(cam.lean.abs());
        c.view_dip_max = c.view_dip_max.max(cam.dip.abs());
        c.view_step_max = c.view_step_max.max(cam.step.abs());
        c.view_bob_max = c
            .view_bob_max
            .max((cam.offset[2] - cam.step - cam.dip).abs());
        c.view_turn_max = c
            .view_turn_max
            .max(cam.angles[0].abs().max(cam.angles[1].abs()));
    }

    /// The view model of the weapon on show (`BG_GetViewmodelWeaponIndex`: the off-hand weapon while one is thrown),
    /// built when it changes. `clip_empty`: its magazine is.
    fn view_model(
        &mut self,
        dt: f32,
        ps: &PlayerState,
        feet: [f32; 3],
        look: [f32; 3],
        clip_empty: bool,
    ) -> Vec<ModelInstance> {
        let index = sim::pm::viewmodel_weapon(ps);
        let key = (index, ps.viewmodel_index);
        if self.vm.as_ref().is_none_or(|(k, _)| *k != key) {
            if self.vm_spare.as_ref().is_some_and(|(k, _)| *k == key) {
                std::mem::swap(&mut self.vm, &mut self.vm_spare);
                if let Some((_, vm)) = self.vm.as_mut() {
                    vm.resume();
                }
                self.c.weapon = self.weapons.name(index).to_owned();
            } else if self.vm.is_some() {
                self.vm_spare = self.vm.take();
            }
        }
        if self.vm.as_ref().is_none_or(|(k, _)| *k != key) {
            let name = self.weapons.name(index).to_owned();
            // The hands the script gave the player (`setviewmodel`), else the weapon's own.
            let hands = self
                .net
                .ui_ref()
                .map(|ui| ui.model(ps.viewmodel_index))
                .filter(|n| self.lib.content.model(n).is_some())
                .map(str::to_owned);
            match self
                .lib
                .content
                .weapon(&name)
                .cloned()
                .ok_or_else(|| format!("weapon {name} not loaded"))
                .and_then(|d| ViewModel::new(&self.lib.content, &d, hands.as_deref()))
            {
                Ok(v) => {
                    self.c.weapon = name;
                    self.vm = Some((key, v));
                }
                // Weapon 0 is "none": the player has no body yet.
                Err(e) if index != 0 => {
                    self.c.unknown_weapon.insert(format!("{index}:{name}"), e);
                }
                Err(_) => {}
            }
        }
        let Some((_, vm)) = self.vm.as_mut() else {
            self.sight = None;
            return Vec::new();
        };
        let mut shown = ps.clone();
        shown.origin = feet;
        shown.viewangles = look;
        self.c.frames_with_viewmodel += 1;
        let speed = self.kick.take_gun_speed();
        self.c.gun_speed_given += speed[0].abs() + speed[1].abs();
        vm.kick_gun(speed);
        vm.set_felt(self.fx.hit, self.look.shock_end());
        vm.set_clip_empty(clip_empty);
        let models = vm.update(&shown, dt);
        for alias in vm.take_sounds() {
            self.sound.play_ui(&alias);
        }
        let g = vm.gun_state();
        let recoil = g.offset[0].abs().max(g.offset[1].abs());
        self.c.max_gun_recoil = self.c.max_gun_recoil.max(recoil);
        self.c.max_gun_sway = self
            .c
            .max_gun_sway
            .max(g.sway_angles[0].abs().max(g.sway_angles[1].abs()));
        self.c.gun_recoil_settled += u64::from(self.c.gun_recoil_live && recoil == 0.0);
        self.c.gun_recoil_live = recoil != 0.0;
        self.c.gun_springs = vm.has_recoil_spring();
        let sight = vm.sight();
        // Through the scope the original draws no gun.
        let scoped = sight.overlay.is_some();
        self.sight = Some(sight);
        if scoped { Vec::new() } else { models }
    }

    /// The scripted models of the level (`script_model`: props, cars, objectives), posed at the origin and angles the
    /// server gave them, between the snapshots around the interpolation moment like the players they move with.
    fn script_models(&mut self, dt: f32, st: i32, own: u16) -> Vec<ModelInstance> {
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own));
        let Some(ui) = self.net.ui() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = 0;
        self.c.script_models_unloaded.clear();
        for e in ents
            .iter()
            .filter(|e| matches!(e.etype, etype::SCRIPT_MODEL | etype::PLANE))
        {
            seen += 1;
            let name = ui.model(e.model);
            match self.lib.content.model(name) {
                Some(model) => {
                    let posed = if e.eflags & eflags::PHYSICS_LAUNCH != 0 {
                        self.launches.pose(e, model, dt, self.boxes.world())
                    } else {
                        Some((e.origin, e.angles))
                    };
                    // A launch that was given up is not drawn.
                    let Some((origin, angles)) = posed else {
                        continue;
                    };
                    let mut m = ModelInstance::new(model.clone(), render::ModelKind::World);
                    m.origin = origin;
                    m.angles = angles;
                    m.light_origin = origin;
                    out.push(m);
                    if !self.c.script_models_names.contains(name) {
                        self.c.script_models_names.insert(name.to_owned());
                    }
                }
                None => {
                    self.c.script_models_unloaded.insert(name.to_owned());
                }
            }
        }
        self.launches.finish_frame(dt);
        self.c.script_models_seen = seen;
        self.c.script_models_drawn = out.len();
        out
    }

    /// The map's inline models that the newest snapshot has as entities (`script_brushmodel`), ahead of it by the way
    /// they moved since, the same way the player standing on one is carried: a lift under the feet is drawn under the
    /// feet.
    fn brush_models(&mut self, snap: &net::Snapshot, st: i32) -> Vec<render::BrushInstance> {
        let mut out = Vec::new();
        let mut seen = 0;
        for e in snap.entities.iter().filter(|e| e.etype == etype::BRUSH) {
            if !self
                .brush_surfaces
                .get(usize::from(e.model))
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            seen += 1;
            let by = net::predict::mover_offset(e.velocity, st - snap.server_time);
            out.push(render::BrushInstance {
                model: e.model,
                origin: [0, 1, 2].map(|i| e.origin[i] + by[i]),
                angles: e.angles,
            });
        }
        self.c.brush_models_seen = seen;
        self.c.brush_models_max = self.c.brush_models_max.max(seen);
        out
    }

    /// The weapons lying on the floor, in the world model of their weapon and variant.
    fn items(&self, snap: &net::Snapshot) -> Vec<ModelInstance> {
        let mut out = Vec::new();
        for e in snap.entities.iter().filter(|e| e.etype == etype::ITEM) {
            let Some(def) = self.lib.content.weapon(self.weapons.name(e.weapon)) else {
                continue;
            };
            let Some(model) = def
                .world_models
                .get(usize::from(e.model))
                .cloned()
                .flatten()
                .or_else(|| def.world_models.first().cloned().flatten())
            else {
                continue;
            };
            let mut m = ModelInstance::new(model, render::ModelKind::World);
            m.origin = e.origin;
            m.angles = e.angles;
            m.light_origin = e.origin;
            out.push(m);
        }
        out
    }

    /// The grenades, rockets and other projectiles in flight or at rest (`CG_Missile`): the weapon's projectile model
    /// along the entity's angles, and the flight loop of weapons that have one following it.
    fn projectiles(&mut self, flying: &[EntityState], st: i32) -> Vec<ModelInstance> {
        let mut out = Vec::new();
        let mut looping = Vec::new();
        for e in flying {
            let Some(def) = self.lib.content.weapon(self.weapons.name(e.weapon)) else {
                continue;
            };
            if let Some(alias) = def
                .sounds
                .projectile_sound
                .as_deref()
                .filter(|a| !a.is_empty())
            {
                self.sound.missile_loop(e.number, alias, e.origin);
                looping.push(e.number);
            }
            // Not drawn until its launch time (a rocket leaves the tube before it shows).
            let until_launch = ((e.eflags.wrapping_sub(st as u32) << 8) as i32) >> 8;
            let Some(model) = def.projectile_model.clone().filter(|_| until_launch <= 0) else {
                continue;
            };
            let mut m = ModelInstance::new(model, render::ModelKind::World);
            m.origin = e.origin;
            m.angles = e.angles;
            m.light_origin = e.origin;
            out.push(m);
        }
        self.sound.missile_loops_end(&looping);
        self.c.projectiles_max = self.c.projectiles_max.max(out.len());
        out
    }

    /// The vehicles (helicopters), between the snapshots around the interpolation moment, with their rotors turning.
    fn vehicles(&mut self, dt: f32, st: i32, own: u16) -> Vec<ModelInstance> {
        self.rotor_clock += dt;
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own));
        let Some(ui) = self.net.ui() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = 0;
        self.c.vehicles_unloaded.clear();
        for e in ents.iter().filter(|e| e.etype == etype::VEHICLE) {
            seen += 1;
            if !self.draw_vehicles {
                continue;
            }
            let name = ui.model(e.model);
            let Some(model) = self.lib.content.model(name) else {
                self.c.vehicles_unloaded.insert(name.to_owned());
                continue;
            };
            let rotors = self
                .rotors
                .entry(name.to_owned())
                .or_insert_with(|| Rotors::new(&self.lib.content, model).ok());
            let mut m = ModelInstance::new(model.clone(), render::ModelKind::World);
            m.origin = e.origin;
            m.angles = e.angles;
            m.light_origin = e.origin;
            if let Some(r) = rotors {
                m.bones = r.pose(self.rotor_clock).to_vec();
            }
            out.push(m);
        }
        self.c.vehicles_seen = seen;
        self.c.vehicles_drawn = out.len();
        self.c.vehicles_max = self.c.vehicles_max.max(out.len());
        out
    }

    /// Models of every other player, between the snapshots around the interpolation moment.
    fn remote_players(&mut self, dt: f32, st: i32, own: u16, show_own: bool) -> Vec<ModelInstance> {
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, (!show_own).then_some(own));
        if !show_own {
            // Not seen through its own eyes: the body is made anew (and falls anew) the next time it is.
            self.remotes.remove(&own);
        }
        let now = Instant::now();
        let mut out = Vec::new();
        let mut players = 0;
        let mut drawn = 0;
        // Where the guns are, for the flashes and shells that follow them.
        let mut guns = HashMap::new();
        for e in ents.iter().filter(|e| e.etype == etype::PLAYER) {
            players += 1;
            let def = self
                .weapons
                .get(e.weapon)
                .and_then(|i| self.lib.content.weapon(&i.name));
            // The player's own events are heard as the prediction raises them, not twice.
            if e.client != own {
                self.sound.events(
                    &Who {
                        own: false,
                        entity: e.number,
                        origin: e.origin,
                        weapon: def.map(|d| &**d),
                        weapon_of: &|w| self.lib.content.weapon(self.weapons.name(w)).cloned(),
                        quiet: e.perks & sim::pm::PERK_QUIETER != 0,
                        turret: false,
                    },
                    e.event_seq,
                    &e.recent_events(),
                );
            }
            // A dead player is not drawn: the body the server's corpse entity stands for is, and it falls from the
            // pose the player was last seen in.
            if e.eflags & eflags::DEAD != 0 {
                continue;
            }
            let weapon = self
                .weapons
                .get(e.weapon)
                .and_then(|i| self.lib.content.weapon(&i.name))
                .cloned();
            // A gunner on a turret holds nothing: the weapon leaves the body.
            let held = weapon
                .as_ref()
                .filter(|_| e.eflags & eflags::TURRET == 0)
                .and_then(|w| w.world_models.first().cloned().flatten())
                .and_then(|m| m.name.as_deref().map(str::to_owned));
            // A player the scripts have not given a model yet (just joined) is not drawn.
            let Some(set) = self.body_set(e.model, e.eflags).map(|set| PlayerModelSet {
                weapon: held.clone(),
                ..set
            }) else {
                continue;
            };
            match self.remotes.get_mut(&e.client) {
                Some(r) if r.body != set.body => {
                    // A new class: another body.
                    match self.lib.player(&set) {
                        Ok(p) => {
                            r.player = p;
                            r.body.clone_from(&set.body);
                            r.weapon = held;
                        }
                        Err(e) => {
                            self.c.player_faults.insert(e);
                        }
                    }
                }
                None => match self.lib.player(&set) {
                    Ok(player) => {
                        self.remotes.insert(
                            e.client,
                            Remote {
                                player,
                                body: set.body.clone(),
                                weapon: held,
                                seen: now,
                            },
                        );
                    }
                    Err(e) => {
                        self.c.player_faults.insert(e);
                    }
                },
                Some(r) if r.weapon != held => match self.lib.player(&set) {
                    Ok(p) => {
                        r.player.rearm(p);
                        r.weapon = held;
                    }
                    Err(e) => {
                        self.c.player_faults.insert(e);
                    }
                },
                Some(_) => {}
            }
            let Some(r) = self.remotes.get_mut(&e.client) else {
                continue;
            };
            r.seen = now;
            let input = pose_input(e, weapon.as_deref(), false);
            r.player.update(dt, &input);
            guns.insert(
                e.client,
                ViewTags {
                    flash: r.player.weapon_tag(e.origin, "tag_flash"),
                    brass: r.player.weapon_tag(e.origin, "tag_brass"),
                    knife: None,
                    camera: None,
                },
            );
            if matches!(
                e.weapon_state,
                sim::pm::weapon_state::RELOADING
                    | sim::pm::weapon_state::RELOAD_START
                    | sim::pm::weapon_state::MELEE_INIT
                    | sim::pm::weapon_state::OFFHAND_HOLD
            ) {
                self.c.remote_weapon_frames += 1;
            }
            if let Some(c) = r.player.torso_animation() {
                self.c.torso_clips.insert(c);
            }
            drawn += 1;
            out.extend(r.player.instances(e.origin));
        }
        out.extend(self.corpse_models(dt, &ents, now));
        self.effects.set_remote_tags(guns);
        let present: Vec<u16> = ents
            .iter()
            .filter(|e| e.etype == etype::PLAYER)
            .map(|e| e.number)
            .collect();
        self.sound.forget_absent(&present);
        self.c.max_players_seen = self.c.max_players_seen.max(players);
        self.c.max_players_drawn = self.c.max_players_drawn.max(drawn);
        self.remotes.retain(|_, r| now - r.seen < GONE_AFTER);
        out
    }

    #[cfg(target_arch = "wasm32")]
    pub fn sound(&self) -> &ClientSound {
        &self.sound
    }

    /// What the run saw, for the client report.
    pub fn report(&mut self) -> Value {
        let s = self.net.stats();
        let moved = self.c.start.map_or(0.0, |s| {
            ((self.c.end[0] - s[0]).powi(2) + (self.c.end[1] - s[1]).powi(2)).sqrt()
        });
        let mut report = json!({
            "connected": self.net.connected(),
            "spawned": self.c.spawned,
            "snapshots": s.map(|s| s.packets_in),
            "bytes_in": s.map(|s| s.bytes_in),
            "bytes_out": s.map(|s| s.bytes_out),
            "unusable_snapshots": self.net.unusable(),
            "usercmds": self.c.cmds,
            "predictions": self.c.predictions,
            "prediction_corrections": self.pred.corrections,
            "players_seen_max": self.c.max_players_seen,
            "moved": moved,
            "path": self.c.path,
            "attack_cmds": self.c.shots,
            "target_frames": self.c.target_frames,
            "in_range_frames": self.c.in_range_frames,
            "weapon": self.c.weapon,
            "viewmodel_frames": self.c.frames_with_viewmodel,
            "weapons_without_models": self.c.unknown_weapon,
            "skin_faults": render::skin::skin_faults(),
            "script_models_seen": self.c.script_models_seen,
            "script_models_drawn": self.c.script_models_drawn,
            "script_models_unloaded": self.c.script_models_unloaded,
            "script_models_names": self.c.script_models_names,
            "vehicles_seen": self.c.vehicles_seen,
            "vehicles_drawn": self.c.vehicles_drawn,
            "vehicles_max_drawn": self.c.vehicles_max,
            "vehicles_unloaded": self.c.vehicles_unloaded,
            "events": self.c.events,
            "fx": {
                "played": self.effects.played,
                "missing": self.effects.missing,
                "quads_max": self.c.fx_quads_max,
                "decals_max": self.c.fx_decals_max,
                "ragdolls": self.c.ragdolls,
                "vision": self.look.naked,
                "vision_night": self.look.night,
                "look_missing": self.look.missing,
                "shocks": self.look.shocks,
                "props": self.props.len(),
                "props_woken": self.props.woken,
                "live_elems_max": self.c.fx_live_max,
            },
            "eye": self.last_eye.map(|e| e.to_array()),
            "eye_speed": eye_speed_summary(&self.c.eye_speeds),
            "sound": self.sound.report(),
        });
        report["fx"]["tracers_max"] = json!(self.c.fx_tracers_max);
        report["gun_speed_given"] = json!(self.c.gun_speed_given);
        report["gun_recoil_max"] = json!(self.c.max_gun_recoil);
        report["gun_sway_max"] = json!(self.c.max_gun_sway);
        report["gun_recoil_settled"] = json!(self.c.gun_recoil_settled);
        report["gun_has_spring"] = json!(self.c.gun_springs);
        report["view_kick_max"] = json!(self.c.max_kick_up);
        report["view_kick_in_cmd_max"] = json!(self.c.max_kick_in_cmd);
        report["view_kick_settled"] = json!(self.c.kick_settled);
        report["projectiles_max_drawn"] = self.c.projectiles_max.into();
        report["brush_models_seen"] = self.c.brush_models_seen.into();
        report["brush_models_max"] = self.c.brush_models_max.into();
        report["fx"]["looped_fx_max"] = self.c.looped_fx_max.into();
        report["fx"]["corpses_max"] = self.c.corpses_max.into();
        report["fx"]["camera_shake_max"] = self.c.shake_max.into();
        report["fx"]["camera_sway_max"] = self.c.sway_max.into();
        report["view"] = json!({
            "steps": self.pred.steps,
            "stair_frames": self.c.stair_frames,
            "stair_snaps": self.c.stair_snaps,
            "stair_last_snap": self.c.stair_last_snap,
            "lean_frames": self.c.lean_frames,
            "lean_max": self.c.view_lean_max,
            "landings": self.camera.landings,
            "dip_max": self.c.view_dip_max,
            "step_max": self.c.view_step_max,
            "bob_max": self.c.view_bob_max,
            "turn_max": self.c.view_turn_max,
            "camera_tag_frames": self.c.camera_tag_frames,
            "death_view_frames": self.c.death_view_frames,
            "death_view_range_max": self.c.death_view_range_max,
            "kill_cam_frames": self.c.kill_cam_frames,
            "dof_frames": self.c.dof_frames,
        });
        report["damage_events"] = json!(self.c.damage_events);
        report["damage_flash_max"] = json!(self.c.max_damage_flash);
        report["damage_wedges_max"] = json!(self.c.max_damage_wedges);
        report["players_drawn_max"] = self.c.max_players_drawn.into();
        report["player_faults"] = json!(self.c.player_faults);
        report["torso_clips"] = json!(self.c.torso_clips);
        report["remote_weapon_frames"] = self.c.remote_weapon_frames.into();
        report
    }

    /// Where the player's body is, for the autoplay and the report.
    fn feet(&self) -> [f32; 3] {
        self.c.end
    }
}

fn team_of(flags: u32) -> Option<Team> {
    if flags & eflags::TEAM_AXIS != 0 {
        Some(Team::Axis)
    } else if flags & eflags::TEAM_ALLIES != 0 {
        Some(Team::Allies)
    } else {
        None
    }
}

/// The pose of a remote player from what the snapshot says of it.
fn pose_input(
    e: &EntityState,
    weapon: Option<&assets::zone::weapon::WeaponDef>,
    dead: bool,
) -> PlayerPoseInput {
    let pm_type = PmType::from_u8(e.pm_type);
    let ps = PlayerState {
        pm_flags: e.pm_flags,
        pm_type: if dead && pm_type < PmType::Dead {
            PmType::Dead
        } else {
            pm_type
        },
        velocity: e.velocity,
        viewangles: e.angles,
        leanf: e.angles[2] / 45.0,
        movement_dir: e.move_dir,
        weapon_pos_frac: f32::from(e.ads) / 255.0,
        weapon_state: e.weapon_state,
        torso_pitch: e.torso_pitch,
        waist_pitch: e.waist_pitch,
        damage_timer: i32::from(e.damage_timer),
        damage_duration: i32::from(e.damage_duration),
        e_flags: if e.eflags & eflags::TURRET != 0 {
            sim::pm::ef::TURRET_ACTIVE
        } else {
            0
        },
        ..PlayerState::default()
    };
    let moving = e.velocity[0].hypot(e.velocity[1]) > 10.0;
    let mut i = PlayerPoseInput::from_ps(&ps, moving, weapon);
    i.sprinting = e.pm_flags & pmf::SPRINTING != 0;
    i.torso_wire = Some(TorsoWire {
        clip: e.torso_clip,
        cap: e.torso_cap,
        seq: e.torso_seq,
    });
    i.legs_wire = Some(LegsWire {
        clip: e.legs_clip,
        seq: e.legs_seq,
    });
    i.seed = u32::from(e.event_seq) << 16 | u32::from(e.damage_duration);
    i
}

// ---- autoplay -------------------------------------------------------------------------------------------------

/// A bot's brain for the harness: walks, turns away from walls, turns toward the nearest enemy it can see and fires.
#[derive(Default)]
struct Auto {
    t: f32,
    turn_until: f32,
    turn_dir: f32,
    stuck: f32,
    last_feet: Option<[f32; 3]>,
    reload_at: f32,
    target_seen_at: f32,
}

impl NetPlay {
    /// The autoplay answers the menus the scripts open (team, then class) as a person would; a
    /// person's menus are the menu runtime's, which drains the same events.
    fn answer_menus(&mut self) {
        let Some(ui) = self.net.ui() else { return };
        let events = ui.drain_events();
        if let Some(name) = events.iter().rev().find_map(|e| match e {
            net::ui::UiEvent::Map { name } => Some(name.clone()),
            _ => None,
        }) {
            self.new_level = Some(name);
        }
        let Some(join) = self.auto_join.as_mut() else {
            self.ui_events.extend(events);
            return;
        };
        let (answers, shown) = join.filter(events, self.auto.is_some());
        for a in answers {
            self.net.command(&a);
        }
        self.ui_events.extend(shown);
    }

    fn autoplay(&mut self, dt: f32, st: i32, own: u16) -> InputFrame {
        let Some(mut a) = self.auto.take() else {
            return InputFrame::default();
        };
        a.t += dt;
        let feet = self.feet();
        // Stuck when barely moving while trying to.
        if let Some(l) = a.last_feet {
            let d = (feet[0] - l[0]).hypot(feet[1] - l[1]);
            a.stuck = if d < 30.0 * dt { a.stuck + dt } else { 0.0 };
        }
        a.last_feet = Some(feet);
        let mut f = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        if a.stuck > 0.3 && a.t > a.turn_until {
            // Turn toward the side with more room.
            let from = Vec3::from(feet) + Vec3::Z * 40.0;
            let room = |off: f32| {
                let y = (self.angles[1] + off).to_radians();
                let to = from + Vec3::new(y.cos(), y.sin(), 0.0) * 400.0;
                self.boxes
                    .world()
                    .trace(
                        from.to_array(),
                        to.to_array(),
                        [-12.0, -12.0, 0.0],
                        [12.0, 12.0, 30.0],
                        ENTITYNUM_NONE,
                        contents::SOLID,
                    )
                    .fraction
            };
            let (left, right) = (room(60.0) + room(120.0), room(-60.0) + room(-120.0));
            a.turn_until = a.t + 0.7;
            a.turn_dir = if left >= right { 1.0 } else { -1.0 };
            a.stuck = 0.0;
        }
        let my_team = self
            .net
            .latest()
            .and_then(|s| s.entity(own))
            .map(|e| e.eflags & (eflags::TEAM_AXIS | eflags::TEAM_ALLIES))
            .unwrap_or(0);
        let eye = self.last_eye.unwrap_or(Vec3::from(feet) + Vec3::Z * 60.0);
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own));
        let mut best: Option<(f32, Vec3)> = None;
        let mut nearest: Option<(f32, Vec3)> = None;
        const SEE: f32 = 1500.0;
        const FIRE: f32 = 900.0;
        for e in ents.iter().filter(|e| e.etype == etype::PLAYER) {
            let flags = e.eflags;
            if flags & eflags::DEAD != 0
                || flags & (eflags::TEAM_AXIS | eflags::TEAM_ALLIES) == my_team
            {
                continue;
            }
            let at = Vec3::from(e.origin) + Vec3::Z * 50.0;
            let d = at.distance(eye);
            if nearest.is_none_or(|(n, _)| d < n) {
                nearest = Some((d, at));
            }
            if d > SEE || best.is_some_and(|(b, _)| b <= d) {
                continue;
            }
            let t = self.boxes.world().trace(
                eye.to_array(),
                at.to_array(),
                [0.0; 3],
                [0.0; 3],
                ENTITYNUM_NONE,
                contents::SOLID,
            );
            if t.fraction >= 0.99 {
                best = Some((d, at));
            }
        }
        let visible = best.is_some();
        // Out of sight, the bot walks toward the nearest enemy anyway (a test driver, not a person: it reads the
        // snapshot) and turns away from walls when stuck.
        let aim = best.or(nearest);
        if a.t < a.turn_until && !visible {
            f.look_delta_yaw = a.turn_dir * 200.0 * dt;
            f.move_forward = 0.7;
        } else if let Some((dist, at)) = aim.filter(|(d, _)| *d > 120.0) {
            if visible {
                self.c.target_frames += 1;
                self.c.in_range_frames += u64::from(dist < FIRE);
                a.target_seen_at = a.t;
            }
            let d = at - eye;
            let want_yaw = d.y.atan2(d.x).to_degrees();
            let want_pitch = -d.z.atan2(d.x.hypot(d.y)).to_degrees();
            let wrap = |x: f32| (x + 180.0).rem_euclid(360.0) - 180.0;
            let (ey, ep) = (wrap(want_yaw - self.angles[1]), want_pitch - self.angles[0]);
            // A person turns at a finite rate; so does this.
            let max = 540.0 * dt;
            f.look_delta_yaw = ey.clamp(-max, max);
            f.look_delta_pitch = ep.clamp(-max, max);
            // Pulled for 0.35 s and released for 0.1 s: single-shot weapons fire once per pull, heavy ones need the hold.
            if visible && dist < FIRE && ey.abs() < 4.0 && ep.abs() < 4.0 && a.t % 0.45 < 0.35 {
                f.buttons |= buttons::ATTACK;
            }
            f.move_forward = if dist > 250.0 || !visible { 1.0 } else { 0.0 };
        } else if a.t < a.turn_until {
            f.look_delta_yaw = a.turn_dir * 240.0 * dt;
            f.move_forward = 0.4;
        } else if (a.t * 0.7) as i32 % 5 == 4 {
            f.look_delta_yaw = 45.0 * dt;
        }
        f.look_delta_pitch +=
            (0.0 - self.angles[0]).clamp(-1.0, 1.0) * if aim.is_none() { 1.0 } else { 0.0 };
        // With nothing to shoot at, the bot leans out for a moment now and then, so the camera's lean is exercised
        // against the real server. (It does not hop: a jump is a burst of eye speed the jerkiness check would count.)
        if !visible && a.t % 8.0 < 0.6 {
            f.buttons |= buttons::LEAN_RIGHT;
        }
        if a.t - a.reload_at > 3.0 && a.t - a.target_seen_at > 0.3 {
            a.reload_at = a.t;
            f.buttons |= buttons::RELOAD;
        }
        self.auto = Some(a);
        (f.cursor_yaw, f.cursor_pitch) = (f.look_delta_yaw, f.look_delta_pitch);
        f
    }
}

/// The server turning the player (a spawn, a script) moves the look angles with it: `angles` follow the change
/// of `delta` since the last one seen. While watching another player (`following`) the delta is theirs, not a turn
/// of this player's view; the turn is measured against the last delta that was the player's own.
fn follow_server_turn(
    angles: &mut [f32; 2],
    last: &mut Option<[f32; 3]>,
    delta: [f32; 3],
    following: bool,
) {
    if following {
        return;
    }
    if let Some(old) = last.replace(delta) {
        for i in 0..2 {
            angles[i] += (delta[i] - old[i] + 180.0).rem_euclid(360.0) - 180.0;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_first_snapshot_and_every_respawn_start_a_life() {
        let mut last = None;
        assert!(
            new_life(&mut last, 0),
            "the first snapshot, even with spawn count 0"
        );
        assert!(!new_life(&mut last, 0));
        assert!(new_life(&mut last, 1));
        assert!(!new_life(&mut last, 1));
    }

    use super::*;

    fn remote(f: impl FnOnce(&mut EntityState)) -> PlayerPoseInput {
        let mut e = EntityState::new(3);
        e.etype = net::entity::etype::PLAYER;
        f(&mut e);
        pose_input(&e, None, e.eflags & eflags::DEAD != 0)
    }

    #[test]
    fn a_remote_last_stand_player_plays_the_last_stand_idle() {
        let i = remote(|e| e.pm_type = PmType::LastStand as u8);
        assert_eq!(i.select(), "pb_laststand_idle");
        assert_eq!(remote(|_| {}).select(), "pb_stand_alert");
        // Dead from the flag even if the type byte lags.
        assert!(remote(|e| e.eflags = eflags::DEAD).dead);
    }

    #[test]
    fn a_remote_players_lean_turret_and_body_tilt_reach_the_pose() {
        let i = remote(|e| {
            e.angles = [0.0, 90.0, 22.5];
            e.eflags = eflags::TURRET;
            e.torso_pitch = 12.0;
            e.waist_pitch = 5.0;
            e.damage_timer = 300;
            e.damage_duration = 400;
        });
        assert!(i.walking, "leaning counts as walking");
        assert!(i.turret);
        assert_eq!((i.torso_pitch, i.waist_pitch), (12.0, 5.0));
        assert_eq!((i.damage_timer, i.damage_duration), (300, 400));
    }

    #[test]
    fn a_remote_players_torso_channel_is_the_servers() {
        let i = remote(|e| {
            e.torso_clip = 4;
            e.torso_cap = 15;
            e.torso_seq = 9;
        });
        assert_eq!(
            i.torso_wire,
            Some(TorsoWire {
                clip: 4,
                cap: 15,
                seq: 9
            })
        );
    }

    #[test]
    fn a_watched_players_deltas_do_not_turn_the_watchers_view() {
        let (mut angles, mut last) = ([10.0, 90.0], None);
        follow_server_turn(&mut angles, &mut last, [0.0, 90.0, 0.0], false);
        // A killcam of someone facing elsewhere.
        follow_server_turn(&mut angles, &mut last, [5.0, 200.0, 0.0], true);
        assert_eq!(angles, [10.0, 90.0]);
        // Respawned facing 30 degrees further round: the turn counts from the last delta of their own.
        follow_server_turn(&mut angles, &mut last, [0.0, 120.0, 0.0], false);
        assert_eq!(angles, [10.0, 120.0]);
    }
}

/// The server's `setchannelvolumes` (`chanvol <priority> <shock> <fade ms>`) and `deactivatechannelvolumes`
/// (`chanvoloff <priority> <fade ms>`); true if `line` was one.
fn channel_volume_command(
    line: &str,
    sound: &mut ClientSound,
    file: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let mut w = line.split_whitespace();
    let word = w.next();
    let (prio, rest): (u8, Vec<&str>) = match (word, w.next().and_then(|p| p.parse().ok())) {
        (Some("chanvol" | "chanvoloff"), Some(p)) => (p, w.collect()),
        _ => return false,
    };
    let fade = |s: Option<&&str>| s.and_then(|f| f.parse().ok()).unwrap_or(0);
    if word == Some("chanvoloff") {
        sound.clear_channel_volumes(prio, fade(rest.first()));
    } else if let Some(name) = rest.first() {
        let text = file(&format!("shock/{name}.shock")).or_else(|| file("shock/default.shock"));
        if let Some(t) = text {
            let p = crate::look::ShockParams::parse(&t);
            sound.set_channel_volumes(prio, p.volumes(), fade(rest.get(1)));
        }
    }
    true
}
