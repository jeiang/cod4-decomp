// SPDX-License-Identifier: GPL-3.0-or-later
//! A client's whole network side behind one object: connect, exchange packets, keep the snapshot
//! history. No window or GPU, so the game client, the listen server and the harness all use it.

use crate::connect::{ConnectState, Connector};
use crate::oob::Oob;
use crate::session::{ClientLink, Stats};
use crate::snapshot::Snapshot;
use crate::transport::Transport;
use crate::view::SnapshotBuffer;
use sim::pm::UserCmd;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

enum Phase {
    Connecting(Connector),
    Playing(ClientLink),
    Refused(String),
}

pub struct NetClient<T: Transport> {
    t: T,
    server: SocketAddr,
    phase: Phase,
    started: Instant,
    pub snaps: SnapshotBuffer,
    /// Server console commands received and not yet taken.
    pub commands: Vec<String>,
    buf: Vec<u8>,
}

impl<T: Transport> NetClient<T> {
    pub fn new(t: T, server: SocketAddr, name: &str, password: &str, qport: u16) -> Self {
        Self {
            t,
            server,
            phase: Phase::Connecting(Connector::new(server, qport, name, password)),
            started: Instant::now(),
            snaps: SnapshotBuffer::default(),
            commands: Vec::new(),
            buf: vec![0; 2048],
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn connected(&self) -> bool {
        matches!(self.phase, Phase::Playing(_))
    }

    /// Why the server said no, if it did.
    pub fn refused(&self) -> Option<&str> {
        match &self.phase {
            Phase::Refused(r) => Some(r),
            _ => None,
        }
    }

    pub fn stats(&self) -> Option<&Stats> {
        match &self.phase {
            Phase::Playing(l) => Some(&l.stats),
            _ => None,
        }
    }

    /// Snapshots that arrived as deltas from one this client no longer had.
    pub fn unusable(&self) -> u64 {
        match &self.phase {
            Phase::Playing(l) => l.unusable,
            _ => 0,
        }
    }

    /// Takes everything that has arrived (waiting up to `wait` for the first datagram) and
    /// advances the handshake.
    pub fn pump(&mut self, wait: Duration) {
        let mut wait = wait;
        let now = self.now_ms();
        if let Phase::Connecting(c) = &mut self.phase {
            c.poll(now, &mut self.t);
        }
        while let Ok(Some((n, from))) = self.t.recv_from(&mut self.buf, Some(wait)) {
            wait = Duration::ZERO;
            let now = self.now_ms();
            let packet = &self.buf[..n];
            if let Some(o) = Oob::parse(packet) {
                if let Phase::Connecting(c) = &mut self.phase {
                    c.handle(from, o);
                    match c.state() {
                        ConnectState::Connected => {
                            self.phase = Phase::Playing(ClientLink::new(self.server, c.qport));
                        }
                        ConnectState::Refused(r) => self.phase = Phase::Refused(r.clone()),
                        _ => {}
                    }
                }
            } else if from == self.server
                && let Phase::Playing(link) = &mut self.phase
                && let Some(m) = link.receive(packet)
            {
                self.commands.extend(m.reliable);
                if let Some(s) = m.snapshot {
                    self.snaps.push(now, s);
                }
            }
        }
    }

    /// Queues a console command for the server (reliable, in order).
    pub fn command(&mut self, cmd: &str) {
        if let Phase::Playing(l) = &mut self.phase {
            let _ = l.command(cmd);
        }
    }

    /// Records `cmd` and sends the newest few commands with the acks.
    pub fn send_cmd(&mut self, cmd: UserCmd) {
        if let Phase::Playing(l) = &mut self.phase {
            l.push_cmd(cmd);
            l.send(&mut self.t);
        }
    }

    /// Sends the acks and reliable commands without a new input command.
    pub fn send(&mut self) {
        if let Phase::Playing(l) = &mut self.phase {
            l.send(&mut self.t);
        }
    }

    pub fn latest(&self) -> Option<&Snapshot> {
        self.snaps.latest()
    }

    /// Leaves politely; the server frees the slot at once instead of after its timeout.
    pub fn disconnect(&mut self) {
        let server = self.server;
        for _ in 0..3 {
            self.t.send_to(server, &Oob::Disconnect.encode());
        }
    }
}
