// SPDX-License-Identifier: GPL-3.0-or-later
//! Client slots: connect, begin, spawn, the per-command think, the end-of-frame update and
//! disconnect (`ClientConnect`, `ClientBegin`, `ClientSpawn`, `ClientThink_real`,
//! `ClientEndFrame`, `ClientDisconnect` of the original).
//!
//! A client's entity number is its slot. The entity exists while the slot is connected; the
//! player state, the session fields scripts read (`sessionstate`, `score`, ...) and the last
//! command live in [`Client`]. Bots and network clients differ only in where their commands
//! come from.

use gsc::{Array, EntClass, Key, Value, Vm};
use sim::Vec3;
use sim::cm::ENTITYNUM_NONE;
use sim::contents;
use sim::pm::{self, PLAYER_MAXS, PLAYER_MINS, PlayerState, PmType, UserCmd, ev, pmf};
use sim::weapon::PlayerWeapons;

use crate::bot::Brain;
use crate::game::{Ent, EntKind, Game, ScriptCall, TRIGGER_HURT_CONTENTS};
use crate::playeranim::{PlayerPoseInput, PlayerPoseState};

/// `team_t` as the scripts see it (`sessionteam`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Team {
    #[default]
    Free,
    Axis,
    Allies,
    Spectator,
}

impl Team {
    pub fn name(self) -> &'static str {
        match self {
            Team::Free => "none",
            Team::Axis => "axis",
            Team::Allies => "allies",
            Team::Spectator => "spectator",
        }
    }

    pub fn from_name(s: &str) -> Option<Team> {
        Some(match s {
            "none" => Team::Free,
            "axis" => Team::Axis,
            "allies" => Team::Allies,
            "spectator" => Team::Spectator,
            _ => return None,
        })
    }

    /// Index into per-team tables (`1` allies, `2` axis), `0` for the others.
    pub fn index(self) -> usize {
        match self {
            Team::Allies => 1,
            Team::Axis => 2,
            _ => 0,
        }
    }
}

/// `sessionstate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Session {
    Playing,
    Dead,
    #[default]
    Spectator,
    Intermission,
}

