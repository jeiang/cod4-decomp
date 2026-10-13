// SPDX-License-Identifier: GPL-3.0-only
//! The master server (`cod4e-master`): public servers send it a `heartbeat`, clients ask it for the list.
//!
//! The flow follows the stock master's (`SV_MasterHeartbeat` on the server, the `globalservers` request on the
//! client) in this game's own out-of-band format ([`net::oob`]). A heartbeat adds nothing by itself: the master
//! sends `getinfo <challenge>` to the address the heartbeat came from and lists the server only when that address
//! answers with the challenge, so a forged source address cannot put a victim on the list or keep a dead server on
//! it. A `flatline` (the server is shutting down) is checked the same way: the sender is probed and dropped when
//! it does not answer. Servers that stop sending heartbeats expire.
//!
//! [`Master`] holds the state and takes the clock as an argument, so the rules are testable without sockets.

#![cfg_attr(test, allow(clippy::print_stderr, clippy::print_stdout))]

use net::master::HEARTBEAT_SECS;
use net::oob::{Challenger, Oob, SERVERS_PER_PACKET};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// How long a server stays listed without a verified heartbeat: three missed ones.
pub const TTL: Duration = Duration::from_secs(HEARTBEAT_SECS * 3);
/// How long the master waits for the answer to its `getinfo`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Servers one IP address may have listed or being verified at once.
pub const MAX_PER_IP: usize = 8;
/// Servers listed at once.
pub const MAX_SERVERS: usize = 4096;
/// Heartbeats one address may send in a burst: every server it may list, with a few to spare.
const HEARTBEAT_BURST: f32 = (MAX_PER_IP + 2) as f32;
/// Heartbeats being verified at once.
pub const MAX_PENDING: usize = 1024;
/// Addresses in one `getservers` answer.
pub const MAX_REPLY: usize = 8 * SERVERS_PER_PACKET;
/// Addresses a limiter tracks; beyond it only the global bucket applies.
const MAX_TRACKED_IPS: usize = 8192;

/// Token buckets: one per source IP and one for every source together, so neither one sender nor a crowd of
/// spoofed ones can make the master send without bound.
struct Limiter {
    per_ip: HashMap<IpAddr, (f32, Instant)>,
    all: (f32, Instant),
    burst: f32,
    per_sec: f32,
    all_burst: f32,
    all_per_sec: f32,
}

fn refill(b: &mut (f32, Instant), now: Instant, burst: f32, per_sec: f32) {
    let dt = now.saturating_duration_since(b.1).as_secs_f32();
    b.0 = (b.0 + dt * per_sec).min(burst);
    b.1 = now;
}

impl Limiter {
    fn new(burst: f32, per_sec: f32, all_burst: f32, all_per_sec: f32, now: Instant) -> Self {
        Limiter {
            per_ip: HashMap::new(),
            all: (all_burst, now),
            burst,
            per_sec,
            all_burst,
            all_per_sec,
        }
    }

    fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        refill(&mut self.all, now, self.all_burst, self.all_per_sec);
        if self.all.0 < 1.0 {
            return false;
        }
        if self.per_ip.len() >= MAX_TRACKED_IPS {
            // An address idle long enough to have refilled is the same as one never seen.
            let idle = Duration::from_secs_f32(self.burst / self.per_sec);
            self.per_ip
                .retain(|_, (_, t)| now.saturating_duration_since(*t) < idle);
        }
        if self.per_ip.len() >= MAX_TRACKED_IPS {
            // No room to track it: refuse, so a flood of addresses cannot slip past the per-address rule.
            return false;
        }
        let b = self.per_ip.entry(ip).or_insert((self.burst, now));
        refill(b, now, self.burst, self.per_sec);
        if b.0 < 1.0 {
            return false;
        }
        b.0 -= 1.0;
        self.all.0 -= 1.0;
        true
    }
}

struct Listed {
    protocol: u32,
    verified: Instant,
}

