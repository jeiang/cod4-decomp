// SPDX-License-Identifier: GPL-3.0-or-later
//! The server's network side: accepts connecting clients, collects their usercmds and console
//! commands, and builds and sends each client's snapshot every frame. The game state it reads
//! lives in [`crate::game::Game`]; joining and leaving go through the same slot calls the bots
//! use, so scripts cannot tell a person from a bot.

use crate::client::{Conn, Session, Team};
use crate::game::{EntKind, Game};
use crate::ui::Dest;
use net::connect::{ConnectRequest, Gate, serve};
use net::entity::{EntityState, etype};
use net::oob::{Challenger, Oob};
use net::transport::{Transport, UdpTransport};
use net::ui::ServerCmd;
use net::{ServerLink, Snapshot, field, ps};
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
}

pub struct Peer {
    pub link: ServerLink,
    pub cmds: VecDeque<UserCmd>,
    pub name: String,
    last_heard: Instant,
}

pub enum Inbound {
    Connect(ConnectRequest),
    Left(SocketAddr),
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
    pub t: UdpTransport,
    challenger: Challenger,
    started: Instant,
    /// By client number.
    pub peers: Vec<Option<Peer>>,
    /// Console commands from clients, in arrival order: `(client, line)`.
    pub inbox: Vec<(u16, String)>,
    pub stats: NetStats,
    buf: Vec<u8>,
}

impl NetSv {
    pub fn new(t: UdpTransport, max_clients: usize) -> Self {
        Self {
            t,
            challenger: Challenger::new(),
            started: Instant::now(),
            peers: (0..max_clients).map(|_| None).collect(),
            inbox: Vec::new(),
            stats: NetStats::default(),
            buf: vec![0; 2048],
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

    pub fn add_peer(&mut self, slot: u16, req: &ConnectRequest, name: &str) {
        self.peers[usize::from(slot)] = Some(Peer {
            link: ServerLink::new(req.from, req.qport),
            cmds: VecDeque::new(),
            name: name.to_owned(),
            last_heard: Instant::now(),
        });
        self.stats.joins += 1;
        self.t.send_to(req.from, &Oob::ConnectResponse.encode());
    }

    /// Every connected client, for a map change; they rejoin with [`Self::put_peer`].
    pub fn take_peers(&mut self) -> Vec<(Peer, String)> {
        self.peers
            .iter_mut()
            .filter_map(Option::take)
            .map(|mut p| {
                p.cmds.clear();
                let name = p.name.clone();
                (p, name)
            })
            .collect()
    }

    pub fn put_peer(&mut self, slot: u16, mut peer: Peer) {
        peer.last_heard = Instant::now();
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
        out
    }

    /// Delivers what scripts queued since the last frame: configstring changes to everybody,
    /// then the one-shot commands to their destinations, in order.
    pub fn flush_ui(&mut self, game: &mut Game) {
        game.refresh_client_info();
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

    /// Builds and sends every client's snapshot for the frame at `server_time`.
    pub fn send_snapshots(&mut self, game: &Game, server_time: i32) {
        let entities = world_entities(game);
        for slot in 0..self.peers.len() {
            let Some(peer) = self.peers[slot].as_mut() else {
                continue;
            };
            let Some(c) = game.client(slot as u16) else {
                continue;
            };
            let mut ps = c.ps.clone();
            field::canonicalize(ps::fields(), &mut ps);
            let snap = Snapshot {
                num: 0,
                server_time,
                ps,
                inv: Box::new(c.inv.to_words()),
                entities: entities.clone(),
                hud: game.visible_hud(slot as u16),
                objectives: game.visible_objectives(slot as u16),
            }
            .canonical();
            let bytes = peer.link.send(&mut self.t, Some(snap));
            self.stats.snapshots_out += 1;
            self.stats.bytes_out += bytes as u64;
        }
    }
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
                s.eflags = match c.team {
                    Team::Axis => eflags::TEAM_AXIS,
                    Team::Allies => eflags::TEAM_ALLIES,
                    _ => 0,
                } | if c.ps.pm_type >= PmType::Dead {
                    eflags::DEAD
                } else {
                    0
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
    out
}
