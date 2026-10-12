// SPDX-License-Identifier: GPL-3.0-only
//! Finding servers: the stock join menu's list (LAN discovery and favorites) and the map query that precedes joining.
//!
//! A server answers `getinfo <challenge>` with its `hostname`, `mapname`, `gametype`, `clients` and `sv_maxclients`
//! ([`net::oob`]). The browser sends one to the LAN broadcast address and the loopback address on each of the standard
//! ports ([`PORTS`], so a client-hosted `--listen` server shows up too) and to every favorite, then collects the
//! replies without ever blocking: a reply is a [`Entry`], its ping the time since the request went out.
//! [`query`] is the blocking form for joining a typed address: it must learn the map before the client can load it.
//! The Internet list asks the masters named by `cl_masters` for their servers ([`net::master`]) and then sends each
//! of those a `getinfo` just as the LAN scan does.
//! The Server Info popup asks the selected server for a `getstatus` instead: its full info string and its players
//! ([`Status`]), shown through [`status_rows`].

use net::oob::{Oob, StatusPlayer};
use std::cmp::Ordering;
use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::net::ToSocketAddrs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;
use web_time::Instant;

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
/// The Internet list waits longer: a master and then every server it names are a round trip each.
const INTERNET_WINDOW: Duration = Duration::from_secs(5);
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

    /// Whether joining needs a password the player has not given (`given` is the `password` dvar).
    pub fn lacks_password(&self, given: &str) -> bool {
        self.password && given.is_empty()
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
    /// A challenge or a server list from `from`, which the master query handles.
    Master(SocketAddr, Oob),
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

    /// Sends one packet.
    fn send(&mut self, to: SocketAddr, oob: &Oob) {
        let _ = self.sock.send_to(&oob.encode(), to);
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
                Some(o @ (Oob::Challenge(_) | Oob::Servers(_))) => out.push(Reply::Master(from, o)),
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

/// The masters `text` (`cl_masters`: `host[:port]` words) names. A name that does not resolve is left out. Blocking;
/// the list runs it on a thread. (A browser cannot send datagrams, so it has no masters.)
#[cfg(not(target_arch = "wasm32"))]
fn parse_masters(text: &str) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for w in text.split_whitespace().take(net::master::MASTER_SLOTS) {
        let with_port = if w
            .rsplit_once(':')
            .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
        {
            w.to_owned()
        } else {
            format!("{w}:{}", net::master::MASTER_PORT)
        };
        if let Some(a) = with_port
            .to_socket_addrs()
            .ok()
            .and_then(|mut i| i.find(SocketAddr::is_ipv4))
            && !out.contains(&a)
        {
            out.push(a);
        }
    }
    out
}

/// Which stock list is shown (`ui_netSource`): 0 local network, 1 internet (the masters' servers), 2 favorites.
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

/// Ping ascending with the unanswered rows (ping 0) last, then game mode and name: the tie-break of every column.
fn by_ping(a: &Entry, b: &Entry) -> Ordering {
    (a.ping == 0, a.ping)
        .cmp(&(b.ping == 0, b.ping))
        .then_with(|| a.gametype.cmp(&b.gametype))
        .then_with(|| a.hostname.to_lowercase().cmp(&b.hostname.to_lowercase()))
}

/// Orders two rows by stock column `col` (`LAN_CompareServers`): the column's value, a tie settled by [`by_ping`]
/// whichever way the column runs; `down` reverses the column (the ping column wholly). A column that holds nothing to
/// order by leaves the rows as they are.
fn compare(col: usize, down: bool, a: &Entry, b: &Entry) -> Ordering {
    let by = match col {
        0 => a.password.cmp(&b.password),
        // A server of unknown hardware goes last when ascending.
        1 => (a.hardware == 0, a.hardware).cmp(&(b.hardware == 0, b.hardware)),
        2 => a.hostname.to_lowercase().cmp(&b.hostname.to_lowercase()),
        3 => a.map.cmp(&b.map),
        4 => a.clients.cmp(&b.clients),
        5 => a.gametype.cmp(&b.gametype),
        6 => a.voice.cmp(&b.voice),
        7 => a.pure.cmp(&b.pure),
        8 => a.modded.cmp(&b.modded),
        9 => a.punkbuster.cmp(&b.punkbuster),
        10 => return if down { by_ping(b, a) } else { by_ping(a, b) },
        _ => return Ordering::Equal,
    };
    let by = if down { by.reverse() } else { by };
    by.then_with(|| by_ping(a, b))
}

/// What the join menu's filter popup leaves in the list (`UI_DoServerRefresh`): each field is a `ui_browser*` dvar,
/// the tri-state ones holding -1 for "either".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filter {
    /// `ui_browserShowFull` and `ui_browserShowEmpty`: whether full and empty servers are listed.
    pub show_full: bool,
    pub show_empty: bool,
    /// `ui_browserShowPassword`: 1 only passworded servers, 0 only open ones.
    pub password: i32,
    /// `ui_browserShowDedicated`: only dedicated servers. (`ui_browserShowPure` is not a filter here: see
    /// `Shell::server_filter`.)
    pub dedicated_only: bool,
    /// `ui_browserMod`, `ui_browserFriendlyfire`, `ui_browserKillcam` and `ui_browserShowPunkBuster`: the value the
    /// server's `mod`, `ff`, `kc` or `pb` must hold.
    pub modded: i32,
    pub friendly_fire: i32,
    pub killcam: i32,
    pub punkbuster: i32,
}