/// A `getinfo` sent and not yet answered. The protocol a heartbeat claims is not kept: only the server's own
/// answer decides what it is listed under, so a forged heartbeat cannot relabel a real server.
struct Pending {
    deadline: Instant,
    /// Sent for a `flatline`: unanswered, the server is dropped from the list.
    flatline: bool,
}

pub struct Master {
    challenger: Challenger,
    started: Instant,
    listed: HashMap<SocketAddr, Listed>,
    pending: HashMap<SocketAddr, Pending>,
    heartbeats: Limiter,
    queries: Limiter,
}

impl Master {
    pub fn new(now: Instant) -> Self {
        Master {
            challenger: Challenger::new(),
            started: now,
            listed: HashMap::new(),
            pending: HashMap::new(),
            // A server sends one a minute: every server of a host at once (a restart), then slowly.
            heartbeats: Limiter::new(HEARTBEAT_BURST, 1.0 / 10.0, 200.0, 50.0, now),
            queries: Limiter::new(4.0, 1.0 / 2.0, 500.0, 200.0, now),
        }
    }

    /// Servers on the list.
    pub fn len(&self) -> usize {
        self.listed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.listed.is_empty()
    }

    fn secs(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_secs()
    }

    /// Servers of `protocol`, at most [`MAX_REPLY`].
    pub fn servers(&self, protocol: u32) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self
            .listed
            .iter()
            .filter(|(_, l)| l.protocol == protocol)
            .map(|(a, _)| *a)
            .collect();
        v.sort();
        v.truncate(MAX_REPLY);
        v
    }

    /// Servers from `ip` listed or being verified.
    fn count_ip(&self, ip: IpAddr) -> usize {
        let listed = self.listed.keys().filter(|a| a.ip() == ip).count();
        let new = self
            .pending
            .keys()
            .filter(|a| a.ip() == ip && !self.listed.contains_key(a))
            .count();
        listed + new
    }

    /// One packet from `from`; the packets to send back.
    pub fn handle(&mut self, from: SocketAddr, oob: Oob, now: Instant) -> Vec<(SocketAddr, Oob)> {
        match oob {
            Oob::Heartbeat(_) => self.probe(from, false, now),
            Oob::Flatline if self.listed.contains_key(&from) => self.probe(from, true, now),
            Oob::InfoResponse(kv) => {
                self.verify(from, &kv, now);
                Vec::new()
            }
            Oob::GetChallenge if self.queries.allow(from.ip(), now) => {
                let c = self.challenger.issue(from, self.secs(now));
                vec![(from, Oob::Challenge(c))]
            }
            Oob::GetServers {
                challenge,
                protocol,
            } if self.queries.allow(from.ip(), now) => {
                if !self.challenger.check(from, challenge, self.secs(now)) {
                    logging::dedup!(WARN, "bad-challenge", %from, "getservers with a bad challenge dropped");
                    return vec![(from, Oob::Error("Bad challenge.".into()))];
                }
                let all = self.servers(protocol);
                if all.is_empty() {
                    return vec![(from, Oob::Servers(Vec::new()))];
                }
                all.chunks(SERVERS_PER_PACKET)
                    .map(|c| (from, Oob::Servers(c.to_vec())))
                    .collect()
            }
            Oob::GetChallenge | Oob::GetServers { .. } => {
                logging::dedup!(WARN, "query-limited", %from, "query dropped: rate limit");
                Vec::new()
            }
            Oob::Flatline => {
                logging::dedup!(WARN, "flatline-unlisted", %from, "flatline from an unlisted address dropped");
                Vec::new()
            }
            _ => {
                logging::dedup!(WARN, "unexpected", %from, "unexpected packet for a master dropped");
                Vec::new()
            }
        }
    }

    /// Sends the challenge that proves `from` owns its address.
    fn probe(&mut self, from: SocketAddr, flatline: bool, now: Instant) -> Vec<(SocketAddr, Oob)> {
        if !self.heartbeats.allow(from.ip(), now) {
            logging::dedup!(WARN, "heartbeat-limited", %from, "heartbeat dropped: rate limit");
            return Vec::new();
        }
        let known = self.listed.contains_key(&from);
        if !known
            && (self.listed.len() >= MAX_SERVERS
                || self.pending.len() >= MAX_PENDING && !self.pending.contains_key(&from)
                || self.count_ip(from.ip()) >= MAX_PER_IP && !self.pending.contains_key(&from))
        {
            logging::dedup!(WARN, "heartbeat-refused", %from, listed = self.listed.len(), pending = self.pending.len(), "heartbeat dropped: list or per-address limit reached");
            return Vec::new();
        }
        // A forged heartbeat must not cancel the removal a `flatline` asked for; a server that still answers is
        // kept anyway.
        let flatline = flatline || self.pending.get(&from).is_some_and(|p| p.flatline);
        self.pending.insert(
            from,
            Pending {
                deadline: now + PROBE_TIMEOUT,
                flatline,
            },
        );
        let c = self.challenger.issue(from, self.secs(now));
        vec![(from, Oob::GetInfo(c))]
    }

    /// The answer to a `getinfo`: lists (or keeps) the server when it carries the challenge.
    fn verify(&mut self, from: SocketAddr, kv: &[(String, String)], now: Instant) {
        if !self.pending.contains_key(&from) {
            logging::dedup!(WARN, "unsolicited-info", %from, "getinfo answer from an address never probed dropped");
            return;
        }
        let get = |k: &str| kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        let proven = get("challenge")
            .and_then(|v| v.parse().ok())
            .is_some_and(|c| self.challenger.check(from, c, self.secs(now)));
        if !proven {
            logging::dedup!(WARN, "bad-info-challenge", %from, "getinfo answer without the challenge dropped");
            return;
        }
        self.pending.remove(&from);
        // A server that is no longer public (`sv_public 0`) says so in its answer: it is taken off the list.
        if get("public") == Some("0") {
            self.listed.remove(&from);
            return;
        }
        let Some(protocol) = get("protocol").and_then(|v| v.parse().ok()) else {
            logging::dedup!(WARN, "no-protocol", %from, "getinfo answer without a protocol dropped");
            return;
        };
        self.listed.insert(
            from,
            Listed {
                protocol,
                verified: now,
            },
        );
    }

    /// Forgets servers that stopped answering; call every so often.
    pub fn expire(&mut self, now: Instant) {
        let dead: Vec<SocketAddr> = self
            .pending
            .iter()
            .filter(|(_, p)| now >= p.deadline && p.flatline)
            .map(|(a, _)| *a)
            .collect();
        for a in dead {
            self.listed.remove(&a);
        }
        self.pending.retain(|_, p| now < p.deadline);
        self.listed
            .retain(|_, l| now.saturating_duration_since(l.verified) < TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::oob::PROTOCOL;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    /// Heartbeat from `a` and the answer a real server there gives.
    fn list(m: &mut Master, a: SocketAddr, now: Instant) {
        let out = m.handle(a, Oob::Heartbeat(PROTOCOL), now);
        let Some((to, Oob::GetInfo(c))) = out.into_iter().next() else {
            panic!("no getinfo for {a}");
        };
        assert_eq!(to, a);
        m.handle(a, answer(c, PROTOCOL), now);
    }

    /// What a server's `getinfo` answer to challenge `c` looks like.
    fn answer(c: u32, protocol: u32) -> Oob {
        Oob::InfoResponse(vec![
            ("protocol".into(), protocol.to_string()),
            ("challenge".into(), c.to_string()),
        ])
    }

    /// What a client at `c` is told by `getchallenge` then `getservers`.
    fn ask(m: &mut Master, c: SocketAddr, protocol: u32, now: Instant) -> Vec<SocketAddr> {
        let Some((_, Oob::Challenge(ch))) = m.handle(c, Oob::GetChallenge, now).pop() else {
            panic!("no challenge");
        };
        m.handle(
            c,
            Oob::GetServers {
                challenge: ch,
                protocol,
            },
            now,
        )
        .into_iter()
        .flat_map(|(_, o)| match o {
            Oob::Servers(l) => l,
            _ => Vec::new(),
        })
        .collect()
    }

    #[test]
    fn a_server_is_listed_only_after_it_answers_the_challenge() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let s = addr("203.0.113.5:28960");
        let out = m.handle(s, Oob::Heartbeat(PROTOCOL), t);
        assert!(matches!(out[..], [(a, Oob::GetInfo(_))] if a == s));
        assert!(m.is_empty());
        // A reply with the wrong challenge, or from an address that was never asked, lists nothing.
        m.handle(
            s,
            Oob::InfoResponse(vec![("challenge".into(), "1".into())]),
            t,
        );
        let Oob::GetInfo(c) = out[0].1 else { panic!() };
        let other = addr("203.0.113.6:28960");
        m.handle(other, answer(c, PROTOCOL), t);
        assert!(m.is_empty());
        m.handle(s, answer(c, PROTOCOL), t);
        assert_eq!(m.servers(PROTOCOL), vec![s]);
    }

    #[test]
    fn a_client_gets_the_servers_of_its_protocol_and_needs_the_challenge() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let (a, b) = (addr("203.0.113.5:28960"), addr("198.51.100.7:28961"));
        list(&mut m, a, t);
        list(&mut m, b, t);
        let client = addr("192.0.2.9:40000");
        let mut got = ask(&mut m, client, PROTOCOL, t);
        got.sort();
        assert_eq!(got, vec![b, a]);
        assert_eq!(
            ask(&mut m, client, PROTOCOL + 1, t + Duration::from_secs(60)),
            vec![]
        );
        // A forged `getservers` without a challenge from that address is refused.
        let forged = m.handle(
            client,
            Oob::GetServers {
                challenge: 1,
                protocol: PROTOCOL,
            },
            t + Duration::from_secs(120),
        );
        assert!(matches!(forged[..], [(_, Oob::Error(_))]));
    }

    #[test]
    fn a_long_list_is_split_into_packets() {
        let mut t = Instant::now();
        let mut m = Master::new(t);
        for i in 0..(SERVERS_PER_PACKET * 2 + 5) {
            // Each from its own address: the per-address limits are not what this is about.
            let a = SocketAddr::from(([203, 0, (i / 250) as u8, (i % 250) as u8 + 1], 28960));
            list(&mut m, a, t);
            t += Duration::from_millis(1);
        }
        let client = addr("192.0.2.9:40000");
        let Some((_, Oob::Challenge(ch))) = m.handle(client, Oob::GetChallenge, t).pop() else {
            panic!()
        };
        let out = m.handle(
            client,
            Oob::GetServers {
                challenge: ch,
                protocol: PROTOCOL,
            },
            t,
        );
        assert_eq!(out.len(), 3);
        assert!(
            out.iter()
                .all(|(_, o)| matches!(o, Oob::Servers(l) if l.len() <= SERVERS_PER_PACKET))
        );
    }

    #[test]
    fn silent_servers_expire_and_a_verified_heartbeat_renews() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let (a, b) = (addr("203.0.113.5:28960"), addr("198.51.100.7:28960"));
        list(&mut m, a, t);
        list(&mut m, b, t);
        let later = t + Duration::from_secs(HEARTBEAT_SECS * 2);
        list(&mut m, b, later);
        m.expire(t + TTL + Duration::from_secs(1));
        assert_eq!(m.servers(PROTOCOL), vec![b]);
    }

    #[test]
    fn a_flatline_drops_a_server_that_stops_answering_but_not_one_that_still_does() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let (a, b) = (addr("203.0.113.5:28960"), addr("198.51.100.7:28960"));
        list(&mut m, a, t);
        list(&mut m, b, t);
        // `a` says goodbye and goes silent; `b`'s address is forged into a flatline but `b` still answers.
        let pa = m.handle(a, Oob::Flatline, t);
        let pb = m.handle(b, Oob::Flatline, t);
        let Oob::GetInfo(c) = pb[0].1 else { panic!() };
        assert!(matches!(pa[..], [(_, Oob::GetInfo(_))]));
        m.handle(b, answer(c, PROTOCOL), t);
        m.expire(t + PROBE_TIMEOUT + Duration::from_secs(1));
        assert_eq!(m.servers(PROTOCOL), vec![b]);
        // Nobody else can flatline an address that is not listed.
        assert!(m.handle(addr("192.0.2.77:1"), Oob::Flatline, t).is_empty());
    }

    #[test]
    fn heartbeats_are_rate_limited_per_address() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let s = addr("203.0.113.5:28960");
        let sent = (0..20)
            .filter(|_| !m.handle(s, Oob::Heartbeat(PROTOCOL), t).is_empty())
            .count();
        assert_eq!(sent, HEARTBEAT_BURST as usize);
        // Another address is not affected, and the first recovers with time.
        assert!(
            !m.handle(addr("203.0.113.6:28960"), Oob::Heartbeat(PROTOCOL), t)
                .is_empty()
        );
        assert!(
            !m.handle(s, Oob::Heartbeat(PROTOCOL), t + Duration::from_secs(15))
                .is_empty()
        );
    }

    #[test]
    fn one_address_cannot_list_more_than_its_share() {
        let mut t = Instant::now();
        let mut m = Master::new(t);
        for port in 0..(MAX_PER_IP as u16 + 4) {
            // Spread over time: only the cap on servers per address is tested.
            t += Duration::from_secs(30);
            let a = SocketAddr::from(([203, 0, 113, 5], 28960 + port));
            if usize::from(port) < MAX_PER_IP {
                list(&mut m, a, t);
            } else {
                assert!(m.handle(a, Oob::Heartbeat(PROTOCOL), t).is_empty());
            }
        }
        assert_eq!(m.len(), MAX_PER_IP);
        list(&mut m, addr("203.0.113.6:28960"), t);
        assert_eq!(m.len(), MAX_PER_IP + 1);
    }

    #[test]
    fn challenge_requests_are_rate_limited() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let c = addr("192.0.2.9:40000");
        let answered = (0..20)
            .filter(|_| !m.handle(c, Oob::GetChallenge, t).is_empty())
            .count();
        assert!(answered < 20 && answered > 0);
    }

    #[test]
    fn a_forged_heartbeat_cannot_relabel_a_real_server() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let s = addr("203.0.113.5:28960");
        list(&mut m, s, t);
        // Someone claims protocol 0 for the real server's address; the real server then answers the challenge.
        let later = t + Duration::from_secs(HEARTBEAT_SECS);
        let out = m.handle(s, Oob::Heartbeat(0), later);
        let Oob::GetInfo(c) = out[0].1 else { panic!() };
        m.handle(s, answer(c, PROTOCOL), later);
        assert_eq!(m.servers(PROTOCOL), vec![s]);
        assert!(m.servers(0).is_empty());
        // An answer that does not name a protocol lists nothing.
        let other = addr("203.0.113.6:28960");
        let out = m.handle(other, Oob::Heartbeat(PROTOCOL), later);
        let Oob::GetInfo(c) = out[0].1 else { panic!() };
        m.handle(
            other,
            Oob::InfoResponse(vec![("challenge".into(), c.to_string())]),
            later,
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn a_server_that_says_it_is_not_public_any_more_is_taken_off() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let s = addr("203.0.113.5:28960");
        list(&mut m, s, t);
        let out = m.handle(s, Oob::Flatline, t);
        let Oob::GetInfo(c) = out[0].1 else { panic!() };
        m.handle(
            s,
            Oob::InfoResponse(vec![
                ("protocol".into(), PROTOCOL.to_string()),
                ("public".into(), "0".into()),
                ("challenge".into(), c.to_string()),
            ]),
            t,
        );
        assert!(m.is_empty());
    }

    #[test]
    fn a_forged_heartbeat_does_not_cancel_a_flatline() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let s = addr("203.0.113.5:28960");
        list(&mut m, s, t);
        m.handle(s, Oob::Flatline, t);
        m.handle(s, Oob::Heartbeat(PROTOCOL), t);
        m.expire(t + PROBE_TIMEOUT + Duration::from_secs(1));
        assert!(m.is_empty());
    }

    /// An address for server number `i`, one server per address.
    fn nth(i: usize) -> SocketAddr {
        SocketAddr::from(([10 + (i >> 16) as u8, (i >> 8) as u8, i as u8, 1], 28960))
    }

    #[test]
    fn no_more_than_max_servers_are_listed() {
        let mut t = Instant::now();
        let mut m = Master::new(t);
        for i in 0..MAX_SERVERS {
            // Slow enough for the global heartbeat limit.
            t += Duration::from_millis(25);
            list(&mut m, nth(i), t);
        }
        assert_eq!(m.len(), MAX_SERVERS);
        t += Duration::from_millis(25);
        assert!(
            m.handle(nth(MAX_SERVERS), Oob::Heartbeat(PROTOCOL), t)
                .is_empty()
        );
        // A listed server can still renew.
        t += Duration::from_secs(1);
        assert!(!m.handle(nth(0), Oob::Heartbeat(PROTOCOL), t).is_empty());
    }

    #[test]
    fn no_more_than_max_pending_heartbeats_are_waiting_for_an_answer() {
        let mut t = Instant::now();
        let mut m = Master::new(t);
        for i in 0..MAX_PENDING {
            t += Duration::from_millis(25);
            assert!(!m.handle(nth(i), Oob::Heartbeat(PROTOCOL), t).is_empty());
        }
        t += Duration::from_millis(25);
        assert!(
            m.handle(nth(MAX_PENDING), Oob::Heartbeat(PROTOCOL), t)
                .is_empty()
        );
        // One that is already waiting is probed again, and room is made as the others time out.
        t += Duration::from_millis(25);
        assert!(!m.handle(nth(0), Oob::Heartbeat(PROTOCOL), t).is_empty());
        t += PROBE_TIMEOUT * 2;
        m.expire(t);
        assert!(
            !m.handle(nth(MAX_PENDING), Oob::Heartbeat(PROTOCOL), t)
                .is_empty()
        );
    }

    #[test]
    fn a_limiter_tracks_a_bounded_number_of_addresses() {
        let t = Instant::now();
        let mut l = Limiter::new(1.0, 0.1, 1e9, 1e9, t);
        for i in 0..MAX_TRACKED_IPS {
            assert!(l.allow(nth(i).ip(), t));
        }
        assert_eq!(l.per_ip.len(), MAX_TRACKED_IPS);
        // Full of addresses that are not idle: a new one is refused instead of growing the table.
        assert!(!l.allow(nth(MAX_TRACKED_IPS).ip(), t));
        assert_eq!(l.per_ip.len(), MAX_TRACKED_IPS);
        // Once they have refilled (idle) they are forgotten and room is made.
        assert!(l.allow(nth(MAX_TRACKED_IPS).ip(), t + Duration::from_secs(11)));
        assert!(l.per_ip.len() < MAX_TRACKED_IPS);
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn dropped_packets_are_warned() {
        let t = Instant::now();
        let mut m = Master::new(t);
        let s: SocketAddr = "203.0.113.9:28960".parse().unwrap();
        let (out, events) = logging::capture(|| m.handle(s, Oob::Disconnect, t));
        assert!(out.is_empty());
        assert!(events.iter().any(|e| e.level == tracing::Level::WARN));
        // An answer to a getinfo that was never sent is dropped too.
        let reply = Oob::InfoResponse(vec![("challenge".into(), "1".into())]);
        let (_, events) = logging::capture(|| m.handle(s, reply, t));
        assert!(events.iter().any(|e| e.level == tracing::Level::WARN));
    }
}
