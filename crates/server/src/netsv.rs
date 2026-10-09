// SPDX-License-Identifier: GPL-3.0-only
//! The server's network side: accepts connecting clients, collects their usercmds and console
//! commands, and builds and sends each client's snapshot every frame. The game state it reads
//! lives in [`crate::game::Game`]; joining and leaving go through the same slot calls the bots
//! use, so scripts cannot tell a person from a bot.

use crate::archive::{ArchPlayer, Archive, Frame};
use crate::ban::BanList;
use crate::client::{Conn, Session, Team};
use crate::game::{EntKind, Game, SoundTo};
use crate::ui::Dest;
use net::connect::{ConnectRequest, Gate, serve};
use net::entity::{EntityState, etype};
use net::oob::{Challenger, Oob};
use net::snapshot::Follow;
use net::transport::{MAX_MESSAGE, Transport};
use net::ui::HudElem;
use net::ui::ServerCmd;
use net::{ServerLink, Snapshot};
use sim::pm::{PmType, UserCmd};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// A client that has said nothing for this long is dropped.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Usercmds taken from one client per server frame; a client cannot run faster than real time.
const MAX_CMDS_PER_FRAME: usize = 24;
/// Commands kept for a client that is ahead of the server's frames.
const MAX_QUEUED_CMDS: usize = 64;

/// `EntityState::eflags` bits the server sets on players.
pub mod eflags {
    pub const TEAM_AXIS: u32 = 1 << 0;
    pub const TEAM_ALLIES: u32 = 1 << 1;
    pub const DEAD: u32 = 1 << 2;
    /// The player is mounted on a turret (`EF_TURRET_ACTIVE`).
    pub const TURRET: u32 = 1 << 3;
}

pub struct Peer {
    pub link: ServerLink,
    pub cmds: VecDeque<UserCmd>,
    pub name: String,
    last_heard: Instant,
    /// Effect names announced so far (`fx <index> <name>` commands).
    fx_sent: usize,
}

pub enum Inbound {
    Connect(ConnectRequest),
    Left(SocketAddr),
    /// An `rcon` packet; whether it is allowed is the server's to judge.
    Rcon {
        from: SocketAddr,
        password: String,
        command: String,
    },
}

/// Counters for the report.
#[derive(Debug, Default, Clone, Copy)]
pub struct NetStats {
    pub snapshots_out: u64,
    pub bytes_out: u64,
    pub bytes_in: u64,
    pub joins: u64,
    pub timeouts: u64,
}

pub struct NetSv {
    pub t: Box<dyn Transport + Send>,
    challenger: Challenger,
    started: Instant,
    /// By client number.
    pub peers: Vec<Option<Peer>>,
    /// Console commands from clients, in arrival order: `(client, line)`.
    pub inbox: Vec<(u16, String)>,
    pub stats: NetStats,
    /// Server time of the newest snapshot sent (what pings are measured against).
    now: i32,
    /// The last [`crate::archive::KEEP_MS`] of the world, for killcams.
    archive: Archive,
    buf: Vec<u8>,
    /// Addresses refused at connect.
    pub bans: BanList,
}