impl Default for Filter {
    /// Lists every server.
    fn default() -> Self {
        Filter {
            show_full: true,
            show_empty: true,
            password: -1,
            dedicated_only: false,
            modded: -1,
            friendly_fire: -1,
            killcam: -1,
            punkbuster: -1,
        }
    }
}

impl Filter {
    /// Whether the list shows `e`. A server that has not answered says nothing the filter could test, so it stays.
    pub fn passes(&self, e: &Entry) -> bool {
        if e.ping <= 0 {
            return true;
        }
        let is = |want: i32, has: bool| want < 0 || want == i32::from(has);
        (self.show_empty || e.clients != 0)
            && (self.show_full || e.clients != e.max_clients)
            && is(self.password, e.password)
            && (!self.dedicated_only || (1..=3).contains(&e.hardware))
            && is(self.modded, e.modded)
            && (self.friendly_fire < 0 || self.friendly_fire == e.friendly_fire)
            && is(self.killcam, e.killcam)
            && is(self.punkbuster, e.punkbuster)
    }
}

/// The lists behind the join menu.
#[derive(Default)]
pub struct ServerList {
    browser: Option<Browser>,
    lan: Vec<Entry>,
    favorites: Vec<Entry>,
    internet: Vec<Entry>,
    /// The masters asked for the Internet list, and the servers they named (only those may fill it).
    masters: Vec<SocketAddr>,
    wanted: HashSet<SocketAddr>,
    /// The names of `cl_masters` being looked up (on a thread: a slow resolver must not stall the menu).
    resolving: Option<std::sync::mpsc::Receiver<Vec<SocketAddr>>>,
    /// The last lookup found no master although some were named.
    unresolved: bool,
    /// Column the list is sorted by and whether it runs downwards.
    sort: Option<(usize, bool)>,
    selected: Option<SocketAddr>,
    /// When the running refresh started and which list it is for; it counts as running for [`REFRESH_WINDOW`].
    refresh_started: Option<(Instant, Source)>,
    /// The server a `getstatus` is out to, when it was last sent and how often.
    status_req: Option<(SocketAddr, Instant, u32)>,
    /// An answer that arrived and has not been taken yet.
    status: Option<Status>,
    /// Where the favorites are kept (`None`: nowhere, they last until the client exits). A browser has no files.
    #[cfg(not(target_arch = "wasm32"))]
    store: Option<std::path::PathBuf>,
    /// The lines of the favorites file that are not listed favorites, written back as they were.
    #[cfg(not(target_arch = "wasm32"))]
    kept_lines: Vec<String>,
}

