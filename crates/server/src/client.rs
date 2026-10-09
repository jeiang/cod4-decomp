// SPDX-License-Identifier: GPL-3.0-only
//! Client slots: connect, begin, spawn, the per-command think, the end-of-frame update and
//! disconnect (`ClientConnect`, `ClientBegin`, `ClientSpawn`, `ClientThink_real`,
//! `ClientEndFrame`, `ClientDisconnect` of the original).
//!
//! Spectator movement and following (`SpectatorThink`, `StopFollowing`, `SpectatorClientEndFrame`) translated in part from
//! KisakCOD (game_mp/g_active_mp.cpp, game_mp/g_cmds_mp.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
use sim::weapon::{OffhandClass, PlayerWeapons};

use crate::bot::Brain;
use crate::game::{Ent, EntKind, Game, ScriptCall};
use crate::gunmotion::GunMotion;
use crate::playeranim::{PlayerPoseInput, PlayerPoseState};
use crate::ui::Dest;
use net::ui::ServerCmd;

/// [`Client::spec_allow`] bits (`allowspectateteam`).
pub mod spec {
    pub const ALLIES: u8 = 1;
    pub const AXIS: u8 = 2;
    /// Players on no team (free for all).
    pub const NONE: u8 = 4;
    pub const FREELOOK: u8 = 8;

    pub fn from_name(s: &str) -> Option<u8> {
        Some(match s {
            "allies" => ALLIES,
            "axis" => AXIS,
            "none" => NONE,
            "freelook" => FREELOOK,
            _ => return None,
        })
    }
}

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
    /// `setrank`: rank and prestige, published in the client info for the scoreboard.
    pub rank: u8,
    pub prestige: u8,
    pub has_radar: bool,
    pub status_icon: String,
    pub head_icon: String,
    pub head_icon_team: String,
    pub spectator_client: i32,
    /// `allowspectateteam`: which sides this spectator may follow ([`spec`] bits).
    pub spec_allow: u8,
    pub kill_cam_entity: i32,
    pub archive_time: f32,
    pub ps_offset_time: i32,
    pub move_speed_scale: f32,
    pub frozen: bool,
    pub last_stand: bool,
    /// Level time until which a player the Last Stand perk saved takes no damage (`lastStandTime`).
    pub last_stand_time: i32,
    pub noclip: bool,
    pub ufo: bool,
    pub last_cmd_time: i32,
    pub last_spawn_time: i32,
    /// The level time before which this person's next client command is dropped (`sv_floodProtect`).
    pub next_cmd_at: i32,
    pub spawn_count: i32,
    /// Shots fired and shots that hurt an enemy, for the match report.
    pub shots: u32,
    pub hits: u32,
    pub buttons: i32,
    pub old_buttons: i32,
    pub latched_buttons: i32,
    /// The trigger +activate was pressed on, until its hold is over (`useHoldEntity`), and when it began.
    pub use_hold_ent: Option<u16>,
    pub use_hold_time: i32,
    /// The use press was consumed; a held button is not a fresh press until released.
    pub use_button_done: bool,
    /// Damage taken since the last end frame, as health points (`damage_blood`), the way the last blow travelled and
    /// whether it came from no direction at all.
    pub damage_blood: i32,
    pub damage_from: [f32; 3],
    pub damage_from_world: bool,
    /// When `damage_count` of the player state was last set, less 20 ms (`damageTime`).
    pub damage_time: i32,
    /// The gun's springs and clocks, advanced once per usercmd, and what the last one made of the aim.
    pub gun: GunMotion,
    pub allow_ads: bool,
    pub inv: PlayerWeapons,
    /// Per-frame notify state (`G_ClientDoPerFrameNotifies`).
    pub last_weapon: u32,
    pub prev_firing: bool,
    pub prev_sprinting: bool,
    pub prev_night_vision: bool,
    /// The level time after which a player who has not moved or fired is dropped (`g_inactivity`), and whether the
    /// warning went out.
    pub inactivity_at: i32,
    pub inactivity_warned: bool,
    /// A person on this machine (a loopback connection): never dropped for inactivity (`sess.localClient`).
    pub local: bool,
    /// `pingPlayer`: until when the enemies' compasses show this player (0: not pinged).
    pub compass_ping_until: i32,
    /// Level time of the player's latest shot: enemies' compasses show where it came from for a moment.
    pub last_fire_time: i32,
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
            rank: 0,
            prestige: 0,
            has_radar: false,
            status_icon: String::new(),
            head_icon: String::new(),
            head_icon_team: "none".into(),
            spectator_client: -1,
            spec_allow: 0,
            kill_cam_entity: -1,
            archive_time: 0.0,
            ps_offset_time: 0,
            move_speed_scale: 1.0,
            frozen: false,
            last_stand: false,
            last_stand_time: 0,
            noclip: false,
            ufo: false,
            last_cmd_time: 0,
            last_spawn_time: 0,
            next_cmd_at: 0,
            spawn_count: 0,
            shots: 0,
            hits: 0,
            buttons: 0,
            old_buttons: 0,
            latched_buttons: 0,
            use_hold_ent: None,
            use_hold_time: 0,
            use_button_done: false,
            damage_blood: 0,
            damage_from: [0.0; 3],
            damage_from_world: false,
            damage_time: 0,
            gun: GunMotion::default(),
            allow_ads: true,
            inv: PlayerWeapons::new(),
            last_weapon: 0,
            prev_firing: false,
            prev_sprinting: false,
            prev_night_vision: false,
            inactivity_at: 60_000,
            inactivity_warned: false,
            local: false,
            compass_ping_until: 0,
            last_fire_time: i32::MIN / 2,
            stats: std::collections::HashMap::new(),
            bot_brain: None,
            pose: PlayerPoseState::default(),
        }
    }

    pub fn connected(&self) -> bool {
        self.conn == Conn::Connected
    }
}