impl NetSv {
    pub fn new(t: Box<dyn Transport + Send>, max_clients: usize) -> Self {
        Self {
            t,
            challenger: Challenger::new(),
            started: Instant::now(),
            peers: (0..max_clients).map(|_| None).collect(),
            inbox: Vec::new(),
            stats: NetStats::default(),
            now: 0,
            archive: Archive::default(),
            buf: vec![0; MAX_MESSAGE],
            bans: BanList::default(),
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.t.local_addr()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.iter().flatten().count()
    }

    pub fn slot_of(&self, addr: SocketAddr) -> Option<u16> {
        self.peers
            .iter()
            .position(|p| p.as_ref().is_some_and(|p| p.link.addr == addr))
            .map(|n| n as u16)
    }

    /// Reads datagrams for up to `wait` (until none is waiting after the first), files client
    /// traffic with its peer and returns what needs the game: connects and leaves.
    pub fn poll(
        &mut self,
        wait: Duration,
        info: &dyn Fn() -> Vec<(String, String)>,
    ) -> Vec<Inbound> {
        let mut out = Vec::new();
        let mut wait = wait;
        let mut buf = std::mem::take(&mut self.buf);
        while let Ok(Some((n, from))) = self.t.recv_from(&mut buf, Some(wait)) {
            wait = Duration::ZERO;
            let packet = &buf[..n];
            self.stats.bytes_in += n as u64;
            if let Some(o) = Oob::parse(packet) {
                if let Oob::Rcon { password, command } = o {
                    out.push(Inbound::Rcon {
                        from,
                        password,
                        command,
                    });
                    continue;
                }
                match serve(
                    &self.challenger,
                    self.started.elapsed().as_secs(),
                    from,
                    o,
                    info,
                ) {
                    Gate::Reply(r) => self.t.send_to(from, &r.encode()),
                    Gate::Accept(req) => out.push(Inbound::Connect(req)),
                    Gate::Left => out.push(Inbound::Left(from)),
                    Gate::Ignore => {}
                }
            } else if let Some(slot) = self.slot_of(from)
                && let Some(peer) = self.peers[usize::from(slot)].as_mut()
                && let Some(p) = peer.link.receive(packet)
            {
                peer.last_heard = Instant::now();
                for (_, c) in p.cmds {
                    if peer.cmds.len() < MAX_QUEUED_CMDS {
                        peer.cmds.push_back(c);
                    }
                }
                self.inbox.extend(p.reliable.into_iter().map(|l| (slot, l)));
            }
        }
        self.buf = buf;
        out
    }

    /// The measured round trip of a connected client in ms, once it has acknowledged a snapshot.
    pub fn ping_of(&self, slot: u16) -> Option<i32> {
        self.peers
            .get(usize::from(slot))?
            .as_ref()?
            .link
            .ping(self.now)
    }

    /// Address, qport and ms since the last packet of a connected client.
    pub fn peer_line(&self, slot: u16) -> Option<(SocketAddr, u16, u128)> {
        let p = self.peers.get(usize::from(slot))?.as_ref()?;
        Some((
            p.link.addr,
            p.link.qport,
            p.last_heard.elapsed().as_millis(),
        ))
    }

    pub fn add_peer(&mut self, slot: u16, req: &ConnectRequest, name: &str) {
        self.peers[usize::from(slot)] = Some(Peer {
            link: ServerLink::new(req.from, req.qport),
            cmds: VecDeque::new(),
            name: name.to_owned(),
            last_heard: Instant::now(),
            fx_sent: 0,
        });
        self.stats.joins += 1;
        self.t.send_to(req.from, &Oob::ConnectResponse.encode());
    }

    /// Every connected client, for a map change; they rejoin with [`Self::put_peer`].
    pub fn take_peers(&mut self) -> Vec<(u16, Peer, String)> {
        self.peers
            .iter_mut()
            .enumerate()
            .filter_map(|(slot, p)| Some((slot as u16, p.take()?)))
            .map(|(slot, mut p)| {
                p.cmds.clear();
                let name = p.name.clone();
                (slot, p, name)
            })
            .collect()
    }

    pub fn put_peer(&mut self, slot: u16, mut peer: Peer) {
        peer.last_heard = Instant::now();
        // The new level numbers its effects afresh: the client needs the names again.
        peer.fx_sent = 0;
        self.peers[usize::from(slot)] = Some(peer);
    }

    pub fn remove_peer(&mut self, slot: u16) {
        self.peers[usize::from(slot)] = None;
    }

    /// Clients to drop for silence.
    pub fn timed_out(&mut self) -> Vec<u16> {
        let gone: Vec<u16> = self
            .peers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.as_ref().is_some_and(|p| p.last_heard.elapsed() > TIMEOUT))
            .map(|(n, _)| n as u16)
            .collect();
        self.stats.timeouts += gone.len() as u64;
        gone
    }

    /// The usercmds one client may run this frame.
    pub fn take_cmds(&mut self, slot: u16) -> Vec<UserCmd> {
        let Some(p) = self
            .peers
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
        else {
            return Vec::new();
        };
        let n = p.cmds.len().min(MAX_CMDS_PER_FRAME);
        p.cmds.drain(..n).collect()
    }

    /// Sends a console command to one client, reliably.
    pub fn command(&mut self, slot: u16, line: &str) {
        if let Some(p) = self
            .peers
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
        {
            let _ = p.link.command(line);
        }
    }

