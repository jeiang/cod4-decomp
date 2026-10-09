// SPDX-License-Identifier: GPL-3.0-only
//! Playing on a server: the connection, the usercmds built from the player's input, prediction of the player's own
//! movement, and the models of everyone else drawn from the interpolated snapshots.
//!
//! Every render frame the network is read; about every 8 ms (the original's `cl_maxpackets` ceiling) the input becomes
//! one usercmd stamped with the client's estimate of the server clock. The player's body is the newest snapshot's
//! player state with the unacknowledged commands replayed on top ([`net::predict`]); other players are drawn
//! [`net::view::INTERP_DELAY_MS`] behind the server clock between the two snapshots around that moment, which is also
//! the moment the server rewinds them to when it judges this client's shots.

mod hud;
mod names;

use crate::crosshair::Reticle;
use crate::damage::{DamageHud, DamageView};
use crate::effects::Effects;
use crate::events::{ClientEvent, Events};
use crate::helicopter::Rotors;
use crate::input::{Feedback, InputFrame, Seen, buttons, scan_own};
use crate::kick::{Kick, Scope};
use crate::look::{Look, LookOut, cap_turn};
use crate::models::{Library, Player, PlayerModelSet, Team};
use crate::props::{Launches, Props};
use crate::ragdoll::Ragdoll;
use crate::sound::{ClientSound, Who};
use crate::viewmodel::{Sight, ViewModel};
use crate::wire::Wire;
use glam::Vec3;
use net::client::NetClient;
use net::entity::{EntityState, etype};
use net::predict::{Env, PlayerBoxes, Predictor};
use render::ModelInstance;
use serde_json::{Value, json};
use server::netsv::eflags;
use server::playeranim::{PlayerPoseInput, TorsoWire};
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::{ANGLE_UNIT, Params, PlayerState, PmType, UserCmd, pmf};
use sim::weapon::fire::aim_spread_degrees;
use sim::weapon::{PlayerWeapons, WeaponParams, WeaponTable};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use web_time::Instant;

/// The shortest interval between two usercmds.
const CMD_INTERVAL: Duration = Duration::from_millis(8);
/// How often the scoreboard asks for fresh rows while it is up (`UpdateScores`).
const SCORES_INTERVAL: Duration = Duration::from_secs(2);
/// How long a player model is kept after the snapshots stop mentioning it.
const GONE_AFTER: Duration = Duration::from_millis(500);

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
    /// The aim zoom and scope overlay of the held weapon.
    pub sight: Option<Sight>,
    /// The field of view the world is drawn with regardless of the weapon: a turret's or the intermission's.
    pub fixed_fov: Option<f32>,
    /// Effect sprites and decals.
    pub meshes: Vec<render::DynMesh>,
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
}

struct Remote {
    player: Player,
    /// The body model the player is drawn with.
    body: String,
    /// World model of the weapon the player holds.
    weapon: Option<String>,
    seen: Instant,
    /// Whether the player was dead as of the last frame; `None` before the first.
    dead: Option<bool>,
    /// The body of a player who died while watched: where it fell from, which way it faced, and the simulation.
    ragdoll: Option<([f32; 3], f32, Ragdoll)>,
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
    max_kick_in_cmd: f32,
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
    fx_decals_max: usize,
    /// Ragdolls made for players seen dying.
    ragdolls: u64,
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
    lib: Library,
    weapons: WeaponTable,
    params: Params,
    boxes: PlayerBoxes,
    pred: Predictor,
    /// Pitch (positive down) and yaw (positive left) in degrees, as the player has turned.
    angles: [f32; 2],
    /// The kick of the player's own shots; kept apart from `angles`, the player's aim.
    kick: Kick,
    /// The screen's answer to the hits the player takes, and what the HUD draws of it this frame.
    damage: DamageView,
    damage_hud: DamageHud,
    pitch_limits: (f32, f32),
    cmd_time: i32,
    last_cmd: Instant,
    want_weapon: Option<u16>,
    /// The weapon held before an action slot picked another, for the slot's second press.
    before_slot: Option<u16>,
    /// A night-vision slot was pressed: the next command carries the button.
    nv_press: bool,
    /// The own player's event counter last acted on, and what the input layer is yet to be told of it.
    own_events: Seen,
    feedback: Feedback,
    /// Where the map pick of a location selection points, 0..1 across and down the map.
    pub loc_cursor: [f32; 2],
    remotes: HashMap<u16, Remote>,
    /// The impulse of each recent death by client number, until the body is made.
    pushes: HashMap<u16, [f32; 3]>,
    vm: Option<((u16, u16), ViewModel)>,
    /// What aiming did to the view in the last frame.
    sight: Option<Sight>,
    c: Counters,
    auto: Option<Auto>,
    auto_join: Option<net::ui::AutoJoin>,
    /// Events for the menu runtime (a person's menus drain them; autoplay answers them itself).
    ui_events: Vec<net::ui::UiEvent>,
    last_eye: Option<Vec3>,
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
    /// Earthquakes shaking the view.
    shakes: sim::shake::CameraShakes,
    props: Props,
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
            weapons,
            params: Params {
                mantle_anims: lib.content.mantle_anims(),
                ..Params::default()
            },
            boxes: PlayerBoxes::new(clipmap),
            pred: Predictor::default(),
            angles: [0.0; 2],
            kick: Kick::default(),
            damage: DamageView::default(),
            damage_hud: DamageHud::default(),
            pitch_limits,
            cmd_time: 0,
            last_cmd: Instant::now(),
            want_weapon: None,
            before_slot: None,
            nv_press: false,
            own_events: Seen::default(),
            feedback: Feedback::default(),
            loc_cursor: [0.5; 2],
            remotes: HashMap::new(),
            pushes: HashMap::new(),
            vm: None,
            sight: None,
            c: Counters::default(),
            auto: autoplay.then(Auto::default),
            auto_join: autoplay.then(net::ui::AutoJoin::default),
            ui_events: Vec::new(),
            last_eye: None,
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
            look: Look::new((map.art.glow, map.art.film)),
            max_turn: [0.0; 2],
            shock_sensitivity: 1.0,
            effects: Effects::new(&lib.content, world),
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

