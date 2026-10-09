// SPDX-License-Identifier: GPL-3.0-only
//! The two ends of an established connection. [`ServerLink`] is the server's state for one client;
//! [`ClientLink`] is the client's. Each owns a [`Netchan`], the reliable command queues and the
//! snapshot history that delta coding needs, and turns messages into packets and back.
//!
//! Server to client message: reliable block (with the ack of the client's commands), then an
//! optional snapshot delta-coded against the newest snapshot the client acknowledged.
//! Client to server message: the `qport`, a reliable block, the number of the newest snapshot
//! received, and the last few user commands (each a delta from the one before).

use crate::bits::{BitReader, BitWriter, Overflow};
use crate::netchan::{FRAGMENT_SIZE, Netchan};
use crate::reliable::{ReliableIn, ReliableOut};
use crate::snapshot::{BACKUP, Snapshot, SnapshotError, read_snapshot, write_snapshot};
use crate::transport::Transport;
use crate::ui::{ClientUiState, ServerCmd};
use crate::usercmd::{read_cmd, write_cmd};
use sim::pm::UserCmd;
use std::net::SocketAddr;

/// User commands a client packet carries (the newest plus repeats of the previous ones).
pub const REDUNDANT_CMDS: usize = 3;
/// Newest commands the client keeps to repeat.
const CMD_HISTORY: usize = 16;
/// Most commands one packet can claim.
const MAX_CMDS_PER_PACKET: u32 = 16;

/// Bytes of reliable commands a message may carry beside a snapshot.
const RELIABLE_BUDGET: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub bytes_out: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub packets_in: u64,
    pub snapshots_out: u64,
    pub full_snapshots_out: u64,
    pub snapshot_bytes_out: u64,
}

// ---- server side ---------------------------------------------------------------------------

/// What one client packet delivered.
#[derive(Debug, Default)]
pub struct ClientPacket {
    /// Commands newer than any delivered before, oldest first, with their numbers.
    pub cmds: Vec<(u32, UserCmd)>,
    pub reliable: Vec<String>,
}

pub struct ServerLink {
    pub addr: SocketAddr,
    pub qport: u16,
    chan: Netchan,
    rel_out: ReliableOut,
    rel_in: ReliableIn,
    /// Sent snapshots by `num % BACKUP`.
    frames: Vec<Option<Snapshot>>,
    next_num: u32,
    acked: u32,
    last_cmd: u32,
    scratch: Vec<u8>,
    pub stats: Stats,
}