    /// The commands that bring a client into `map`: the level change, then every configstring.
    pub fn world_commands(game: &mut Game, map: &str) -> Vec<String> {
        game.refresh_client_info();
        let mut all: Vec<(u16, String)> = game
            .configstrings
            .iter()
            .map(|(i, s)| (*i as u16, s.clone()))
            .collect();
        all.sort_by_key(|(i, _)| *i);
        let mut out = vec![
            ServerCmd::Map {
                name: map.to_owned(),
            }
            .encode(),
        ];
        out.extend(config_commands(all));
        for (night, name) in game.vision.iter().enumerate() {
            if let Some(name) = name {
                out.push(
                    ServerCmd::Vision {
                        night: night == 1,
                        name: name.clone(),
                        ms: 0,
                    }
                    .encode(),
                );
            }
        }
        out
    }

    /// The scoreboard as reliable commands for `slot` (rows with their pings).
    pub fn scoreboard_commands(&self, game: &Game) -> Vec<String> {
        const PER_COMMAND: usize = 18;
        let mut rows = game.score_rows();
        for r in &mut rows {
            if r.ping == 0 {
                r.ping = self
                    .peers
                    .get(usize::from(r.client))
                    .and_then(Option::as_ref)
                    .and_then(|p| p.link.ping(self.now))
                    .unwrap_or(0);
            }
        }
        let total = rows.len() as u16;
        let (limit, axis, allies) = (game.score_limit(), game.team_score[1], game.team_score[2]);
        let mut out = Vec::new();
        let mut start = 0;
        for chunk in rows.chunks(PER_COMMAND) {
            out.push(
                ServerCmd::Scores {
                    axis,
                    allies,
                    limit,
                    start,
                    total,
                    rows: chunk.to_vec(),
                }
                .encode(),
            );
            start += chunk.len() as u16;
        }
        if out.is_empty() {
            out.push(
                ServerCmd::Scores {
                    axis,
                    allies,
                    limit,
                    start: 0,
                    total: 0,
                    rows: Vec::new(),
                }
                .encode(),
            );
        }
        out
    }

    /// Sends `slot` the scoreboard.
    pub fn send_scoreboard(&mut self, slot: u16, game: &Game) {
        let lines = self.scoreboard_commands(game);
        self.command_lines(slot, &lines);
    }

    fn command_lines(&mut self, slot: u16, lines: &[String]) {
        if let Some(p) = self
            .peers
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
        {
            for l in lines {
                let _ = p.link.command(l.clone());
            }
        }
    }

    /// Delivers what scripts queued since the last frame: configstring changes to everybody,
    /// then the one-shot commands to their destinations, in order.
    pub fn flush_ui(&mut self, game: &mut Game) {
        game.refresh_client_info();
        for c in std::mem::take(&mut game.ui.score_requests) {
            self.send_scoreboard(c, game);
        }
        let dirty = std::mem::take(&mut game.ui.dirty_cs);
        let out = std::mem::take(&mut game.ui.out);
        if !dirty.is_empty() {
            let entries = dirty
                .into_iter()
                .map(|i| {
                    let s = game.configstrings.get(&u32::from(i)).cloned();
                    (i, s.unwrap_or_default())
                })
                .collect();
            for line in config_commands(entries) {
                for p in self.peers.iter_mut().flatten() {
                    let _ = p.link.command(line.clone());
                }
            }
        }
        for o in out {
            let line = o.cmd.encode();
            for (slot, p) in self.peers.iter_mut().enumerate() {
                let Some(p) = p else { continue };
                let wanted = match o.to {
                    Dest::All => true,
                    Dest::Client(c) => usize::from(c) == slot,
                    Dest::Team(t) => game.client(slot as u16).is_some_and(|c| c.team == t),
                };
                if wanted {
                    let _ = p.link.command(line.clone());
                }
            }
        }
    }

    /// Sends the game's queued sound commands to the clients that should hear them.
    pub fn send_sounds(&mut self, game: &mut Game) {
        for (to, line) in std::mem::take(&mut game.sound_out) {
            for slot in 0..self.peers.len() as u16 {
                let hears = match to {
                    SoundTo::All => true,
                    SoundTo::Client(n) => n == slot,
                    SoundTo::Team(t) => game.client(slot).is_some_and(|c| c.team == t),
                };
                if hears {
                    self.command(slot, &line);
                }
            }
        }
    }

