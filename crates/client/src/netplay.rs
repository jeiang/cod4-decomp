// SPDX-License-Identifier: GPL-3.0-or-later
//! Playing on a server: the connection, the usercmds built from the player's input, prediction of the player's own
//! movement, and the models of everyone else drawn from the interpolated snapshots.
//!
//! Every render frame the network is read; about every 8 ms (the original's `cl_maxpackets` ceiling) the input becomes
//! one usercmd stamped with the client's estimate of the server clock. The player's body is the newest snapshot's
//! player state with the unacknowledged commands replayed on top ([`net::predict`]); other players are drawn
//! [`net::view::INTERP_DELAY_MS`] behind the server clock between the two snapshots around that moment, which is also
//! the moment the server rewinds them to when it judges this client's shots.

mod hud;

use crate::crosshair::Reticle;
use crate::effects::Effects;
use crate::events::{ClientEvent, Events};
use crate::helicopter::Rotors;
use crate::input::{InputFrame, buttons};
use crate::look::{Look, LookOut};
use crate::models::{Library, Player, PlayerModelSet, Team};
use crate::props::Props;
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
use server::playeranim::PlayerPoseInput;
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

/// What the render loop draws this frame.
pub struct NetFrame {
    /// Eye position.
    pub origin: Vec3,
    /// Radians, counter-clockwise from +x.
    pub yaw: f32,
    /// Radians, positive up.
    pub pitch: f32,
    pub models: Vec<ModelInstance>,
    /// The aim zoom and scope overlay of the held weapon.
    pub sight: Option<Sight>,
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
    predictions: u64,
    max_players_seen: usize,
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
    /// Events received, by kind.
    events: std::collections::BTreeMap<&'static str, u64>,
    fx_quads_max: usize,
    fx_decals_max: usize,
    /// Ragdolls made for players seen dying.
    ragdolls: u64,
    fx_live_max: usize,
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
    pitch_limits: (f32, f32),
    cmd_time: i32,
    last_cmd: Instant,
    want_weapon: Option<u16>,
    /// The weapon held before an action slot picked another, for the slot's second press.
    before_slot: Option<u16>,
    /// A night-vision slot was pressed: the next command carries the button.
    nv_press: bool,
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
    props: Props,
    look: Look,
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
            params: Params::default(),
            boxes: PlayerBoxes::new(clipmap),
            pred: Predictor::default(),
            angles: [0.0; 2],
            pitch_limits,
            cmd_time: 0,
            last_cmd: Instant::now(),
            want_weapon: None,
            before_slot: None,
            nv_press: false,
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
            hud_view: None,
            reticle: None,
            sound,
            props: Props::new(
                lib.content
                    .clipmap()
                    .map_or(&[][..], |c| &c.dyn_entities[..]),
            ),
            look: Look::new((map.art.glow, map.art.film)),
            effects: Effects::new(&lib.content, world),
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
        live.scope = self.sight.as_ref().and_then(|s| s.overlay.clone());
        live.reticle.clone_from(&self.reticle);
        if live.kill_icons.len() != self.kill_icons.len() {
            live.kill_icons.clone_from(&self.kill_icons);
        }
        live.server_addr.clone_from(&self.server_addr);
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
        self.last_eye = None;
        self.c.spawned = false;
        self.c.start = None;
        Ok(())
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

