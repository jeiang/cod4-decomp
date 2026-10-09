// SPDX-License-Identifier: GPL-3.0-only
//! The challenge-response handshake from both ends. Neither side keeps state for a stranger:
//! the server answers `getchallenge` with a keyed hash of the sender's address and only a valid
//! `connect` creates a slot, so a spoofed source address cannot make it send much or hold memory.

use crate::oob::{Challenger, Oob, PROTOCOL};
use crate::transport::Transport;
use std::net::SocketAddr;

/// A `connect` that passed the challenge and protocol checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub from: SocketAddr,
    pub qport: u16,
    pub name: String,
    pub password: String,
}

/// What the server does with an out-of-band packet from `from`.
pub enum Gate {
    /// Send this reply.
    Reply(Oob),
    /// The sender may join: allocate a slot, then reply [`Oob::ConnectResponse`].
    Accept(ConnectRequest),
    /// A `getfile` that passed the challenge check: the server decides whether to serve it.
    File {
        from: SocketAddr,
        name: String,
        offset: u64,
    },
    /// Nothing to do. A connectionless `disconnect` is one of these: anybody can forge a source address, so only
    /// the sequence-checked reliable `disconnect` of a connected client ends a session.
    Ignore,
}

/// `info` is the answer to `getinfo` (server name, map, players).
pub fn serve(
    challenger: &Challenger,
    now_secs: u64,
    from: SocketAddr,
    oob: Oob,
    info: impl FnOnce() -> Vec<(String, String)>,
) -> Gate {
    match oob {
        Oob::GetChallenge => Gate::Reply(Oob::Challenge(challenger.issue(from, now_secs))),
        Oob::GetInfo(c) => {
            let mut kv = info();
            kv.push(("challenge".into(), c.to_string()));
            Gate::Reply(Oob::InfoResponse(kv))
        }
        Oob::Connect {
            challenge,
            protocol,
            qport,
            name,
            password,
        } => {
            if protocol != PROTOCOL {
                Gate::Reply(Oob::Error(format!(
                    "Server uses protocol {PROTOCOL}, you have {protocol}."
                )))
            } else if !challenger.check(from, challenge, now_secs) {
                Gate::Reply(Oob::Error("Bad challenge.".into()))
            } else {
                Gate::Accept(ConnectRequest {
                    from,
                    qport,
                    name,
                    password,
                })
            }
        }
        Oob::GetFile {
            challenge,
            name,
            offset,
        } => {
            if challenger.check(from, challenge, now_secs) {
                Gate::File { from, name, offset }
            } else {
                Gate::Reply(Oob::Error(crate::download::BAD_CHALLENGE.into()))
            }
        }
        // No `Oob::Disconnect` arm: see `Gate::Ignore`.
        _ => Gate::Ignore,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectState {
    Challenging,
    Connecting(u32),
    Connected,
    Refused(String),
}

/// The client's side: asks for a challenge, answers it, waits for the response; repeats each
/// request every `RETRY_MS` until the server answers or the caller gives up.
pub struct Connector {
    pub server: SocketAddr,
    pub qport: u16,
    name: String,
    password: String,
    state: ConnectState,
    last_send_ms: Option<u64>,
    sent: u32,
}

const RETRY_MS: u64 = 500;
/// Requests sent in all before the server counts as unreachable (`cl_connectTimeout`: about 20 s).
const MAX_ATTEMPTS: u32 = 40;

impl Connector {
    pub fn new(server: SocketAddr, qport: u16, name: &str, password: &str) -> Self {
        Self {
            server,
            qport,
            name: name.to_owned(),
            password: password.to_owned(),
            state: ConnectState::Challenging,
            last_send_ms: None,
            sent: 0,
        }
    }

    /// The server has not answered after [`MAX_ATTEMPTS`] requests.
    pub fn gave_up(&self) -> bool {
        self.sent >= MAX_ATTEMPTS
            && matches!(
                self.state,
                ConnectState::Challenging | ConnectState::Connecting(_)
            )
    }

    pub fn state(&self) -> &ConnectState {
        &self.state
    }

    /// Sends the current request when it is due.
    pub fn poll(&mut self, now_ms: u64, t: &mut dyn Transport) {
        if self.last_send_ms.is_some_and(|l| now_ms < l + RETRY_MS) {
            return;
        }
        let msg = match &self.state {
            ConnectState::Challenging => Oob::GetChallenge,
            ConnectState::Connecting(c) => Oob::Connect {
                challenge: *c,
                protocol: PROTOCOL,
                qport: self.qport,
                name: self.name.clone(),
                password: self.password.clone(),
            },
            ConnectState::Connected | ConnectState::Refused(_) => return,
        };
        self.last_send_ms = Some(now_ms);
        self.sent += 1;
        t.send_to(self.server, &msg.encode());
    }

    pub fn handle(&mut self, from: SocketAddr, oob: Oob) {
        if from != self.server {
            return;
        }
        match (&self.state, oob) {
            (ConnectState::Challenging, Oob::Challenge(c)) => {
                self.state = ConnectState::Connecting(c);
                self.last_send_ms = None;
            }
            (ConnectState::Connecting(_), Oob::ConnectResponse) => {
                self.state = ConnectState::Connected;
            }
            (ConnectState::Challenging | ConnectState::Connecting(_), Oob::Error(e)) => {
                self.state = ConnectState::Refused(e);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MemNet;

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    fn pump(
        ch: &Challenger,
        server: &mut impl Transport,
        client: &mut impl Transport,
        c: &mut Connector,
        now: u64,
        accepted: &mut Vec<ConnectRequest>,
    ) {
        let mut buf = [0u8; 1400];
        c.poll(now, client);
        while let Ok(Some((n, from))) = server.recv_from(&mut buf, None) {
            if let Some(o) = Oob::parse(&buf[..n]) {
                match serve(ch, now / 1000, from, o, Vec::new) {
                    Gate::Reply(r) => server.send_to(from, &r.encode()),
                    Gate::Accept(req) => {
                        accepted.push(req);
                        server.send_to(from, &Oob::ConnectResponse.encode());
                    }
                    _ => {}
                }
            }
        }
        while let Ok(Some((n, from))) = client.recv_from(&mut buf, None) {
            if let Some(o) = Oob::parse(&buf[..n]) {
                c.handle(from, o);
            }
        }
    }

    #[test]
    fn handshake_completes_and_retries_through_loss() {
        let net = MemNet::new();
        let mut cl = net.endpoint(addr(2));
        let ch = Challenger::new();
        let mut c = Connector::new(addr(1), 99, "Player One", "");
        let mut acc = Vec::new();
        // The first request vanishes: nobody listens at the server address yet.
        c.poll(0, &mut cl);
        let mut s = net.endpoint(addr(1));
        for t in [100, 600, 700, 1200, 1300] {
            pump(&ch, &mut s, &mut cl, &mut c, t, &mut acc);
        }
        assert_eq!(c.state(), &ConnectState::Connected);
        assert_eq!(acc.len(), 1);
        assert_eq!((acc[0].name.as_str(), acc[0].qport), ("Player One", 99));
    }

    #[test]
    fn wrong_challenge_or_protocol_is_refused_without_a_slot() {
        let ch = Challenger::new();
        let from = addr(5);
        let bad = Oob::Connect {
            challenge: 12345,
            protocol: PROTOCOL,
            qport: 1,
            name: "x".into(),
            password: String::new(),
        };
        assert!(matches!(
            serve(&ch, 0, from, bad, Vec::new),
            Gate::Reply(Oob::Error(_))
        ));
        let good = ch.issue(from, 0);
        let old = Oob::Connect {
            challenge: good,
            protocol: PROTOCOL + 1,
            qport: 1,
            name: "x".into(),
            password: String::new(),
        };
        assert!(matches!(
            serve(&ch, 0, from, old, Vec::new),
            Gate::Reply(Oob::Error(_))
        ));
    }

    #[test]
    fn an_answer_from_another_address_is_ignored() {
        let mut c = Connector::new(addr(1), 1, "n", "");
        c.handle(addr(9), Oob::Challenge(5));
        assert_eq!(c.state(), &ConnectState::Challenging);
    }

    #[test]
    fn a_forged_disconnect_changes_nothing() {
        let ch = Challenger::new();
        assert!(matches!(
            serve(&ch, 0, addr(5), Oob::Disconnect, Vec::new),
            Gate::Ignore
        ));
    }
}
