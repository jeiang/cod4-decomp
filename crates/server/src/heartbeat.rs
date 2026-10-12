// SPDX-License-Identifier: GPL-3.0-only
//! Telling the master servers this server exists (`SV_MasterHeartbeat`): a public server sends `heartbeat` to each
//! of `sv_master1`..`sv_master5` every [`HEARTBEAT_SECS`] and a `flatline` when it stops being public or shuts down.
//! The master then asks this server for its `getinfo` (the game socket already answers it) to check the address.

use net::master::{HEARTBEAT_SECS, MASTER_PORT};
use net::oob::{Oob, PROTOCOL};
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

/// How long a resolved master name is kept before it is looked up again.
const RESOLVE_TTL: Duration = Duration::from_secs(600);

/// `host[:port]` as the IPv4 address of a master (the port defaults to [`MASTER_PORT`]).
pub fn resolve_name(text: &str) -> Option<SocketAddr> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let with_port = if t
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        t.to_owned()
    } else {
        format!("{t}:{MASTER_PORT}")
    };
    with_port.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
}

#[derive(Default)]
pub struct Heartbeats {
    /// Masters told about this server, and so owed a `flatline`.
    announced: Vec<SocketAddr>,
    next: Option<Instant>,
    names: HashMap<String, (Option<SocketAddr>, Instant)>,
}

impl Heartbeats {
    /// What to send now. `masters` are the `sv_master` values; while not `public` nothing is announced and masters
    /// that were told get a `flatline`.
    pub fn step(
        &mut self,
        now: Instant,
        public: bool,
        masters: &[String],
        lookup: &mut dyn FnMut(&str) -> Option<SocketAddr>,
    ) -> Vec<(SocketAddr, Oob)> {
        let mut targets = Vec::new();
        if public {
            for m in masters {
                let m = m.trim();
                if m.is_empty() {
                    continue;
                }
                let stale = self
                    .names
                    .get(m)
                    .is_none_or(|(_, at)| now.saturating_duration_since(*at) >= RESOLVE_TTL);
                if stale {
                    self.names.insert(m.to_owned(), (lookup(m), now));
                }
                if let Some(a) = self.names[m].0
                    && !targets.contains(&a)
                {
                    targets.push(a);
                }
            }
        }
        let mut out: Vec<_> = self
            .announced
            .iter()
            .filter(|a| !targets.contains(a))
            .map(|a| (*a, Oob::Flatline))
            .collect();
        let due = self.next.is_none_or(|n| now >= n);
        for a in &targets {
            if due || !self.announced.contains(a) {
                out.push((*a, Oob::Heartbeat(PROTOCOL)));
            }
        }
        if due {
            self.next = Some(now + Duration::from_secs(HEARTBEAT_SECS));
        }
        self.announced = targets;
        out
    }

    /// The `heartbeat` console command: send again at the next step.
    pub fn send_now(&mut self) {
        self.next = None;
    }

    /// The server is stopping: tell every master that was told about it.
    pub fn finish(&mut self) -> Vec<(SocketAddr, Oob)> {
        self.next = None;
        std::mem::take(&mut self.announced)
            .into_iter()
            .map(|a| (a, Oob::Flatline))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn fixed(text: &str) -> Option<SocketAddr> {
        match text {
            "a" => Some(addr("192.0.2.1:28950")),
            "b" => Some(addr("192.0.2.2:28950")),
            _ => None,
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_public_server_beats_each_master_once_a_minute() {
        let mut h = Heartbeats::default();
        let t = Instant::now();
        let m = names(&["a", "", "b", "a", "unresolvable"]);
        let out = h.step(t, true, &m, &mut fixed);
        assert_eq!(
            out,
            vec![
                (addr("192.0.2.1:28950"), Oob::Heartbeat(PROTOCOL)),
                (addr("192.0.2.2:28950"), Oob::Heartbeat(PROTOCOL)),
            ]
        );
        assert!(
            h.step(t + Duration::from_secs(1), true, &m, &mut fixed)
                .is_empty()
        );
        let later = t + Duration::from_secs(HEARTBEAT_SECS);
        assert_eq!(h.step(later, true, &m, &mut fixed).len(), 2);
    }

    #[test]
    fn a_server_that_is_not_public_says_nothing_and_a_public_one_stopping_says_flatline() {
        let mut h = Heartbeats::default();
        let t = Instant::now();
        assert!(h.step(t, false, &names(&["a"]), &mut fixed).is_empty());
        h.step(t, true, &names(&["a", "b"]), &mut fixed);
        // `b` was dropped from the settings: it is told, `a` is not.
        let out = h.step(t + Duration::from_secs(1), true, &names(&["a"]), &mut fixed);
        assert_eq!(out, vec![(addr("192.0.2.2:28950"), Oob::Flatline)]);
        // Going private flatlines the rest.
        let out = h.step(
            t + Duration::from_secs(2),
            false,
            &names(&["a"]),
            &mut fixed,
        );
        assert_eq!(out, vec![(addr("192.0.2.1:28950"), Oob::Flatline)]);
        h.step(t + Duration::from_secs(3), true, &names(&["b"]), &mut fixed);
        assert_eq!(h.finish(), vec![(addr("192.0.2.2:28950"), Oob::Flatline)]);
        assert!(h.finish().is_empty());
    }

    #[test]
    fn a_master_added_later_is_told_at_once() {
        let mut h = Heartbeats::default();
        let t = Instant::now();
        h.step(t, true, &names(&["a"]), &mut fixed);
        let out = h.step(
            t + Duration::from_secs(1),
            true,
            &names(&["a", "b"]),
            &mut fixed,
        );
        assert_eq!(
            out,
            vec![(addr("192.0.2.2:28950"), Oob::Heartbeat(PROTOCOL))]
        );
    }

    #[test]
    fn master_addresses_take_the_default_port() {
        assert_eq!(resolve_name("127.0.0.1"), Some(addr("127.0.0.1:28950")));
        assert_eq!(resolve_name(" 127.0.0.1:9 "), Some(addr("127.0.0.1:9")));
        assert_eq!(resolve_name(""), None);
    }
}