    pub fn disconnect(&mut self) {
        self.net.disconnect();
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
                self.autoplay(dt, st, own)
            };
            &auto_input
        } else {
            input
        };

        let picking = self.net.latest().is_some_and(|s| s.ps.loc_selection != 0);
        if picking {
            // The mouse moves the point on the map, not the view.
            self.loc_cursor[0] =
                (self.loc_cursor[0] - input.look_delta_yaw * LOC_CURSOR_SPEED).clamp(0.0, 1.0);
            self.loc_cursor[1] =
                (self.loc_cursor[1] + input.look_delta_pitch * LOC_CURSOR_SPEED).clamp(0.0, 1.0);
        } else {
            self.loc_cursor = [0.5; 2];
            self.angles[1] += input.look_delta_yaw;
            self.angles[0] = (self.angles[0] + input.look_delta_pitch)
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
            let eye = Vec3::new(
                ps.origin[0],
                ps.origin[1],
                ps.origin[2] + ps.view_height_current,
            );
            self.last_eye = Some(eye);
            self.hud_view = Some((ps.clone(), ps.viewangles[1]));
            self.reticle = None;
            let mut models = self.remote_players(dt, st, ps.client_num);
            models.extend(self.script_models(&snap));
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
            let (yaw, pitch) = (
                ps.viewangles[1].to_radians(),
                -ps.viewangles[0].to_radians(),
            );
            // The effects the followed player's guns and the map's events start are as visible to a watcher.
            self.boxes.sync(&snap);
            let drawn = self.fx_frame(dt, st, ps.client_num, &events, eye, (yaw, pitch));
            models.extend(self.props.instances());
            models.extend(drawn.models);
            return Some(NetFrame {
                origin: eye,
                yaw: yaw + look.kick[1].to_radians(),
                pitch: pitch + look.kick[0].to_radians(),
                models,
                sight: self.sight.clone(),
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

        let (events, commands) = self.take_events(&snap);
        let look = self.look.frame(st);
        let mut models = self.remote_players(dt, st, own);
        models.extend(self.script_models(&snap));
        models.extend(self.vehicles(dt, st, own));
        if !dead {
            models.extend(self.view_model(dt, &ps, feet, self.angles));
        }
        let yaw = if dead {
            ps.viewangles[1]
        } else {
            self.angles[1]
        };
        let pitch = if dead { 0.0 } else { self.angles[0] };
        let (yaw, pitch) = (yaw.to_radians(), -pitch.to_radians());
        let drawn = self.fx_frame(dt, st, own, &events, eye, (yaw, pitch));
        models.extend(self.props.instances());
        models.extend(drawn.models);
        Some(NetFrame {
            origin: eye,
            yaw: yaw + look.kick[1].to_radians(),
            pitch: pitch + look.kick[0].to_radians(),
            models,
            sight: self.sight.clone(),
            meshes: drawn.meshes,
            events,
            commands,
            look,
        })
    }

    /// Plays what `events` start and advances the effects and the props to `st`; returns what to draw from `eye`
    /// looking along `(yaw, pitch)` radians. `own` is whose gun the first-person flash comes from.
    fn fx_frame(
        &mut self,
        dt: f32,
        st: i32,
        own: u16,
        events: &[ClientEvent],
        eye: Vec3,
        (yaw, pitch): (f32, f32),
    ) -> crate::effects::Drawn {
        self.effects
            .set_view(own, self.vm.as_ref().and_then(|(_, v)| v.tags()));
        for e in events {
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
        let missiles: Vec<_> = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own))
            .iter()
            .filter(|e| e.etype == etype::MISSILE)
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
        self.effects.update(st, self.boxes.world());
        for s in self.effects.take_sounds() {
            self.sound.play_world(&s.alias, s.origin.to_array());
        }
        self.props.update(dt, self.boxes.world());
        let drawn = self.effects.draw(eye, yaw, pitch);
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
        self.net.commands.retain(|c| {
            !net::ui::ServerCmd::parse(c).is_some_and(|c| look.command(&c, now, &file))
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
        if cmd.starts_with("callvote ") || cmd.starts_with("vote ") {
            self.net.command(cmd);
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
        let cmd = UserCmd {
            selected_location,
            server_time: self.cmd_time,
            buttons,
            angles: [
                pack(self.angles[0], delta[0]),
                pack(self.angles[1], delta[1]),
                0,
            ],
            weapon: weapon as u8,
            forwardmove: axis(input.move_forward),
            rightmove: axis(input.move_right),
            ..UserCmd::default()
        };
        if input.buttons & buttons::ATTACK != 0 {
            self.c.shots += 1;
        }
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
        let models = vm.update(&shown, dt);
        let sight = vm.sight();
        // Through the scope the original draws no gun.
        let scoped = sight.overlay.is_some();
        self.sight = Some(sight);
        if scoped { Vec::new() } else { models }
    }

    /// The scripted models of the level (`script_model`: props, cars, objectives), posed at the origin and angles the
    /// server gave them. They move in steps of the snapshots; the stock scripts only move them a few at a time.
    fn script_models(&mut self, snap: &net::Snapshot) -> Vec<ModelInstance> {
        let Some(ui) = self.net.ui() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = 0;
        self.c.script_models_unloaded.clear();
        for e in snap
            .entities
            .iter()
            .filter(|e| e.etype == etype::SCRIPT_MODEL)
        {
            seen += 1;
            let name = ui.model(e.model);
            match self.lib.content.model(name) {
                Some(model) => {
                    let mut m = ModelInstance::new(model.clone(), render::ModelKind::World);
                    m.origin = e.origin;
                    m.angles = e.angles;
                    m.light_origin = e.origin;
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
        self.c.script_models_seen = seen;
        self.c.script_models_drawn = out.len();
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
                },
                e.event_seq,
                &[(e.event, e.event_parm)],
            );
            let Some(team) = team_of(e.eflags) else {
                continue;
            };
            let weapon = self
                .weapons
                .get(e.weapon)
                .and_then(|i| self.lib.content.weapon(&i.name))
                .cloned();
            let held = weapon
                .as_ref()
                .and_then(|w| w.world_models.first().cloned().flatten())
                .and_then(|m| m.name.as_deref().map(str::to_owned));
            let set = self.lib.team_models(team).map(|set| PlayerModelSet {
                weapon: held.clone(),
                ..set
            });
            match self.remotes.get_mut(&e.client) {
                None => {
                    if let Some(player) = set.and_then(|s| self.lib.player(&s).ok()) {
                        self.remotes.insert(
                            e.client,
                            Remote {
                                player,
                                weapon: held,
                                seen: now,
                                dead: None,
                                ragdoll: None,
                            },
                        );
                    }
                }
                Some(r) if r.weapon != held => {
                    if let Some(p) = set.and_then(|s| self.lib.player(&s).ok()) {
                        r.player.rearm(p);
                        r.weapon = held;
                    }
                }
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
            match &mut r.ragdoll {
                Some((at, yaw, body)) => {
                    body.update(dt, self.boxes.world());
                    out.extend(r.player.instances_posed(*at, *yaw, &body.bones()));
                }
                None => out.extend(r.player.instances(e.origin)),
            }
        }
        self.c.max_players_seen = self.c.max_players_seen.max(players);
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
        json!({
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
        })
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
    let ps = PlayerState {
        pm_flags: e.pm_flags,
        pm_type: if dead { PmType::Dead } else { PmType::Normal },
        velocity: e.velocity,
        viewangles: e.angles,
        movement_dir: e.move_dir,
        weapon_pos_frac: f32::from(e.ads) / 255.0,
        ..PlayerState::default()
    };
    let moving = e.velocity[0].hypot(e.velocity[1]) > 10.0;
    let mut i = PlayerPoseInput::from_ps(&ps, moving, weapon);
    i.sprinting = e.pm_flags & pmf::SPRINTING != 0;
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
        let answers: Vec<String> = events.iter().filter_map(|e| join.step(e)).collect();
        for a in answers {
            self.net.command(&a);
        }
        // The scripted player has no use for the menus the server opens, but its screen shows the rest.
        self.ui_events.extend(events.into_iter().filter(|e| {
            !matches!(
                e,
                net::ui::UiEvent::OpenMenu { .. }
                    | net::ui::UiEvent::CloseMenu { .. }
                    | net::ui::UiEvent::CloseIngameMenu
            )
        }));
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
