// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding servers: the stock join menu's list (LAN discovery and favorites) and the map query that precedes joining.
//!
//! A server answers `getinfo <challenge>` with its `hostname`, `mapname`, `gametype`, `clients` and `sv_maxclients`
//! ([`net::oob`]). The browser sends one to the LAN broadcast address and the loopback address on each of the standard
//! ports ([`PORTS`], so a client-hosted `--listen` server shows up too) and to every favorite, then collects the
//! replies without ever blocking: a reply is a [`Entry`], its ping the time since the request went out.
//! [`query`] is the blocking form for joining a typed address: it must learn the map before the client can load it.

use net::oob::Oob;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// Ports a server is looked for on: the original's default and the next three (a few servers on one host).
pub const PORTS: [u16; 4] = [28960, 28961, 28962, 28963];
/// The port assumed for an address typed without one.
pub const DEFAULT_PORT: u16 = PORTS[0];
/// Most rows a list keeps.
const MAX_ENTRIES: usize = 256;

/// One server as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub addr: SocketAddr,
    pub hostname: String,
    pub map: String,
    pub gametype: String,
    pub clients: i32,
    pub max_clients: i32,
    /// Milliseconds; 0 until the server has answered.
    pub ping: i32,
}

impl Entry {
    /// A favorite that has not answered yet.
    pub fn unanswered(addr: SocketAddr) -> Self {
        Entry {
            addr,
            hostname: addr.to_string(),
            map: String::new(),
            gametype: String::new(),
            clients: 0,
            max_clients: 0,
            ping: 0,
        }
    }

    /// From an `infoResponse`; `None` when it names no map (not a game server's answer).
    pub fn from_info(addr: SocketAddr, kv: &[(String, String)], ping_ms: i32) -> Option<Self> {
        let get = |k: &str| {
            kv.iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(k))
                .map_or("", |(_, v)| v.as_str())
        };
        let num = |k: &str| get(k).trim().parse::<i32>().unwrap_or(0);
        let map = get("mapname");
        if map.is_empty() {
            return None;
        }
        Some(Entry {
            addr,
            hostname: get("hostname").to_owned(),
            map: map.to_ascii_lowercase(),
            gametype: get("gametype").to_ascii_lowercase(),
            clients: num("clients"),
            max_clients: num("sv_maxclients"),
            ping: ping_ms.max(1),
        })
    }
}

