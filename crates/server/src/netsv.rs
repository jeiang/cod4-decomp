// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (server_mp/sv_snapshot_mp.cpp SV_AddEntitiesVisibleFromPoint; game_mp/g_scr_main_mp.cpp ScrCmd_Hide, ScrCmd_ShowToPlayer; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The server's network side: accepts connecting clients, collects their usercmds and console
//! commands, and builds and sends each client's snapshot every frame. The game state it reads
//! lives in [`crate::game::Game`]; joining and leaving go through the same slot calls the bots
//! use, so scripts cannot tell a person from a bot.

use crate::archive::{ArchPlayer, Archive, Frame};
use crate::ban::BanList;
use crate::client::{Conn, Session, Team};
use crate::fire::view_origin;
use crate::game::{EntKind, Game, SoundTo, WorldFx};
use crate::ui::Dest;
use net::connect::{ConnectRequest, Gate, serve};
use net::entity::{EntityState, etype};
use net::oob::{Challenger, Oob, StatusPlayer};
use net::snapshot::Follow;
use net::transport::{MAX_MESSAGE, Transport};
use net::ui::HudElem;
use net::ui::ServerCmd;
use net::voice::{self, Voice};
use net::{ServerLink, Snapshot};
use sim::pm::{PmType, UserCmd};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// Silence after which a client that has been heard from is dropped (`sv_timeout`'s default).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(240);
/// Silence after which a client that never sent anything since it connected is dropped (`sv_connectTimeout`).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
/// Datagrams one [`NetSv::poll`] reads at most. What is left waits in the socket for the next frame, so a flood
/// cannot starve the game frame.
const MAX_PACKETS_PER_POLL: usize = 1024;
/// Challenge times remembered for the ping limits.
const MAX_CHALLENGES: usize = 1024;
/// How long a challenge time is worth keeping.
const CHALLENGE_KEEP: Duration = Duration::from_secs(30);
/// Addresses the query limiter tracks at once.
const MAX_TRACKED_IPS: usize = 4096;
/// Queries (`getchallenge`, `getinfo`, `getstatus`) one address outside the local network may make in a burst and
/// then every second: room for several players behind one NAT joining together.
const QUERY_BURST: f32 = 16.0;
const QUERY_PER_SEC: f32 = 8.0;
/// The same for all addresses together, so a flood from many spoofed sources cannot make the server a reflector.
const QUERY_BURST_ALL: f32 = 256.0;
const QUERY_PER_SEC_ALL: f32 = 128.0;
/// Usercmds taken from one client per server frame; a client cannot run faster than real time.
const MAX_CMDS_PER_FRAME: usize = 24;
/// Voice frames kept between two services of the network (a service is a server frame; a talker sends 50 a second).
const MAX_VOICE_IN: usize = 512;
/// Voice frames a speaker may send a second (a talker sends 50) and the burst allowed on top.
const VOICE_PER_SEC: f32 = 60.0;
const VOICE_BURST: f32 = 12.0;
/// `getfile` requests one address may make a second, and the burst; each is answered with a window of 32 KiB.
const FILE_PER_SEC: f32 = 20.0;
const FILE_BURST: f32 = 40.0;

/// How long after a relayed frame a player still counts as talking (`istalking`, the talk balloon over their head).
pub const TALKING_MS: i32 = 300;

/// Commands kept for a client that is ahead of the server's frames.
const MAX_QUEUED_CMDS: usize = 64;

/// Commands behind at which a client no longer gets the ones it can do without (prints, chat, kill feed): it is
/// already struggling and each one only adds to the backlog.
const IGNORABLE_BEHIND: usize = 64;

/// What a client hears when it sends game packets to a server that has no slot for it.
pub const NOT_CONNECTED: &str = "Not connected to the server";

/// How long a client may take no commands, with a backlog, before it counts as lagging.
const STALL: Duration = Duration::from_secs(1);

/// `EntityState::eflags` bits the server sets on players.
pub mod eflags {
    pub const TEAM_AXIS: u32 = 1 << 0;
    pub const TEAM_ALLIES: u32 = 1 << 1;
    pub const DEAD: u32 = 1 << 2;
    /// A script model the script `physicslaunch`ed (`TR_PHYSICS`): clients simulate it as a rigid body launched from
    /// `origin` and `angles`, struck at `launch_point` by `velocity`, and draw it where the body is.
    pub const PHYSICS_LAUNCH: u32 = 1 << 3;
    /// The player is speaking over voice chat (`EF_TALK`): the server relayed a frame of theirs a moment ago.
    pub const TALKING: u32 = 1 << 4;
    /// The server has heard nothing from the player for [`CONNECTION_INTERRUPTED_MS`] (`EF_CONNECTION_INTERRUPTED`).
    pub const CONNECTION_INTERRUPTED: u32 = 1 << 5;
    /// The player is mounted on a turret (`EF_TURRET_ACTIVE`).
    pub const TURRET: u32 = 1 << 6;
    /// A script pinged the player (`pingPlayer`): the other team's compass shows them for a moment.
    pub const PING: u32 = 1 << 7;
    /// The entity was moved by fiat since the last snapshot (`EF_TELEPORT_BIT`, flipped): not slid there.
    pub const TELEPORT: u32 = net::entity::TELEPORT_BIT;
}

/// Time the server has listened to a client without hearing it, after which others are shown the
/// connection-interrupted marker over it. Only listening counts: a long frame or a map load, during which the
/// socket is not read, is not silence.
const CONNECTION_INTERRUPTED_MS: u64 = 1000;

pub struct Peer {
    pub link: ServerLink,
    pub cmds: VecDeque<UserCmd>,
    pub name: String,
    last_heard: Instant,
    /// Milliseconds spent waiting for packets since this client's last one.
    unheard_ms: u64,
    heard_now: bool,
    /// Effect names announced so far (`fx <index> <name>` commands).
    fx_sent: usize,
    /// How many of `Game::sounds` the client has been told the names of.
    snd_sent: usize,
    /// A reliable command did not fit: the peer is too far behind and is dropped at the next service
    /// ([`NetSv::overflowed`]).
    overflowed: bool,
    /// Newest command sequence it had acknowledged at the last flush, and since when that has not moved while
    /// commands waited.
    last_ack: u32,
    ack_moved: Instant,
    /// It has taken nothing for [`STALL`] with [`IGNORABLE_BEHIND`] commands waiting: lagging, not in a burst.
    stalled: bool,
    /// The bytes a second it asked to be sent (`rate` of its userinfo), 0 when it named none.
    rate: i32,
    /// Milliseconds between the snapshots it asked for (`snaps`), 0 when it named none: one every server frame.
    snapshot_msec: i32,
    /// Server time before which no snapshot goes out (the rate and `snaps` pacing).
    next_snapshot: i32,
    /// Players this client muted (`mute <n>`), by bit: their voice is not relayed to it.
    muted: u64,
    /// The speaker's voice frames allowed now (a token bucket of [`VOICE_BURST`], refilled at [`VOICE_PER_SEC`]) and when
    /// it was last filled.
    voice_tokens: f32,
    voice_at: Instant,
    /// Bytes of voice queued for this client since its last snapshot, counted against its rate.
    voice_bytes: usize,
    /// It gave the private password (or was kept from a map in a private slot): it may take a private slot.
    pub privileged: bool,
    /// A client packet has arrived since it connected.
    heard_any: bool,
    connected_at: Instant,
}

impl Peer {
    /// Whether any packet has arrived from the client since it connected.
    pub fn heard(&self) -> bool {
        self.heard_any
    }

    /// How long the client has been connected.
    pub fn age(&self) -> Duration {
        self.connected_at.elapsed()
    }

    /// Queues a reliable command. A client already [`IGNORABLE_BEHIND`] behind does not get the commands that only
    /// inform; one that cannot take a command at all is flagged for dropping instead of carrying on desynced.
    pub fn queue(&mut self, line: impl Into<String>) {
        let line = line.into();
        if self.stalled && ignorable(&line) {
            return;
        }
        if self.link.command(line).is_err() {
            self.overflowed = true;
        }
    }
}