    /// Builds and sends every client's snapshot for the frame at `server_time`.
    pub fn send_snapshots(&mut self, game: &Game, server_time: i32) {
        self.now = server_time;
        let entities = world_entities(game);
        if game.archive_enabled {
            self.archive.record(Frame {
                time: server_time,
                entities: entities.clone(),
                players: (0..game.max_clients as u16)
                    .map(|n| archived_player(game, n))
                    .collect(),
            });
        } else if !self.archive.is_empty() {
            self.archive.clear();
        }
        for slot in 0..self.peers.len() {
            if self.peers[slot].is_none() {
                continue;
            }
            let Some(snap) = self.snapshot_for(game, slot as u16, server_time, &entities) else {
                continue;
            };
            let peer = self.peers[slot].as_mut().expect("peer");
            while peer.fx_sent < game.fx.len() {
                let Some(name) = game.fx.name(peer.fx_sent + 1) else {
                    break;
                };
                if peer
                    .link
                    .command(format!("fx {} {name}", peer.fx_sent + 1))
                    .is_err()
                {
                    break;
                }
                peer.fx_sent += 1;
            }
            let bytes = peer.link.send(&mut self.t, Some(snap.canonical()));
            self.stats.snapshots_out += 1;
            self.stats.bytes_out += bytes as u64;
        }
    }

    /// How far back, in milliseconds, history reaches at `now`.
    pub fn archive_span(&self, now: i32) -> i32 {
        self.archive.span(now)
    }

    /// What client `slot` sees: its own view, another player's live view while it spectates, or
    /// a replay of the past while `archivetime` is set (a killcam).
    fn snapshot_for(
        &self,
        game: &Game,
        slot: u16,
        server_time: i32,
        entities: &[EntityState],
    ) -> Option<Snapshot> {
        let c = game.client(slot)?;
        let mut snap = Snapshot {
            num: 0,
            server_time,
            ps: c.ps.clone(),
            inv: Box::new(c.inv.to_words()),
            entities: entities.to_vec(),
            hud: game.visible_hud(slot),
            objectives: game.visible_objectives(slot),
            follow: None,
        };
        let followed = u16::try_from(c.spectator_client)
            .ok()
            .filter(|t| *t != slot && c.session == Session::Spectator);
        let Some(target) = followed else {
            return Some(snap);
        };
        let archive_ms = (c.archive_time * 1000.0) as i32;
        let past = (archive_ms > 0)
            .then(|| self.archive.at(server_time - archive_ms))
            .flatten();
        let mut follow = Follow {
            own: slot,
            followed: target,
            archive_ms: 0,
            ps_offset_ms: c.ps_offset_time,
            entity: u16::try_from(c.kill_cam_entity).ok(),
        };
        if let Some(f) = past {
            follow.archive_ms = (server_time - f.time).max(1) as u32;
            snap.entities.clone_from(&f.entities);
            if let Some(Some(p)) = f.players.get(usize::from(target)) {
                snap.ps = p.ps.clone();
                snap.inv = p.inv.clone();
                snap.objectives = p.objectives;
                // The target's archived elements as they were, then this client's own live
                // ones that are not archived (the killcam's skip text and timer).
                snap.hud = p.hud.clone();
                snap.hud
                    .extend(game.visible_hud(slot).into_iter().filter(|h| !h.archived()));
                snap.hud.sort_by_key(|h| h.id);
                snap.hud.dedup_by_key(|h| h.id);
            } else {
                return Some(snap);
            }
        } else if let Some(t) = game.client(target).filter(|t| t.connected()) {
            snap.ps = t.ps.clone();
            snap.inv = Box::new(t.inv.to_words());
            snap.objectives = game.visible_objectives(target);
        } else {
            return Some(snap);
        }
        snap.follow = Some(follow);
        Some(snap)
    }
}

/// One player's screen for the archive, `None` when they are not in the match.
fn archived_player(game: &Game, n: u16) -> Option<ArchPlayer> {
    let c = game.client(n).filter(|c| c.connected())?;
    if !matches!(c.session, Session::Playing | Session::Dead) {
        return None;
    }
    Some(ArchPlayer {
        ps: c.ps.clone(),
        inv: Box::new(c.inv.to_words()),
        hud: game
            .visible_hud(n)
            .into_iter()
            .filter(HudElem::archived)
            .collect(),
        objectives: game.visible_objectives(n),
    })
}

/// Configstring updates as reliable commands, a few hundred bytes each.
fn config_commands(entries: Vec<(u16, String)>) -> Vec<String> {
    const CHUNK: usize = 600;
    let mut out = Vec::new();
    let (mut cur, mut size) = (Vec::new(), 0);
    for (i, s) in entries {
        let s = crate::ui::clip(&s);
        size += s.len() + 12;
        cur.push((i, s));
        if size >= CHUNK {
            out.push(ServerCmd::ConfigStrings(std::mem::take(&mut cur)).encode());
            size = 0;
        }
    }
    if !cur.is_empty() {
        out.push(ServerCmd::ConfigStrings(cur).encode());
    }
    out
}

