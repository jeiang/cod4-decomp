// SPDX-License-Identifier: GPL-3.0-only
//! Out-of-band packets: a `0xffffffff` marker, then one line of text. They carry the
//! challenge-response connect and server discovery; everything else is in the [`Netchan`](crate::Netchan).

use crate::netchan::OOB_MARKER;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::SocketAddr;

/// Bumped when the wire format changes; a server refuses other versions.
pub const PROTOCOL: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Oob {
    GetChallenge,
    /// The server's challenge for the sender's address.
    Challenge(u32),
    Connect {
        challenge: u32,
        protocol: u32,
        /// Random per-client value the server echoes inside every later packet.
        qport: u16,
        name: String,
        password: String,
    },
    ConnectResponse,
    /// Server discovery: reply with [`Oob::InfoResponse`].
    GetInfo(u32),
    /// `\key\value` pairs.
    InfoResponse(Vec<(String, String)>),
    Disconnect,
    Error(String),
    /// `rcon <password> <command>`: run a console command on the server (see the server's
    /// `rcon_password`).
    Rcon {
        password: String,
        command: String,
    },
    /// `print\n<text>`: console output the server redirected to the sender of an `rcon`.
    Print(String),
}

/// Most text one [`Oob::Print`] carries (`SV_FlushRedirect`); longer output is sent in pieces.
pub const PRINT_CHUNK: usize = 1294;

impl Oob {
    /// `text` as the [`Oob::Print`] packets that carry it, each at most [`PRINT_CHUNK`] bytes
    /// (cut on a character boundary). Empty text still gives one packet: the sender is waiting.
    pub fn print_chunks(text: &str) -> Vec<Oob> {
        let mut out = Vec::new();
        let mut rest = text;
        while rest.len() > PRINT_CHUNK {
            let mut cut = PRINT_CHUNK;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (head, tail) = rest.split_at(cut);
            out.push(Oob::Print(head.to_owned()));
            rest = tail;
        }
        out.push(Oob::Print(rest.to_owned()));
        out
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace(['"', '\n', '\r'], "'"))
}

fn unquote_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

impl Oob {
    pub fn encode(&self) -> Vec<u8> {
        let text = match self {
            Oob::GetChallenge => "getchallenge".to_owned(),
            Oob::Challenge(c) => format!("challengeResponse {c}"),
            Oob::Connect {
                challenge,
                protocol,
                qport,
                name,
                password,
            } => format!(
                "connect {challenge} {protocol} {qport} {} {}",
                quote(name),
                quote(password)
            ),
            Oob::ConnectResponse => "connectResponse".to_owned(),
            Oob::GetInfo(c) => format!("getinfo {c}"),
            Oob::InfoResponse(kv) => {
                let mut s = String::from("infoResponse ");
                for (k, v) in kv {
                    s.push('\\');
                    s.push_str(&k.replace('\\', "/"));
                    s.push('\\');
                    s.push_str(&v.replace('\\', "/"));
                }
                s
            }
            Oob::Disconnect => "disconnect".to_owned(),
            Oob::Error(e) => format!("error {e}"),
            Oob::Rcon { password, command } => format!("rcon {} {command}", quote(password)),
            Oob::Print(t) => format!("print\n{t}"),
        };
        let mut v = OOB_MARKER.to_le_bytes().to_vec();
        v.extend_from_slice(text.as_bytes());
        v
    }

    /// `None` for a datagram that is not out-of-band or is not understood.
    pub fn parse(packet: &[u8]) -> Option<Oob> {
        if !is_oob(packet) {
            return None;
        }
        let text = std::str::from_utf8(&packet[4..]).ok()?;
        // Output keeps its own trailing newlines.
        if let Some(t) = text.strip_prefix("print\n") {
            return Some(Oob::Print(t.trim_end_matches('\0').to_owned()));
        }
        let text = text.trim_end_matches(['\0', '\n']);
        let (cmd, rest) = text.split_once(' ').unwrap_or((text, ""));
        Some(match cmd {
            "getchallenge" => Oob::GetChallenge,
            "challengeResponse" => Oob::Challenge(rest.trim().parse().ok()?),
            "connect" => {
                let w = unquote_words(rest);
                Oob::Connect {
                    challenge: w.first()?.parse().ok()?,
                    protocol: w.get(1)?.parse().ok()?,
                    qport: w.get(2)?.parse().ok()?,
                    name: w.get(3)?.clone(),
                    password: w.get(4).cloned().unwrap_or_default(),
                }
            }
            "connectResponse" => Oob::ConnectResponse,
            "getinfo" => Oob::GetInfo(rest.trim().parse().ok()?),
            "infoResponse" => {
                let mut it = rest.split('\\').skip(1);
                let mut kv = Vec::new();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    kv.push((k.to_owned(), v.to_owned()));
                }
                Oob::InfoResponse(kv)
            }
            "disconnect" => Oob::Disconnect,
            "error" => Oob::Error(rest.to_owned()),
            "rcon" => {
                let rest = rest.trim_start();
                // The password is one word, quoted or not; the command is everything after it.
                let (password, command) = match rest.strip_prefix('"') {
                    Some(q) => q.split_once('"').unwrap_or((q, "")),
                    None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
                };
                Oob::Rcon {
                    password: password.to_owned(),
                    command: command.trim_start().to_owned(),
                }
            }
            _ => return None,
        })
    }
}