/// Commands the game plays fine without: console and feed prints, chat, kill feed.
fn ignorable(line: &str) -> bool {
    match ServerCmd::parse(line) {
        Some(ServerCmd::Print { kind, .. }) => kind != net::ui::PrintKind::Bold,
        Some(ServerCmd::Chat { .. } | ServerCmd::Obituary(_)) => true,
        _ => false,
    }
}

pub enum Inbound {
    /// A `connect` that passed the challenge, and the milliseconds since that sender asked for the challenge when
    /// the server keeps track of them (see [`NetSv::track_ping`]).
    Connect(ConnectRequest, Option<u32>),
    /// A `getfile` with a good challenge: whether to serve it is the server's to judge.
    GetFile {
        from: SocketAddr,
        name: String,
        offset: u64,
    },
    /// An `rcon` packet; whether it is allowed is the server's to judge.
    Rcon {
        from: SocketAddr,
        password: String,
        command: String,
    },
}

/// What a `getstatus` is answered with: the server info settings and the players.
pub struct Status {
    pub info: Vec<(String, String)>,
    pub players: Vec<StatusPlayer>,
}

/// Counters for the report.
#[derive(Debug, Default, Clone, Copy)]
pub struct NetStats {
    pub snapshots_out: u64,
    pub bytes_out: u64,
    pub bytes_in: u64,
    pub joins: u64,
    pub timeouts: u64,
    /// Queries dropped by the rate limit.
    pub queries_limited: u64,
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
    /// Voice frames clients sent since the last [`NetSv::take_voice`]: `(speaker slot, sequence, frame)`.
    voice_in: Vec<(u16, u16, voice::Frame)>,
    /// `getfile` budgets by address.
    file_budget: std::collections::HashMap<std::net::IpAddr, (f32, Instant)>,
    /// Silence that drops a client ([`Self::timed_out`]); the server sets them from `sv_timeout` and
    /// `sv_connectTimeout`.
    pub timeout: Duration,
    pub connect_timeout: Duration,
    /// Remember when each sender asked for its challenge, for `sv_minPing`/`sv_maxPing`.
    pub track_ping: bool,
    challenged: HashMap<SocketAddr, Instant>,
    query_limit: QueryLimit,
}

/// Whether `ip` is on the local network (or this machine): exempt from the ping limits and the per-address
/// connection cap, and given a fast rate.
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(i) => i.is_loopback() || i.is_private() || i.is_link_local(),
        IpAddr::V6(i) => i.is_loopback(),
    }
}

/// Token buckets for connectionless queries: one per address and one for all, so neither a single client nor a
/// crowd of spoofed sources can make the server answer without bound. Not applied to `connect` (a client repeats
/// it until answered) or `rcon` (it has its own limit).
#[derive(Default)]
struct QueryLimit {
    per_ip: HashMap<IpAddr, (f32, Instant)>,
    all: Option<(f32, Instant)>,
}

fn refill(bucket: &mut (f32, Instant), now: Instant, burst: f32, per_sec: f32) {
    let dt = now.saturating_duration_since(bucket.1).as_secs_f32();
    bucket.0 = (bucket.0 + dt * per_sec).min(burst);
    bucket.1 = now;
}