/// The favorites file in the user's folder. The stock game keeps them in `servercache.dat`, a dump of its in-memory
/// server structs together with the Internet cache; ours is a text file of one `address:port` a line that a person
/// can read and edit, and that is not tied to this client's data layout. A line that is no usable address (a host
/// name, say) is kept as it is.
#[cfg(not(target_arch = "wasm32"))]
pub const FAVORITES_FILE: &str = "favorites.txt";

/// Most favorites kept (`MAX_FAVOURITESERVERS`).
pub const MAX_FAVORITES: usize = 128;
#[cfg(not(target_arch = "wasm32"))]
const FAVORITES_HEADER: &str = "# Favorite servers, one address:port a line.";

/// Whether `a` can be asked: a port and a host to send to.
fn askable(a: &SocketAddr) -> bool {
    a.port() != 0 && !a.ip().is_unspecified()
}

/// What a favorites file holds: the addresses it lists (in order, without repeats, at most [`MAX_FAVORITES`]) and
/// every other line (a host name not yet resolved, a comment, an address that cannot be asked, the surplus), kept
/// so that saving the file again does not lose them.
#[cfg(not(target_arch = "wasm32"))]
fn parse_favorites(text: &str) -> (Vec<SocketAddr>, Vec<String>) {
    let mut found: Vec<SocketAddr> = Vec::new();
    let mut other = Vec::new();
    for line in text.lines().map(str::trim) {
        match line.parse::<SocketAddr>() {
            Ok(a) if askable(&a) && found.contains(&a) => {}
            Ok(a) if askable(&a) && found.len() < MAX_FAVORITES => found.push(a),
            Ok(a) if askable(&a) => other.push(line.to_owned()),
            _ if line.is_empty() || line == FAVORITES_HEADER => {}
            _ => other.push(line.to_owned()),
        }
    }
    (found, other)
}

/// The text of a favorites file for `favorites` and the `other` lines of the file read.
#[cfg(not(target_arch = "wasm32"))]
fn favorites_text(favorites: &[Entry], other: &[String]) -> String {
    let mut s = format!("{FAVORITES_HEADER}\n");
    for f in favorites {
        s += &format!("{}\n", f.addr);
    }
    for l in other {
        s += &format!("{l}\n");
    }
    s
}