impl ServerLink {
    pub fn new(addr: SocketAddr, qport: u16) -> Self {
        Self {
            addr,
            qport,
            chan: Netchan::new(),
            rel_out: ReliableOut::default(),
            rel_in: ReliableIn::default(),
            frames: (0..BACKUP).map(|_| None).collect(),
            next_num: 1,
            acked: 0,
            last_cmd: 0,
            scratch: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// Queues a reliable command for the client. `Err`: the client is too far behind to keep.
    pub fn command(&mut self, cmd: impl Into<String>) -> Result<(), Overflow> {
        self.rel_out.push(cmd)
    }

    pub fn commands_pending(&self) -> usize {
        self.rel_out.pending()
    }

    /// Sequence of the newest command the client has acknowledged.
    pub fn commands_acked(&self) -> u32 {
        self.rel_out.acked()
    }

    pub fn dropped(&self) -> u32 {
        self.chan.dropped
    }

    /// Takes a packet from this client's address.
    pub fn receive(&mut self, packet: &[u8]) -> Option<ClientPacket> {
        let mut msg = std::mem::take(&mut self.scratch);
        let out = if self.chan.process(packet, &mut msg) {
            self.stats.bytes_in += packet.len() as u64;
            self.stats.packets_in += 1;
            self.parse(&msg).ok()
        } else {
            None
        };
        self.scratch = msg;
        out
    }

    fn parse(&mut self, msg: &[u8]) -> Result<ClientPacket, Overflow> {
        let mut r = BitReader::new(msg);
        if r.read_u16()? != self.qport {
            return Err(Overflow);
        }
        let mut out = ClientPacket::default();
        self.rel_out.ack(r.read_u32()?);
        self.rel_in.read(&mut r, &mut out.reliable)?;
        let ack = r.read_u32()?;
        if ack == 0 || ack < self.next_num {
            // A late packet may carry an older ack: never move backwards.
            if ack == 0 || ack.wrapping_sub(self.acked) < 1 << 31 {
                self.acked = ack;
            }
        }
        let last = r.read_u32()?;
        let n = r.read_bits(5)?;
        if n > MAX_CMDS_PER_PACKET {
            return Err(Overflow);
        }
        let mut prev = UserCmd::default();
        for i in 0..n {
            let c = read_cmd(&mut r, &prev)?;
            prev = c;
            let num = last.wrapping_sub(n - 1 - i);
            if num.wrapping_sub(self.last_cmd) < 1 << 31 && num != self.last_cmd {
                self.last_cmd = num;
                out.cmds.push((num, c));
            }
        }
        Ok(out)
    }

    /// Sends the reliable commands and, when given, a snapshot (its `num` is assigned here).
    /// Returns the snapshot's size in bytes (0 without one).
    pub fn send(&mut self, t: &mut dyn Transport, snap: Option<Snapshot>) -> usize {
        let mut w = BitWriter::with_capacity(FRAGMENT_SIZE);
        w.write_u32(self.rel_in.received());
        self.rel_out.write(&mut w, RELIABLE_BUDGET);
        let mut snap_bytes = 0;
        match snap {
            Some(mut s) => {
                w.write_bool(true);
                s.num = self.next_num;
                self.next_num = self.next_num.wrapping_add(1);
                let before = w.bit_len();
                let base = self.base_for(s.num);
                write_snapshot(&mut w, base, &s);
                let full = base.is_none();
                snap_bytes = (w.bit_len() - before).div_ceil(8);
                self.stats.snapshots_out += 1;
                self.stats.snapshot_bytes_out += snap_bytes as u64;
                self.stats.full_snapshots_out += u64::from(full);
                let slot = (s.num % BACKUP) as usize;
                self.frames[slot] = Some(s);
            }
            None => w.write_bool(false),
        }
        let addr = self.addr;
        let (mut bytes, mut packets) = (0u64, 0u64);
        self.chan.transmit(w.as_bytes(), |d| {
            t.send_to(addr, d);
            bytes += d.len() as u64;
            packets += 1;
        });
        self.stats.bytes_out += bytes;
        self.stats.packets_out += packets;
        snap_bytes
    }

    /// The acknowledged snapshot if it is still in the history and recent enough to delta from.
    fn base_for(&self, num: u32) -> Option<&Snapshot> {
        if self.acked == 0 || num.wrapping_sub(self.acked) >= BACKUP - 2 {
            return None;
        }
        self.frames[(self.acked % BACKUP) as usize]
            .as_ref()
            .filter(|s| s.num == self.acked)
    }

    /// Round trip estimate in ms: how old (by server time) the newest snapshot the client
    /// acknowledged is at `now`. `None` before the first ack.
    pub fn ping(&self, now: i32) -> Option<i32> {
        if self.acked == 0 {
            return None;
        }
        self.frames[(self.acked % BACKUP) as usize]
            .as_ref()
            .filter(|s| s.num == self.acked)
            .map(|s| now.wrapping_sub(s.server_time).max(0))
    }

    /// The newest snapshot number the client acknowledged (0 = none yet).
    pub fn acked(&self) -> u32 {
        self.acked
    }
}

// ---- client side ---------------------------------------------------------------------------

/// What one server message delivered.
#[derive(Debug, Default)]
pub struct ServerMessage {
    pub snapshot: Option<Snapshot>,
    /// Reliable commands that are not user interface commands (those go to [`ClientLink::ui`]).
    pub reliable: Vec<String>,
    /// The server started a new level in this message: the snapshot clock restarts.
    pub new_map: bool,
}

pub struct ClientLink {
    pub server: SocketAddr,
    qport: u16,
    chan: Netchan,
    rel_out: ReliableOut,
    rel_in: ReliableIn,
    frames: Vec<Option<Snapshot>>,
    /// 0 asks the server for a full snapshot.
    ack: u32,
    cmds: std::collections::VecDeque<(u32, UserCmd)>,
    next_cmd: u32,
    scratch: Vec<u8>,
    pub stats: Stats,
    /// Snapshots that arrived as a delta from one this side no longer had.
    pub unusable: u64,
    /// What the server told the user interface: fed by every message this link receives.
    pub ui: ClientUiState,
}

impl ClientLink {
    pub fn new(server: SocketAddr, qport: u16) -> Self {
        Self {
            server,
            qport,
            chan: Netchan::new(),
            rel_out: ReliableOut::default(),
            rel_in: ReliableIn::default(),
            frames: (0..BACKUP).map(|_| None).collect(),
            ack: 0,
            cmds: Default::default(),
            next_cmd: 1,
            scratch: Vec::new(),
            stats: Stats::default(),
            unusable: 0,
            ui: ClientUiState::new(),
        }
    }

    pub fn command(&mut self, cmd: impl Into<String>) -> Result<(), Overflow> {
        self.rel_out.push(cmd)
    }

    /// Records a command to send; returns its number.
    pub fn push_cmd(&mut self, cmd: UserCmd) -> u32 {
        let n = self.next_cmd;
        self.next_cmd += 1;
        if self.cmds.len() == CMD_HISTORY {
            self.cmds.pop_front();
        }
        self.cmds.push_back((n, cmd));
        n
    }

    pub fn dropped(&self) -> u32 {
        self.chan.dropped
    }

    /// Takes a datagram from the server.
    pub fn receive(&mut self, packet: &[u8]) -> Option<ServerMessage> {
        let mut msg = std::mem::take(&mut self.scratch);
        let out = if self.chan.process(packet, &mut msg) {
            self.stats.bytes_in += packet.len() as u64;
            self.stats.packets_in += 1;
            self.parse(&msg).ok()
        } else {
            None
        };
        self.scratch = msg;
        out
    }

    fn parse(&mut self, msg: &[u8]) -> Result<ServerMessage, Overflow> {
        let mut r = BitReader::new(msg);
        let mut out = ServerMessage::default();
        self.rel_out.ack(r.read_u32()?);
        let mut cmds = Vec::new();
        self.rel_in.read(&mut r, &mut cmds)?;
        for c in cmds {
            match ServerCmd::parse(&c) {
                Some(cmd) => {
                    out.new_map |= matches!(cmd, ServerCmd::Map { .. });
                    self.ui.apply(cmd);
                }
                None => out.reliable.push(c),
            }
        }
        if r.read_bool()? {
            let frames = &self.frames;
            match read_snapshot(&mut r, |n| {
                frames[(n % BACKUP) as usize]
                    .as_ref()
                    .filter(|s| s.num == n)
            }) {
                Ok(s) => {
                    self.ack = s.num;
                    self.ui.apply_snapshot(&s);
                    self.frames[(s.num % BACKUP) as usize] = Some(s.clone());
                    out.snapshot = Some(s);
                }
                Err(SnapshotError::MissingBase) => {
                    self.unusable += 1;
                    self.ack = 0;
                }
                Err(SnapshotError::Malformed) => return Err(Overflow),
            }
        }
        Ok(out)
    }

    /// Sends the reliable commands, the snapshot ack and the newest commands.
    pub fn send(&mut self, t: &mut dyn Transport) {
        let mut w = BitWriter::with_capacity(256);
        w.write_u16(self.qport);
        w.write_u32(self.rel_in.received());
        self.rel_out.write(&mut w, RELIABLE_BUDGET);
        w.write_u32(self.ack);
        let n = self.cmds.len().min(REDUNDANT_CMDS);
        w.write_u32(self.cmds.back().map_or(0, |c| c.0));
        w.write_bits(n as u32, 5);
        let mut prev = UserCmd::default();
        for (_, c) in self.cmds.iter().skip(self.cmds.len() - n) {
            write_cmd(&mut w, &prev, c);
            prev = *c;
        }
        let to = self.server;
        let (mut bytes, mut packets) = (0u64, 0u64);
        self.chan.transmit(w.as_bytes(), |d| {
            t.send_to(to, d);
            bytes += d.len() as u64;
            packets += 1;
        });
        self.stats.bytes_out += bytes;
        self.stats.packets_out += packets;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityState, etype};
    use crate::transport::{Impaired, MemNet};
    use std::time::Duration;

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    fn world(tick: u32) -> Snapshot {
        let mut s = Snapshot::empty();
        s.server_time = tick as i32 * 33;
        s.ps.command_time = tick as i32 * 33;
        s.ps.origin = [tick as f32 * 3.5, 2.0, 8.0];
        for k in 0..24u16 {
            s.entities.push(EntityState {
                number: k * 2,
                etype: etype::PLAYER,
                origin: [f32::from(k) * 50.0 + tick as f32, 3.0, 0.0],
                angles: [0.0, (tick * 5 + u32::from(k)) as f32 % 360.0, 0.0],
                ..EntityState::default()
            });
        }
        s.canonical()
    }

    fn cmd(i: u32) -> UserCmd {
        UserCmd {
            server_time: i as i32 * 8,
            forwardmove: 127,
            angles: [0, (i % 65536) as i32, 0],
            ..UserCmd::default()
        }
    }

    /// Runs `ticks` ticks over a lossy link and returns (server, client, delivered snapshots).
    fn run(loss: f32, reorder: f32, ticks: u32) -> (ServerLink, ClientLink, Vec<Snapshot>) {
        let net = MemNet::new();
        let mut st = Impaired::new(net.endpoint(addr(1)), 7, loss, 0.02, reorder);
        let mut ct = Impaired::new(net.endpoint(addr(2)), 9, loss, 0.02, reorder);
        let mut srv = ServerLink::new(addr(2), 77);
        let mut cli = ClientLink::new(addr(1), 77);
        let mut got = Vec::new();
        let mut buf = [0u8; 2000];
        for t in 1..=ticks {
            srv.command(format!("cmd {t}")).unwrap();
            srv.send(&mut st, Some(world(t)));
            while let Ok(Some((n, _))) = ct.recv_from(&mut buf, None) {
                if let Some(m) = cli.receive(&buf[..n]) {
                    got.extend(m.snapshot);
                }
            }
            for k in 0..4 {
                cli.push_cmd(cmd(t * 4 + k));
            }
            cli.send(&mut ct);
            while let Ok(Some((n, _))) = st.recv_from(&mut buf, Some(Duration::ZERO)) {
                srv.receive(&buf[..n]);
            }
        }
        (srv, cli, got)
    }

    #[test]
    fn lossless_link_delivers_every_snapshot_in_order_as_deltas() {
        let (srv, cli, got) = run(0.0, 0.0, 120);
        assert_eq!(got.len(), 120);
        assert_eq!(cli.dropped(), 0);
        for s in &got {
            assert_eq!(s.server_time, s.num as i32 * 33);
        }
        assert_eq!(srv.stats.full_snapshots_out, 1);
        assert!(srv.stats.snapshot_bytes_out / srv.stats.snapshots_out < 200);
    }

    #[test]
    fn a_lossy_reordering_link_still_converges_and_reliable_commands_arrive_once_in_order() {
        let net = MemNet::new();
        let mut st = Impaired::new(net.endpoint(addr(1)), 3, 0.2, 0.05, 0.1);
        let mut ct = Impaired::new(net.endpoint(addr(2)), 5, 0.2, 0.05, 0.1);
        let mut srv = ServerLink::new(addr(2), 5);
        let mut cli = ClientLink::new(addr(1), 5);
        let mut buf = [0u8; 2000];
        let (mut cmds, mut commands, mut snaps) = (Vec::new(), Vec::new(), Vec::new());
        for t in 1..=400u32 {
            if t <= 100 {
                srv.command(format!("s{t}")).unwrap();
                cli.command(format!("c{t}")).unwrap();
            }
            srv.send(&mut st, Some(world(t)));
            while let Ok(Some((n, _))) = ct.recv_from(&mut buf, None) {
                if let Some(m) = cli.receive(&buf[..n]) {
                    commands.extend(m.reliable);
                    snaps.extend(m.snapshot);
                }
            }
            cli.push_cmd(cmd(t));
            cli.send(&mut ct);
            while let Ok(Some((n, _))) = st.recv_from(&mut buf, None) {
                if let Some(p) = srv.receive(&buf[..n]) {
                    cmds.extend(p.cmds);
                }
            }
        }
        let expect: Vec<String> = (1..=100).map(|i| format!("s{i}")).collect();
        assert_eq!(commands, expect);
        assert!(snaps.len() > 200, "{} snapshots arrived", snaps.len());
        // Every delivered snapshot equals what the sender built for that tick.
        for s in &snaps {
            let mut want = world(s.server_time as u32 / 33);
            want.num = s.num;
            assert_eq!(s, &want);
        }
        let nums: Vec<u32> = cmds.iter().map(|c| c.0).collect();
        assert!(
            nums.windows(2).all(|w| w[0] < w[1]),
            "commands run once, in order"
        );
        assert!(nums.len() > 250);
        // Commands repeat, so only the last few packets can be missing.
        assert!(*nums.last().unwrap() >= 395, "{nums:?}");
    }

    #[test]
    fn a_stale_ack_or_wrong_qport_changes_nothing() {
        let mut srv = ServerLink::new(addr(2), 9);
        let mut cli = ClientLink::new(addr(1), 10);
        let net = MemNet::new();
        let mut t = net.endpoint(addr(2));
        let mut sink = net.endpoint(addr(1));
        cli.push_cmd(cmd(1));
        cli.send(&mut t);
        let mut buf = [0u8; 2000];
        let (n, _) = sink.recv_from(&mut buf, None).unwrap().unwrap();
        assert!(srv.receive(&buf[..n]).is_none(), "qport mismatch");
    }

    #[test]
    fn an_unacknowledged_client_gets_full_snapshots_and_a_big_one_fragments() {
        let net = MemNet::new();
        let mut st = net.endpoint(addr(1));
        let mut ct = net.endpoint(addr(2));
        let mut srv = ServerLink::new(addr(2), 1);
        let mut cli = ClientLink::new(addr(1), 1);
        let mut big = world(1);
        for k in 100..600u16 {
            big.entities.push(EntityState {
                number: k,
                origin: [f32::from(k) * 7.3, -2.2 * f32::from(k), 9.0],
                angles: [1.0, f32::from(k), 0.0],
                model: k % 50,
                ..EntityState::default()
            });
        }
        let big = big.canonical();
        srv.send(&mut st, Some(big.clone()));
        let mut buf = [0u8; 2000];
        let mut datagrams = 0;
        let mut got = None;
        while let Some((n, _)) = ct.recv_from(&mut buf, None).unwrap() {
            datagrams += 1;
            if let Some(m) = cli.receive(&buf[..n]) {
                got = m.snapshot;
            }
        }
        assert!(datagrams > 1, "{datagrams}");
        let mut want = big;
        want.num = 1;
        assert_eq!(got.unwrap(), want);
    }
}
