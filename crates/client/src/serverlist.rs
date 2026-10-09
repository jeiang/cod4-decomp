// SPDX-License-Identifier: GPL-3.0-only
//! Finding servers: the stock join menu's list (LAN discovery and favorites) and the map query that precedes joining.
//!
//! A server answers `getinfo <challenge>` with its `hostname`, `mapname`, `gametype`, `clients` and `sv_maxclients`
//! ([`net::oob`]). The browser sends one to the LAN broadcast address and the loopback address on each of the standard
//! ports ([`PORTS`], so a client-hosted `--listen` server shows up too) and to every favorite, then collects the
//! replies without ever blocking: a reply is a [`Entry`], its ping the time since the request went out.
//! [`query`] is the blocking form for joining a typed address: it must learn the map before the client can load it.
//! The Server Info popup asks the selected server for a `getstatus` instead: its full info string and its players
//! ([`Status`]), shown through [`status_rows`].

use net::oob::{Oob, StatusPlayer};
#[cfg(not(target_arch = "wasm32"))]
use std::net::ToSocketAddrs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Ports a server is looked for on: the original's default and the next three (a few servers on one host).
pub const PORTS: [u16; 4] = [28960, 28961, 28962, 28963];
/// The port assumed for an address typed without one.
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_PORT: u16 = PORTS[0];
/// Most rows a list keeps.
const MAX_ENTRIES: usize = 256;
/// How long a refresh waits for answers before the list counts as complete: a LAN server answers within a few
/// milliseconds, so what has not come by then is not coming.
const REFRESH_WINDOW: Duration = Duration::from_millis(1500);
/// A `getstatus` unanswered for this long is sent again (`UI_BuildServerStatus` retries every 500 ms)...
const STATUS_RETRY: Duration = Duration::from_millis(500);
/// ...this many times, then the popup stays empty.
const STATUS_TRIES: u32 = 8;
/// Most lines the Server Info list shows.
const STATUS_LINES: usize = 128;

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
    /// `pswrd`: joining needs a password.
    pub password: bool,
    /// `voice`, `pure`, `pb`: the server's voice chat, pure IWD and PunkBuster settings.
    pub voice: bool,
    pub pure: bool,
    pub punkbuster: bool,
    /// `mod`: the server runs modified content (the original's list marks the others).
    pub modded: bool,
    /// `hw`: what the server runs on (the original's hardware icon).
    pub hardware: i32,
    /// `ff` and `kc`: `scr_team_fftype` and `scr_game_allowkillcam`.
    pub friendly_fire: i32,
    pub killcam: bool,
    /// `minPing` and `maxPing` the server lets in (0: no limit).
    pub min_ping: i32,
    pub max_ping: i32,
    /// `game`: the server's `fs_game` directory, empty for stock.
    pub game: String,
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
            password: false,
            voice: false,
            pure: false,
            punkbuster: false,
            modded: false,
            hardware: 0,
            friendly_fire: 0,
            killcam: false,
            min_ping: 0,
            max_ping: 0,
            game: String::new(),
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
            password: num("pswrd") != 0,
            voice: num("voice") != 0,
            pure: num("pure") != 0,
            punkbuster: num("pb") != 0,
            modded: num("mod") != 0,
            hardware: num("hw"),
            friendly_fire: num("ff"),
            killcam: num("kc") != 0,
            min_ping: num("minPing"),
            max_ping: num("maxPing"),
            game: get("game").to_owned(),
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

/// What a server said about itself to a `getstatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub addr: SocketAddr,
    pub info: Vec<(String, String)>,
    pub players: Vec<StatusPlayer>,
}

/// A reply the browser's socket received.
pub enum Reply {
    Info(Entry),
    Status(Status),
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