impl QueryLimit {
    fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        let all = self.all.get_or_insert((QUERY_BURST_ALL, now));
        refill(all, now, QUERY_BURST_ALL, QUERY_PER_SEC_ALL);
        if all.0 < 1.0 {
            return false;
        }
        if self.per_ip.len() >= MAX_TRACKED_IPS {
            // An address idle long enough to have refilled is the same as one never seen.
            self.per_ip
                .retain(|_, (_, t)| now.saturating_duration_since(*t) < Duration::from_secs(10));
        }
        let full = self.per_ip.len() >= MAX_TRACKED_IPS;
        let bucket = if full || is_lan(ip) {
            // No room to track this address, or a local one: only the global limit applies to it.
            None
        } else {
            let b = self.per_ip.entry(ip).or_insert((QUERY_BURST, now));
            refill(b, now, QUERY_BURST, QUERY_PER_SEC);
            Some(b)
        };
        if let Some(b) = bucket {
            if b.0 < 1.0 {
                return false;
            }
            b.0 -= 1.0;
        }
        if let Some(all) = self.all.as_mut() {
            all.0 -= 1.0;
        }
        true
    }
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
            voice_in: Vec::new(),
            file_budget: std::collections::HashMap::new(),
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            track_ping: false,
            challenged: HashMap::new(),
            query_limit: QueryLimit::default(),
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.t.local_addr()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.iter().flatten().count()
    }

    pub fn peer(&self, slot: u16) -> Option<&Peer> {
        self.peers.get(usize::from(slot))?.as_ref()
    }

    pub fn slot_of(&self, addr: SocketAddr) -> Option<u16> {
        self.peers
            .iter()
            .position(|p| p.as_ref().is_some_and(|p| p.link.addr == addr))
            .map(|n| n as u16)
    }

    /// The client at `ip` that said it was `qport` when it connected: the same session after a NAT gave it another
    /// port, or one that reconnects.
    pub fn session_of(&self, ip: IpAddr, qport: u16) -> Option<u16> {
        self.peers
            .iter()
            .position(|p| {
                p.as_ref()
                    .is_some_and(|p| p.link.addr.ip() == ip && p.link.qport == qport)
            })
            .map(|n| n as u16)
    }

    /// Clients connected from `ip`.
    pub fn clients_from(&self, ip: IpAddr) -> usize {
        self.peers
            .iter()
            .flatten()
            .filter(|p| p.link.addr.ip() == ip)
            .count()
    }

    /// Makes room for `max` clients (`SV_ChangeMaxClients`). Every slot must be free: a map change takes the peers
    /// first ([`Self::take_peers`]).
    pub fn resize(&mut self, max: usize) {
        debug_assert!(self.peers.iter().all(Option::is_none));
        self.peers.resize_with(max, || None);
    }

    /// Files a client packet with the peer in `slot`. False when the session did not accept it (old, forged or
    /// for another qport).
    fn deliver(&mut self, slot: u16, packet: &[u8]) -> bool {
        let Some(peer) = self.peers[usize::from(slot)].as_mut() else {
            return false;
        };
        let Some(p) = peer.link.receive(packet) else {
            return false;
        };
        peer.last_heard = Instant::now();
        peer.heard_now = true;
        peer.heard_any = true;
        peer.unheard_ms = 0;
        for (_, c) in p.cmds {
            if peer.cmds.len() < MAX_QUEUED_CMDS {
                peer.cmds.push_back(c);
            }
        }
        self.inbox.extend(p.reliable.into_iter().map(|l| (slot, l)));
        let now = Instant::now();
        peer.voice_tokens = (peer.voice_tokens
            + now.duration_since(peer.voice_at).as_secs_f32() * VOICE_PER_SEC)
            .min(VOICE_BURST);
        peer.voice_at = now;
        for (seq, frame) in p.voice {
            if peer.voice_tokens >= 1.0 && self.voice_in.len() < MAX_VOICE_IN {
                peer.voice_tokens -= 1.0;
                self.voice_in.push((slot, seq, frame));
            }
        }
        true
    }

    /// A packet from an address no client has, but with the qport of one at the same IP: that client's NAT gave it
    /// another port. The session takes the new address when the packet is one it accepts (the netchan sequence
    /// and the qport both check out), so a replayed or forged packet moves nothing.
    fn deliver_moved(&mut self, from: SocketAddr, packet: &[u8]) -> bool {
        let Some(qport) = net::packet_qport(packet) else {
            return false;
        };
        let Some(slot) = self.session_of(from.ip(), qport) else {
            return false;
        };
        let Some(peer) = self.peers[usize::from(slot)].as_mut() else {
            return false;
        };
        let old = std::mem::replace(&mut peer.link.addr, from);
        if self.deliver(slot, packet) {
            return true;
        }
        if let Some(peer) = self.peers[usize::from(slot)].as_mut() {
            peer.link.addr = old;
        }
        false
    }

    /// Reads datagrams for up to `wait` (until none is waiting after the first), files client
    /// traffic with its peer and returns what needs the game: connects and leaves.
    pub fn poll(
        &mut self,
        wait: Duration,
        info: &dyn Fn() -> Vec<(String, String)>,
        status: &dyn Fn() -> Status,
    ) -> Vec<Inbound> {
        let mut out = Vec::new();
        let mut wait = wait;
        let listening = Instant::now();
        for p in self.peers.iter_mut().flatten() {
            p.heard_now = false;
        }
        let mut buf = std::mem::take(&mut self.buf);
        let mut read = 0;
        while read < MAX_PACKETS_PER_POLL
            && let Ok(Some((n, from))) = self.t.recv_from(&mut buf, Some(wait))
        {
            read += 1;
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
                let query = matches!(o, Oob::GetChallenge | Oob::GetInfo(_) | Oob::GetStatus(_));
                if query && !self.query_limit.allow(from.ip(), Instant::now()) {
                    self.stats.queries_limited += 1;
                    continue;
                }
                if let Oob::GetStatus(c) = o {
                    let Status { mut info, players } = status();
                    info.push(("challenge".into(), c.to_string()));
                    self.t
                        .send_to(from, &Oob::StatusResponse { info, players }.encode());
                    continue;
                }
                if self.track_ping && matches!(o, Oob::GetChallenge) {
                    self.note_challenge(from);
                }
                match serve(
                    &self.challenger,
                    self.started.elapsed().as_secs(),
                    from,
                    o,
                    info,
                ) {
                    Gate::Reply(r) => self.t.send_to(from, &r.encode()),
                    Gate::Accept(req) => {
                        let ping = self
                            .challenged
                            .remove(&from)
                            .map(|t| t.elapsed().as_millis().min(u128::from(u32::MAX)) as u32);
                        out.push(Inbound::Connect(req, ping));
                    }
                    Gate::File { from, name, offset } => {
                        if self.file_allowed(from) {
                            out.push(Inbound::GetFile { from, name, offset });
                        }
                    }
                    Gate::Ignore => {}
                }
            } else if let Some(slot) = self.slot_of(from) {
                self.deliver(slot, packet);
            } else if !self.deliver_moved(from, packet) {
                // A client the server no longer has (kicked, timed out, the server restarted) is told, so it does
                // not sit in a match that is gone.
                self.t
                    .send_to(from, &Oob::Error(NOT_CONNECTED.into()).encode());
            }
        }
        self.buf = buf;
        let listened = listening.elapsed().as_millis() as u64;
        for p in self.peers.iter_mut().flatten().filter(|p| !p.heard_now) {
            p.unheard_ms += listened;
        }
        out
    }

    /// Whether `from` may have a file window now: not banned, and within its request budget.
    fn file_allowed(&mut self, from: SocketAddr) -> bool {
        let now = Instant::now();
        if self.bans.is_banned(from.ip(), now) {
            return false;
        }
        if self.file_budget.len() > 1024 {
            self.file_budget.clear();
        }
        let (tokens, at) = self
            .file_budget
            .entry(from.ip())
            .or_insert((FILE_BURST, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f32() * FILE_PER_SEC).min(FILE_BURST);
        *at = now;
        if *tokens < 1.0 {
            return false;
        }
        *tokens -= 1.0;
        true
    }

    /// Takes the voice frames received since the last call.
    pub fn take_voice(&mut self) -> Vec<(u16, u16, voice::Frame)> {
        std::mem::take(&mut self.voice_in)
    }

    /// Forwards `speaker`'s frame to every client `hears` says yes to, except those that muted the speaker.
    pub fn relay_voice(
        &mut self,
        speaker: u16,
        seq: u16,
        frame: voice::Frame,
        hears: impl Fn(u16) -> bool,
    ) {
        let bit = 1u64 << (speaker & 63);
        for (slot, p) in self.peers.iter_mut().enumerate() {
            let Some(p) = p else { continue };
            if slot == usize::from(speaker) || p.muted & bit != 0 || !hears(slot as u16) {
                continue;
            }
            p.voice_bytes += voice::FRAME_BYTES + 3;
            p.link.push_voice(Voice {
                speaker: speaker as u8,
                seq,
                frame,
            });
        }
    }

    /// `mute <n>` / `unmute <n>` from `slot`: stops (or resumes) relaying player `who` to it.
    pub fn set_muted(&mut self, slot: u16, who: u16, muted: bool) {
        if who >= 64 {
            return;
        }
        if let Some(p) = self
            .peers
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
        {
            let bit = 1u64 << who;
            if muted {
                p.muted |= bit;
            } else {
                p.muted &= !bit;
            }
        }
    }

    /// Answers a `getfile` with the window of `path` that starts at `offset`.
    pub fn send_file(&mut self, to: SocketAddr, path: &std::path::Path, offset: u64) {
        match net::download::window(path, offset) {
            Ok(chunks) => {
                for c in chunks {
                    let bytes = c.encode();
                    self.stats.bytes_out += bytes.len() as u64;
                    self.t.send_to(to, &bytes);
                }
            }
            Err(e) => self.refuse(to, &format!("Cannot read the file: {e}")),
        }
    }

    /// An out-of-band error to `to`.
    pub fn refuse(&mut self, to: SocketAddr, why: &str) {
        self.t.send_to(to, &Oob::Error(why.to_owned()).encode());
    }

    /// Remembers that `from` asked for a challenge now.
    fn note_challenge(&mut self, from: SocketAddr) {
        if self.challenged.len() >= MAX_CHALLENGES {
            self.challenged.retain(|_, t| t.elapsed() < CHALLENGE_KEEP);
        }
        if self.challenged.len() < MAX_CHALLENGES {
            self.challenged.insert(from, Instant::now());
        }
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
            unheard_ms: 0,
            heard_now: false,
            fx_sent: 0,
            snd_sent: 0,
            overflowed: false,
            last_ack: 0,
            ack_moved: Instant::now(),
            stalled: false,
            rate: 0,
            snapshot_msec: 0,
            next_snapshot: 0,
            muted: 0,
            voice_tokens: VOICE_BURST,
            voice_at: Instant::now(),
            voice_bytes: 0,
            privileged: false,
            heard_any: false,
            connected_at: Instant::now(),
        });
        // The slot may have held someone a client muted: the new person starts unmuted.
        for p in self.peers.iter_mut().flatten() {
            p.muted &= !(1u64 << (slot & 63));
        }
        self.stats.joins += 1;
        self.t.send_to(req.from, &Oob::ConnectResponse.encode());
    }

    /// The name a person's peer carries across a map change.
    pub fn rename(&mut self, slot: u16, name: &str) {
        if let Some(p) = self
            .peers
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
        {
            name.clone_into(&mut p.name);
        }
    }

    /// `SV_UserinfoChanged`: what a client's userinfo asks for. The rate is kept within 1000 to 90000 bytes a second
    /// (a client on the local network gets 99999); `snaps` within 1 to 30 a second. Asking for neither leaves the
    /// client with a snapshot every server frame, which is the original's behaviour only above 20 frames a second.
    pub fn set_userinfo(&mut self, slot: u16, info: &str) {
        let Some(p) = self
            .peers
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
        else {
            return;
        };
        let local = is_lan(p.link.addr.ip());
        let rate = info_value(info, "rate");
        p.rate = if local {
            99_999
        } else if rate.is_empty() {
            0
        } else {
            crate::cvar::parse_int(rate).clamp(1000, MAX_RATE)
        };
        let snaps = info_value(info, "snaps");
        p.snapshot_msec = if snaps.is_empty() {
            0
        } else {
            1000 / crate::cvar::parse_int(snaps).clamp(1, 30)
        };
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
        peer.unheard_ms = 0;
        // The new level numbers its effects afresh: the client needs the names again.
        peer.fx_sent = 0;
        peer.snd_sent = 0;
        peer.stalled = false;
        peer.next_snapshot = 0;
        self.peers[usize::from(slot)] = Some(peer);
    }

    /// Frees the slot. With a `notice` the client is told why first, so it shows it instead of sitting in a match
    /// that no longer has it (a few copies: the datagram may be lost).
    pub fn remove_peer(&mut self, slot: u16, notice: Option<&str>) {
        if let (Some(p), Some(why)) = (self.peers[usize::from(slot)].as_ref(), notice) {
            self.notify(p.link.addr, why);
        }
        self.peers[usize::from(slot)] = None;
    }

    /// Tells `addr` the connection is over, and why.
    pub fn notify(&mut self, addr: SocketAddr, why: &str) {
        let packet = Oob::Error(why.to_owned()).encode();
        for _ in 0..3 {
            self.t.send_to(addr, &packet);
        }
    }

    /// A console print for every client but `except`.
    pub fn print_to_others(&mut self, except: u16, text: &str) {
        let line = ServerCmd::Print {
            kind: net::ui::PrintKind::Normal,
            text: text.to_owned(),
        }
        .encode();
        for (slot, p) in self.peers.iter_mut().enumerate() {
            if let Some(p) = p
                && slot != usize::from(except)
            {
                p.queue(line.clone());
            }
        }
    }

    /// Clients that fell too far behind on reliable commands.
    pub fn overflowed(&self) -> Vec<u16> {
        (0..self.peers.len() as u16)
            .filter(|n| {
                self.peers[usize::from(*n)]
                    .as_ref()
                    .is_some_and(|p| p.overflowed)
            })
            .collect()
    }

    /// Clients whose session the transport reports closed (a browser tab closed or reloaded): their slots are freed
    /// at once, not after the silence timeout.
    pub fn closed(&mut self) -> Vec<u16> {
        let gone = self.t.take_closed();
        gone.into_iter().filter_map(|a| self.slot_of(a)).collect()
    }

    /// Clients to drop for silence: [`Self::timeout`] since the last packet, or [`Self::connect_timeout`] for one
    /// that never sent any since it connected.
    pub fn timed_out(&mut self) -> Vec<u16> {
        let (timeout, connect_timeout) = (self.timeout, self.connect_timeout);
        let gone: Vec<u16> = self
            .peers
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.as_ref().is_some_and(|p| {
                    p.last_heard.elapsed()
                        > if p.heard_any {
                            timeout
                        } else {
                            connect_timeout
                        }
                })
            })
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
            p.queue(line);
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
                p.queue(l.clone());
            }
        }
    }

    /// Delivers what scripts queued since the last frame: configstring changes to everybody,
    /// then the one-shot commands to their destinations, in order.
    pub fn flush_ui(&mut self, game: &mut Game) {
        for p in self.peers.iter_mut().flatten() {
            let (pending, acked) = (p.link.commands_pending(), p.link.commands_acked());
            if acked != p.last_ack || pending == 0 {
                (p.last_ack, p.ack_moved) = (acked, Instant::now());
            }
            p.stalled = pending > IGNORABLE_BEHIND && p.ack_moved.elapsed() > STALL;
        }
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
                    p.queue(line.clone());
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
                    p.queue(line.clone());
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
                    SoundTo::TeamExcept(t, skip) => {
                        slot != skip && game.client(slot).is_some_and(|c| c.team == t)
                    }
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
        let mut entities = world_snapshot(game);
        let quiet = self.peers.iter().enumerate().filter_map(|(slot, p)| {
            let p = p.as_ref()?;
            (p.unheard_ms > CONNECTION_INTERRUPTED_MS).then_some(slot as u16)
        });
        mark_interrupted(&mut entities, quiet);
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
        let max_rate = game.cvars.int("sv_maxRate");
        for slot in 0..self.peers.len() {
            let Some(peer) = self.peers[slot].as_mut() else {
                continue;
            };
            if server_time < peer.next_snapshot {
                // Not its turn: the reliable commands still go out, which a snapshot would have carried.
                if peer.link.commands_pending() > 0 {
                    peer.link.send(&mut self.t, None);
                }
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
                // A full window is the client catching up: the rest follows next frame.
                if peer
                    .link
                    .command(format!("fx {} {name}", peer.fx_sent + 1))
                    .is_err()
                {
                    break;
                }
                peer.fx_sent += 1;
            }
            while peer.snd_sent < game.sounds.len() {
                let Some(name) = game.sounds.name(peer.snd_sent + 1) else {
                    break;
                };
                if peer
                    .link
                    .command(format!("sndname {} {name}", peer.snd_sent + 1))
                    .is_err()
                {
                    break;
                }
                peer.snd_sent += 1;
            }
            let bytes = peer.link.send(&mut self.t, Some(snap.canonical()))
                + std::mem::take(&mut peer.voice_bytes);
            peer.next_snapshot =
                server_time + snapshot_delay(peer.rate, peer.snapshot_msec, bytes, max_rate);
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
        world: &[Sent],
    ) -> Option<Snapshot> {
        let c = game.client(slot)?;
        let mut snap = Snapshot {
            num: 0,
            server_time,
            ps: c.ps.clone(),
            inv: Box::new(c.inv.to_words()),
            entities: Vec::new(),
            actors: Vec::new(),
            hud: game.visible_hud(slot),
            objectives: game.visible_objectives(slot),
            follow: None,
        };
        let followed = u16::try_from(c.spectator_client)
            .ok()
            .filter(|t| *t != slot && c.session == Session::Spectator);
        let Some(target) = followed else {
            (snap.entities, snap.actors) = tell(game, world, slot, view_origin(&c.ps), &[], true);
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
        let keep = [Some(target), follow.entity];
        if let Some(f) = past {
            follow.archive_ms = (server_time - f.time).max(1) as u32;
            if let Some(Some(p)) = f.players.get(usize::from(target)) {
                // What the watched player's eyes could see then.
                snap.entities = tell(game, &f.entities, slot, view_origin(&p.ps), &keep, false).0;
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
                snap.entities = tell(game, &f.entities, slot, view_origin(&c.ps), &keep, false).0;
                return Some(snap);
            }
        } else if let Some(t) = game.client(target).filter(|t| t.connected()) {
            (snap.entities, snap.actors) = tell(game, world, slot, view_origin(&t.ps), &keep, true);
            snap.ps = t.ps.clone();
            snap.inv = Box::new(t.inv.to_words());
            snap.objectives = game.visible_objectives(target);
        } else {
            (snap.entities, snap.actors) = tell(game, world, slot, view_origin(&c.ps), &keep, true);
            return Some(snap);
        }
        // The watcher's own prompts ride on the state of the player watched.
        snap.ps.other_flags = c.ps.other_flags;
        snap.follow = Some(follow);
        Some(snap)
    }
}

/// What client `viewer` is sent of `world` when its eye is at `eye`: the entities it may see and, for the compass,
/// the players beyond its sight (`SV_AddEntitiesVisibleFromPoint`). `keep` are entities it gets regardless (the
/// player it watches, the killcam's entity) and its own.
fn tell(
    game: &Game,
    world: &[Sent],
    viewer: u16,
    eye: [f32; 3],
    keep: &[Option<u16>],
    with_actors: bool,
) -> (Vec<EntityState>, Vec<EntityState>) {
    let pvs = game.world.as_ref().and_then(|w| w.collision().pvs_at(eye));
    let (mut seen, mut actors) = (Vec::new(), Vec::new());
    for s in world {
        let n = s.state.number;
        let always = n == viewer || keep.contains(&Some(n));
        if !always
            && s.shown_to
                .is_some_and(|m| viewer >= 64 || m >> viewer & 1 == 0)
        {
            continue;
        }
        let in_sight = always
            || match (&pvs, &s.clusters) {
                (Some(p), Some(c)) => c.iter().any(|&c| p.sees(c)),
                _ => true,
            };
        if in_sight {
            seen.push(s.state.clone());
        } else if with_actors
            && s.state.etype == etype::PLAYER
            && let Some(shot) = compass_shows(game, viewer, &s.state)
        {
            actors.push(compass_state(&s.state, shot));
        }
    }
    (seen, actors)
}

/// How long after a shot the enemies' compasses still show where it came from.
const FIRE_SHOWN_MS: i32 = 2500;

/// Whether the compass of `viewer` may know where the player `p` is, beyond its sight, and if so whether as one
/// who just fired. Friends always; enemies only as far as the radar legitimately shows them (a UAV, a ping, a
/// shot), so a wall hides them (upstream sends such entities by `broadcastTime`, never all).
fn compass_shows(game: &Game, viewer: u16, p: &EntityState) -> Option<bool> {
    let me = game.client(viewer)?;
    let theirs = game.client(p.client)?;
    let shot = game.level.time - theirs.last_fire_time <= FIRE_SHOWN_MS;
    let friend = matches!(me.team, Team::Axis | Team::Allies) && me.team == theirs.team;
    let all_seen = me.team == Team::Spectator
        || friend
        || me.ps.radar_enabled
        || game.cvars.bool("g_compassShowEnemies");
    (all_seen || shot || p.eflags & eflags::PING != 0).then_some(shot && !friend)
}

/// What client `viewer` is sent at this moment: the entities it may see and the players beyond its sight the
/// compass is told about.
pub fn visible_to(game: &Game, viewer: u16) -> (Vec<EntityState>, Vec<EntityState>) {
    let eye = game.client(viewer).map_or([0.0; 3], |c| view_origin(&c.ps));
    tell(game, &world_snapshot(game), viewer, eye, &[], true)
}

/// The part of a player the compass and the names over heads read: where, which way, which side, whether dead or
/// pinged (`shot` flags a recent shot as a ping), the markers and head icon, and the events that show a shot.
fn compass_state(p: &EntityState, shot: bool) -> EntityState {
    let mut a = EntityState::new(p.number);
    a.etype = p.etype;
    a.client = p.client;
    a.origin = p.origin;
    a.angles = [0.0, p.angles[1], 0.0];
    a.eflags = p.eflags | if shot { eflags::PING } else { 0 };
    a.perks = p.perks;
    a.head_icon = p.head_icon;
    a.head_icon_team = p.head_icon_team;
    (a.event, a.prior_events) = (p.event, p.prior_events);
    a.event_seq = p.event_seq;
    a.canonical()
}

/// The `rate` of a stock profile (`seta rate "25000"`), and the most a client may ask for.
const STOCK_RATE: i32 = 25_000;
const MAX_RATE: i32 = 90_000;

/// Milliseconds until the next snapshot of a client with `rate` and `snapshot_msec` after one of `bytes` (`SV_RateMsec`): its `snaps` interval, or
/// the time its rate (at most `max_rate` when that is set, at least 1000) takes to carry the message if longer.
fn snapshot_delay(rate: i32, snapshot_msec: i32, bytes: usize, max_rate: i32) -> i32 {
    // Our snapshots are bigger than the original's delta-compressed ones, so the stock profile's 25000 B/s would
    // hold a full snapshot back for two frames: a rate of 25000 or more is treated as the 90000 maximum.
    let mut rate = if rate >= STOCK_RATE { MAX_RATE } else { rate };
    if max_rate > 0 {
        let cap = max_rate.max(1000);
        rate = if rate > 0 { rate.min(cap) } else { cap };
    }
    if rate <= 0 {
        return snapshot_msec;
    }
    let size = bytes.min(1500) as i32;
    (1000 * (size + 48) / rate).max(snapshot_msec)
}

/// The value of `key` in a `\key\value` string, `""` when it has none.
pub fn info_value<'a>(info: &'a str, key: &str) -> &'a str {
    let mut it = info.split('\\').skip(1);
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k.eq_ignore_ascii_case(key) {
            return v;
        }
    }
    ""
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