impl ServerList {
    /// Scans again for the list `source` shows: the LAN (`full` first forgets what it found, otherwise the answers
    /// update it in place), the favorites, or the Internet (the masters `master_text` names are looked up, asked,
    /// then the servers they name).
    /// A refresh of a list that is already being refreshed is ignored (`false`), so a button held down does not
    /// flood the network.
    pub fn refresh(&mut self, source: Source, full: bool, master_text: &str) -> bool {
        if self.refreshing(source) {
            return false;
        }
        self.refresh_started = Some((Instant::now(), source));
        if self.browser.is_none() {
            self.browser = Browser::new().ok();
        }
        if full && source == Source::Lan {
            self.lan.clear();
        }
        if source == Source::Internet {
            self.internet.clear();
            self.wanted.clear();
            self.masters.clear();
            self.unresolved = false;
            self.resolving = None;
            if !master_text.trim().is_empty() {
                let text = master_text.to_owned();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(parse_masters(&text));
                    });
                    self.resolving = Some(rx);
                }
                // A browser has no UDP: there is no master to find.
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = text;
                    self.unresolved = true;
                }
            }
        }
        let Some(b) = self.browser.as_mut() else {
            return true;
        };
        b.restart();
        match source {
            Source::Lan => b.ask_lan(),
            Source::Internet => {}
            _ => {
                for f in &self.favorites {
                    b.ask(f.addr);
                }
            }
        }
        true
    }

    /// Whether a refresh of `source` is still collecting answers (the join menu then shows how far it got).
    pub fn refreshing(&self, source: Source) -> bool {
        self.refresh_started.is_some_and(|(t, s)| {
            let window = if s == Source::Internet {
                INTERNET_WINDOW
            } else {
                REFRESH_WINDOW
            };
            s == source && t.elapsed() < window
        })
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
        if !askable(&addr)
            || self.favorites.len() >= MAX_FAVORITES
            || self.favorites.iter().any(|f| f.addr == addr)
        {
            return;
        }
        self.favorites.push(Entry::unanswered(addr));
        self.save_favorites();
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
        self.save_favorites();
    }

    /// Keeps the favorites in `<dir>/`[`FAVORITES_FILE`] from now on and takes in those it holds (a missing file is
    /// an empty list). Call once, before the first refresh.
    #[cfg(target_arch = "wasm32")]
    pub fn load_favorites(&mut self, _dir: Option<std::path::PathBuf>) {}

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_favorites(&mut self, dir: Option<std::path::PathBuf>) {
        let Some(path) = dir.map(|d| d.join(FAVORITES_FILE)) else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (found, other) = parse_favorites(&text);
                self.favorites
                    .extend(found.into_iter().map(Entry::unanswered));
                self.kept_lines = other;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // A file that cannot be read is left alone: saving over it would destroy what it holds.
            Err(e) => {
                eprintln!("cannot read the favorites in {}: {e}", path.display());
                return;
            }
        }
        self.store = Some(path);
    }

    #[cfg(target_arch = "wasm32")]
    fn save_favorites(&self) {}

    #[cfg(not(target_arch = "wasm32"))]
    fn save_favorites(&self) {
        if let Some(path) = &self.store {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| {
                    std::fs::write(path, favorites_text(&self.favorites, &self.kept_lines))
                });
            if let Err(e) = written {
                eprintln!("cannot save the favorites to {}: {e}", path.display());
            }
        }
    }

    /// Takes in the replies that arrived; call once per frame while the join menu is up.
    pub fn poll(&mut self) {
        if let Some(Ok(found)) = self.resolving.as_ref().map(|rx| rx.try_recv()) {
            self.resolving = None;
            self.unresolved = found.is_empty();
            self.masters = found;
            if let Some(b) = self.browser.as_mut() {
                b.restart();
                net::master::begin(&self.masters, |to, o| b.send(to, &o));
            }
        }
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        for r in b.poll() {
            match r {
                Reply::Info(e) => self.take(e),
                Reply::Master(from, o) => self.master_reply(from, o),
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

    /// A master's challenge is answered; a list of servers is asked for its servers' info.
    fn master_reply(&mut self, from: SocketAddr, o: Oob) {
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        let Some(list) = net::master::reply(&self.masters, from, o, |to, o| b.send(to, &o)) else {
            return;
        };
        // Pings count from here, not from the request to the master.
        b.restart();
        for a in list {
            if self.wanted.len() < MAX_ENTRIES && self.wanted.insert(a) {
                b.ask(a);
            }
        }
    }

    /// One reply: it refreshes a favorite of that address, and (for a LAN address) is listed on the LAN.
    pub(crate) fn take(&mut self, e: Entry) {
        if let Some(f) = self.favorites.iter_mut().find(|f| f.addr == e.addr) {
            *f = e.clone();
        }
        if self.wanted.contains(&e.addr) {
            match self.internet.iter_mut().find(|o| o.addr == e.addr) {
                Some(o) => *o = e.clone(),
                None => self.internet.push(e.clone()),
            }
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

    /// Whether the Internet list could not find any of the masters `cl_masters` names.
    pub fn masters_unresolved(&self) -> bool {
        self.unresolved
    }

    /// Rows of `source` as shown.
    pub fn rows(&self, source: Source) -> &[Entry] {
        match source {
            Source::Lan => &self.lan,
            Source::Internet => &self.internet,
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
        for list in [&mut self.lan, &mut self.favorites, &mut self.internet] {
            list.sort_by(|a, b| compare(col, down, a, b));
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
        assert!(!l.refreshing(Source::Lan));
        l.refresh(Source::Lan, true, "");
        assert!(l.refreshing(Source::Lan));
        assert!(!l.refreshing(Source::Favorites), "each list has its own");
        // A second request for the same list is ignored: what the list holds now is kept.
        l.take(entry("192.168.1.9:28960", "Me", "mp_crash", 2));
        l.refresh(Source::Lan, true, "");
        assert_eq!(l.rows(Source::Lan).len(), 1);
        l.stop_refresh();
        assert!(!l.refreshing(Source::Lan));
        // A favorites refresh leaves the LAN list alone; the internet one with no master has nothing to ask but still takes its time.
        l.refresh(Source::Favorites, true, "");
        assert_eq!(l.rows(Source::Lan).len(), 1);
        l.refresh(Source::Internet, true, "");
        assert!(l.refreshing(Source::Internet));
    }

    /// A master and a server that sends it heartbeats, on loopback threads.
    #[test]
    fn the_internet_list_shows_the_servers_a_master_names() {
        use net::{Transport, UdpTransport};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let listed = Arc::new(AtomicUsize::new(0));
        let lo = SocketAddr::from(([127, 0, 0, 1], 0));
        let mut mt = UdpTransport::bind(lo).unwrap();
        let master = mt.local_addr();
        let master_thread = {
            let (stop, listed) = (stop.clone(), listed.clone());
            std::thread::spawn(move || {
                let mut m = master::Master::new(Instant::now());
                let mut buf = vec![0u8; 2048];
                while !stop.load(Ordering::Relaxed) {
                    let got = mt.recv_from(&mut buf, Some(Duration::from_millis(20)));
                    let now = Instant::now();
                    if let Ok(Some((n, from))) = got
                        && let Some(o) = Oob::parse(&buf[..n])
                    {
                        for (to, r) in m.handle(from, o, now) {
                            mt.send_to(to, &r.encode());
                        }
                    }
                    listed.store(m.len(), Ordering::Relaxed);
                }
            })
        };
        let mut st = UdpTransport::bind(lo).unwrap();
        let server = st.local_addr();
        let server_thread = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                st.send_to(master, &Oob::Heartbeat(net::oob::PROTOCOL).encode());
                let mut buf = vec![0u8; 2048];
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(Some((n, from))) =
                        st.recv_from(&mut buf, Some(Duration::from_millis(20)))
                        && let Some(Oob::GetInfo(c)) = Oob::parse(&buf[..n])
                    {
                        let proto = net::oob::PROTOCOL.to_string();
                        let info = kv(&[
                            ("protocol", &proto),
                            ("hostname", "Far Away"),
                            ("mapname", "mp_crash"),
                            ("gametype", "war"),
                            ("challenge", &c.to_string()),
                        ]);
                        st.send_to(from, &Oob::InfoResponse(info).encode());
                    }
                }
            })
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while listed.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut l = ServerList::default();
        assert!(l.refresh(Source::Internet, true, &master.to_string()));
        while l.rows(Source::Internet).is_empty() && Instant::now() < deadline {
            l.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        stop.store(true, Ordering::Relaxed);
        master_thread.join().unwrap();
        server_thread.join().unwrap();
        let rows = l.rows(Source::Internet);
        assert_eq!(rows.len(), 1, "the master's server is listed");
        assert_eq!(
            (rows[0].addr, rows[0].hostname.as_str()),
            (server, "Far Away")
        );
    }

    #[test]
    fn a_server_nobody_asked_for_never_lands_on_the_internet_list() {
        let mut l = ServerList::default();
        l.take(entry("8.8.8.8:28960", "Far", "mp_bog", 3));
        assert!(l.rows(Source::Internet).is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn master_names_default_their_port_and_skip_the_unresolvable() {
        assert_eq!(
            parse_masters("127.0.0.1 127.0.0.1:99 127.0.0.1 not.a.host.invalid"),
            vec![
                addr(&format!("127.0.0.1:{}", net::master::MASTER_PORT)),
                addr("127.0.0.1:99")
            ]
        );
        assert!(parse_masters("  ").is_empty());
    }

    fn hosts(l: &ServerList) -> Vec<String> {
        l.rows(Source::Favorites)
            .iter()
            .map(|e| e.hostname.clone())
            .collect()
    }

    /// Every column of the list orders its rows, a tie is settled by ping, and a second request reverses it.
    #[test]
    fn every_column_sorts_and_ties_go_by_ping() {
        let mut a = entry("10.0.0.1:28960", "a", "mp_crash", 50);
        let mut b = entry("10.0.0.2:28960", "b", "mp_crash", 20);
        let mut c = entry("10.0.0.3:28960", "c", "mp_crash", 30);
        a.password = true;
        b.pure = true;
        c.pure = true;
        c.punkbuster = true;
        a.voice = true;
        b.modded = true;
        a.hardware = 0;
        b.hardware = 2;
        c.hardware = 1;
        for (col, up, down) in [
            (0, "bca", "abc"),
            (1, "cba", "abc"),
            (6, "bca", "abc"),
            (7, "abc", "bca"),
            (8, "cab", "bca"),
            (9, "bac", "cba"),
        ] {
            let mut l = ServerList::default();
            for e in [&a, &b, &c] {
                l.favorites.push(e.clone());
            }
            l.sort_by(col);
            assert_eq!(hosts(&l).concat(), up, "column {col} ascending");
            l.sort_by(col);
            assert_eq!(hosts(&l).concat(), down, "column {col} descending");
        }
    }

    #[test]
    fn the_filter_drops_the_servers_the_popup_rules_out() {
        let mut open = entry("10.0.0.1:28960", "open", "mp_crash", 20);
        open.pure = true;
        open.hardware = 2;
        let mut full = open.clone();
        full.clients = full.max_clients;
        let mut empty = open.clone();
        empty.clients = 0;
        let mut locked = open.clone();
        locked.password = true;
        let mut modded = open.clone();
        modded.modded = true;
        let mut ff = open.clone();
        ff.friendly_fire = 2;
        let mut kc = open.clone();
        kc.killcam = true;
        let mut pb = open.clone();
        pb.punkbuster = true;
        let mut home = open.clone();
        home.hardware = 0;
        let mut loose = open.clone();
        loose.pure = false;
        let all = [
            &open, &full, &empty, &locked, &modded, &ff, &kc, &pb, &home, &loose,
        ];
        let shown =
            |f: Filter| -> Vec<usize> { (0..all.len()).filter(|&i| f.passes(all[i])).collect() };
        assert_eq!(shown(Filter::default()).len(), all.len());
        let base = Filter::default();
        assert!(
            !shown(Filter {
                show_full: false,
                ..base
            })
            .contains(&1)
        );
        assert!(
            !shown(Filter {
                show_empty: false,
                ..base
            })
            .contains(&2)
        );
        assert_eq!(
            shown(Filter {
                password: 1,
                ..base
            }),
            [3],
            "only the passworded one"
        );
        assert!(
            !shown(Filter {
                password: 0,
                ..base
            })
            .contains(&3)
        );
        assert!(
            !shown(Filter {
                dedicated_only: true,
                ..base
            })
            .contains(&8)
        );
        assert_eq!(shown(Filter { modded: 1, ..base }), [4]);
        assert!(!shown(Filter { modded: 0, ..base }).contains(&4));
        assert_eq!(
            shown(Filter {
                friendly_fire: 2,
                ..base
            }),
            [5]
        );
        assert_eq!(shown(Filter { killcam: 1, ..base }), [6]);
        assert_eq!(
            shown(Filter {
                punkbuster: 1,
                ..base
            }),
            [7]
        );
        // A favorite that has not answered has nothing to test and is kept.
        let silent = Entry::unanswered(addr("10.0.0.9:28960"));
        assert!(
            Filter {
                password: 1,
                ..base
            }
            .passes(&silent)
        );
    }

    #[test]
    fn a_passworded_server_needs_one_only_until_it_is_given() {
        let mut e = entry("10.0.0.1:28960", "a", "mp_crash", 20);
        assert!(!e.lacks_password(""));
        e.password = true;
        assert!(e.lacks_password(""));
        assert!(!e.lacks_password("secret"));
    }

    #[test]
    fn favorites_survive_a_restart_and_a_removal_is_kept_too() {
        let dir = std::env::temp_dir().join(format!("cod4e-favorites-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut l = ServerList::default();
        l.load_favorites(Some(dir.clone()));
        assert!(
            l.rows(Source::Favorites).is_empty(),
            "no file is no favorites"
        );
        l.add_favorite(addr("10.0.0.1:28960"));
        l.add_favorite(addr("10.0.0.2:28961"));
        l.add_favorite(addr("10.0.0.1:28960"));
        let mut again = ServerList::default();
        again.load_favorites(Some(dir.clone()));
        let kept: Vec<_> = again
            .rows(Source::Favorites)
            .iter()
            .map(|e| e.addr)
            .collect();
        assert_eq!(kept, [addr("10.0.0.1:28960"), addr("10.0.0.2:28961")]);
        again.remove_favorite(addr("10.0.0.1:28960"));
        let mut third = ServerList::default();
        third.load_favorites(Some(dir.clone()));
        let kept: Vec<_> = third
            .rows(Source::Favorites)
            .iter()
            .map(|e| e.addr)
            .collect();
        assert_eq!(kept, [addr("10.0.0.2:28961")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host name, a comment and an address nobody can ask are not favorites but are not thrown away by a save;
    /// a file that cannot be read is never overwritten; the list stops at the stock 128.
    #[test]
    fn favorites_file_keeps_what_it_cannot_list_and_is_never_overwritten_unread() {
        let dir = std::env::temp_dir().join(format!("cod4e-favorites-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(FAVORITES_FILE);
        std::fs::write(
            &file,
            "# mine\nplay.example.org:28960\n0.0.0.0:28960\n10.0.0.5:0\n10.0.0.3:28960\n",
        )
        .unwrap();
        let mut l = ServerList::default();
        l.load_favorites(Some(dir.clone()));
        let listed: Vec<_> = l.rows(Source::Favorites).iter().map(|e| e.addr).collect();
        assert_eq!(listed, [addr("10.0.0.3:28960")]);
        l.add_favorite(addr("0.0.0.0:28960"));
        l.add_favorite(addr("10.0.0.6:0"));
        l.add_favorite(addr("10.0.0.4:28960"));
        let saved = std::fs::read_to_string(&file).unwrap();
        for line in [
            "# mine",
            "play.example.org:28960",
            "0.0.0.0:28960",
            "10.0.0.5:0",
            "10.0.0.4:28960",
        ] {
            assert!(saved.lines().any(|l| l == line), "{line} survives a save");
        }
        assert_eq!(
            l.rows(Source::Favorites).len(),
            2,
            "unaskable addresses are not added"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // A favorites path that is a directory cannot be read: nothing is written over it.
        let dir = std::env::temp_dir().join(format!("cod4e-favorites-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(FAVORITES_FILE)).unwrap();
        let mut l = ServerList::default();
        l.load_favorites(Some(dir.clone()));
        l.add_favorite(addr("10.0.0.1:28960"));
        assert!(dir.join(FAVORITES_FILE).is_dir());
        let _ = std::fs::remove_dir_all(&dir);

        // The cap.
        let mut l = ServerList::default();
        for i in 0..MAX_FAVORITES + 5 {
            l.add_favorite(addr(&format!("10.1.{}.{}:28960", i / 200, i % 200 + 1)));
        }
        assert_eq!(l.rows(Source::Favorites).len(), MAX_FAVORITES);
    }
}
