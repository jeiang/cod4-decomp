// SPDX-License-Identifier: GPL-3.0-or-later
//! Playing on a server: the connection, the usercmds built from the player's input, prediction of the player's own
//! movement, and the models of everyone else drawn from the interpolated snapshots.
//!
//! Every render frame the network is read; about every 8 ms (the original's `cl_maxpackets` ceiling) the input becomes
//! one usercmd stamped with the client's estimate of the server clock. The player's body is the newest snapshot's
//! player state with the unacknowledged commands replayed on top ([`net::predict`]); other players are drawn
//! [`net::view::INTERP_DELAY_MS`] behind the server clock between the two snapshots around that moment, which is also
//! the moment the server rewinds them to when it judges this client's shots.

use crate::input::{InputFrame, buttons};
use crate::models::{Library, Player, Team};
use crate::sound::{ClientSound, Who};
use crate::viewmodel::ViewModel;
use glam::Vec3;
use net::UdpTransport;
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
use sim::weapon::{PlayerWeapons, WeaponTable};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The shortest interval between two usercmds.
const CMD_INTERVAL: Duration = Duration::from_millis(8);
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
}

struct Remote {
    player: Player,
    seen: Instant,
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
}

pub struct NetPlay {
    net: NetClient<UdpTransport>,
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
    remotes: HashMap<u16, Remote>,
    vm: Option<(u16, ViewModel)>,
    c: Counters,
    auto: Option<Auto>,
    auto_join: Option<net::ui::AutoJoin>,
    last_eye: Option<Vec3>,
    sound: ClientSound,
}

impl NetPlay {
    pub fn connect(
        lib: Library,
        clipmap: Arc<assets::zone::clipmap::Clipmap>,
        server: SocketAddr,
        name: &str,
        pitch_limits: (f32, f32),
        autoplay: bool,
        sound: ClientSound,
    ) -> Result<Self, String> {
        let t = UdpTransport::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
            .map_err(|e| format!("cannot open a UDP socket: {e}"))?;
        let qport = (std::process::id() & 0xffff) as u16;
        let weapons =
            WeaponTable::new(&lib.content.weapons()).map_err(|e| format!("weapon table: {e:?}"))?;
        Ok(Self {
            net: NetClient::new(t, server, name, "", qport),
            lib,
            weapons,
            params: Params::default(),
            boxes: PlayerBoxes::new(clipmap),
            pred: Predictor::default(),
            angles: [0.0; 2],
            pitch_limits,
            cmd_time: 0,
            last_cmd: Instant::now(),
            want_weapon: None,
            remotes: HashMap::new(),
            vm: None,
            c: Counters::default(),
            auto: autoplay.then(Auto::default),
            auto_join: autoplay.then(net::ui::AutoJoin::default),
            last_eye: None,
            sound,
        })
    }

    pub fn refused(&self) -> Option<&str> {
        self.net.refused()
    }

    /// The server has put the player in the world (alive at least once).
    pub fn spawned(&self) -> bool {
        self.c.spawned
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
        // Autoplay replaces the player's input with a bot's.
        let auto_input;
        let input = if self.auto.is_some() {
            auto_input = self.autoplay(dt, st, own);
            &auto_input
        } else {
            input
        };

        self.angles[1] += input.look_delta_yaw;
        self.angles[0] = (self.angles[0] + input.look_delta_pitch)
            .clamp(self.pitch_limits.0, self.pitch_limits.1);
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
            let mut models = self.remote_players(dt, st, ps.client_num);
            models.extend(self.view_model(dt, &ps, ps.origin, &snap));
            return Some(NetFrame {
                origin: eye,
                yaw: ps.viewangles[1].to_radians(),
                pitch: -ps.viewangles[0].to_radians(),
                models,
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
        self.c.spawned |= !dead;
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
        self.last_eye = Some(eye);
        self.hear(dt, eye, &ps, &snap);

        let mut models = self.remote_players(dt, st, own);
        if !dead {
            models.extend(self.view_model(dt, &ps, feet, &snap));
        }
        let yaw = if dead {
            ps.viewangles[1]
        } else {
            self.angles[1]
        };
        let pitch = if dead { 0.0 } else { self.angles[0] };
        Some(NetFrame {
            origin: eye,
            yaw: yaw.to_radians(),
            pitch: -pitch.to_radians(),
            models,
        })
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

    fn command(&mut self, cmd: &str) {
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
        let delta = snap.ps.delta_angles;
        let pack = |deg: f32, d: f32| (((deg - d) / ANGLE_UNIT).round() as i32) & 0xffff;
        let weapon = self
            .want_weapon
            .unwrap_or_else(|| PlayerWeapons::from_words(&snap.inv).selected());
        let axis = |v: f32| (v.clamp(-1.0, 1.0) * 127.0) as i8;
        let cmd = UserCmd {
            server_time: self.cmd_time,
            buttons: input.buttons as i32,
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
        snap: &net::Snapshot,
    ) -> Vec<ModelInstance> {
        let index = ps.weapon as u16;
        if self.vm.as_ref().is_none_or(|(i, _)| *i != index) {
            self.vm = None;
            let name = self.weapons.name(index).to_owned();
            let team = team_of(snap.entity(ps.client_num).map_or(0, |e| e.eflags));
            let hands = self
                .lib
                .team_models(team.unwrap_or(Team::Allies))
                .and_then(|s| s.viewhands);
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
                    self.vm = Some((index, v));
                }
                // Weapon 0 is "none": the player has no body yet.
                Err(e) if index != 0 => {
                    self.c.unknown_weapon.insert(format!("{index}:{name}"), e);
                }
                Err(_) => {}
            }
        }
        let Some((_, vm)) = self.vm.as_mut() else {
            return Vec::new();
        };
        let mut shown = ps.clone();
        shown.origin = feet;
        shown.viewangles = [self.angles[0], self.angles[1], 0.0];
        self.c.frames_with_viewmodel += 1;
        vm.update(&shown, dt)
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
            if !self.remotes.contains_key(&e.client) {
                let r = self
                    .lib
                    .team_models(team)
                    .and_then(|set| self.lib.player(&set).ok());
                if let Some(player) = r {
                    self.remotes.insert(e.client, Remote { player, seen: now });
                }
            }
            let Some(r) = self.remotes.get_mut(&e.client) else {
                continue;
            };
            r.seen = now;
            let weapon = self
                .weapons
                .get(e.weapon)
                .and_then(|i| self.lib.content.weapon(&i.name));
            let dead = e.eflags & eflags::DEAD != 0;
            let input = pose_input(e, weapon.map(|w| &**w), dead);
            r.player.update(dt, &input);
            out.extend(r.player.instances(e.origin));
        }
        self.c.max_players_seen = self.c.max_players_seen.max(players);
        self.remotes.retain(|_, r| now - r.seen < GONE_AFTER);
        out
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
            "eye": self.last_eye.map(|e| e.to_array()),
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
        let Some(join) = self.auto_join.as_mut() else {
            return;
        };
        let Some(ui) = self.net.ui() else { return };
        let answers: Vec<String> = ui
            .drain_events()
            .iter()
            .filter_map(|e| join.step(e))
            .collect();
        for a in answers {
            self.net.command(&a);
        }
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