/// Everything a client can see, in entity-number order, already rounded as the wire rounds it.
pub fn world_entities(game: &Game) -> Vec<EntityState> {
    let mut out = Vec::new();
    for (n, e) in game.ents.iter().enumerate() {
        let Some(e) = e else { continue };
        let n = n as u16;
        let state = match e.kind {
            EntKind::Client => {
                let Some(c) = game.client(n) else { continue };
                if c.conn != Conn::Connected
                    || !matches!(c.session, Session::Playing | Session::Dead)
                {
                    continue;
                }
                let mut s = EntityState::new(n);
                s.etype = etype::PLAYER;
                s.client = n;
                // The body model the scripts gave this player (`playerModelForWeapon`): in a mode without teams the
                // session team is none, but the model is still there to draw.
                s.model = game.models.find(&e.model) as u16;
                s.origin = c.ps.origin;
                s.angles = [c.ps.viewangles[0], c.ps.viewangles[1], c.ps.leanf * 45.0];
                s.velocity = c.ps.velocity;
                s.weapon = c.ps.weapon as u16;
                s.pm_type = c.ps.pm_type as u8;
                s.pm_flags = c.ps.pm_flags & 0x1f_ffff;
                s.weapon_state = c.ps.weapon_state;
                s.ads = (c.ps.weapon_pos_frac.clamp(0.0, 1.0) * 255.0) as u8;
                s.move_dir = c.ps.movement_dir;
                s.event = c.ps.events[usize::from(c.ps.event_sequence.wrapping_sub(1) & 3)];
                s.event_parm =
                    c.ps.event_parms[usize::from(c.ps.event_sequence.wrapping_sub(1) & 3)];
                s.event_seq = c.ps.event_sequence;
                s.torso_pitch = c.ps.torso_pitch;
                s.waist_pitch = c.ps.waist_pitch;
                s.damage_timer = c.ps.damage_timer.clamp(0, i32::from(u16::MAX)) as u16;
                s.damage_duration = c.ps.damage_duration.clamp(0, i32::from(u16::MAX)) as u16;
                s.flinch_dir = c.ps.flinch_yaw_anim & 3;
                s.eflags = match c.team {
                    Team::Axis => eflags::TEAM_AXIS,
                    Team::Allies => eflags::TEAM_ALLIES,
                    _ => 0,
                } | if c.ps.pm_type >= PmType::Dead {
                    eflags::DEAD
                } else {
                    0
                } | if c.ps.e_flags & sim::pm::ef::TURRET_ACTIVE != 0 {
                    eflags::TURRET
                } else {
                    0
                };
                s
            }
            EntKind::Plain if &*e.classname == "script_model" => {
                // The index names the model in the clients' configstrings; one that was never registered cannot be
                // drawn.
                let model = game.models.find(&e.model);
                if e.hidden || model == 0 {
                    continue;
                }
                let mut s = EntityState::new(n);
                s.etype = etype::SCRIPT_MODEL;
                s.origin = e.origin;
                s.angles = e.angles;
                s.model = model as u16;
                s
            }
            EntKind::Plain if e.veh.is_some() => {
                let (Some(v), model) = (e.veh.as_deref(), game.models.find(&e.model)) else {
                    continue;
                };
                if e.hidden || model == 0 {
                    continue;
                }
                let mut s = EntityState::new(n);
                s.etype = etype::VEHICLE;
                s.origin = e.origin;
                s.angles = e.angles;
                s.velocity = v.vel;
                s.model = model as u16;
                s.client = v.owner;
                s.pm_type = v.stage;
                s.eflags = match game.client(v.owner).map(|c| c.team) {
                    Some(Team::Axis) => eflags::TEAM_AXIS,
                    Some(Team::Allies) => eflags::TEAM_ALLIES,
                    _ => 0,
                };
                s
            }
            _ => {
                let Some(m) = e.missile.as_ref() else {
                    continue;
                };
                let mut s = EntityState::new(n);
                s.etype = etype::MISSILE;
                s.origin = e.origin;
                s.angles = e.angles;
                s.weapon = m.weapon;
                s.client = m.parent.unwrap_or(1023);
                s
            }
        };
        out.push(state.canonical());
    }
    out.extend(game.tempev.live(game.level.time).cloned());
    out
}