    /// Plays effect `name` in front of the player every 1.5 s of server time.
    pub fn set_fx_demo(&mut self, name: String) {
        self.fx_demo = Some((name, None));
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
            self.params.mantle_anims = lib.content.mantle_anims();
            self.lib = lib;
            self.boxes = PlayerBoxes::new(clipmap);
            self.sound = sound;
        }
        self.pred = Predictor::default();
        self.cmd_time = 0;
        self.want_weapon = None;
        self.remotes.clear();
        self.vm = None;
        self.events = Events::default();
        self.own_events = Seen::default();
        self.shakes.clear();
        self.last_eye = None;
        self.scan = crate::hud::NameScan::default();
        self.c.spawned = false;
        self.c.start = None;
        Ok(())
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

    /// What the player state says the input layer must change (forced stance, ADS reset, frozen); take it each
    /// frame and give it to `Input::apply`.
    pub fn take_input_feedback(&mut self) -> Feedback {
        self.feedback.take()
    }

    /// Runs one render frame of play. `None` until the server has given the player a body.
    pub fn frame(&mut self, dt: f32, input: &InputFrame) -> Option<NetFrame> {
        self.net.pump(Duration::ZERO);
        self.answer_menus();
        let now_ms = self.net.now_ms();
        let st = self.net.snaps.server_time(now_ms);
        let own = self.net.latest().map(|s| s.own());
        let (Some(st), Some(own)) = (st, own) else {
            self.net.send();
            return None;
        };
        self.live_time = st;
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
        if self.last_cmd.elapsed() >= CMD_INTERVAL || self.c.cmds == 0 {
            self.last_cmd = Instant::now();
            self.send_cmd(input, st);
        } else {
            self.net.send();
        }

        let snap = self.net.latest()?.clone();
        if snap.follow.is_some() {
            // Watching another player (a killcam or a followed spectator): the snapshot's
            // player state is theirs, so nothing is predicted; draw their view as it came.
            let ps = snap.ps.clone();
            self.kick.clear();
            // The followed player's hits turn the view and show on the screen as the player's own would.
            self.damage
                .look(&ps, st, [ps.viewangles[0], ps.viewangles[1]]);
            let hit_view = self.damage.view_angles(st, ps.weapon_pos_frac, false);
            self.damage_hud = self.damage.hud(st, ps.viewangles[1]);
            let eye = Vec3::new(
                ps.origin[0],
                ps.origin[1],
                ps.origin[2] + ps.view_height_current,
            );
            self.last_eye = Some(eye);
            self.hud_view = Some((ps.clone(), ps.viewangles[1]));
            self.reticle = None;
            let mut models = self.remote_players(dt, st, ps.client_num);
            self.scan_names(
                st,
                ps.client_num,
                eye,
                (ps.viewangles[0], ps.viewangles[1]),
                false,
            );
            models.extend(self.script_models(&snap, dt));
            models.extend(self.items(&snap));
            models.extend(self.vehicles(dt, st, ps.client_num));
            models.extend(self.view_model(
                dt,
                &ps,
                ps.origin,
                [ps.viewangles[0], ps.viewangles[1]],
            ));
            let (events, commands) = self.take_events(&snap);
            self.look
                .goggles(ps.weapon_flags & sim::pm::wf::NIGHTVISION != 0, st);
            self.look
                .goggles(ps.weapon_flags & sim::pm::wf::NIGHTVISION != 0, st);
            let look = self.look.frame(st);
            self.shock_effects(&look);
            let (yaw, pitch) = (
                ps.viewangles[1].to_radians(),
                -ps.viewangles[0].to_radians(),
            );
            // The effects the followed player's guns and the map's events start are as visible to a watcher.
            self.boxes.sync(&snap);
            let drawn = self.fx_frame(dt, st, ps.client_num, &events, eye, (yaw, pitch, 0.0));
            models.extend(self.props.instances());
            models.extend(drawn.models);
            return Some(NetFrame {
                origin: eye,
                yaw: yaw + (look.kick[1] + drawn.sway[1]).to_radians(),
                pitch: pitch + (look.kick[0] - drawn.sway[0] - hit_view[0]).to_radians(),
                roll: (drawn.sway[2] + hit_view[1]).to_radians(),
                models,
                sight: self.sight.clone(),
                fixed_fov: fixed_fov(&ps),
                meshes: drawn.meshes,
                events,
                commands,
                look,
            });
        }
        self.boxes.sync(&snap);
        let env = Env {
            world: self.boxes.world(),
            weapons: &self.weapons,
            params: &self.params,
            speed: 190,
        };
        let p = self.pred.predict(&snap, &env);
        self.c.predictions += 1;
        let ps = p.ps;
        let err = self.pred.error_at(ps.command_time);
        let feet = [
            ps.origin[0] + err[0],
            ps.origin[1] + err[1],
            ps.origin[2] + err[2],
        ];
        let dead = matches!(ps.pm_type, PmType::Dead | PmType::DeadLinked);
        let def = self.lib.content.weapon(self.weapons.name(ps.weapon as u16));
        if dead {
            self.kick.clear();
        } else {
            self.kick.shots(&ps, self.weapons.info(ps.weapon as u16));
            self.kick.step(
                dt,
                ps.weapon_pos_frac,
                def.map(|d| [d.hip_view_kick_center_speed, d.ads_view_kick_center_speed]),
            );
            self.kick
                .idle(dt, &ps, def.and_then(|d| Scope::of(d)).as_ref());
        }
        // A hit turns the camera but not the aim, and the HUD shows it.
        let overlay = def.is_some_and(|d| d.overlay_reticle != 0);
        let hit_view = if dead {
            self.damage.clear();
            [0.0; 2]
        } else {
            let view = [
                self.angles[0] + self.kick.angles()[0],
                self.angles[1] + self.kick.angles()[1],
            ];
            self.damage.look(&ps, st, view);
            self.damage.view_angles(st, ps.weapon_pos_frac, overlay)
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
        let spring = self.kick.spring();
        self.c.kicked = spring != [0.0; 3];
        self.c.kick_settled += u64::from(was_kicked && !self.c.kicked);
        self.c.max_kick_up = self.c.max_kick_up.max(-spring[0]);
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
        let eye = Vec3::new(feet[0], feet[1], feet[2] + ps.view_height_current);
        let yaw_deg = if dead {
            ps.viewangles[1]
        } else {
            self.angles[1]
        };
        if let Some(last) = self.last_eye
            && dt > 0.0
            && dt < 0.05
            && !dead
        {
            let step = eye.distance(last);
            // A respawn or a teleport is not walking.
            if step < 64.0 {
                self.c.eye_speeds.push(step / dt);
            }
        }
        self.last_eye = Some(eye);
        self.hud_view = Some((ps.clone(), yaw_deg));
        self.reticle = if dead { None } else { self.reticle_of(&ps) };
        self.hear(dt, eye, &ps, &snap);
        self.feedback
            .merge(scan_own(&mut self.own_events, &snap.ps));

        let (events, commands) = self.take_events(&snap);
        let look = self.look.frame(st);
        self.shock_effects(&look);
        let mut models = self.remote_players(dt, st, own);
        let view = if dead {
            (0.0, ps.viewangles[1])
        } else {
            (self.angles[0] + kick[0], self.angles[1] + kick[1])
        };
        self.scan_names(st, own, eye, view, dead);
        models.extend(self.script_models(&snap, dt));
        models.extend(self.items(&snap));
        models.extend(self.vehicles(dt, st, own));
        if !dead {
            models.extend(self.view_model(
                dt,
                &ps,
                feet,
                [self.angles[0] + kick[0], self.angles[1] + kick[1]],
            ));
        }
        let yaw = if dead {
            ps.viewangles[1]
        } else {
            self.angles[1] + kick[1]
        };
        let pitch = if dead {
            0.0
        } else {
            self.angles[0] + kick[0] + hit_view[0]
        };
        let (yaw, pitch) = (yaw.to_radians(), -pitch.to_radians());
        let roll = if dead {
            0.0
        } else {
            (kick[2] + hit_view[1]).to_radians()
        };
        let drawn = self.fx_frame(dt, st, own, &events, eye, (yaw, pitch, roll));
        models.extend(self.props.instances());
        models.extend(drawn.models);
        Some(NetFrame {
            origin: eye,
            yaw: yaw + (look.kick[1] + drawn.sway[1]).to_radians(),
            pitch: pitch + (look.kick[0] - drawn.sway[0]).to_radians(),
            roll: roll + drawn.sway[2].to_radians(),
            models,
            sight: self.sight.clone(),
            fixed_fov: fixed_fov(&ps),
            meshes: drawn.meshes,
            events,
            commands,
            look,
        })
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
            self.props.event(
                e,
                &|w| {
                    weapons
                        .get(w)
                        .is_some_and(|i| i.weap_type == sim::weapon::WeaponType::Bullet)
                },
                self.boxes.world(),
            );
            let content = &self.lib.content;
            self.effects.event(e, &|w| {
                weapons
                    .get(w)
                    .and_then(|i| content.weapon(&i.name))
                    .cloned()
            });
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
        self.c.fx_decals_max = self.c.fx_decals_max.max(drawn.decals);
        self.c.fx_live_max = self.c.fx_live_max.max(self.effects.live_elems());
        drawn
    }

    /// The crosshair of the weapon in `ps`: the spread the server would shoot with right now.
    fn reticle_of(&self, ps: &PlayerState) -> Option<Reticle> {
        let index = ps.weapon as u16;
        let weapon = self.lib.content.weapon(self.weapons.name(index))?.clone();
        let spread_deg = aim_spread_degrees(self.weapons.info(index), ps, &WeaponParams::default());
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
    fn hear(&mut self, dt: f32, eye: Vec3, ps: &PlayerState, snap: &net::Snapshot) {
        let yaw = self.angles[1].to_radians();
        self.sound.frame(eye.to_array(), yaw, dt);
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
        let s = &snap.ps;
        let newest: Vec<(u8, u8)> = (0..4u8)
            .map(|i| s.event_sequence.wrapping_sub(4 - i))
            .map(|n| {
                (
                    s.events[usize::from(n & 3)],
                    s.event_parms[usize::from(n & 3)],
                )
            })
            .collect();
        let def = self.lib.content.weapon(self.weapons.name(ps.weapon as u16));
        self.sound.events(
            &Who {
                own: true,
                entity: s.client_num,
                origin: eye.to_array(),
                weapon: def.map(|d| &**d),
                weapon_of: &|w| self.lib.content.weapon(self.weapons.name(w)).cloned(),
                quiet: s.perks & sim::pm::PERK_QUIETER != 0,
            },
            s.event_sequence,
            &newest,
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
                    self.want_weapon = Some(param);
                }
            }
            at::ALT_MODE => {
                let alt = self.weapons.info(cur).alt_weapon;
                if alt != 0 && inv.has(alt) {
                    self.want_weapon = Some(alt);
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
        let step: i32 = match cmd {
            "weapnext" => 1,
            "weapprev" => -1,
            _ => return,
        };
        let Some(snap) = self.net.latest() else {
            return;
        };
        let inv = PlayerWeapons::from_words(&snap.inv);
        let list: Vec<u16> = inv.list(&self.weapons).collect();
        let cur = self.want_weapon.unwrap_or_else(|| inv.selected());
        if let Some(i) = list.iter().position(|w| *w == cur) {
            let n = list.len() as i32;
            self.want_weapon = Some(list[(i as i32 + step).rem_euclid(n) as usize]);
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
        let kick = self.kick.spring();
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

    /// The view model of the held weapon, built when the weapon changes.
    fn view_model(
        &mut self,
        dt: f32,
        ps: &PlayerState,
        feet: [f32; 3],
        look: [f32; 2],
    ) -> Vec<ModelInstance> {
        let index = ps.weapon as u16;
        let key = (index, ps.viewmodel_index);
        if self.vm.as_ref().is_none_or(|(k, _)| *k != key) {
            self.vm = None;
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
        shown.viewangles = [look[0], look[1], 0.0];
        self.c.frames_with_viewmodel += 1;
        vm.kick_gun(self.kick.take_gun_speed());
        let models = vm.update(&shown, dt);
        let sight = vm.sight();
        // Through the scope the original draws no gun.
        let scoped = sight.overlay.is_some();
        self.sight = Some(sight);
        if scoped { Vec::new() } else { models }
    }

    /// The scripted models of the level (`script_model`: props, cars, objectives), posed at the origin and angles the
    /// server gave them. They move in steps of the snapshots; the stock scripts only move them a few at a time.
    fn script_models(&mut self, snap: &net::Snapshot, dt: f32) -> Vec<ModelInstance> {
        let Some(ui) = self.net.ui() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = 0;
        self.c.script_models_unloaded.clear();
        for e in snap
            .entities
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
    /// Their loops follow them.
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
            self.sound.follow(e.number, e.origin);
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
    fn remote_players(&mut self, dt: f32, st: i32, own: u16) -> Vec<ModelInstance> {
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own));
        let now = Instant::now();
        let mut out = Vec::new();
        let mut players = 0;
        let mut drawn = 0;
        for e in ents.iter().filter(|e| e.etype == etype::PLAYER) {
            players += 1;
            let def = self
                .weapons
                .get(e.weapon)
                .and_then(|i| self.lib.content.weapon(&i.name));
            self.sound.events(
                &Who {
                    own: false,
                    entity: e.number,
                    origin: e.origin,
                    weapon: def.map(|d| &**d),
                    weapon_of: &|w| self.lib.content.weapon(self.weapons.name(w)).cloned(),
                    quiet: e.perks & sim::pm::PERK_QUIETER != 0,
                },
                e.event_seq,
                &[(e.event, e.event_parm)],
            );
            let weapon = self
                .weapons
                .get(e.weapon)
                .and_then(|i| self.lib.content.weapon(&i.name))
                .cloned();
            let held = weapon
                .as_ref()
                .and_then(|w| w.world_models.first().cloned().flatten())
                .and_then(|m| m.name.as_deref().map(str::to_owned));
            // The body the scripts gave the player; failing that, the stock body of the team's faction.
            let scripted = self
                .net
                .ui()
                .map(|u| u.model(e.model).to_owned())
                .and_then(|n| self.lib.body_models(&n));
            let set = scripted
                .or_else(|| team_of(e.eflags).and_then(|t| self.lib.team_models(t)))
                .map(|set| PlayerModelSet {
                    weapon: held.clone(),
                    ..set
                });
            // A player the scripts have not given a model yet (just joined) is not drawn.
            let Some(set) = set else {
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
                                dead: None,
                                ragdoll: None,
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
            let dead = e.eflags & eflags::DEAD != 0;
            let input = pose_input(e, weapon.as_deref(), dead);
            // A player who dies in view falls as a ragdoll from the pose they stood in.
            if dead && r.dead == Some(false) {
                let push = self.pushes.remove(&e.client).unwrap_or_default();
                let body = r.player.ragdoll(e.origin, push);
                r.ragdoll = Some((e.origin, r.player.yaw(), body));
                self.c.ragdolls += 1;
            } else if !dead {
                r.ragdoll = None;
            }
            r.dead = Some(dead);
            r.player.update(dt, &input);
            if let Some(c) = r.player.torso_animation() {
                self.c.torso_clips.insert(c);
            }
            drawn += 1;
            match &mut r.ragdoll {
                Some((at, yaw, body)) => {
                    body.update(dt, self.boxes.world());
                    out.extend(r.player.instances_posed(*at, *yaw, &body.bones()));
                }
                None => out.extend(r.player.instances(e.origin)),
            }
        }
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
        report["view_kick_max"] = json!(self.c.max_kick_up);
        report["view_kick_in_cmd_max"] = json!(self.c.max_kick_in_cmd);
        report["view_kick_settled"] = json!(self.c.kick_settled);
        report["projectiles_max_drawn"] = self.c.projectiles_max.into();
        report["fx"]["looped_fx_max"] = self.c.looped_fx_max.into();
        report["fx"]["camera_shake_max"] = self.c.shake_max.into();
        report["fx"]["camera_sway_max"] = self.c.sway_max.into();
        report["damage_events"] = json!(self.c.damage_events);
        report["damage_flash_max"] = json!(self.c.max_damage_flash);
        report["damage_wedges_max"] = json!(self.c.max_damage_wedges);
        report["players_drawn_max"] = self.c.max_players_drawn.into();
        report["player_faults"] = json!(self.c.player_faults);
        report["torso_clips"] = json!(self.c.torso_clips);
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
