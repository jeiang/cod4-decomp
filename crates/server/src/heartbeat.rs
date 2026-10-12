// SPDX-License-Identifier: GPL-3.0-only
//! Telling the master servers this server exists (`SV_MasterHeartbeat`): a public server sends `heartbeat` to each
//! of `sv_master1`..`sv_master5` every [`HEARTBEAT_SECS`] and a `flatline` when it stops being public or shuts down.
//! The master then asks this server for its `getinfo` (the game socket already answers it) to check the address.

use net::master::{HEARTBEAT_SECS, MASTER_PORT};
use net::oob::{Oob, PROTOCOL};
use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

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

/// How long a resolved master name is kept before it is looked up again, and how soon a failed lookup is retried.
const RESOLVE_TTL: Duration = Duration::from_secs(600);
const RETRY_TTL: Duration = Duration::from_secs(15);

/// A name lookup: blocking, so [`Heartbeats`] runs it on a worker thread and never on the server's frame.
pub type Lookup = fn(&str) -> Option<SocketAddr>;

pub struct Heartbeats {
    /// Masters told about this server, and so owed a `flatline`.
    announced: Vec<SocketAddr>,
    next: Option<Instant>,
    /// Looked-up masters: the address (the last good one is kept while a newer lookup runs) and when it was learned.
    names: HashMap<String, (Option<SocketAddr>, Instant)>,
    /// Names being looked up.
    looking: HashSet<String>,
    done: Receiver<(String, Option<SocketAddr>)>,
    started: Sender<(String, Option<SocketAddr>)>,
}

impl Default for Heartbeats {
    fn default() -> Self {
        let (started, done) = mpsc::channel();
        Heartbeats {
            announced: Vec::new(),
            next: None,
            names: HashMap::new(),
            looking: HashSet::new(),
            done,
            started,
        }
    }
}

impl Heartbeats {
    /// What to send now. `masters` are the `sv_master` values, looked up with `lookup` in the background (nothing
    /// is sent to a master until its lookup has finished); while not `public` nothing is announced and masters that
    /// were told get a `flatline`.
    pub fn step(
        &mut self,
        now: Instant,
        public: bool,
        masters: &[String],
        lookup: Lookup,
    ) -> Vec<(SocketAddr, Oob)> {
        while let Ok((name, found)) = self.done.try_recv() {
            self.looking.remove(&name);
            self.names.insert(name, (found, now));
        }
        let mut targets = Vec::new();
        if public {
            for m in masters {
                let m = m.trim();
                if m.is_empty() {
                    continue;
                }
                let fresh = self.names.get(m).is_some_and(|(found, at)| {
                    let ttl = if found.is_some() {
                        RESOLVE_TTL
                    } else {
                        RETRY_TTL
                    };
                    now.saturating_duration_since(*at) < ttl
                });
                if !fresh && self.looking.insert(m.to_owned()) {
                    let (name, tx) = (m.to_owned(), self.started.clone());
                    std::thread::spawn(move || {
                        let found = lookup(&name);
                        let _ = tx.send((name, found));
                    });
                }
                if let Some((Some(a), _)) = self.names.get(m)
                    && !targets.contains(a)
                {
                    targets.push(*a);
                }
            }
        }
        // Names no longer set are forgotten.
        self.names
            .retain(|k, _| masters.iter().any(|m| m.trim() == k));
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
        if due && !targets.is_empty() {
            self.next = Some(now + Duration::from_secs(HEARTBEAT_SECS));
        }
        self.announced = targets;
        out
    }

    /// Whether any lookup is still running (tests wait for them).
    #[cfg(test)]
    fn settle(&mut self) {
        while !self.looking.is_empty() {
            let (name, found) = self.done.recv().unwrap();
            self.looking.remove(&name);
            self.names.insert(name, (found, Instant::now()));
        }
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

    /// A step, then the lookups it started finishing, then a step that can use them.
    fn step(h: &mut Heartbeats, t: Instant, public: bool, m: &[String]) -> Vec<(SocketAddr, Oob)> {
        let first = h.step(t, public, m, fixed);
        h.settle();
        let mut out = first;
        out.extend(h.step(t, public, m, fixed));
        out
    }

    #[test]
    fn a_public_server_beats_each_master_once_a_minute() {
        let mut h = Heartbeats::default();
        let t = Instant::now();
        let m = names(&["a", "", "b", "a", "unresolvable"]);
        // The first step only starts the lookups: nothing is sent until they are in.
        assert!(h.step(t, true, &m, fixed).is_empty());
        h.settle();
        let out = h.step(t, true, &m, fixed);
        assert_eq!(
            out,
            vec![
                (addr("192.0.2.1:28950"), Oob::Heartbeat(PROTOCOL)),
                (addr("192.0.2.2:28950"), Oob::Heartbeat(PROTOCOL)),
            ]
        );
        assert!(
            h.step(t + Duration::from_secs(1), true, &m, fixed)
                .is_empty()
        );
        let later = t + Duration::from_secs(HEARTBEAT_SECS);
        assert_eq!(h.step(later, true, &m, fixed).len(), 2);
    }

    #[test]
    fn a_failed_lookup_is_tried_again_soon_and_a_good_one_is_kept() {
        fn flaky(text: &str) -> Option<SocketAddr> {
            use std::sync::atomic::{AtomicBool, Ordering};
            static UP: AtomicBool = AtomicBool::new(false);
            // The first lookup fails, the later ones work.
            UP.swap(true, Ordering::SeqCst)
                .then(|| fixed(text))
                .flatten()
        }
        let mut h = Heartbeats::default();
        let t = Instant::now();
        let m = names(&["a"]);
        h.step(t, true, &m, flaky);
        h.settle();
        assert!(h.step(t, true, &m, flaky).is_empty());
        // Not asked again at once, but well before the time a good answer is kept.
        h.step(t + Duration::from_secs(1), true, &m, flaky);
        assert!(h.looking.is_empty());
        let retry = t + RETRY_TTL + Duration::from_secs(1);
        h.step(retry, true, &m, flaky);
        h.settle();
        assert_eq!(h.step(retry, true, &m, flaky).len(), 1);
    }

    #[test]
    fn a_server_that_is_not_public_says_nothing_and_a_public_one_stopping_says_flatline() {
        let mut h = Heartbeats::default();
        let t = Instant::now();
        assert!(step(&mut h, t, false, &names(&["a"])).is_empty());
        step(&mut h, t, true, &names(&["a", "b"]));
        // `b` was dropped from the settings: it is told, `a` is not.
        let out = h.step(t + Duration::from_secs(1), true, &names(&["a"]), fixed);
        assert_eq!(out, vec![(addr("192.0.2.2:28950"), Oob::Flatline)]);
        // Going private flatlines the rest.
        let out = h.step(t + Duration::from_secs(2), false, &names(&["a"]), fixed);
        assert_eq!(out, vec![(addr("192.0.2.1:28950"), Oob::Flatline)]);
        step(&mut h, t + Duration::from_secs(3), true, &names(&["b"]));
        assert_eq!(h.finish(), vec![(addr("192.0.2.2:28950"), Oob::Flatline)]);
        assert!(h.finish().is_empty());
    }

    #[test]
    fn a_master_added_later_is_told_at_once() {
        let mut h = Heartbeats::default();
        let t = Instant::now();
        step(&mut h, t, true, &names(&["a"]));
        let out = step(
            &mut h,
            t + Duration::from_secs(1),
            true,
            &names(&["a", "b"]),
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
