// SPDX-License-Identifier: GPL-3.0-only
//! A client's whole network side behind one object: connect, exchange packets, keep the snapshot
//! history. No window or GPU, so the game client, the listen server and the harness all use it.

use crate::connect::{ConnectState, Connector};
use crate::oob::Oob;
use crate::session::{ClientLink, Stats};
use crate::snapshot::Snapshot;
use crate::transport::Transport;
use crate::ui::ClientUiState;
use crate::view::SnapshotBuffer;
use sim::pm::UserCmd;
use std::net::SocketAddr;
use std::time::Duration;
use web_time::Instant;

enum Phase {
    Connecting(Connector),
    Playing(Box<ClientLink>),
    Refused(String),
    /// The server ended a connection that was up (a kick, its notice, or silence): why.
    Dropped(String),
}

/// How long the server may stay silent before the connection is taken as dead (`cl_timeout`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(40);
/// A gap this long between two `pump` calls is the caller not running (a level load, a suspended window), not the
/// server being silent: the silence clock starts over.
const STALL_MS: u64 = 2000;
/// Shown when the server says nothing for [`DEFAULT_TIMEOUT`], or never answers the connect.
pub const TIMED_OUT: &str = "Server connection timed out";

pub struct NetClient<T: Transport> {
    t: T,
    server: SocketAddr,
    phase: Phase,
    started: Instant,
    /// When the last packet from the server arrived, and when `pump` last ran, in `now_ms` time.
    heard_ms: u64,
    pumped_ms: u64,
    timeout: Duration,
    pub snaps: SnapshotBuffer,
    /// Server console commands received and not yet taken.
    pub commands: Vec<String>,
    /// The person's profile, sent as soon as the connection is up ([`PROFILE_DONE`] ends it).
    profile: Vec<String>,
    /// The `userinfo` command that goes first once the connection is up (rate, snapshot rate).
    userinfo: Option<String>,
    buf: Vec<u8>,
}

/// The `userinfo` command a client sends for its name, `rate` (bytes a second it can take) and `snaps` (snapshots a
/// second it wants): the original's `userinfo` string, which the server reads whole each time.
pub fn userinfo_command(name: &str, rate: i32, snaps: i32) -> String {
    let clean = |s: &str| {
        s.replace(['\\', '"', ';'], "")
            .replace(char::is_control, "")
    };
    format!(
        "userinfo \"\\name\\{}\\rate\\{rate}\\snaps\\{snaps}\"",
        clean(name)
    )
}

/// The command that ends a client's profile upload: the server holds a new person back until it arrives, as the
/// original holds the game state until the stats are in.
pub const PROFILE_DONE: &str = "statsdone";

impl<T: Transport> NetClient<T> {
    pub fn new(t: T, server: SocketAddr, name: &str, password: &str, qport: u16) -> Self {
        Self {
            t,
            server,
            phase: Phase::Connecting(Connector::new(server, qport, name, password)),
            started: Instant::now(),
            heard_ms: 0,
            pumped_ms: 0,
            timeout: DEFAULT_TIMEOUT,
            snaps: SnapshotBuffer::default(),
            commands: Vec::new(),
            profile: Vec::new(),
            userinfo: None,
            buf: vec![0; 2048],
        }
    }

    /// Commands (`statsync ...`) that carry the person's profile to the server right after the connection is up. A
    /// client with no profile sends none and still ends the upload.
    pub fn set_profile(&mut self, commands: Vec<String>) {
        self.profile = commands;
    }