impl Session {
    pub fn name(self) -> &'static str {
        match self {
            Session::Playing => "playing",
            Session::Dead => "dead",
            Session::Spectator => "spectator",
            Session::Intermission => "intermission",
        }
    }

    pub fn from_name(s: &str) -> Option<Session> {
        Some(match s {
            "playing" => Session::Playing,
            "dead" => Session::Dead,
            "spectator" => Session::Spectator,
            "intermission" => Session::Intermission,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Conn {
    #[default]
    Free,
    Connecting,
    Connected,
}

/// One client slot (`gclient_s` and the session part the scripts read).
pub struct Client {
    pub conn: Conn,
    pub bot: bool,
    pub name: String,
    pub ps: PlayerState,
    pub cmd: UserCmd,
    pub old_cmd: UserCmd,
    pub team: Team,
    pub session: Session,
    pub max_health: i32,
    pub score: i32,
    pub deaths: i32,
    pub kills: i32,
    pub assists: i32,
    pub has_radar: bool,
    pub status_icon: String,
    pub head_icon: String,
    pub head_icon_team: String,
    pub spectator_client: i32,
    pub kill_cam_entity: i32,
    pub archive_time: f32,
    pub ps_offset_time: i32,
    pub move_speed_scale: f32,
    pub frozen: bool,
    pub last_stand: bool,
    pub noclip: bool,
    pub ufo: bool,
    pub last_cmd_time: i32,
    pub last_spawn_time: i32,
    pub spawn_count: i32,
    pub buttons: i32,
    pub old_buttons: i32,
    pub latched_buttons: i32,
    /// Pitch/yaw/roll recoil kick applied to the view this frame (`viewkick` and weapon kick).
    pub damage_time: i32,
    pub allow_ads: bool,
    pub inv: PlayerWeapons,
    /// Per-frame notify state (`G_ClientDoPerFrameNotifies`).
    pub last_weapon: u32,
    pub prev_firing: bool,
    pub prev_sprinting: bool,
    /// `setstat`/`getstat` values.
    pub stats: std::collections::HashMap<i32, i32>,
    pub bot_brain: Option<Box<Brain>>,
    /// The body animation clock locational hits are tested against.
    pub pose: PlayerPoseState,
}

impl Client {
    pub fn new(num: u16, bot: bool, name: String) -> Self {
        let ps = PlayerState {
            client_num: num,
            pm_type: PmType::Spectator,
            ..PlayerState::default()
        };
        Self {
            conn: Conn::Connecting,
            bot,
            name,
            ps,
            cmd: UserCmd::default(),
            old_cmd: UserCmd::default(),
            team: Team::Free,
            session: Session::Spectator,
            max_health: 100,
            score: 0,
            deaths: 0,
            kills: 0,
            assists: 0,
            has_radar: false,
            status_icon: String::new(),
            head_icon: String::new(),
            head_icon_team: "none".into(),
            spectator_client: -1,
            kill_cam_entity: -1,
            archive_time: 0.0,
            ps_offset_time: 0,
            move_speed_scale: 1.0,
            frozen: false,
            last_stand: false,
            noclip: false,
            ufo: false,
            last_cmd_time: 0,
            last_spawn_time: 0,
            spawn_count: 0,
            buttons: 0,
            old_buttons: 0,
            latched_buttons: 0,
            damage_time: 0,
            allow_ads: true,
            inv: PlayerWeapons::new(),
            last_weapon: 0,
            prev_firing: false,
            prev_sprinting: false,
            stats: std::collections::HashMap::new(),
            bot_brain: None,
            pose: PlayerPoseState::default(),
        }
    }

    pub fn connected(&self) -> bool {
        self.conn == Conn::Connected
    }
}

/// `AngleNormalize180`.
pub fn angle_180(a: f32) -> f32 {
    let v = a * (1.0 / 360.0);
    (v - (v + 0.5).floor()) * 360.0
}

impl Game {
    pub fn client(&self, n: u16) -> Option<&Client> {
        self.clients.get(usize::from(n))
    }

    pub fn client_mut(&mut self, n: u16) -> Option<&mut Client> {
        self.clients.get_mut(usize::from(n))
    }

    /// Connected clients by ascending slot.
    pub fn connected_clients(&self) -> impl Iterator<Item = (u16, &Client)> {
        self.clients
            .iter()
            .enumerate()
            .filter(|(_, c)| c.connected())
            .map(|(i, c)| (i as u16, c))
    }

    /// `ClientConnect` for the lowest free slot: creates the entity and queues
    /// `CodeCallback_PlayerConnect`. The caller runs the queued call, then [`Game::client_begin`].
    pub fn connect_client(&mut self, vm: &mut Vm, bot: bool, name: &str) -> Option<u16> {
        let n = self
            .clients
            .iter()
            .position(|c| c.conn == Conn::Free)
            .filter(|n| *n < self.max_clients)? as u16;
        let mut e = Ent::new(EntKind::Client, "player");
        e.mins = PLAYER_MINS;
        e.maxs = PLAYER_MAXS;
        self.ents[usize::from(n)] = Some(e);
        self.level.num_entities = self.level.num_entities.max(usize::from(n) + 1);
        self.clients[usize::from(n)] = Client::new(n, bot, name.to_owned());
        let obj = vm.entity(n, EntClass::Entity);
        let mut pers = Array::new();
        if bot {
            // The dev script marks its test clients this way; the stat integrity check at
            // connect skips them.
            pers.set(Key::Str("isBot".into()), Value::Int(1));
        }
        obj.set(&"pers".into(), Value::Array(std::rc::Rc::new(pers)));
        if let Some(f) = self.callbacks.player_connect {
            self.calls.push(ScriptCall {
                func: f,
                this: Some(n),
                args: Vec::new(),
            });
        }
        Some(n)
    }

    /// `ClientBegin`: the connect callback waits for this.
    pub fn client_begin(&mut self, vm: &mut Vm, n: u16) {
        let level_time = self.level.time;
        let Some(c) = self.client_mut(n) else { return };
        c.conn = Conn::Connected;
        c.ps.pm_type = PmType::Spectator;
        c.cmd.server_time = level_time;
        vm.notify_entity(n, "begin", &[]);
    }

    /// `ClientDisconnect`: scripts hear `menuresponse "disconnect"`, then the disconnect
    /// callback runs, then the entity goes.
    pub fn disconnect_client(&mut self, vm: &mut Vm, n: u16) {
        if !self.client(n).is_some_and(Client::connected) {
            return;
        }
        vm.notify_entity(
            n,
            "menuresponse",
            &[Value::str("disconnect"), Value::str("-1")],
        );
        if let Some(f) = self.callbacks.player_disconnect {
            self.calls.push(ScriptCall {
                func: f,
                this: Some(n),
                args: Vec::new(),
            });
        }
        self.pending_free.push(n);
    }

    /// Frees the slots whose disconnect callback has run.
    pub fn finish_disconnects(&mut self, vm: &mut Vm) {
        for n in std::mem::take(&mut self.pending_free) {
            self.free_entity(vm, n);
            self.clients[usize::from(n)] = Client::new(n, false, String::new());
            self.clients[usize::from(n)].conn = Conn::Free;
        }
    }

    /// `SetClientViewAngle`.
    pub fn set_client_view_angle(&mut self, n: u16, angle: Vec3) {
        let Some(c) = self.client_mut(n) else { return };
        for (i, a) in angle.iter().enumerate() {
            c.ps.delta_angles[i] = angle_180(a - c.cmd.angles[i] as f32 * pm::ANGLE_UNIT);
        }
        c.ps.viewangles = angle;
        if let Some(Some(e)) = self.ents.get_mut(usize::from(n)) {
            e.angles = [0.0, angle[1], 0.0];
        }
    }

    /// `G_SetClientContents`.
    pub fn set_client_contents(&mut self, n: u16) {
        let Some(c) = self.client(n) else { return };
        let solid = !c.noclip && !c.ufo && c.session != Session::Dead;
        if let Some(e) = self.ent_mut(n) {
            e.contents = if solid { contents::PLAYER } else { 0 };
        }
    }

    /// `ClientSpawn`: resets the player state at `origin` facing `angles` and runs one think
    /// so the first snapshot is valid. Session fields survive.
    pub fn client_spawn(&mut self, vm: &mut Vm, n: u16, origin: Vec3, angles: Vec3) {
        let time = self.level.time;
        let max_clients = self.max_clients as u16;
        let Some(c) = self.client_mut(n) else { return };
        let keep = c.ps.e_flags & 0x10_0002;
        let spawn_count = c.spawn_count + 1;
        let mut ps = PlayerState {
            client_num: n,
            e_flags: keep ^ 2,
            ..PlayerState::default()
        };
        ps.view_height_target = pm::VIEW_STAND;
        ps.view_height_current = pm::VIEW_STAND as f32;
        ps.origin = origin;
        ps.pm_flags |= pmf::RESPAWNED;
        ps.speed = 190;
        c.ps = ps;
        c.inv = PlayerWeapons::new();
        c.last_weapon = 0;
        c.spawn_count = spawn_count;
        c.last_spawn_time = time;
        c.last_stand = false;
        c.noclip = false;
        c.ufo = false;
        c.buttons = c.cmd.buttons;
        c.latched_buttons = 0;
        c.cmd.server_time = time;
        c.ps.command_time = time - 100;
        let _ = max_clients;
        self.stats.spawns += 1;
        if spawn_count > 1 {
            self.stats.respawns += 1;
        }
        if let Some(e) = self.ent_mut(n) {
            e.origin = origin;
            e.mins = PLAYER_MINS;
            e.maxs = PLAYER_MAXS;
            e.classname = "player".into();
            e.mv.pos.tr = sim::traj::Trajectory::stationary(origin);
        }
        if let Some(e) = self.ent_mut(n) {
            e.takedamage = true;
        }
        self.set_client_contents(n);
        self.set_client_view_angle(n, angles);
        self.relink(n);
        self.client_end_frame(vm, n);
        let cmd = self.clients[usize::from(n)].cmd;
        self.client_think(vm, n, cmd);
    }

    /// `ClientThink_real`: one usercmd through movement, then what the movement caused.
    pub fn client_think(&mut self, vm: &mut Vm, n: u16, mut cmd: UserCmd) {
        let level_time = self.level.time;
        let Some(c) = self.clients.get_mut(usize::from(n)) else {
            return;
        };
        if !c.connected() {
            return;
        }
        cmd.server_time = cmd.server_time.clamp(level_time - 1000, level_time + 200);
        let msec = cmd.server_time - c.ps.command_time;
        if msec < 1 {
            return;
        }
        c.old_cmd = c.cmd;
        c.cmd = cmd;
        match c.session {
            Session::Intermission | Session::Spectator => {
                c.ps.command_time = cmd.server_time;
                return;
            }
            _ => {}
        }
        c.old_buttons = c.buttons;
        c.buttons = cmd.buttons;
        c.latched_buttons = c.buttons & !c.old_buttons;
        let old_events = c.ps.event_sequence;
        let Some(world) = self.world.as_ref() else {
            return;
        };
        // A person's shots are judged where the shooter saw the others.
        if !c.bot && self.cvars.bool("g_lagcomp") {
            self.lag_time = Some(cmd.server_time - net::view::INTERP_DELAY_MS);
        }
        let out = pm::run_usercmd(
            &mut c.ps,
            &mut c.inv,
            cmd,
            c.old_cmd,
            self.cvars.int("g_speed"),
            &self.weapons,
            &self.pm_params,
            world,
        );
        let touched = out.touched;
        let (mins, maxs) = (out.mins, out.maxs);
        let out = out.weapon_out;
        let origin = c.ps.origin;
        let yaw = c.ps.viewangles[1];
        if let Some(e) = self.ent_mut(n) {
            e.origin = origin;
            e.angles = [0.0, yaw, 0.0];
            e.mins = mins;
            e.maxs = maxs;
        }
        self.relink(n);
        self.client_events(vm, n, old_events);
        self.weapon_events(vm, n, &out);
        self.lag_time = None;
        for t in touched {
            let (other, me) = (self.entity_value(vm, t), self.entity_value(vm, n));
            vm.notify_entity(n, "touch", &[other]);
            vm.notify_entity(t, "touch", &[me]);
        }
        self.touch_triggers(vm, n);
    }

    /// `ClientEvents`: the predictable events the movement raised this command.
    fn client_events(&mut self, vm: &mut Vm, n: u16, old_sequence: u8) {
        let Some(c) = self.client(n) else { return };
        let seq = c.ps.event_sequence;
        let (events, parms) = (c.ps.events, c.ps.event_parms);
        let mut i = old_sequence;
        if seq.wrapping_sub(old_sequence) > 4 {
            i = seq.wrapping_sub(4);
        }
        while i != seq {
            let (event, parm) = (events[usize::from(i & 3)], parms[usize::from(i & 3)]);
            self.handle_client_event(vm, n, event, parm);
            i = i.wrapping_add(1);
        }
    }

    fn handle_client_event(&mut self, vm: &mut Vm, n: u16, event: u8, parm: u8) {
        match event {
            ev::RELOAD_START_NOTIFY => vm.notify_entity(n, "reload_start", &[]),
            ev::PULLBACK_WEAPON | ev::PREP_OFFHAND => {
                let name = Value::str(self.weapons.name(u16::from(parm)));
                vm.notify_entity(n, "grenade_pullback", &[name]);
            }
            _ => {}
        }
        if (ev::LANDING_PAIN_FIRST..ev::LANDING_PAIN_FIRST + 28).contains(&event) {
            let frac = if parm < 100 {
                f32::from(parm) * 0.01
            } else {
                1.1
            };
            if frac != 0.0 {
                let max = self.client(n).map_or(100, |c| c.max_health);
                let damage = (max as f32 * frac) as i32;
                self.damage_fall(vm, n, damage);
            }
        }
    }

    /// `G_TouchTriggers`: triggers the player's box overlaps hear `touch`; damage volumes
    /// hurt.
    fn touch_triggers(&mut self, vm: &mut Vm, n: u16) {
        let Some(c) = self.client(n) else { return };
        if c.ps.pm_type > PmType::NormalLinked {
            return;
        }
        let Some(e) = self.ent(n) else { return };
        let (lo, hi) = (
            [
                e.origin[0] + e.mins[0] - 20.0,
                e.origin[1] + e.mins[1] - 20.0,
                e.origin[2] + e.mins[2] - 20.0,
            ],
            [
                e.origin[0] + e.maxs[0] + 20.0,
                e.origin[1] + e.maxs[1] + 20.0,
                e.origin[2] + e.maxs[2] + 20.0,
            ],
        );
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let mut list = Vec::new();
        world.area_entities(lo, hi, TRIGGER_HURT_CONTENTS, |t| {
            list.push(t);
            true
        });
        let (pmin, pmax) = (
            [e.origin[0] - 15.0, e.origin[1] - 15.0, e.origin[2]],
            [e.origin[0] + 15.0, e.origin[1] + 15.0, e.origin[2] + 70.0],
        );
        for t in list {
            let Some(te) = self.ent(t) else { continue };
            let Some(le) = self.world.as_ref().and_then(|w| w.entity(t)) else {
                continue;
            };
            let over = (0..3).all(|i| pmin[i] <= le.abs_max[i] && pmax[i] >= le.abs_min[i]);
            if !over {
                continue;
            }
            let class = te.classname.clone();
            let (me, other) = (self.entity_value(vm, n), self.entity_value(vm, t));
            vm.notify_entity(t, "touch", std::slice::from_ref(&me));
            vm.notify_entity(n, "touch", &[other]);
            match &*class {
                "trigger_hurt" => self.hurt_touch(vm, t, n),
                "trigger_multiple" | "trigger_radius" => vm.notify_entity(t, "trigger", &[me]),
                _ => {}
            }
        }
    }

    /// The script value for entity `n`.
    pub fn entity_value(&self, vm: &mut Vm, n: u16) -> Value {
        Value::Object(vm.entity(n, EntClass::Entity))
    }

    /// Files every player's body for this frame in the lag-compensation history.
    pub fn lag_record(&mut self) {
        let time = self.level.time;
        for n in 0..self.clients.len() {
            let c = &self.clients[n];
            let Some(e) = self.ents.get(n).and_then(Option::as_ref) else {
                continue;
            };
            if !c.connected() {
                self.lag.clear(n as u16);
                continue;
            }
            self.lag.record(
                n as u16,
                crate::lagcomp::Sample {
                    time,
                    origin: e.origin,
                    mins: e.mins,
                    maxs: e.maxs,
                    pose: c.pose.clone(),
                },
            );
        }
    }

    /// `ClientEndFrame`: state that follows from the session after scripts ran.
    pub fn client_end_frame(&mut self, _vm: &mut Vm, n: u16) {
        let gravity = self.cvars.int("g_gravity");
        let Some(c) = self.clients.get_mut(usize::from(n)) else {
            return;
        };
        if !c.connected() && c.conn != Conn::Connecting {
            return;
        }
        c.ps.gravity = gravity;
        if c.frozen {
            c.ps.pm_flags |= pmf::FROZEN;
        } else {
            c.ps.pm_flags &= !pmf::FROZEN;
        }
        c.ps.move_speed_scale_multiplier = c.move_speed_scale;
        let health = self.ents[usize::from(n)].as_ref().map_or(0, |e| e.health);
        let linked = self.is_linked(n);
        let c = &mut self.clients[usize::from(n)];
        c.ps.pm_type = match c.session {
            Session::Intermission => PmType::Intermission,
            Session::Spectator => PmType::Spectator,
            _ if c.noclip => PmType::Noclip,
            _ if c.ufo => PmType::Ufo,
            Session::Dead => PmType::Dead,
            Session::Playing if health <= 0 => PmType::Dead,
            Session::Playing if c.last_stand => PmType::LastStand,
            Session::Playing => PmType::Normal,
        };
        if linked {
            c.ps.pm_type = match c.ps.pm_type {
                PmType::Normal => PmType::NormalLinked,
                PmType::Dead => PmType::DeadLinked,
                t => t,
            };
        }
        self.set_client_contents(n);
        self.update_pose(n);
        self.per_frame_notifies(_vm, n);
    }

    /// Advances the player's body animation by one server frame.
    fn update_pose(&mut self, n: u16) {
        let Some(c) = self.client(n) else { return };
        let weapon = self.content.weapon(self.weapons.name(c.ps.weapon as u16));
        let trying = c.cmd.forwardmove != 0 || c.cmd.rightmove != 0;
        let input = PlayerPoseInput::from_ps(&c.ps, trying, weapon.map(|w| &**w));
        let dt = self.level.frametime as f32 * 0.001;
        self.clients[usize::from(n)].pose.update(dt, &input);
    }

    /// `G_ClientDoPerFrameNotifies`: weapon change, firing and sprint edges.
    fn per_frame_notifies(&mut self, vm: &mut Vm, n: u16) {
        let Some(c) = self.clients.get_mut(usize::from(n)) else {
            return;
        };
        if c.conn != Conn::Connected {
            return;
        }
        let weapon = c.ps.weapon;
        let changed = weapon != c.last_weapon;
        c.last_weapon = weapon;
        let firing = c.ps.weapon_state == pm::weapon_state::FIRING && c.ps.pm_type < PmType::Dead;
        let sprinting = c.ps.pm_flags & pmf::SPRINTING != 0;
        let edges = [
            (
                firing,
                std::mem::replace(&mut c.prev_firing, firing),
                "begin_firing",
                "end_firing",
            ),
            (
                sprinting,
                std::mem::replace(&mut c.prev_sprinting, sprinting),
                "sprint_begin",
                "sprint_end",
            ),
        ];
        if changed {
            let name = Value::str(self.weapons.name(weapon as u16));
            vm.notify_entity(n, "weapon_change", &[name]);
        }
        for (now, was, on, off) in edges {
            if now != was {
                vm.notify_entity(n, if now { on } else { off }, &[]);
            }
        }
    }

    /// Entities a script may use as the target of a client method: the client entity or an error.
    pub fn is_client(&self, n: u16) -> bool {
        self.ent(n).is_some_and(|e| e.kind == EntKind::Client) && self.client(n).is_some()
    }

    /// Entity numbers of connected clients that are in the match (not spectating).
    pub fn player_ents(&self) -> impl Iterator<Item = u16> + '_ {
        self.connected_clients().map(|(n, _)| n)
    }

    pub fn none_ent() -> u16 {
        ENTITYNUM_NONE
    }
}

/// `Key` for an integer index, for building arrays.
pub fn int_key(i: usize) -> Key {
    Key::Int(i as i32)
}