pub fn is_oob(packet: &[u8]) -> bool {
    packet.len() >= 4 && packet[..4] == OOB_MARKER.to_le_bytes()
}

/// Issues and checks stateless challenges: a keyed hash of the client's address and a coarse
/// time bucket, so the server remembers nothing about a client until it connects.
pub struct Challenger {
    keys: RandomState,
}

/// Seconds a challenge stays valid (it is accepted for the bucket it was made in and the next).
const BUCKET_SECS: u64 = 10;

impl Challenger {
    pub fn new() -> Self {
        Self {
            keys: RandomState::new(),
        }
    }

    fn make(&self, from: SocketAddr, bucket: u64) -> u32 {
        (self.keys.hash_one((from, bucket)) >> 16) as u32
    }

    /// `now_secs` is any monotonic clock in seconds.
    pub fn issue(&self, from: SocketAddr, now_secs: u64) -> u32 {
        self.make(from, now_secs / BUCKET_SECS)
    }

    pub fn check(&self, from: SocketAddr, challenge: u32, now_secs: u64) -> bool {
        let b = now_secs / BUCKET_SECS;
        challenge == self.make(from, b) || (b > 0 && challenge == self.make(from, b - 1))
    }
}

impl Default for Challenger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_packet_round_trips() {
        for o in [
            Oob::GetChallenge,
            Oob::Challenge(123_456),
            Oob::Connect {
                challenge: 7,
                protocol: PROTOCOL,
                qport: 4242,
                name: "Mr Quote \" Space".into(),
                password: String::new(),
            },
            Oob::ConnectResponse,
            Oob::GetInfo(9),
            Oob::InfoResponse(vec![
                ("hostname".into(), "My Server".into()),
                ("clients".into(), "3".into()),
            ]),
            Oob::Disconnect,
            Oob::Error("Server is full.".into()),
            Oob::Rcon {
                password: "pass word".into(),
                command: "set a \"b c\"; status".into(),
            },
            Oob::Print("line one\nline two\n".into()),
        ] {
            let parsed = Oob::parse(&o.encode()).unwrap();
            let expect = match &o {
                Oob::Connect { name, .. } => {
                    let mut e = o.clone();
                    if let Oob::Connect { name: n, .. } = &mut e {
                        *n = name.replace('"', "'");
                    }
                    e
                }
                _ => o.clone(),
            };
            assert_eq!(parsed, expect);
        }
    }

    #[test]
    fn rcon_takes_an_unquoted_password_and_keeps_the_command_whole() {
        let mut p = OOB_MARKER.to_le_bytes().to_vec();
        p.extend_from_slice(b"rcon secret  kick  \"Mr X\"\n");
        assert_eq!(
            Oob::parse(&p),
            Some(Oob::Rcon {
                password: "secret".into(),
                command: "kick  \"Mr X\"".into()
            })
        );
    }

    #[test]
    fn long_output_is_cut_into_packets_that_join_back_to_the_text() {
        let text: String = "é0123456789\n".repeat(400);
        let chunks = Oob::print_chunks(&text);
        assert!(chunks.len() > 1);
        let mut joined = String::new();
        for c in &chunks {
            let Oob::Print(t) = c else { panic!() };
            assert!(t.len() <= PRINT_CHUNK);
            joined.push_str(t);
        }
        assert_eq!(joined, text);
        assert_eq!(Oob::print_chunks(""), vec![Oob::Print(String::new())]);
    }

    #[test]
    fn garbage_is_not_oob() {
        assert_eq!(Oob::parse(b"\x01\x00\x00\x00getchallenge"), None);
        let mut p = OOB_MARKER.to_le_bytes().to_vec();
        p.extend_from_slice(b"bogus");
        assert_eq!(Oob::parse(&p), None);
        p.truncate(4);
        p.extend_from_slice(b"connect x y");
        assert_eq!(Oob::parse(&p), None);
    }

    #[test]
    fn challenges_are_bound_to_address_and_time() {
        let c = Challenger::new();
        let a = SocketAddr::from(([10, 0, 0, 1], 5000));
        let b = SocketAddr::from(([10, 0, 0, 2], 5000));
        let ch = c.issue(a, 100);
        assert!(c.check(a, ch, 100));
        assert!(c.check(a, ch, 100 + BUCKET_SECS));
        assert!(!c.check(a, ch, 100 + 3 * BUCKET_SECS));
        assert!(!c.check(b, ch, 100));
        assert!(!c.check(a, ch.wrapping_add(1), 100));
    }
}