/// Flags the player bodies of the clients in `quiet` as having a connection problem.
fn mark_interrupted(entities: &mut [Sent], quiet: impl Iterator<Item = u16>) {
    for slot in quiet {
        if let Some(e) = entities
            .iter_mut()
            .find(|e| e.state.etype == etype::PLAYER && e.state.number == slot)
        {
            e.state.eflags |= eflags::CONNECTION_INTERRUPTED;
        }
    }
}

/// The team flags of an entity's owner, so the compass can tell a friendly vehicle from an enemy one.
fn owner_team_flags(game: &Game, owner: u16) -> u32 {
    match game.client(owner).map(|c| c.team) {
        Some(Team::Axis) => eflags::TEAM_AXIS,
        Some(Team::Allies) => eflags::TEAM_ALLIES,
        _ => 0,
    }
}

/// How far from its origin a model reaches, so a box around it holds it whatever way it faces.
fn model_reach(game: &Game, name: &str) -> f32 {
    game.content
        .model(name)
        .map_or(128.0, |m| m.radius.max(16.0))
}

/// What the entities everybody may see look like: the entities no hiding applies to.
pub fn world_entities(game: &Game) -> Vec<EntityState> {
    world_snapshot(game)
        .into_iter()
        .filter(|s| s.shown_to.is_none())
        .map(|s| s.state)
        .collect()
}