    /// The `userinfo` command (see [`userinfo_command`]) to send as soon as the connection is up.
    pub fn set_userinfo(&mut self, command: String) {
        self.userinfo = Some(command);
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

    /// Why the connection ended after it was up: the server's notice ("Player kicked") or [`TIMED_OUT`]. Also set when
    /// the connect attempts ran out.
    pub fn dropped(&self) -> Option<&str> {
        match &self.phase {
            Phase::Dropped(r) => Some(r),
            _ => None,
        }
    }

    /// Sets the silence the server may keep before the connection is dropped (`cl_timeout`).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// [`NetClient::ui`] for readers that only look.
    pub fn ui_ref(&self) -> Option<&ClientUiState> {
        match &self.phase {
            Phase::Playing(l) => Some(&l.ui),
            _ => None,
        }
    }

    /// What the server told the user interface, once connected (see [`crate::ui`]).
    pub fn ui(&mut self) -> Option<&mut ClientUiState> {
        match &mut self.phase {
            Phase::Playing(l) => Some(&mut l.ui),
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
        let now = self.now_ms();
        self.pump_at(wait, now);
        // The wait inside is the server's silence, not the caller being away: the gap is counted from here.
        self.pumped_ms = self.now_ms();
    }

    /// Milliseconds since the server last sent a packet; 0 before the connection is up.
    pub fn silent_ms(&self) -> u64 {
        match self.phase {
            Phase::Playing(_) => self.pumped_ms.saturating_sub(self.heard_ms),
            _ => 0,
        }
    }

    fn pump_at(&mut self, wait: Duration, now: u64) {
        let mut wait = wait;
        if now.saturating_sub(self.pumped_ms) > STALL_MS {
            self.heard_ms = now;
        }
        self.pumped_ms = now;
        if let Phase::Connecting(c) = &mut self.phase {
            if c.gave_up() {
                self.phase = Phase::Dropped(TIMED_OUT.into());
            } else {
                c.poll(now, &mut self.t);
            }
        }
        while let Ok(Some((n, from))) = self.t.recv_from(&mut self.buf, Some(wait)) {
            wait = Duration::ZERO;
            let packet = &self.buf[..n];
            if let Some(o) = Oob::parse(packet) {
                if let Phase::Connecting(c) = &mut self.phase {
                    c.handle(from, o);
                    match c.state() {
                        ConnectState::Connected => {
                            let mut link = Box::new(ClientLink::new(self.server, c.qport));
                            if let Some(line) = self.userinfo.take() {
                                let _ = link.command(&line);
                            }
                            for line in self.profile.drain(..) {
                                let _ = link.command(&line);
                            }
                            let _ = link.command(PROFILE_DONE);
                            self.phase = Phase::Playing(link);
                            self.heard_ms = now;
                        }
                        ConnectState::Refused(r) => self.phase = Phase::Refused(r.clone()),
                        _ => {}
                    }
                } else if from == self.server && matches!(self.phase, Phase::Playing(_)) {
                    match o {
                        Oob::Error(why) => self.phase = Phase::Dropped(why),
                        Oob::Disconnect => {
                            self.phase = Phase::Dropped("Disconnected by the server".to_owned());
                        }
                        // The reply to an `rcon`: the console output the server redirected to us.
                        Oob::Print(text) => self.commands.push(
                            crate::ui::ServerCmd::Print {
                                kind: crate::ui::PrintKind::Console,
                                text,
                            }
                            .encode(),
                        ),
                        _ => {}
                    }
                }
            } else if from == self.server
                && let Phase::Playing(link) = &mut self.phase
                && let Some(m) = link.receive(packet)
            {
                self.heard_ms = now;
                self.commands.extend(m.reliable);
                if m.new_map {
                    // The new level's clock starts near zero again; the old history would reject it.
                    self.snaps.clear();
                }
                if let Some(s) = m.snapshot {
                    self.snaps.push(now, s);
                }
            }
        }
        if matches!(self.phase, Phase::Playing(_))
            && now.saturating_sub(self.heard_ms) > self.timeout.as_millis() as u64
        {
            self.phase = Phase::Dropped(TIMED_OUT.into());
        }
    }

    /// `rcon <password> <command>`: a console command for the server, whose output comes back as console prints.
    pub fn rcon(&mut self, password: &str, command: &str) {
        let oob = Oob::Rcon {
            password: password.to_owned(),
            command: command.to_owned(),
        };
        self.t.send_to(self.server, &oob.encode());
    }

    /// Asks the server for the scoreboard (repeat every couple of seconds while it is shown).
    pub fn request_scores(&mut self) {
        self.command(crate::ui::SCORES_REQUEST);
    }

    /// Sends what a click in the script menu `menu` answers (see [`crate::ui::menu_response`]).
    pub fn menu_response(&mut self, menu: &str, response: &str) {
        self.command(&crate::ui::menu_response(menu, response));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::{Gate, serve};
    use crate::oob::Challenger;
    use crate::transport::{MemNet, MemTransport};

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    /// A client at `now` and a bare server endpoint that answers the handshake like `NetSv` does.
    fn joined() -> (NetClient<MemTransport>, MemTransport, SocketAddr) {
        let net = MemNet::new();
        let mut server = net.endpoint(addr(1));
        let mut c = NetClient::new(net.endpoint(addr(2)), addr(1), "p", "", 7);
        let ch = Challenger::new();
        let mut buf = [0u8; 2048];
        let mut from = addr(2);
        for now in (0..).step_by(10) {
            c.pump_at(Duration::ZERO, now);
            while let Ok(Some((n, f))) = server.recv_from(&mut buf, None) {
                from = f;
                match serve(&ch, 0, f, Oob::parse(&buf[..n]).unwrap(), Vec::new) {
                    Gate::Reply(r) => server.send_to(f, &r.encode()),
                    Gate::Accept(_) => server.send_to(f, &Oob::ConnectResponse.encode()),
                    _ => {}
                }
            }
            if c.connected() {
                c.pump_at(Duration::ZERO, now);
                break;
            }
        }
        (c, server, from)
    }

    #[test]
    fn a_silent_server_times_out_but_a_stalled_caller_does_not() {
        let (mut c, _server, _) = joined();
        c.set_timeout(Duration::from_secs(40));
        let t0 = c.pumped_ms;
        // Steady pumping with no packets: the clock runs to the limit.
        for s in 1..=40 {
            c.pump_at(Duration::ZERO, t0 + s * 1000);
        }
        assert!(c.connected(), "40 s of silence is not yet over the limit");
        c.pump_at(Duration::ZERO, t0 + 41_000);
        assert_eq!(c.dropped(), Some(TIMED_OUT));

        // A caller that did not run for a long while (a level load) starts the clock over.
        let (mut c, _server, _) = joined();
        let t0 = c.pumped_ms;
        c.pump_at(Duration::ZERO, t0 + 100_000);
        assert!(c.connected());
    }

    #[test]
    fn the_server_telling_us_to_go_ends_the_connection_with_its_reason() {
        let (mut c, mut server, from) = joined();
        server.send_to(from, &Oob::Error("Player kicked".into()).encode());
        let t = c.pumped_ms + 10;
        c.pump_at(Duration::ZERO, t);
        assert_eq!(c.dropped(), Some("Player kicked"));
        assert!(!c.connected());
    }

    #[test]
    fn unanswered_connect_requests_give_up() {
        let net = MemNet::new();
        let mut c = NetClient::new(net.endpoint(addr(2)), addr(1), "p", "", 7);
        for i in 0..200 {
            c.pump_at(Duration::ZERO, i * RETRY_FOR_TEST);
        }
        assert_eq!(c.dropped(), Some(TIMED_OUT));
    }

    const RETRY_FOR_TEST: u64 = 600;
}