/// `host:port`, or `host` alone for the default port. Resolves names; a browser cannot, so there the page's
/// WebTransport connects by the text and the address is a stand-in.
pub fn resolve(addr: &str) -> Result<SocketAddr, String> {
    let a = addr.trim();
    if a.is_empty() || a.starts_with(':') {
        return Err("no server address".into());
    }
    #[cfg(target_arch = "wasm32")]
    return Ok(crate::web::server_addr(a));
    #[cfg(not(target_arch = "wasm32"))]
    resolve_name(a)
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve_name(a: &str) -> Result<SocketAddr, String> {
    let with_port = if a
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        a.to_owned()
    } else {
        format!("{a}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {a}: {e}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("cannot resolve {a}: no IPv4 address"))
}

/// Loopback, private and link-local addresses: the ones a LAN scan can have reached.
fn is_lan(a: &SocketAddr) -> bool {
    match a.ip() {
        IpAddr::V4(i) => i.is_loopback() || i.is_private() || i.is_link_local(),
        IpAddr::V6(i) => i.is_loopback(),
    }
}

/// A non-blocking `getinfo` client.
pub struct Browser {
    sock: UdpSocket,
    challenge: u32,
    sent: Instant,
    buf: Vec<u8>,
}

impl Browser {
    pub fn new() -> std::io::Result<Self> {
        let sock = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
        sock.set_nonblocking(true)?;
        sock.set_broadcast(true)?;
        let challenge = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| {
                d.subsec_nanos() ^ (d.as_secs() as u32).rotate_left(16)
            })
            .max(1);
        Ok(Browser {
            sock,
            challenge,
            sent: Instant::now(),
            buf: vec![0; 2048],
        })
    }

    /// Asks `to`; failures to send (no route, no broadcast) are ignored, the server simply does not answer.
    pub fn ask(&mut self, to: SocketAddr) {
        let _ = self
            .sock
            .send_to(&Oob::GetInfo(self.challenge).encode(), to);
    }

    /// The LAN scan: broadcast and loopback on every standard port.
    pub fn ask_lan(&mut self) {
        for p in PORTS {
            self.ask(SocketAddr::from((Ipv4Addr::BROADCAST, p)));
            self.ask(SocketAddr::from((Ipv4Addr::LOCALHOST, p)));
        }
    }

    /// Starts a fresh round: replies are timed from now.
    pub fn restart(&mut self) {
        self.sent = Instant::now();
    }

    /// Every reply that has arrived (answers to an older round count; the challenge is fixed per browser).
    pub fn poll(&mut self) -> Vec<Entry> {
        let mut out = Vec::new();
        while let Ok((n, from)) = self.sock.recv_from(&mut self.buf) {
            let ping = self.sent.elapsed().as_millis() as i32;
            if let Some(Oob::InfoResponse(kv)) = Oob::parse(&self.buf[..n])
                && kv
                    .iter()
                    .any(|(k, v)| k == "challenge" && v.parse() == Ok(self.challenge))
                && let Some(e) = Entry::from_info(from, &kv, ping)
            {
                out.push(e);
            }
        }
        out
    }
}

/// Asks one server for its map and settings, waiting up to `timeout` (a request every 300 ms).
pub fn query(addr: SocketAddr, timeout: Duration) -> Result<Entry, String> {
    let mut b = Browser::new().map_err(|e| format!("cannot open a UDP socket: {e}"))?;
    let end = Instant::now() + timeout;
    let mut next = Instant::now();
    loop {
        if Instant::now() >= next {
            b.ask(addr);
            next = Instant::now() + Duration::from_millis(300);
        }
        if let Some(e) = b.poll().into_iter().find(|e| e.addr == addr) {
            return Ok(e);
        }
        if Instant::now() >= end {
            return Err(format!("{addr} did not answer"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Which stock list is shown (`ui_netSource`): 0 local network, 1 internet (no master server, so empty), 2 favorites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Lan,
    Internet,
    Favorites,
}

impl Source {
    pub fn from_dvar(v: &str) -> Self {
        match v.trim().parse::<i32>().unwrap_or(0) {
            1 => Source::Internet,
            2 => Source::Favorites,
            _ => Source::Lan,
        }
    }
}

/// The lists behind the join menu.
#[derive(Default)]
pub struct ServerList {
    browser: Option<Browser>,
    lan: Vec<Entry>,
    favorites: Vec<Entry>,
    /// Column the list is sorted by and whether it runs downwards.
    sort: Option<(usize, bool)>,
    selected: Option<SocketAddr>,
}

impl ServerList {
    /// Forgets the LAN list and scans again; favorites are asked too.
    pub fn refresh(&mut self) {
        if self.browser.is_none() {
            self.browser = Browser::new().ok();
        }
        self.lan.clear();
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        b.restart();
        b.ask_lan();
        for f in &self.favorites {
            b.ask(f.addr);
        }
    }

    /// Adds a favorite (once) and asks it.
    pub fn add_favorite(&mut self, addr: SocketAddr) {
        if self.favorites.iter().any(|f| f.addr == addr) {
            return;
        }
        self.favorites.push(Entry::unanswered(addr));
        if self.browser.is_none() {
            self.browser = Browser::new().ok();
        }
        if let Some(b) = self.browser.as_mut() {
            b.restart();
            b.ask(addr);
        }
    }

    pub fn remove_favorite(&mut self, addr: SocketAddr) {
        self.favorites.retain(|f| f.addr != addr);
    }

    /// Takes in the replies that arrived; call once per frame while the join menu is up.
    pub fn poll(&mut self) {
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        for e in b.poll() {
            self.take(e);
        }
        if self.sort.is_some() {
            self.resort();
        }
    }

    /// One reply: it refreshes a favorite of that address, and (for a LAN address) is listed on the LAN.
    fn take(&mut self, e: Entry) {
        if let Some(f) = self.favorites.iter_mut().find(|f| f.addr == e.addr) {
            *f = e.clone();
        }
        if !is_lan(&e.addr) {
            return;
        }
        // The same server answers on both its loopback and LAN address; the loopback row yields.
        let same = |o: &Entry| {
            o.addr.port() == e.addr.port() && o.hostname == e.hostname && o.map == e.map
        };
        if let Some(i) = self.lan.iter().position(|o| o.addr == e.addr || same(o)) {
            if self.lan[i].addr.ip().is_loopback() || self.lan[i].addr == e.addr {
                self.lan[i] = e;
            }
        } else if self.lan.len() < MAX_ENTRIES {
            self.lan.push(e);
        }
    }

    /// Rows of `source` as shown.
    pub fn rows(&self, source: Source) -> &[Entry] {
        match source {
            Source::Lan => &self.lan,
            Source::Internet => &[],
            Source::Favorites => &self.favorites,
        }
    }

    /// Sorts by stock column `col` (a second request for the same column reverses it).
    pub fn sort_by(&mut self, col: usize) {
        self.sort = Some(match self.sort {
            Some((c, down)) if c == col => (c, !down),
            _ => (col, false),
        });
        self.resort();
    }

    fn resort(&mut self) {
        let Some((col, down)) = self.sort else { return };
        for list in [&mut self.lan, &mut self.favorites] {
            list.sort_by(|a, b| {
                let o = match col {
                    2 => a.hostname.to_lowercase().cmp(&b.hostname.to_lowercase()),
                    3 => a.map.cmp(&b.map),
                    4 => a.clients.cmp(&b.clients),
                    5 => a.gametype.cmp(&b.gametype),
                    // Unanswered rows (ping 0) go last when ascending.
                    10 => (a.ping == 0, a.ping).cmp(&(b.ping == 0, b.ping)),
                    _ => std::cmp::Ordering::Equal,
                };
                if down { o.reverse() } else { o }
            });
        }
    }

    /// Remembers which server the player picked (by address, so a re-sort keeps it).
    pub fn select(&mut self, source: Source, row: usize) {
        self.selected = self.rows(source).get(row).map(|e| e.addr);
    }

    pub fn selected(&self) -> Option<SocketAddr> {
        self.selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn entry(a: &str, host: &str, map: &str, ping: i32) -> Entry {
        Entry {
            addr: addr(a),
            hostname: host.into(),
            map: map.into(),
            gametype: "war".into(),
            clients: 1,
            max_clients: 18,
            ping,
        }
    }

    #[test]
    fn info_reply_becomes_a_row_and_a_non_game_reply_is_dropped() {
        let e = Entry::from_info(
            addr("192.168.1.5:28960"),
            &kv(&[
                ("hostname", "Fun"),
                ("mapname", "MP_Crash"),
                ("gametype", "WAR"),
                ("clients", "7"),
                ("sv_maxclients", "18"),
            ]),
            0,
        )
        .unwrap();
        assert_eq!((e.map.as_str(), e.gametype.as_str()), ("mp_crash", "war"));
        assert_eq!((e.clients, e.max_clients), (7, 18));
        assert_eq!(e.ping, 1, "an answered server never shows ping 0");
        assert_eq!(
            Entry::from_info(addr("1.2.3.4:1"), &kv(&[("hostname", "x")]), 5),
            None
        );
    }

    #[test]
    fn typed_addresses_get_the_default_port() {
        assert_eq!(resolve("127.0.0.1").unwrap().port(), 28960);
        assert_eq!(resolve("127.0.0.1:1234").unwrap().port(), 1234);
        assert_eq!(resolve(" 10.0.0.2 ").unwrap().ip().to_string(), "10.0.0.2");
        assert!(resolve("").is_err());
        assert!(resolve(":28960").is_err());
    }

    #[test]
    fn the_same_lan_server_on_loopback_and_lan_is_listed_once() {
        let mut l = ServerList::default();
        l.take(entry("127.0.0.1:28960", "Me", "mp_crash", 1));
        l.take(entry("192.168.1.9:28960", "Me", "mp_crash", 2));
        assert_eq!(l.rows(Source::Lan).len(), 1);
        assert_eq!(l.rows(Source::Lan)[0].addr, addr("192.168.1.9:28960"));
        // A different server on the same port is another row.
        l.take(entry("192.168.1.20:28960", "Other", "mp_bog", 3));
        assert_eq!(l.rows(Source::Lan).len(), 2);
        // Public addresses never land on the LAN list.
        l.take(entry("8.8.8.8:28960", "Far", "mp_bog", 3));
        assert_eq!(l.rows(Source::Lan).len(), 2);
    }

    #[test]
    fn favorites_start_unanswered_and_fill_in_from_the_reply() {
        let mut l = ServerList::default();
        l.favorites.push(Entry::unanswered(addr("8.8.8.8:28960")));
        assert_eq!(l.rows(Source::Favorites)[0].ping, 0);
        l.take(entry("8.8.8.8:28960", "Far", "mp_bog", 40));
        let f = &l.rows(Source::Favorites)[0];
        assert_eq!(
            (f.hostname.as_str(), f.map.as_str(), f.ping),
            ("Far", "mp_bog", 40)
        );
        assert!(l.rows(Source::Internet).is_empty());
    }

    #[test]
    fn sorting_toggles_and_the_selection_follows_the_server() {
        let mut l = ServerList::default();
        l.take(entry("10.0.0.1:28960", "b", "mp_crash", 30));
        l.take(entry("10.0.0.2:28960", "a", "mp_bog", 10));
        l.select(Source::Lan, 0);
        assert_eq!(l.selected(), Some(addr("10.0.0.1:28960")));
        l.sort_by(2);
        assert_eq!(l.rows(Source::Lan)[0].hostname, "a");
        assert_eq!(l.selected(), Some(addr("10.0.0.1:28960")));
        l.sort_by(2);
        assert_eq!(l.rows(Source::Lan)[0].hostname, "b");
        l.sort_by(10);
        assert_eq!(l.rows(Source::Lan)[0].ping, 10);
    }

    #[test]
    fn source_dvar_maps_to_the_stock_lists() {
        assert_eq!(Source::from_dvar("0"), Source::Lan);
        assert_eq!(Source::from_dvar("1"), Source::Internet);
        assert_eq!(Source::from_dvar("2"), Source::Favorites);
        assert_eq!(Source::from_dvar("junk"), Source::Lan);
    }

    #[test]
    fn query_reads_a_real_servers_map_and_times_out_on_a_silent_port() {
        // A tiny stand-in server on loopback answering getinfo like the real one.
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let a = srv.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            let (n, from) = srv.recv_from(&mut buf).unwrap();
            let Some(Oob::GetInfo(c)) = Oob::parse(&buf[..n]) else {
                panic!("not a getinfo")
            };
            let reply = Oob::InfoResponse(kv(&[
                ("hostname", "T"),
                ("mapname", "mp_bog"),
                ("gametype", "dm"),
                ("challenge", &c.to_string()),
            ]));
            srv.send_to(&reply.encode(), from).unwrap();
        });
        let e = query(a, Duration::from_secs(3)).unwrap();
        assert_eq!((e.map.as_str(), e.gametype.as_str()), ("mp_bog", "dm"));
        t.join().unwrap();
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(query(silent.local_addr().unwrap(), Duration::from_millis(400)).is_err());
    }
}