/// One entity of the world with what decides who it is sent to.
#[derive(Debug, Clone)]
pub struct Sent {
    pub state: EntityState,
    /// A hidden entity (`hide`): the clients (bit per slot) it is shown to anyway (`showtoplayer`). `None` for one
    /// everybody may see.
    pub shown_to: Option<u64>,
    /// The visibility clusters its bounds touch; only a client that can see one of them is sent it. `None` for
    /// what is sent regardless (no extent, no map, or too big to bound).
    pub clusters: Option<Vec<i16>>,
}

/// The clusters the box of half-size `reach` around `origin` touches.
fn clusters_around(game: &Game, origin: [f32; 3], reach: f32) -> Option<Vec<i16>> {
    let w = game.world.as_ref()?.collision();
    let c = w.box_clusters(origin.map(|v| v - reach), origin.map(|v| v + reach))?;
    (!c.is_empty()).then_some(c)
}

/// Everything in the world as [`Sent`], in entity-number order, already rounded as the wire rounds it.
pub fn world_snapshot(game: &Game) -> Vec<Sent> {
    let mut out = Vec::new();
    for (n, e) in game.ents.iter().enumerate() {
        let Some(e) = e else { continue };
        let n = n as u16;
        let mut shown_to = None;
        let mut reach = None;
        let mut state = match e.kind {
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
                // The four the player state keeps, so a frame that raised several loses none.
                let at = |back: u8| usize::from(c.ps.event_sequence.wrapping_sub(back) & 3);
                s.set_recent_events(
                    [4, 3, 2, 1].map(|b| (c.ps.events[at(b)], c.ps.event_parms[at(b)])),
                );
                s.event_seq = c.ps.event_sequence;
                reach = Some(48.0);
                s.torso_pitch = c.ps.torso_pitch;
                s.waist_pitch = c.ps.waist_pitch;
                s.damage_timer = c.ps.damage_timer.clamp(0, i32::from(u16::MAX)) as u16;
                s.damage_duration = c.ps.damage_duration.clamp(0, i32::from(u16::MAX)) as u16;
                let torso = c.pose.torso_wire();
                s.torso_clip = torso.clip;
                s.torso_cap = torso.cap;
                s.torso_seq = torso.seq;
                let legs = c.pose.legs_wire();
                s.legs_clip = legs.clip;
                s.legs_seq = legs.seq;
                s.perks = c.ps.perks & 0xf_ffff;
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
                } | if c.compass_ping_until != 0 {
                    eflags::PING
                } else {
                    0
                } | if c.talking_until > game.level.time {
                    eflags::TALKING
                } else {
                    0
                };
                // The scripts' `headicon` is a precached material; one never registered has no index to show.
                s.head_icon = game.shaders.find(&c.head_icon) as u16;
                s.head_icon_team = Team::from_name(&c.head_icon_team).map_or(0, |t| t as u8);
                s
            }
            EntKind::Item => {
                // A weapon on the floor: the clients draw its world model from the weapon and variant.
                let Some(item) = e.item.as_ref() else {
                    continue;
                };
                if e.hidden {
                    continue;
                }
                let mut s = EntityState::new(n);
                s.etype = etype::ITEM;
                reach = Some(32.0);
                s.origin = e.origin;
                s.angles = e.angles;
                s.weapon = item.weapon;
                s.model = u16::from(item.model);
                s.client = item.dropper.unwrap_or(1023);
                s
            }
            EntKind::Plain if e.x.corpse.is_some() => {
                // A player's body (`clonePlayer`): the clients ragdoll it from the pose of the death clip. `event_seq`
                // counts the bodies made, so a reused slot is told from the body that was in it.
                let Some(c) = e.x.corpse.as_ref() else {
                    continue;
                };
                let mut s = EntityState::new(n);
                s.etype = etype::CORPSE;
                s.client = c.client;
                s.model = game.models.find(&e.model) as u16;
                s.origin = e.origin;
                s.angles = e.angles;
                s.pm_type = PmType::Dead as u8;
                s.legs_clip = c.legs.clip;
                s.legs_seq = c.legs.seq;
                s.event_seq = c.serial;
                s.eflags = eflags::DEAD
                    | match c.team {
                        Team::Axis => eflags::TEAM_AXIS,
                        Team::Allies => eflags::TEAM_ALLIES,
                        _ => 0,
                    };
                reach = Some(48.0);
                s
            }
            EntKind::Brush => {
                let Some(model) = e.brush_model else { continue };
                if e.hidden {
                    shown_to = Some(e.shown_to);
                }
                reach = game
                    .world
                    .as_ref()
                    .and_then(|w| w.collision().model_bounds(model))
                    .map(|(lo, hi)| {
                        let far = |i: usize| lo[i].abs().max(hi[i].abs());
                        (far(0).powi(2) + far(1).powi(2) + far(2).powi(2)).sqrt()
                    });
                let mut s = EntityState::new(n);
                s.etype = etype::BRUSH;
                s.origin = e.origin;
                s.angles = e.angles;
                s.model = model;
                s.eflags = e.contents as u32 & 0xff_ffff;
                if !game.is_linked(n) && e.mv.pos.tr.kind != sim::traj::TrType::Stationary {
                    s.velocity = e.mv.pos.tr.evaluate_delta(game.level.time);
                }
                s
            }
            EntKind::Plain if &*e.classname == "script_model" || e.turret.is_some() => {
                // The index names the model in the clients' configstrings; one that was never registered cannot be
                // drawn.
                let model = game.models.find(&e.model);
                if model == 0 {
                    continue;
                }
                if e.hidden {
                    shown_to = Some(e.shown_to);
                }
                reach = Some(model_reach(game, &e.model));
                let mut s = EntityState::new(n);
                s.etype = etype::SCRIPT_MODEL;
                s.origin = e.origin;
                s.angles = e.angles;
                s.model = model as u16;
                if let Some((point, force)) = e.x.physics_launch {
                    s.eflags = eflags::PHYSICS_LAUNCH;
                    s.launch_point = point;
                    s.velocity = force;
                }
                if let Some(owner) = e.x.plane_owner {
                    s.etype = etype::PLANE;
                    // A plane shows on every compass, so it is sent to everyone.
                    reach = None;
                    s.client = owner;
                    s.eflags |= owner_team_flags(game, owner);
                }
                s
            }
            EntKind::Plain if e.veh.is_some() => {
                let (Some(v), model) = (e.veh.as_deref(), game.models.find(&e.model)) else {
                    continue;
                };
                if e.hidden || model == 0 {
                    continue;
                }
                // Helicopters show on every compass whatever the walls: sent to everyone.
                let mut s = EntityState::new(n);
                s.etype = etype::VEHICLE;
                s.origin = e.origin;
                s.angles = e.angles;
                s.velocity = v.vel;
                s.model = model as u16;
                s.client = v.owner;
                s.pm_type = v.stage;
                s.eflags = owner_team_flags(game, v.owner);
                s
            }
            EntKind::Plain if e.world_fx.is_some() => {
                let mut s = EntityState::new(n);
                s.origin = e.origin;
                s.angles = e.angles;
                match e.world_fx {
                    Some(WorldFx::Once {
                        effect,
                        triggers,
                        start_ms,
                    }) => {
                        s.etype = etype::FX;
                        s.model = effect;
                        s.event_seq = triggers;
                        s.eflags = start_ms as u32 & 0xff_ffff;
                    }
                    Some(WorldFx::Looped {
                        effect,
                        period_ms,
                        cull,
                    }) => {
                        s.etype = etype::LOOP_FX;
                        s.model = effect;
                        s.pm_flags = period_ms;
                        s.velocity[0] = cull;
                    }
                    None => continue,
                }
                s
            }
            _ => {
                let Some(m) = e.missile.as_ref() else {
                    continue;
                };
                let mut s = EntityState::new(n);
                s.etype = etype::MISSILE;
                reach = Some(32.0);
                s.origin = e.origin;
                s.angles = e.angles;
                s.velocity = m.pos.evaluate_delta(game.level.time);
                // When it may be drawn (`CG_Missile`'s launch time), as the low 24 bits of the server time.
                s.eflags = m.launch_time as u32 & 0xff_ffff;
                s.weapon = m.weapon;
                s.client = m.parent.unwrap_or(1023);
                s
            }
        };
        // Moved by fiat since the last snapshot. Not for a missile or effect, whose `eflags` hold a time.
        if e.teleport
            && matches!(
                state.etype,
                etype::PLAYER | etype::SCRIPT_MODEL | etype::PLANE | etype::VEHICLE
            )
        {
            state.eflags |= eflags::TELEPORT;
        }
        let clusters = reach.and_then(|r| clusters_around(game, state.origin, r));
        out.push(Sent {
            state: state.canonical(),
            shown_to,
            clusters,
        });
    }
    add_loop_sounds(game, &mut out);
    out.extend(
        game.tempev
            .live(game.level.time)
            .cloned()
            .map(|state| Sent {
                state,
                shown_to: None,
                clusters: None,
            }),
    );
    out
}