/// `ClientCleanName`: what a name is made of once the colour codes (`^` and the character after it) and the leading
/// spaces are gone and a run of more than three spaces is cut to three, at most 15 characters; a name with nothing
/// left is `UnnamedPlayer`.
pub fn clean_client_name(name: &str) -> String {
    const MAX: usize = 15;
    let mut out = String::new();
    let mut spaces = 0;
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        // Control characters would forge lines of the log and the console, and steer the localizer.
        if c.is_control() || (out.is_empty() && c == ' ') {
            continue;
        }
        if c == '^' {
            chars.next();
            continue;
        }
        if c == ' ' {
            spaces += 1;
            if spaces > 3 {
                continue;
            }
        } else {
            spaces = 0;
        }
        if out.chars().count() >= MAX {
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        "UnnamedPlayer".into()
    } else {
        out
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

    /// Spectator `n` follows the next player after the one it follows whom it may watch, or
    /// nobody (`spectatorclient` -1) when there is none.
    pub fn spectate_next(&mut self, n: u16) {
        self.spectate_cycle(n, 1);
    }

    /// `Cmd_FollowCycle_f`: like [`Self::spectate_next`], and backwards for `dir` -1. Only a spectator who is not
    /// held on a client by the scripts steers.
    pub fn spectate_cycle(&mut self, n: u16, dir: i32) {
        let Some(me) = self.client(n) else { return };
        let allow = me.spec_allow;
        let from = me.spectator_client;
        let count = self.clients.len() as i32;
        let pick = (1..=count)
            .map(|i| (from + i * dir).rem_euclid(count) as u16)
            .find(|&t| {
                t != n
                    && self.client(t).is_some_and(|c| {
                        c.connected()
                            && c.session == Session::Playing
                            && allow
                                & match c.team {
                                    Team::Allies => spec::ALLIES,
                                    Team::Axis => spec::AXIS,
                                    _ => spec::NONE,
                                }
                                != 0
                    })
            });
        if let Some(c) = self.client_mut(n) {
            c.spectator_client = pick.map_or(-1, i32::from);
        }
    }

    /// `StopFollowing`: the spectator leaves the player it watched and flies from just behind and above where that
    /// player's eyes were, looking the way they did (a little lower).
    pub fn stop_following(&mut self, n: u16) {
        let Some(c) = self.client_mut(n) else { return };
        let target = u16::try_from(std::mem::replace(&mut c.spectator_client, -1)).ok();
        c.kill_cam_entity = -1;
        c.archive_time = 0.0;
        let Some(seen) = target.and_then(|t| self.client(t)).map(|t| t.ps.clone()) else {
            return;
        };
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let eye = crate::fire::view_origin(&seen);
        let (forward, _, up) = pm::math::angle_vectors(&seen.viewangles);
        let end: Vec3 = std::array::from_fn(|i| eye[i] - 40.0 * forward[i] + 10.0 * up[i]);
        let t = sim::cm::Collide::trace(
            world,
            eye,
            end,
            [-8.0; 3],
            [8.0; 3],
            sim::cm::ENTITYNUM_NONE,
            sim::contents::MASK_DEADSOLID,
        );
        let at: Vec3 = std::array::from_fn(|i| eye[i] + (end[i] - eye[i]) * t.fraction);
        let mut look = seen.viewangles;
        look[0] += 15.0;
        if let Some(e) = self.ent_mut(n) {
            e.origin = at;
        }
        if let Some(c) = self.client_mut(n) {
            c.ps.origin = at;
            c.ps.e_flags ^= sim::pm::ef::TELEPORT_BIT;
            c.ps.velocity = [0.0; 3];
        }
        self.set_client_view_angle(n, look);
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
            crate::script::free_client_hud_elems(self, vm, n);
            self.free_entity(vm, n);
            self.clients[usize::from(n)] = Client::new(n, false, String::new());
            self.clients[usize::from(n)].conn = Conn::Free;
            // Nobody keeps watching the slot, which a newcomer may take.
            for c in &mut self.clients {
                if c.spectator_client == i32::from(n) && c.archive_time <= 0.0 {
                    c.spectator_client = -1;
                }
            }
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
        let solid = !c.noclip && !c.ufo && matches!(c.session, Session::Playing);
        if let Some(e) = self.ent_mut(n) {
            e.contents = if solid { contents::PLAYER } else { 0 };
        }
        // A spectator or a player at the scoreboard is nowhere in the world.
        if self
            .client(n)
            .is_some_and(|c| matches!(c.session, Session::Spectator | Session::Intermission))
            && let Some(w) = self.world.as_mut()
        {
            w.unlink(n);
        }
    }

    /// `SpectatorClientEndFrame`'s follow upkeep: a watched player who is gone (or may no longer be watched) ends the
    /// following, and a spectator barred from free flight who watches nobody is put on the next player.
    fn spectator_upkeep(&mut self, n: u16) {
        let Some(c) = self.client(n) else { return };
        if c.session != Session::Spectator || c.archive_time > 0.0 {
            return;
        }
        if let Ok(t) = u16::try_from(c.spectator_client) {
            let allow = c.spec_allow;
            let watchable = t != n
                && self.client(t).is_some_and(|w| {
                    w.connected()
                        && w.session == Session::Playing
                        && allow
                            & match w.team {
                                Team::Allies => spec::ALLIES,
                                Team::Axis => spec::AXIS,
                                _ => spec::NONE,
                            }
                            != 0
                });
            if !watchable {
                self.stop_following(n);
            }
        }
        let Some(c) = self.client(n) else { return };
        if c.spectator_client < 0 && c.spec_allow & spec::FREELOOK == 0 {
            self.spectate_cycle(n, 1);
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
            spawn_count: spawn_count as u16,
            e_flags: keep ^ sim::pm::ef::TELEPORT_BIT,
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
        (c.inactivity_at, c.inactivity_warned) = (time + 60_000, false);
        c.last_stand = false;
        c.last_stand_time = 0;
        // Blood the last life's killing blow left is not the new life's.
        c.damage_blood = 0;
        c.damage_from = [0.0; 3];
        c.damage_from_world = false;
        c.damage_time = 0;
        c.gun = GunMotion::default();
        c.noclip = false;
        c.ufo = false;
        c.buttons = c.cmd.buttons;
        c.latched_buttons = 0;
        c.use_hold_ent = None;
        c.use_button_done = false;
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
        self.mark_teleport(n);
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
        if !self.inactivity_timer(vm, n, &cmd) {
            return;
        }
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
        // The weapon a person asked for (the weapon keys, action slots): honoured unless a script has
        // chosen one or the weapons are disabled.
        let want = u16::from(cmd.weapon);
        if want != 0
            && want != c.ps.weapon as u16
            && c.inv.selected() == 0
            && c.inv.has(want)
            && c.ps.weapon_flags & pm::wf::DISABLED == 0
            && c.ps.pm_type < PmType::Dead
        {
            c.inv.select(want);
        }
        c.old_cmd = c.cmd;
        c.cmd = cmd;
        match c.session {
            Session::Intermission => {
                c.ps.command_time = cmd.server_time;
                c.old_buttons = c.buttons;
                c.buttons = cmd.buttons;
                c.latched_buttons = c.buttons & !c.old_buttons;
                return;
            }
            Session::Spectator => {
                self.spectator_think(n, cmd);
                return;
            }
            _ => {}
        }
        c.old_buttons = c.buttons;
        if !c.use_button_done {
            c.old_buttons &= !(pm::button::USE | pm::button::USE_RELOAD);
        }
        c.buttons = cmd.buttons;
        if c.buttons & (pm::button::USE | pm::button::USE_RELOAD) == 0 {
            c.use_button_done = false;
        }
        c.latched_buttons = c.buttons & !c.old_buttons;
        let old_events = c.ps.event_sequence;
        c.ps.speed = self.cvars.int("g_speed");
        // `ClientThink_real` works out the aim of this command's shots before `Pmove`, from the state as the last
        // command left it, and caps the step at 200 ms.
        c.gun_step(self.weapons.get(c.ps.weapon as u16), msec.min(200));
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
        let mantle = out.mantle;
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
        if let Some((end, duration)) = mantle {
            self.add_mantle_blockage(n, end, duration);
        }
        self.client_events(vm, n, old_events);
        self.weapon_events(vm, n, &out);
        self.lag_time = None;
        for t in touched {
            let (other, me) = (self.entity_value(vm, t), self.entity_value(vm, n));
            vm.notify_entity(n, "touch", &[other]);
            vm.notify_entity(t, "touch", &[me]);
        }
        self.update_activate(vm, n);
        self.location_input(vm, n, &cmd);
    }

    /// `ClientInactivityTimer`: a player who neither moves nor shoots nor jumps for `g_inactivity` seconds is warned
    /// ten seconds before and then dropped. False when the player went.
    fn inactivity_timer(&mut self, vm: &mut Vm, n: u16, cmd: &UserCmd) -> bool {
        let (time, limit) = (self.level.time, self.cvars.int("g_inactivity"));
        let Some(c) = self.clients.get_mut(usize::from(n)) else {
            return true;
        };
        if !c.connected()
            || c.bot
            || matches!(c.session, Session::Spectator | Session::Intermission)
        {
            return true;
        }
        if limit == 0 {
            (c.inactivity_at, c.inactivity_warned) = (time + 60_000, false);
        } else if cmd.forwardmove != 0
            || cmd.rightmove != 0
            || cmd.buttons & (pm::button::ATTACK | pm::button::JUMP) != 0
        {
            (c.inactivity_at, c.inactivity_warned) =
                (time.saturating_add(limit.saturating_mul(1000)), false);
        } else if c.local {
            // A person on this machine is never dropped or warned.
        } else if time > c.inactivity_at {
            self.inactive.push(n);
            self.disconnect_client(vm, n);
            return false;
        } else if time > c.inactivity_at - 10_000 && !c.inactivity_warned {
            c.inactivity_warned = true;
            self.send(
                Dest::Client(n),
                ServerCmd::Announce {
                    text: "&GAME_INACTIVEDROPWARNING".into(),
                },
            );
        }
        true
    }

    /// `StuckInClient`: a living player whose body overlaps another living player's (they were spawned or pushed
    /// together) is shoved apart from it, both along the line between them, at `g_playerCollisionEjectSpeed`.
    fn stuck_in_client(&mut self, n: u16) {
        let playing = |g: &Game, k: u16| {
            g.client(k)
                .is_some_and(|c| c.connected() && c.session == Session::Playing)
                && g.ent(k).is_some_and(|e| {
                    e.health > 0 && e.contents & (contents::PLAYER | contents::CORPSE) != 0
                })
        };
        if !playing(self, n) {
            return;
        }
        let Some(me) = self.ent(n) else { return };
        let (o, mins, maxs) = (me.origin, me.mins, me.maxs);
        let touching = (0..self.max_clients as u16).find(|&k| {
            k != n
                && playing(self, k)
                && self.ent(k).is_some_and(|h| {
                    let overlap = (0..3).all(|a| {
                        o[a] + maxs[a] >= h.origin[a] + h.mins[a]
                            && o[a] + mins[a] <= h.origin[a] + h.maxs[a]
                    });
                    let d = [h.origin[0] - o[0], h.origin[1] - o[1]];
                    let reach = maxs[0] + h.maxs[0];
                    overlap && d[0] * d[0] + d[1] * d[1] <= reach * reach
                })
        });
        let Some(k) = touching else { return };
        let ho = self.ent(k).map_or(o, |h| h.origin);
        let jitter = |g: &mut Game| (g.rand() as f32 / u32::MAX as f32).mul_add(2.0, -1.0);
        let mut d = [ho[0] - o[0] + jitter(self), ho[1] - o[1] + jitter(self)];
        let len = d[0].hypot(d[1]);
        if len > 0.0 {
            d = [d[0] / len, d[1] / len];
        }
        let eject = self.cvars.int("g_playerCollisionEjectSpeed") as f32;
        let moving = |c: &Client| {
            if c.ps.velocity[0].hypot(c.ps.velocity[1]) > 0.0 {
                eject
            } else {
                0.0
            }
        };
        let (Some(mine), Some(theirs)) = (self.client(n), self.client(k)) else {
            return;
        };
        let (mut self_speed, mut hit_speed) = (moving(mine), moving(theirs));
        if self_speed < 0.0001 && hit_speed < 0.0001 {
            (self_speed, hit_speed) = (mine.ps.speed as f32, theirs.ps.speed as f32);
        }
        if let Some(h) = self.client_mut(k) {
            (h.ps.velocity[0], h.ps.velocity[1]) = (hit_speed * d[0], hit_speed * d[1]);
            h.ps.pm_time = 300;
            h.ps.pm_flags |= pmf::TIME_HARDLANDING;
        }
        if let Some(m) = self.client_mut(n) {
            (m.ps.velocity[0], m.ps.velocity[1]) = (-self_speed * d[0], -self_speed * d[1]);
            m.ps.pm_time = 300;
            m.ps.pm_flags |= pmf::TIME_HARDLANDING;
        }
    }

    /// `SpectatorThink`: attack and ads step through the players that may be watched, melee leaves the one
    /// watched, and a spectator who watches nobody (and is not in a killcam) flies with the movement code.
    fn spectator_think(&mut self, n: u16, cmd: UserCmd) {
        let Some(c) = self.clients.get_mut(usize::from(n)) else {
            return;
        };
        c.old_buttons = c.buttons;
        c.buttons = cmd.buttons;
        c.latched_buttons = c.buttons & !c.old_buttons;
        let (pressed, changed) = (c.latched_buttons, c.buttons ^ c.old_buttons);
        let held = c.archive_time > 0.0;
        let freelook = c.spec_allow & spec::FREELOOK != 0;
        if !held {
            if c.spectator_client >= 0 && freelook && changed & pm::button::MELEE != 0 {
                self.stop_following(n);
            }
            if pressed & pm::button::ATTACK != 0 {
                self.spectate_cycle(n, 1);
            } else if pressed & pm::button::ADS != 0 {
                self.spectate_cycle(n, -1);
            }
        }
        let c = &mut self.clients[usize::from(n)];
        if held || c.spectator_client >= 0 {
            c.ps.command_time = cmd.server_time;
            return;
        }
        c.ps.pm_type = PmType::Spectator;
        c.ps.speed = if freelook { 400 } else { 0 };
        let Some(world) = self.world.as_ref() else {
            return;
        };
        pm::run_usercmd(
            &mut c.ps,
            &mut c.inv,
            cmd,
            c.old_cmd,
            0,
            &self.weapons,
            &self.pm_params,
            world,
        );
        let (origin, yaw) = (c.ps.origin, c.ps.viewangles[1]);
        if let Some(e) = self.ent_mut(n) {
            e.origin = origin;
            e.angles = [0.0, yaw, 0.0];
        }
        // A spectator is nowhere in the world.
        if let Some(w) = self.world.as_mut() {
            w.unlink(n);
        }
    }

    /// `G_AddPlayerMantleBlockage`: an invisible player-sized box at the end of a mantle keeps
    /// anyone else out of the landing spot while the climb runs and `g_mantleBlockTimeBuffer`
    /// ms after. The climbing player's own traces pass through it.
    fn add_mantle_blockage(&mut self, owner: u16, end: Vec3, duration: i32) {
        let Some(o) = self.ent(owner) else { return };
        let mut e = Ent::new(EntKind::Plain, "player_mantle_block");
        e.origin = end;
        e.mins = o.mins;
        e.maxs = o.maxs;
        e.contents = contents::PLAYERCLIP;
        e.owner = Some(owner);
        e.free_at = Some(self.level.time + duration + self.cvars.int("g_mantleBlockTimeBuffer"));
        match self.spawn(e) {
            Ok(n) => self.relink(n),
            Err(err) => self.print(format!(
                "WARNING: mantle blocker for client {owner}: {err}\n"
            )),
        }
    }

    /// The answer of a location selection (`beginLocationSelection`): a confirm turns the map point the
    /// player picked into world coordinates, a cancel says so.
    fn location_input(&mut self, vm: &mut Vm, n: u16, cmd: &UserCmd) {
        let Some(c) = self.client(n) else { return };
        if c.ps.loc_selection == 0 {
            return;
        }
        let pressed = c.latched_buttons;
        if pressed & pm::button::LOC_CONFIRM != 0 {
            let [sx, sy, nx, ny, ux, uy] = self.compass.unwrap_or([1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
            let fx = (f32::from(cmd.selected_location[0]) + 128.0) / 255.0 * sx;
            let fy = (f32::from(cmd.selected_location[1]) + 128.0) / 255.0 * sy;
            let at = [fx * ny + ux - fy * nx, uy - fx * nx - fy * ny, 0.0];
            vm.notify_entity(n, "confirm_location", &[Value::Vector(at)]);
        } else if pressed & pm::button::LOC_CANCEL != 0 {
            vm.notify_entity(n, "cancel_location", &[]);
        }
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
            ev::SWITCH_OFFHAND
                if self.weapons.info(u16::from(parm)).offhand_class == OffhandClass::Frag =>
            {
                self.attempt_live_grenade_pickup(vm, n);
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

    /// `ps.radarEnabled`: the client's team has radar (`setteamradar`) or the client has (`hasradar`).
    pub fn client_radar(&self, n: u16) -> bool {
        self.client(n).is_some_and(|c| {
            c.has_radar
                || match c.team {
                    Team::Free => self.team_radar[0],
                    Team::Axis => self.team_radar[1],
                    Team::Allies => self.team_radar[2],
                    Team::Spectator => false,
                }
        })
    }

    /// `ClientEndFrame`: state that follows from the session after scripts ran.
    pub fn client_end_frame(&mut self, _vm: &mut Vm, n: u16) {
        self.spectator_upkeep(n);
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
        let radar = self.client_radar(n);
        let c = &mut self.clients[usize::from(n)];
        c.ps.radar_enabled = radar;
        c.ps.health = health;
        c.ps.max_health = c.max_health;
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
        c.ps.other_flags = if c.session == Session::Spectator {
            let following = c.spectator_client >= 0;
            let held = c.archive_time > 0.0;
            let any_team = c.spec_allow & (spec::ALLIES | spec::AXIS | spec::NONE) != 0;
            let mut f = 0;
            if following {
                f |= pm::other::FOLLOWING;
            }
            if !held && (following || any_team) {
                f |= pm::other::CAN_CYCLE;
            }
            if !held && following && c.spec_allow & spec::FREELOOK != 0 {
                f |= pm::other::CAN_STOP;
            }
            f
        } else {
            0
        };
        if linked {
            c.ps.pm_type = match c.ps.pm_type {
                PmType::Normal => PmType::NormalLinked,
                PmType::Dead => PmType::DeadLinked,
                t => t,
            };
        }
        self.damage_feedback(n);
        self.set_client_contents(n);
        self.update_cursor_hints(n);
        self.update_pose(n);
        self.stuck_in_client(n);
        self.per_frame_notifies(_vm, n);
        let time = self.level.time;
        if let Some(c) = self.client_mut(n)
            && c.compass_ping_until != 0
            && time >= c.compass_ping_until
        {
            c.compass_ping_until = 0;
        }
    }

    /// Advances the player's body animation by one server frame.
    fn update_pose(&mut self, n: u16) {
        let Some(c) = self.client(n) else { return };
        let weapon = self.content.weapon(self.weapons.name(c.ps.weapon as u16));
        let trying = c.cmd.forwardmove != 0 || c.cmd.rightmove != 0;
        let input = PlayerPoseInput::from_ps(&c.ps, trying, weapon.map(|w| &**w));
        let dt = self.level.frametime as f32 * 0.001;
        let anims = self.player_anims.clone();
        let clips: &dyn crate::playeranim::Clips = match &anims {
            Some(a) => &**a,
            None => &crate::playeranim::NoClips,
        };
        self.clients[usize::from(n)].pose.update(clips, dt, &input);
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
        let night_vision = c.ps.weapon_flags & pm::wf::NIGHTVISION != 0;
        let edges = [
            (
                night_vision,
                std::mem::replace(&mut c.prev_night_vision, night_vision),
                "night_vision_on",
                "night_vision_off",
            ),
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

#[cfg(test)]
mod name_tests {
    use super::clean_client_name;

    #[test]
    fn colour_codes_and_leading_spaces_go_and_a_long_run_of_spaces_is_cut() {
        assert_eq!(clean_client_name("^1Red^7Dead"), "RedDead");
        assert_eq!(clean_client_name("   Ann"), "Ann");
        assert_eq!(clean_client_name("a      b"), "a   b");
        assert_eq!(clean_client_name("trailing^"), "trailing");
        assert_eq!(clean_client_name("a\n  9:99 K;x\x15"), "a  9:99 K;x");
        assert_eq!(clean_client_name("a\n  9:99 K;x\x15"), "a  9:99 K;x");
    }

    #[test]
    fn a_name_is_at_most_15_characters_and_an_empty_one_is_unnamed() {
        assert_eq!(
            clean_client_name("abcdefghijklmnopqrstuvwxyz"),
            "abcdefghijklmno"
        );
        // The colour codes do not count toward the 15.
        assert_eq!(
            clean_client_name("^1abcdefghij^2klmnopqrst"),
            "abcdefghijklmno"
        );
        assert_eq!(clean_client_name(""), "UnnamedPlayer");
        assert_eq!(clean_client_name("   ^1^2 "), "UnnamedPlayer");
    }
}

#[cfg(test)]
mod spectate_tests {
    use super::*;
    use crate::content::Content;
    use crate::cvar::Cvars;

    fn game() -> Game {
        let mut g = Game::new(Cvars::new(), Content::default());
        g.clients = (0..5)
            .map(|n| Client::new(n, false, format!("p{n}")))
            .collect();
        for (n, team) in [
            (1, Team::Allies),
            (2, Team::Axis),
            (3, Team::Allies),
            (4, Team::Axis),
        ] {
            let c = &mut g.clients[n];
            c.conn = Conn::Connected;
            c.team = team;
            c.session = Session::Playing;
        }
        g.clients[0].conn = Conn::Connected;
        g
    }

    #[test]
    fn attack_steps_through_the_players_a_spectator_may_watch_and_wraps() {
        let mut g = game();
        g.clients[0].spec_allow = spec::ALLIES;
        g.spectate_next(0);
        assert_eq!(g.clients[0].spectator_client, 1);
        g.spectate_next(0);
        assert_eq!(g.clients[0].spectator_client, 3);
        g.spectate_next(0);
        assert_eq!(g.clients[0].spectator_client, 1, "wraps");
        // Both sides, and the dead and the spectators are skipped.
        g.clients[0].spec_allow = spec::ALLIES | spec::AXIS;
        g.clients[2].session = Session::Dead;
        g.spectate_next(0);
        assert_eq!(g.clients[0].spectator_client, 3);
        g.spectate_next(0);
        assert_eq!(g.clients[0].spectator_client, 4);
        // Backwards, as `followprev` steps.
        g.clients[0].spectator_client = 4;
        g.spectate_cycle(0, -1);
        assert_eq!(g.clients[0].spectator_client, 3);
        g.spectate_cycle(0, -1);
        assert_eq!(g.clients[0].spectator_client, 1, "skips the dead one");
        // Nothing allowed: nobody.
        g.clients[0].spec_allow = 0;
        g.spectate_next(0);
        assert_eq!(g.clients[0].spectator_client, -1);
    }

    #[test]
    fn radar_follows_the_team_flag_or_the_players_own() {
        let mut g = game();
        assert!(!(0..5).any(|n| g.client_radar(n)));
        g.team_radar[1] = true;
        // Axis: clients 2 and 4.
        assert_eq!(
            (0..5).filter(|&n| g.client_radar(n)).collect::<Vec<_>>(),
            [2, 4]
        );
        g.clients[1].has_radar = true;
        assert!(g.client_radar(1), "a player's own radar needs no team flag");
        g.team_radar[1] = false;
        assert_eq!(
            (0..5).filter(|&n| g.client_radar(n)).collect::<Vec<_>>(),
            [1]
        );
    }

    #[test]
    fn the_no_team_radar_covers_free_players_but_not_spectators() {
        let mut g = game();
        g.clients[1].team = Team::Free;
        g.clients[3].team = Team::Spectator;
        g.team_radar[0] = true;
        assert_eq!(
            (0..5).filter(|&n| g.client_radar(n)).collect::<Vec<_>>(),
            [0, 1],
            "client 0 has no team yet"
        );
    }

    #[test]
    fn spectate_names_map_to_bits() {
        assert_eq!(spec::from_name("freelook"), Some(spec::FREELOOK));
        assert_eq!(spec::from_name("both"), None);
    }
}