    /// Asks `to` for its status.
    pub fn ask_status(&mut self, to: SocketAddr) {
        let _ = self
            .sock
            .send_to(&Oob::GetStatus(self.challenge).encode(), to);
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
    pub fn poll(&mut self) -> Vec<Reply> {
        let mut out = Vec::new();
        while let Ok((n, from)) = self.sock.recv_from(&mut self.buf) {
            let ping = self.sent.elapsed().as_millis() as i32;
            let mine = |kv: &[(String, String)]| {
                kv.iter()
                    .any(|(k, v)| k == "challenge" && v.parse() == Ok(self.challenge))
            };
            match Oob::parse(&self.buf[..n]) {
                Some(Oob::InfoResponse(kv)) if mine(&kv) => {
                    out.extend(Entry::from_info(from, &kv, ping).map(Reply::Info));
                }
                Some(Oob::StatusResponse { info, players }) if mine(&info) => {
                    out.push(Reply::Status(Status {
                        addr: from,
                        info,
                        players,
                    }));
                }
                _ => {}
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
        let found = b.poll().into_iter().find_map(|r| match r {
            Reply::Info(e) if e.addr == addr => Some(e),
            _ => None,
        });
        if let Some(e) = found {
            return Ok(e);
        }
        if Instant::now() >= end {
            return Err(format!("{addr} did not answer"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How a Server Info value is shown.
enum Shown {
    Text,
    YesNo,
    Gametype,
    Map,
}

/// The info keys the Server Info popup lists first and in this order, each with its label (`serverStatusDvars`).
const STATUS_KEYS: [(&str, &str, Shown); 25] = [
    ("sv_hostname", "@EXE_SV_INFO_SERVERNAME", Shown::Text),
    ("address", "@EXE_SV_INFO_ADDRESS", Shown::Text),
    ("pswrd", "@EXE_SV_INFO_PASSWORD", Shown::YesNo),
    ("gamename", "@EXE_SV_INFO_GAMENAME", Shown::Text),
    ("g_gametype", "@EXE_SV_INFO_GAMETYPE", Shown::Gametype),
    ("sv_pure", "@EXE_SV_INFO_PURE", Shown::YesNo),
    ("mapname", "@EXE_SV_INFO_MAP", Shown::Map),
    ("shortversion", "@EXE_SV_INFO_VERSION", Shown::Text),
    ("protocol", "@EXE_SV_INFO_PROTOCOL", Shown::Text),
    ("sv_maxping", "@EXE_SV_INFO_MAXPING", Shown::Text),
    ("sv_minping", "@EXE_SV_INFO_MINPING", Shown::Text),
    ("sv_maxrate", "@EXE_SV_INFO_MAXRATE", Shown::Text),
    ("sv_floodprotect", "@EXE_SV_INFO_FLOODPROTECT", Shown::YesNo),
    ("sv_allowanonymous", "@EXE_SV_INFO_ALLOWANON", Shown::Text),
    ("sv_maxclients", "@EXE_SV_INFO_MAXCLIENTS", Shown::Text),
    (
        "sv_privateclients",
        "@EXE_SV_INFO_PRIVATECLIENTS",
        Shown::Text,
    ),
    (
        "scr_friendlyFire",
        "@EXE_SV_INFO_FRIENDLY_FIRE",
        Shown::Text,
    ),
    ("fs_game", "@EXE_SV_INFO_MOD", Shown::Text),
    ("mod", "@MENU_MODS", Shown::YesNo),
    ("scr_killcam", "@EXE_SV_INFO_KILLCAM", Shown::YesNo),
    ("g_antilag", "@EXE_SV_INFO_ANTILAG", Shown::YesNo),
    (
        "g_compassShowEnemies",
        "@EXE_SV_INFO_COMPASS_ENEMIES",
        Shown::YesNo,
    ),
    ("sv_voice", "@EXE_SV_INFO_VOICE", Shown::YesNo),
    ("sv_punkbuster", "@MPUI_PUNKBUSTER", Shown::YesNo),
    (
        "sv_disableClientConsole",
        "@EXE_SV_INFO_CLIENT_CONSOLE",
        Shown::YesNo,
    ),
];

/// The Server Info list (feeder 13), four text columns a line (`UI_GetServerStatusInfo`): the address, the known
/// settings by their labels in a fixed order, the other settings by their raw names, a blank line and a header, then
/// the players as number, score, ping and name. A text starting with `@` is a localize key. `gametype` and `map` turn
/// an id into the name to show.
pub fn status_rows(
    s: &Status,
    gametype: &dyn Fn(&str) -> String,
    map: &dyn Fn(&str) -> String,
) -> Vec<[String; 4]> {
    let addr = s.addr.to_string();
    let find = |k: &str| {
        if k == "address" {
            return Some(addr.as_str());
        }
        s.info
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    };
    let line = |k: &str, v: String| [k.to_owned(), String::new(), String::new(), v];
    let mut rows = Vec::new();
    for (key, label, shown) in &STATUS_KEYS {
        let Some(v) = find(key) else { continue };
        let v = match shown {
            Shown::Text => v.to_owned(),
            Shown::YesNo if v.trim().parse::<i32>().unwrap_or(0) != 0 => "@EXE_YES".into(),
            Shown::YesNo => "@EXE_NO".into(),
            Shown::Gametype => gametype(v),
            Shown::Map => map(v),
        };
        rows.push(line(label, v));
    }
    for (k, v) in &s.info {
        let known = STATUS_KEYS.iter().any(|(n, ..)| n.eq_ignore_ascii_case(k));
        if !known && k != "challenge" {
            rows.push(line(k, v.clone()));
        }
    }
    rows.push(Default::default());
    rows.push(
        [
            "@EXE_SV_INFO_NUM",
            "@EXE_SV_INFO_SCORE",
            "@EXE_SV_INFO_PING",
            "@EXE_SV_INFO_NAME",
        ]
        .map(str::to_owned),
    );
    for (i, p) in s.players.iter().enumerate() {
        rows.push([
            i.to_string(),
            p.score.to_string(),
            p.ping.to_string(),
            p.name.clone(),
        ]);
    }
    rows.truncate(STATUS_LINES);
    rows
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
    /// When the running refresh started; it counts as running for [`REFRESH_WINDOW`].
    refresh_started: Option<Instant>,
    /// The server a `getstatus` is out to, when it was last sent and how often.
    status_req: Option<(SocketAddr, Instant, u32)>,
    /// An answer that arrived and has not been taken yet.
    status: Option<Status>,
}

impl ServerList {
    /// Scans again, favorites included; `full` first forgets the LAN list, otherwise the answers update it in place.
    pub fn refresh(&mut self, full: bool) {
        if self.browser.is_none() {
            self.browser = Browser::new().ok();
        }
        if full {
            self.lan.clear();
        }
        self.refresh_started = Some(Instant::now());
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        b.restart();
        b.ask_lan();
        for f in &self.favorites {
            b.ask(f.addr);
        }
    }

    /// Whether a refresh is still collecting answers (the join menu then shows how many servers it has).
    pub fn refreshing(&self) -> bool {
        self.refresh_started
            .is_some_and(|t| t.elapsed() < REFRESH_WINDOW)
    }

    /// Ends the running refresh (`StopRefresh`); answers that come later still land in the lists.
    pub fn stop_refresh(&mut self) {
        self.refresh_started = None;
    }

    /// Asks `addr` for its status, forgetting the last answer; [`ServerList::poll`] keeps asking until it replies.
    pub fn request_status(&mut self, addr: SocketAddr) {
        self.status = None;
        self.status_req = None;
        if self.browser.is_none() {
            self.browser = Browser::new().ok();
        }
        if let Some(b) = self.browser.as_mut() {
            b.ask_status(addr);
            self.status_req = Some((addr, Instant::now(), 1));
        }
    }

    /// The status answer that arrived since the last call.
    pub fn take_status(&mut self) -> Option<Status> {
        self.status.take()
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
        for r in b.poll() {
            match r {
                Reply::Info(e) => self.take(e),
                Reply::Status(st) => {
                    if self.status_req.is_some_and(|(a, ..)| a == st.addr) {
                        self.status_req = None;
                        self.status = Some(st);
                    }
                }
            }
        }
        if let Some((addr, sent, tries)) = self.status_req.as_mut()
            && sent.elapsed() >= STATUS_RETRY
        {
            if *tries >= STATUS_TRIES {
                self.status_req = None;
            } else if let Some(b) = self.browser.as_mut() {
                b.ask_status(*addr);
                *sent = Instant::now();
                *tries += 1;
            }
        }
        if self.sort.is_some() {
            self.resort();
        }
    }

    /// One reply: it refreshes a favorite of that address, and (for a LAN address) is listed on the LAN.
    pub(crate) fn take(&mut self, e: Entry) {
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
    pub fn select(&mut self, addr: Option<SocketAddr>) {
        self.selected = addr;
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
            ..Entry::unanswered(addr(a))
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
        l.select(Some(addr("10.0.0.1:28960")));
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

    fn status(info: &[(&str, &str)], players: Vec<StatusPlayer>) -> Status {
        Status {
            addr: addr("10.0.0.5:28960"),
            info: kv(info),
            players,
        }
    }

    #[test]
    fn server_info_lists_known_settings_first_in_the_stock_order_then_the_players() {
        let st = status(
            &[
                ("mapname", "mp_crash"),
                ("custom", "x"),
                ("pswrd", "1"),
                ("g_gametype", "war"),
                ("challenge", "77"),
                ("sv_hostname", "Fun"),
            ],
            vec![StatusPlayer {
                score: 12,
                ping: 40,
                name: "Ann".into(),
            }],
        );
        let rows = status_rows(&st, &|g| format!("GT:{g}"), &|m| format!("MAP:{m}"));
        let col = |i: usize| rows.iter().map(|r| r[i].as_str()).collect::<Vec<_>>();
        assert_eq!(
            col(0)[..6],
            [
                "@EXE_SV_INFO_SERVERNAME",
                "@EXE_SV_INFO_ADDRESS",
                "@EXE_SV_INFO_PASSWORD",
                "@EXE_SV_INFO_GAMETYPE",
                "@EXE_SV_INFO_MAP",
                "custom"
            ]
        );
        assert_eq!(
            col(3)[..6],
            [
                "Fun",
                "10.0.0.5:28960",
                "@EXE_YES",
                "GT:war",
                "MAP:mp_crash",
                "x"
            ]
        );
        // The asker's own challenge is protocol, not a setting.
        assert!(!col(0).contains(&"challenge"));
        // A blank line, the header, then the player as number, score, ping, name.
        assert_eq!(rows[6], <[String; 4]>::default());
        assert_eq!(rows[7][3], "@EXE_SV_INFO_NAME");
        assert_eq!(rows[8], ["0", "12", "40", "Ann"].map(str::to_owned));
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn a_status_query_is_answered_by_the_server_it_asked() {
        // A stand-in server: the first getstatus is ignored (lost), the second answered.
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let a = srv.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            srv.recv_from(&mut buf).unwrap();
            let (n, from) = srv.recv_from(&mut buf).unwrap();
            let Some(Oob::GetStatus(c)) = Oob::parse(&buf[..n]) else {
                panic!("not a getstatus")
            };
            let reply = Oob::StatusResponse {
                info: kv(&[("mapname", "mp_bog"), ("challenge", &c.to_string())]),
                players: vec![StatusPlayer {
                    score: 3,
                    ping: 9,
                    name: "Bob".into(),
                }],
            };
            srv.send_to(&reply.encode(), from).unwrap();
        });
        let mut l = ServerList::default();
        l.request_status(a);
        let end = Instant::now() + Duration::from_secs(5);
        let got = loop {
            l.poll();
            if let Some(s) = l.take_status() {
                break s;
            }
            assert!(Instant::now() < end, "no status answer");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(got.addr, a);
        assert_eq!(got.players[0].name, "Bob");
        assert!(l.take_status().is_none());
        t.join().unwrap();
    }

    #[test]
    fn a_refresh_runs_until_stopped() {
        let mut l = ServerList::default();
        assert!(!l.refreshing());
        l.refresh(true);
        assert!(l.refreshing());
        l.stop_refresh();
        assert!(!l.refreshing());
    }
}