/// The sound each entity loops (`playloopsound`), in its state. An entity the snapshot would not otherwise carry
/// (a `script_origin`) becomes a plain one that only has its place and its loop.
fn add_loop_sounds(game: &Game, out: &mut Vec<Sent>) {
    let mut added = false;
    for (n, e) in game.ents.iter().enumerate() {
        let Some(e) = e
            .as_ref()
            .filter(|e| e.loop_sound != 0 && e.kind != EntKind::Client)
        else {
            continue;
        };
        let n = n as u16;
        if let Some(s) = out.iter_mut().find(|s| s.state.number == n) {
            s.state.loop_sound = e.loop_sound;
        } else if !e.hidden {
            let mut s = EntityState::new(n);
            s.etype = etype::GENERAL;
            s.origin = e.origin;
            s.angles = e.angles;
            s.loop_sound = e.loop_sound;
            // Heard from wherever it is, so not culled.
            out.push(Sent {
                state: s.canonical(),
                shown_to: None,
                clusters: None,
            });
            added = true;
        }
    }
    if added {
        out.sort_by_key(|s| s.state.number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::ui::PrintKind;

    fn peer() -> Peer {
        Peer {
            link: ServerLink::new(SocketAddr::from(([127, 0, 0, 1], 9)), 1),
            cmds: VecDeque::new(),
            name: String::new(),
            last_heard: Instant::now(),
            fx_sent: 0,
            snd_sent: 0,
            overflowed: false,
            last_ack: 0,
            ack_moved: Instant::now(),
            stalled: false,
            unheard_ms: 0,
            heard_now: false,
            rate: 0,
            snapshot_msec: 0,
            next_snapshot: 0,
            muted: 0,
            voice_tokens: VOICE_BURST,
            voice_at: Instant::now(),
            voice_bytes: 0,
            privileged: false,
            heard_any: false,
            connected_at: Instant::now(),
        }
    }

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    fn joined(net: &mut NetSv, slot: u16, port: u16) {
        let req = ConnectRequest {
            from: addr(port),
            qport: port,
            name: format!("p{slot}"),
            password: String::new(),
        };
        net.add_peer(slot, &req, &req.name.clone());
    }

    /// What `slot` of `net` would hear from a message sent now, read by a client at `port`.
    fn heard(
        net: &mut NetSv,
        slot: u16,
        client: &mut net::session::ClientLink,
        wire: &mut impl Transport,
    ) -> Vec<u8> {
        let mut buf = [0u8; 2048];
        while let Ok(Some(_)) = wire.recv_from(&mut buf, None) {}
        let NetSv { t, peers, .. } = net;
        peers[usize::from(slot)]
            .as_mut()
            .unwrap()
            .link
            .send(t, None);
        let mut speakers = Vec::new();
        while let Ok(Some((n, _))) = wire.recv_from(&mut buf, None) {
            if let Some(m) = client.receive(&buf[..n]) {
                speakers.extend(m.voice.iter().map(|v| v.speaker));
            }
        }
        speakers
    }

    #[test]
    fn a_voice_frame_reaches_those_who_may_hear_it_unless_they_muted_the_speaker() {
        let mem = net::MemNet::new();
        let mut n = NetSv::new(Box::new(mem.endpoint(addr(1))), 4);
        let (mut w1, mut w2, mut w3) = (
            mem.endpoint(addr(11)),
            mem.endpoint(addr(12)),
            mem.endpoint(addr(13)),
        );
        for (slot, port) in [(0, 11), (1, 12), (2, 13)] {
            joined(&mut n, slot, port);
        }
        let (mut c1, mut c2, mut c3) = (
            net::session::ClientLink::new(addr(1), 11),
            net::session::ClientLink::new(addr(1), 12),
            net::session::ClientLink::new(addr(1), 13),
        );
        let frame = [3u8; voice::FRAME_BYTES];
        // Slot 2 is on another team; slot 1 muted the speaker.
        n.set_muted(1, 0, true);
        n.relay_voice(0, 7, frame, |l| l != 2);
        assert_eq!(
            heard(&mut n, 0, &mut c1, &mut w1),
            Vec::<u8>::new(),
            "never to the speaker"
        );
        assert_eq!(
            heard(&mut n, 1, &mut c2, &mut w2),
            Vec::<u8>::new(),
            "muted"
        );
        assert_eq!(
            heard(&mut n, 2, &mut c3, &mut w3),
            Vec::<u8>::new(),
            "not on the team"
        );
        n.set_muted(1, 0, false);
        n.relay_voice(0, 8, frame, |l| l != 2);
        assert_eq!(heard(&mut n, 1, &mut c2, &mut w2), [0]);
        assert_eq!(heard(&mut n, 2, &mut c3, &mut w3), Vec::<u8>::new());
        // A new person in the slot starts unmuted.
        n.set_muted(1, 0, true);
        joined(&mut n, 0, 11);
        n.relay_voice(0, 9, frame, |_| true);
        assert_eq!(heard(&mut n, 1, &mut c2, &mut w2), [0]);
    }

    #[test]
    fn a_speaker_flooding_voice_is_cut_to_the_allowed_rate_and_a_file_flood_is_cut_too() {
        let mem = net::MemNet::new();
        let mut n = NetSv::new(Box::new(mem.endpoint(addr(1))), 2);
        joined(&mut n, 0, 11);
        let mut cli = net::session::ClientLink::new(addr(1), 11);
        let mut wire = mem.endpoint(addr(11));
        for _ in 0..200 {
            cli.push_voice([1; voice::FRAME_BYTES]);
            cli.push_cmd(UserCmd::default());
            cli.send(&mut wire);
        }
        n.poll(Duration::ZERO, &|| Vec::new(), &|| Status {
            info: Vec::new(),
            players: Vec::new(),
        });
        // The burst, plus what a few ms of refill allows, and no more.
        assert!(n.take_voice().len() <= VOICE_BURST as usize + 2);
        let ip = addr(50);
        let served = (0..200).filter(|_| n.file_allowed(ip)).count();
        assert!(served <= FILE_BURST as usize + 2, "{served}");
    }

    #[test]
    fn a_snapshot_waits_for_the_snaps_interval_and_for_what_the_rate_takes_to_send() {
        // No rate and no snaps asked for: every frame.
        assert_eq!(snapshot_delay(0, 0, 1200, 0), 0);
        // 20 snapshots a second.
        assert_eq!(snapshot_delay(0, 50, 100, 0), 50);
        // 1500 bytes and the 48 of header at 5000 bytes a second take 309 ms.
        assert_eq!(snapshot_delay(5000, 50, 1500, 0), 309);
        // A message is counted at most 1500 bytes.
        assert_eq!(snapshot_delay(5000, 50, 9000, 0), 309);
        // The server's cap beats a higher rate, and is never below 1000.
        assert_eq!(snapshot_delay(90_000, 0, 1500, 5000), 309);
        assert_eq!(snapshot_delay(90_000, 0, 1500, 10), 1548);
        // The stock profile's rate does not skip a 33 ms frame for a big snapshot.
        assert_eq!(snapshot_delay(25_000, 33, 1500, 0), 33);
        // A slow link is still paced by what it asked for.
        assert_eq!(snapshot_delay(24_999, 33, 1500, 0), 61);
        // Fast enough: the interval is what counts.
        assert_eq!(snapshot_delay(90_000, 50, 500, 0), 50);
    }

    #[test]
    fn userinfo_values_are_read_by_key_and_a_peer_off_the_local_network_keeps_the_rate_in_range() {
        assert_eq!(info_value("\\name\\Ann\\rate\\25000", "rate"), "25000");
        assert_eq!(info_value("\\name\\Ann", "RATE"), "");
        let mut net = NetSv::new(
            Box::new(net::MemNet::new().endpoint(SocketAddr::from(([127, 0, 0, 1], 1)))),
            2,
        );
        let req = |ip: [u8; 4]| ConnectRequest {
            from: SocketAddr::from((ip, 5)),
            qport: 1,
            name: String::new(),
            password: String::new(),
        };
        net.add_peer(0, &req([8, 8, 8, 8]), "far");
        net.add_peer(1, &req([192, 168, 1, 4]), "near");
        net.set_userinfo(0, "\\rate\\5\\snaps\\100");
        net.set_userinfo(1, "\\rate\\5\\snaps\\10");
        let (far, near) = (net.peer(0).unwrap(), net.peer(1).unwrap());
        assert_eq!((far.rate, far.snapshot_msec), (1000, 33));
        assert_eq!((near.rate, near.snapshot_msec), (99_999, 100));
        net.set_userinfo(0, "\\rate\\900000");
        assert_eq!(net.peer(0).unwrap().rate, 90_000);
    }

    fn print(kind: PrintKind) -> String {
        ServerCmd::Print {
            kind,
            text: "x".into(),
        }
        .encode()
    }

    #[test]
    fn a_client_taking_commands_keeps_its_prints_but_a_stalled_one_loses_them() {
        let mut p = peer();
        let cfg = ServerCmd::ConfigStrings(vec![(1, "a".into())]).encode();
        for _ in 0..=IGNORABLE_BEHIND * 2 {
            p.queue(cfg.clone());
        }
        let n = p.link.commands_pending();
        p.queue(print(PrintKind::Normal));
        assert_eq!(p.link.commands_pending(), n + 1, "a burst is not lag");
        p.stalled = true;
        p.queue(print(PrintKind::Normal));
        p.queue(print(PrintKind::Console));
        assert_eq!(
            p.link.commands_pending(),
            n + 1,
            "a stalled client skips prints"
        );
        p.queue(print(PrintKind::Bold));
        assert_eq!(p.link.commands_pending(), n + 2, "a bold message is kept");
    }

    #[test]
    fn a_command_that_does_not_fit_marks_the_peer_for_dropping() {
        let mut p = peer();
        let cfg = ServerCmd::ConfigStrings(vec![(1, "a".into())]).encode();
        for _ in 0..net::reliable::WINDOW {
            p.queue(cfg.clone());
        }
        assert!(!p.overflowed);
        p.queue(cfg);
        assert!(p.overflowed);
    }

    #[test]
    fn only_the_quiet_players_are_flagged_as_interrupted() {
        let player = |n| EntityState {
            etype: etype::PLAYER,
            ..EntityState::new(n)
        };
        let mut e: Vec<Sent> = [player(1), player(2), EntityState::new(3)]
            .into_iter()
            .map(|state| Sent {
                state,
                shown_to: None,
                clusters: None,
            })
            .collect();
        mark_interrupted(&mut e, [2u16, 3, 9].into_iter());
        let flagged: Vec<bool> = e
            .iter()
            .map(|e| e.state.eflags & eflags::CONNECTION_INTERRUPTED != 0)
            .collect();
        assert_eq!(flagged, [false, true, false]);
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([8, 8, 8, last])
    }

    fn net_sv(max: usize) -> (NetSv, net::transport::MemNet) {
        let mem = net::MemNet::new();
        let n = NetSv::new(
            Box::new(mem.endpoint(SocketAddr::from(([127, 0, 0, 1], 1)))),
            max,
        );
        (n, mem)
    }

    fn poll(n: &mut NetSv) -> Vec<Inbound> {
        n.poll(Duration::ZERO, &Vec::new, &|| Status {
            info: Vec::new(),
            players: Vec::new(),
        })
    }

    fn request(from: SocketAddr, qport: u16) -> ConnectRequest {
        ConnectRequest {
            from,
            qport,
            name: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn the_peer_table_grows_to_a_larger_sv_maxclients() {
        let (mut n, _) = net_sv(2);
        n.resize(40);
        assert_eq!(n.peers.len(), 40);
        n.add_peer(39, &request(SocketAddr::from(([8, 8, 8, 8], 5)), 1), "last");
        assert_eq!(n.peer_count(), 1);
        n.take_peers();
        n.resize(4);
        assert_eq!(n.peers.len(), 4);
    }

    #[test]
    fn queries_from_one_address_are_limited_and_another_address_is_not_affected() {
        let mut l = QueryLimit::default();
        let t0 = Instant::now();
        let allowed = (0..20).filter(|_| l.allow(ip(1), t0)).count();
        assert_eq!(allowed, QUERY_BURST as usize);
        assert!(l.allow(ip(2), t0), "another address has its own bucket");
        // The bucket refills with time (explicit instants: no dependence on how fast the test runs).
        assert!(!l.allow(ip(1), t0 + Duration::from_millis(100)));
        assert!(l.allow(ip(1), t0 + Duration::from_secs(1)));
    }

    #[test]
    fn queries_from_many_addresses_share_a_global_limit() {
        let mut l = QueryLimit::default();
        let t0 = Instant::now();
        let allowed = (0..=255u32)
            .flat_map(|a| (0..4).map(move |b| IpAddr::from([20, 1, b, a as u8])))
            .filter(|ip| l.allow(*ip, t0))
            .count();
        assert_eq!(allowed, QUERY_BURST_ALL as usize);
    }

    #[test]
    fn a_poll_reads_a_bounded_number_of_datagrams_and_leaves_the_rest_for_the_next() {
        let (mut n, mem) = net_sv(2);
        let mut stranger = mem.endpoint(SocketAddr::from(([127, 0, 0, 1], 7)));
        let server = n.local_addr();
        for _ in 0..MAX_PACKETS_PER_POLL + 5 {
            stranger.send_to(server, b"x");
        }
        // Each stranger's datagram is answered with "not connected": count the answers per poll.
        poll(&mut n);
        assert_eq!(answers(&mut stranger), MAX_PACKETS_PER_POLL);
        poll(&mut n);
        assert_eq!(answers(&mut stranger), 5);
    }

    #[test]
    fn a_client_whose_port_changed_keeps_its_slot_but_another_qport_does_not() {
        let (mut n, mem) = net_sv(2);
        let server = n.local_addr();
        let old = SocketAddr::from(([127, 0, 0, 1], 5));
        let moved = SocketAddr::from(([127, 0, 0, 1], 6));
        n.add_peer(0, &request(old, 7), "ann");
        let mut from_moved = mem.endpoint(moved);
        let mut link = net::ClientLink::new(server, 7);
        link.command("say hi").unwrap();
        link.send(&mut from_moved);
        poll(&mut n);
        assert_eq!(
            n.slot_of(moved),
            Some(0),
            "the session follows the new port"
        );
        assert_eq!(n.slot_of(old), None);
        assert_eq!(n.inbox, [(0, "say hi".to_owned())]);
        // A client with another qport at the same IP is a stranger, not the same session.
        let other_addr = SocketAddr::from(([127, 0, 0, 1], 9));
        let mut stranger = mem.endpoint(other_addr);
        let mut other = net::ClientLink::new(server, 8);
        other.command("kill").unwrap();
        other.send(&mut stranger);
        poll(&mut n);
        assert_eq!(n.slot_of(other_addr), None);
        assert_eq!(n.inbox.len(), 1);
    }

    #[test]
    fn silence_drops_a_client_by_whether_it_was_ever_heard() {
        let (mut n, _) = net_sv(2);
        let addr = |port| SocketAddr::from(([8, 8, 8, 8], port));
        n.add_peer(0, &request(addr(1), 1), "heard");
        n.add_peer(1, &request(addr(2), 1), "never");
        for p in n.peers.iter_mut().flatten() {
            p.last_heard = Instant::now() - Duration::from_secs(1);
        }
        n.peers[0].as_mut().unwrap().heard_any = true;
        n.timeout = Duration::from_secs(60);
        n.connect_timeout = Duration::from_millis(500);
        assert_eq!(n.timed_out(), [1]);
        n.timeout = Duration::from_millis(500);
        assert_eq!(n.timed_out(), [0, 1]);
    }

    fn answers(s: &mut net::transport::MemTransport) -> usize {
        let mut buf = [0u8; 2000];
        std::iter::from_fn(|| s.recv_from(&mut buf, None).unwrap()).count()
    }

    #[test]
    fn a_flood_of_queries_is_answered_only_up_to_the_limit() {
        let (mut n, mem) = net_sv(2);
        let server = n.local_addr();
        let mut far = mem.endpoint(SocketAddr::from(([8, 8, 8, 8], 7)));
        for _ in 0..100 {
            far.send_to(server, &Oob::GetInfo(1).encode());
        }
        poll(&mut n);
        let got = answers(&mut far);
        assert!(got >= 1 && got <= QUERY_BURST as usize + 2, "{got} answers");
        assert!(n.stats.queries_limited >= 100 - QUERY_BURST as u64 - 2);
    }

    #[test]
    fn a_disconnect_forged_with_a_players_address_does_not_drop_them() {
        let (mut n, mem) = net_sv(2);
        let server = n.local_addr();
        let player = SocketAddr::from(([8, 8, 8, 8], 5));
        n.add_peer(0, &request(player, 1), "ann");
        let mut spoof = mem.endpoint(player);
        spoof.send_to(server, &Oob::Disconnect.encode());
        assert!(poll(&mut n).is_empty());
        assert_eq!(n.slot_of(player), Some(0));
    }
}
