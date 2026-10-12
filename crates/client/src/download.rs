// SPDX-License-Identifier: GPL-3.0-only
//! Fetching what a server runs and the install lacks (`cl_allowDownload`): the mod's IWDs and `mod.ff`, and the map's
//! zone, loading zone and IWD. A worker thread asks the server for its manifest ([`net::manifest`]), compares each file
//! with the one on disk (size and CRC-32) and fetches the ones that differ, over HTTP(S) when the server names a
//! fast-download URL and over [`net::download`] when it does not (or the web server fails). A mod goes to its folder
//! under `mods/`, a map to `usermaps/<map>/`, and nothing anywhere else: the server is not trusted with a path. The
//! window shows the loading screen with the bytes arrived meanwhile. The browser build has no files to put it in, so
//! this is native only.

use assets::crc::{Crc32, file_crc32};
use net::download::{Progress, fetch};
use net::manifest::{self, File};
use net::{Transport, UdpTransport};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A download in progress, and the join it is for.
pub struct Download {
    pub map: String,
    /// The mod (`fs_game`) the server runs, which the join follows once the files are here.
    pub game: String,
    /// The server to join and what it is playing, once the files are here.
    pub join: (SocketAddr, String),
    progress: Arc<Progress>,
    /// What the worker is doing and which file it is on.
    stage: Arc<Mutex<String>>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<(), String>>,
}

/// What the install at `root` lacks outright to run the mod `game` and play `map`: the mod's folder, or the map's zone
/// (looking where the mod and a custom map's `usermaps/<map>` would have it, and only when it has a zone folder to
/// take it).
pub fn lacks(root: &Path, game: &str, map: &str) -> Option<String> {
    if !game.is_empty() && !assets::paths::game_dir(root, game) {
        return Some(format!("the mod {game}"));
    }
    (net::download::valid_name(map)
        && assets::paths::zone_file(root, game, map).is_none()
        && server::content::zone_dir(root).is_some())
    .then(|| format!("the map {map}"))
}

/// Whether to ask a server running `game` and `map` for its files before joining: it runs a mod, or the map is a
/// custom map (which the server's copy may differ from), or the install lacks the map.
pub fn wanted(root: &Path, game: &str, map: &str) -> bool {
    !game.is_empty()
        || assets::paths::is_usermap(root, game, map)
        || lacks(root, game, map).is_some()
}

impl Download {
    /// Starts fetching what the server at `server` runs (`game`, `map`) into the install at `root`.
    pub fn start(
        root: &Path,
        server: SocketAddr,
        game: &str,
        map: &str,
        gametype: &str,
    ) -> Result<Self, String> {
        let progress = Arc::new(Progress::default());
        let stage = Arc::new(Mutex::new("Asking the server for its files".to_owned()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (p, s, c) = (progress.clone(), stage.clone(), cancel.clone());
        let (root, game_name, map_name) = (root.to_owned(), game.to_owned(), map.to_owned());
        std::thread::Builder::new()
            .name("map-download".into())
            .spawn(move || {
                let result = UdpTransport::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
                    .map_err(|e| e.to_string())
                    .and_then(|mut t| {
                        sync(&mut t, server, &root, &game_name, &map_name, &p, &c, &s)
                    });
                let _ = tx.send(result);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            map: map.to_owned(),
            game: game.to_owned(),
            join: (server, gametype.to_owned()),
            progress,
            stage,
            cancel,
            rx,
        })
    }

    /// Blocks until the worker is done (a direct `--connect`, which has no window to show progress in yet).
    pub fn wait(self) -> Result<(), String> {
        self.rx.recv().map_err(|e| e.to_string())?
    }

    /// The outcome, once the worker is done.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(e) => Some(Err(e.to_string())),
        }
    }

    /// 0 to 1 of the file being fetched.
    pub fn fraction(&self) -> f32 {
        let total = self.progress.total.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.progress.done.load(Ordering::Relaxed) as f32 / total as f32
        }
    }

    /// The line under the progress bar.
    pub fn note(&self) -> String {
        let (done, total) = (
            self.progress.done.load(Ordering::Relaxed),
            self.progress.total.load(Ordering::Relaxed),
        );
        let stage = self.stage.lock().map(|s| s.clone()).unwrap_or_default();
        if total == 0 {
            stage
        } else {
            format!(
                "{stage}: {:.1} of {:.1} MB",
                done as f64 / 1e6,
                total as f64 / 1e6
            )
        }
    }
}

impl Drop for Download {
    /// An abandoned download stops at its next packet and removes what it had.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Whether the file at `path` is the one `file` describes.
fn is_current(path: &Path, file: &File) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == file.size)
        && file_crc32(path).is_ok_and(|(_, crc)| crc == file.crc)
}

/// The files of `listing` this client writes and does not have, each with its install-relative destination. A name
/// [`manifest::dest`] refuses is dropped, as is a map's file when the install has the map from the stock game or the
/// mod, or one of a map other than `map`.
fn plan(root: &Path, game: &str, map: &str, listing: Vec<File>) -> Vec<(File, String)> {
    let own = format!("usermaps/{map}/");
    let has_stock_map = !assets::paths::is_usermap(root, game, map)
        && assets::paths::zone_file(root, game, map).is_some();
    listing
        .into_iter()
        .filter_map(|f| manifest::dest(game, &f.name).map(|d| (f, d)))
        .filter(|(_, d)| !d.starts_with("usermaps/") || (d.starts_with(&own) && !has_stock_map))
        .filter(|(f, d)| !is_current(&root.join(d), f))
        .collect()
}

/// Fetches from `server` everything the install at `root` lacks to run `game` and play `map`.
#[allow(clippy::too_many_arguments)]
fn sync(
    t: &mut dyn Transport,
    server: SocketAddr,
    root: &Path,
    game: &str,
    map: &str,
    progress: &Progress,
    cancel: &AtomicBool,
    stage: &Mutex<String>,
) -> Result<(), String> {
    let (listing, www) = match manifest::fetch_list(t, server, cancel) {
        Ok(l) => l,
        // A server that gives nothing has no say in what an install that has it all runs.
        Err(why) if why == manifest::DOWNLOADS_OFF => {
            return match lacks(root, game, map) {
                Some(what) => Err(format!(
                    "You do not have {what}, and this server does not allow downloads"
                )),
                None => Ok(()),
            };
        }
        Err(why) => return Err(why),
    };
    let www = manifest::valid_www(&www).then_some(www);
    let todo = plan(root, game, map, listing);
    let count = todo.len();
    let need: u64 = todo.iter().map(|(f, _)| f.size).sum();
    if need > manifest::MAX_SUM {
        return Err("The server's files are too large".into());
    }
    if let Some(free) = free_space(root)
        && free < need + SPARE
    {
        return Err(format!(
            "There is not enough room on the disk for the server's files ({:.0} MB needed, {:.0} MB free)",
            need as f64 / 1e6,
            free as f64 / 1e6
        ));
    }
    for (i, (file, dest)) in todo.into_iter().enumerate() {
        if let Ok(mut s) = stage.lock() {
            *s = format!("Downloading {} ({} of {count})", file.name, i + 1);
        }
        progress.done.store(0, Ordering::Relaxed);
        progress.total.store(file.size, Ordering::Relaxed);
        let path = place(root, &dest)?;
        if path.exists() {
            // A file of the same name that is not the server's: the server's replaces it, and the player is told.
            eprintln!("download: replacing {dest}, which differs from the server's copy");
        }
        if let Some(www) = &www {
            match http(
                &manifest::url(www, &file.name),
                server.ip(),
                &path,
                &file,
                progress,
                cancel,
            ) {
                Ok(()) => continue,
                Err(why) if cancel.load(Ordering::Relaxed) => return Err(why),
                // The server's own transfer takes over, as the original falls back to its own.
                Err(why) => eprintln!(
                    "fast download of {} failed ({why}); asking the server",
                    file.name
                ),
            }
            progress.done.store(0, Ordering::Relaxed);
        }
        // A leftover `.part` is not reused: the transfer starts it afresh.
        std::fs::remove_file(part_of(&path)).ok();
        fetch(t, server, &file.name, &path, progress, cancel)?;
        if !is_current(&path, &file) {
            let _ = std::fs::remove_file(&path);
            return Err(format!("{} does not match the server's copy", file.name));
        }
    }
    Ok(())
}

/// Room left on a disk after the download, for the player's own files.
const SPARE: u64 = 64 << 20;

/// Bytes free on the disk `root` is on, when that can be told.
fn free_space(root: &Path) -> Option<u64> {
    let root = std::fs::canonicalize(root).ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|d| root.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

/// `dest` (install-relative, from [`manifest::dest`]) below `root`, its folders made. A folder or file on the way that
/// is a link is refused: a link could lead the write out of the install.
fn place(root: &Path, dest: &str) -> Result<std::path::PathBuf, String> {
    let mut at = root.to_owned();
    for part in dest.split('/') {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!(
                    "{} is a link; not writing through it",
                    at.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if at != root.join(dest) {
                    std::fs::create_dir(&at).map_err(|e| format!("{}: {e}", at.display()))?;
                }
            }
            Err(e) => return Err(format!("{}: {e}", at.display())),
        }
    }
    Ok(at)
}

/// Where the bytes of `dest` arrive before it is whole.
fn part_of(dest: &Path) -> std::path::PathBuf {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    part.into()
}

/// The IPv4 address an IPv6 one carries: mapped (`::ffff:a.b.c.d`), compatible (`::a.b.c.d`), NAT64 (`64:ff9b::/96`)
/// or 6to4 (`2002:aabb:ccdd::/48`).
fn embedded_v4(a: &std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let s = a.segments();
    let v4 = |hi: u16, lo: u16| {
        std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
    };
    match s {
        [0, 0, 0, 0, 0, 0xffff, hi, lo] | [0, 0, 0, 0, 0, 0, hi, lo]
            if !a.is_unspecified() && !a.is_loopback() =>
        {
            Some(v4(hi, lo))
        }
        [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] => Some(v4(hi, lo)),
        [0x2002, hi, lo, ..] => Some(v4(hi, lo)),
        _ => None,
    }
}

/// Whether `ip` is an address a download may be fetched from: the game server's own, or one on the public Internet.
/// A web server named by the game server must not be a way to reach the player's own machine or network.
fn allowed(ip: std::net::IpAddr, server: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    let ip = match ip {
        IpAddr::V6(v6) => embedded_v4(&v6).map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    if ip == server {
        return true;
    }
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                // Shared address space (carrier-grade NAT) and the benchmark range.
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] == 0
                || o[0] >= 240)
        }
        IpAddr::V6(a) => {
            let seg = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                // Unique local (fc00::/7), link local (fe80::/10) and documentation (2001:db8::/32).
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] == 0x2001 && seg[1] == 0x0db8))
        }
    }
}

/// The name lookup of a download: it finds what the system finds and drops the addresses [`allowed`] refuses, at the
/// moment of the connection, so a name that changes its answer cannot slip one past.
#[derive(Debug)]
struct PublicOnly {
    inner: ureq::unversioned::resolver::DefaultResolver,
    server: std::net::IpAddr,
}

impl ureq::unversioned::resolver::Resolver for PublicOnly {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut found = self.inner.resolve(uri, config, timeout)?;
        let mut keep = 0;
        for i in 0..found.len() {
            if allowed(found[i].ip(), self.server) {
                found.swap(keep, i);
                keep += 1;
            }
        }
        if keep == 0 {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the web server is on a private or local address",
            )));
        }
        found.truncate(keep);
        Ok(found)
    }
}

/// Downloads `url` into `dest` (written as `dest.part`, renamed when it is `file`: the size and CRC-32 the manifest
/// gave). Reads no more than the size announced, whatever the web server sends. Redirects are not followed, and the
/// web server must be at the game server's own address or a public one ([`allowed`]).
fn http(
    url: &str,
    server: std::net::IpAddr,
    dest: &Path,
    file: &File,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<(), String> {
    // A web server that crawls is given up on: a floor of 200 kB/s over the file, and half a minute.
    let budget = Duration::from_secs(30 + file.size / 200_000);
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .proxy(None)
        .timeout_global(Some(budget))
        .timeout_connect(Some(Duration::from_secs(10)))
        .build();
    let agent = ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        PublicOnly {
            inner: Default::default(),
            server,
        },
    );
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    if let Some(len) = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        && len != file.size
    {
        return Err("the web server's file is not the one the server listed".into());
    }
    let part = part_of(dest);
    std::fs::remove_file(&part).ok();
    let result = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .map_err(|e| format!("{}: {e}", part.display()))?;
        let mut body = response.into_body().into_reader().take(file.size + 1);
        let (mut crc, mut got) = (Crc32::default(), 0u64);
        let mut buf = vec![0u8; 1 << 16];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Download cancelled".to_owned());
            }
            let n = body.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            got += n as u64;
            if got > file.size {
                return Err("the web server sent more than the server listed".into());
            }
            crc.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            progress.done.store(got, Ordering::Relaxed);
        }
        if got != file.size || crc.finish() != file.crc {
            return Err("the web server's file is not the one the server listed".into());
        }
        out.flush().map_err(|e| e.to_string())
    })();
    match result {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::Oob;
    use net::connect::{Gate, serve};
    use net::oob::Challenger;
    use net::transport::MemNet;
    use std::path::PathBuf;

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    /// Bytes that differ per file and span several windows.
    fn bytes(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed))
            .collect()
    }

    /// A server's install with a synthetic mod and custom map, and what its offer lists.
    fn server_install(root: &Path) -> server::fastdl::Offer {
        put(root, "mods/m/a.iwd", &bytes(3, 40_000));
        put(root, "mods/m/mod.ff", &bytes(5, 2_500));
        put(root, "usermaps/mp_x/mp_x.ff", &bytes(7, 70_001));
        put(root, "usermaps/mp_x/mp_x.iwd", &bytes(11, 1_000));
        server::fastdl::Offer::build(root, "mods/m", Some("mp_x"))
    }

    /// A server on a memory network: answers `getmanifest` with `files` (or says downloads are off) and `getfile`
    /// with what `path` finds, noting each name asked for.
    struct Host {
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl Host {
        fn start(
            net: &MemNet,
            addr: SocketAddr,
            files: Option<(Vec<File>, String)>,
            path: impl Fn(&str) -> Option<PathBuf> + Send + 'static,
        ) -> Self {
            let mut t = net.endpoint(addr);
            let stop = Arc::new(AtomicBool::new(false));
            let asked = Arc::new(Mutex::new(Vec::new()));
            let (flag, log) = (stop.clone(), asked.clone());
            let handle = std::thread::spawn(move || {
                let ch = Challenger::new();
                let mut buf = [0u8; 2048];
                while !flag.load(Ordering::Relaxed) {
                    let Ok(Some((n, from))) =
                        t.recv_from(&mut buf, Some(Duration::from_millis(10)))
                    else {
                        continue;
                    };
                    match serve(&ch, 0, from, Oob::parse(&buf[..n]).unwrap(), Vec::new) {
                        Gate::Reply(r) => t.send_to(from, &r.encode()),
                        Gate::Manifest { from } => match &files {
                            Some((files, www)) => {
                                for p in manifest::packets(files, www) {
                                    t.send_to(from, &p.encode());
                                }
                            }
                            None => t.send_to(
                                from,
                                &Oob::Error(manifest::DOWNLOADS_OFF.into()).encode(),
                            ),
                        },
                        Gate::File { from, name, offset } => {
                            log.lock().unwrap().push(name.clone());
                            match path(&name) {
                                Some(p) => {
                                    for c in net::download::window(&p, offset).unwrap() {
                                        t.send_to(from, &c.encode());
                                    }
                                }
                                None => t.send_to(
                                    from,
                                    &Oob::Error(net::download::NOT_AVAILABLE.into()).encode(),
                                ),
                            }
                        }
                        _ => {}
                    }
                }
            });
            Self {
                stop,
                handle: Some(handle),
                asked,
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Drop for Host {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    const SERVER: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 1);

    fn run_sync(net: &MemNet, root: &Path, game: &str, map: &str) -> Result<(), String> {
        run_sync_from(net, SERVER, root, game, map)
    }

    fn run_sync_from(
        net: &MemNet,
        server: SocketAddr,
        root: &Path,
        game: &str,
        map: &str,
    ) -> Result<(), String> {
        let mut t = net.endpoint(SocketAddr::from(([127, 0, 0, 1], 2)));
        sync(
            &mut t,
            server,
            root,
            game,
            map,
            &Progress::default(),
            &AtomicBool::new(false),
            &Mutex::new(String::new()),
        )
    }

    fn same(a: &Path, b: &Path, rel: &str) {
        assert_eq!(
            std::fs::read(a.join(rel)).unwrap(),
            std::fs::read(b.join(rel)).unwrap(),
            "{rel}"
        );
    }

    /// A web server for the files of `root` under `/fast/`: `mode` says whether it gives them, 404s or serves the
    /// wrong bytes. Counts the requests it got.
    struct Web {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
        hits: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Good,
        Missing,
        Wrong,
        /// Answers every request with a redirect to this address.
        Redirect(SocketAddr),
    }

    impl Web {
        fn start(root: PathBuf, mode: Mode) -> Self {
            use std::io::BufRead;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let hits = Arc::new(Mutex::new(Vec::new()));
            let (flag, log) = (stop.clone(), hits.clone());
            let handle = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    let Ok(mut s) = stream else { continue };
                    let mut r = std::io::BufReader::new(s.try_clone().unwrap());
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h.trim().is_empty() {
                            break;
                        }
                    }
                    let path = line.split(' ').nth(1).unwrap_or("").to_owned();
                    log.lock().unwrap().push(path.clone());
                    let rel = path.strip_prefix("/fast/").unwrap_or("");
                    if let Mode::Redirect(to) = mode {
                        let _ = s.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{to}/fast/{rel}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
                        continue;
                    }
                    let body = std::fs::read(root.join(rel))
                        .ok()
                        .filter(|_| !matches!(mode, Mode::Missing));
                    let reply = match body {
                        Some(mut b) => {
                            if matches!(mode, Mode::Wrong) && !b.is_empty() {
                                b[0] ^= 0xff;
                            }
                            let mut v = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()).into_bytes();
                            v.extend(b);
                            v
                        }
                        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    };
                    let _ = s.write_all(&reply);
                }
            });
            Self {
                addr,
                stop,
                handle: Some(handle),
                hits,
            }
        }

        fn base(&self) -> String {
            format!("http://{}/fast", self.addr)
        }

        fn hits(&self) -> Vec<String> {
            self.hits.lock().unwrap().clone()
        }
    }

    impl Drop for Web {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::net::TcpStream::connect(self.addr);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    const ALL: [&str; 4] = [
        "mods/m/a.iwd",
        "mods/m/mod.ff",
        "usermaps/mp_x/mp_x.ff",
        "usermaps/mp_x/mp_x.iwd",
    ];

    #[test]
    fn a_mod_and_a_map_come_over_the_netchan_and_only_what_differs_is_fetched_again() {
        let (srv, cli) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let offer = Arc::new(server_install(srv.path()));
        let net = MemNet::new();
        let o = offer.clone();
        let host = Host::start(
            &net,
            SERVER,
            Some((offer.files.clone(), String::new())),
            move |n| o.path(n).map(Path::to_owned),
        );
        run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap();
        for f in ALL {
            same(srv.path(), cli.path(), f);
        }
        assert!(!cli.path().join("mods/m/a.iwd.part").exists());
        let first = host.asked();
        for f in ALL {
            assert!(first.iter().any(|n| n == f), "{f}");
        }
        // Whole, it asks for nothing more.
        run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap();
        assert_eq!(host.asked().len(), first.len());
        // One file changed (same size, other bytes) is the one fetched again.
        let mut bad = std::fs::read(cli.path().join("mods/m/mod.ff")).unwrap();
        bad[0] ^= 1;
        std::fs::write(cli.path().join("mods/m/mod.ff"), bad).unwrap();
        run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap();
        let again = &host.asked()[first.len()..];
        assert!(
            again.iter().all(|n| n == "mods/m/mod.ff") && !again.is_empty(),
            "{again:?}"
        );
        same(srv.path(), cli.path(), "mods/m/mod.ff");
    }

    #[test]
    fn a_web_server_is_used_when_offered_and_the_servers_own_transfer_covers_its_failures() {
        for (mode, over_http) in [
            (Mode::Good, true),
            (Mode::Missing, false),
            (Mode::Wrong, false),
        ] {
            let (srv, cli) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let offer = Arc::new(server_install(srv.path()));
            let web = Web::start(srv.path().to_owned(), mode);
            let net = MemNet::new();
            let o = offer.clone();
            let host = Host::start(
                &net,
                SERVER,
                Some((offer.files.clone(), web.base())),
                move |n| o.path(n).map(Path::to_owned),
            );
            run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap();
            for f in ALL {
                same(srv.path(), cli.path(), f);
            }
            assert!(web.hits().len() >= ALL.len());
            assert_eq!(host.asked().is_empty(), over_http);
            assert!(!cli.path().join("mods/m/a.iwd.part").exists());
        }
    }

    #[test]
    fn a_hostile_manifest_writes_nothing_outside_the_mod_and_the_map() {
        let (srv, cli) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let root = cli.path().join("install");
        std::fs::create_dir_all(&root).unwrap();
        put(srv.path(), "any", &bytes(9, 100));
        let crc = assets::crc::crc32(&bytes(9, 100));
        let list = |names: &[&str]| -> Vec<File> {
            names
                .iter()
                .map(|n| File {
                    name: (*n).into(),
                    size: 100,
                    crc,
                })
                .collect()
        };
        let evil = [
            "../evil.iwd",
            "mods/m/../../evil.iwd",
            "mods/other/x.iwd",
            "main/iw_99.iwd",
            "mods/m/run.exe",
            "mods/m/sub/x.iwd",
            "/etc/evil.iwd",
            "usermaps/mp_y/mp_y.ff",
            "usermaps/mp_x/../../evil.ff",
            "mods/m/x.iwd:stream",
            "mods/m/.x.iwd",
        ];
        let any = srv.path().join("any");
        let net = MemNet::new();
        let mut names = evil.to_vec();
        names.push("mods/m/ok.iwd");
        let host = Host::start(
            &net,
            SERVER,
            Some((list(&names), String::new())),
            move |_| Some(any.clone()),
        );
        run_sync(&net, &root, "mods/m", "mp_x").unwrap();
        assert_eq!(host.asked(), ["mods/m/ok.iwd"]);
        let mut written: Vec<String> = Vec::new();
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push(
                        p.strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        walk(cli.path(), cli.path(), &mut written);
        assert_eq!(written, ["install/mods/m/ok.iwd"]);
    }

    #[test]
    fn a_file_that_is_not_the_one_listed_is_not_kept() {
        let (srv, cli) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        put(srv.path(), "x", &bytes(2, 3_000));
        let x = srv.path().join("x");
        let listed = File {
            name: "mods/m/a.iwd".into(),
            size: 3_000,
            crc: 12345,
        };
        let net = MemNet::new();
        let _host = Host::start(
            &net,
            SERVER,
            Some((vec![listed], String::new())),
            move |_| Some(x.clone()),
        );
        let err = run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(!cli.path().join("mods/m/a.iwd").exists());
    }

    #[test]
    fn a_server_that_gives_no_files_matters_only_when_something_is_lacking() {
        let cli = tempfile::tempdir().unwrap();
        put(cli.path(), "zone/english/mp_stock.ff", b"");
        let net = MemNet::new();
        let _host = Host::start(&net, SERVER, None, |_| None);
        let err = run_sync(&net, cli.path(), "mods/m", "mp_stock").unwrap_err();
        assert!(err.contains("mods/m"), "{err}");
        let err = run_sync(&net, cli.path(), "", "mp_gone").unwrap_err();
        assert!(err.contains("mp_gone"), "{err}");
        std::fs::create_dir_all(cli.path().join("mods/m")).unwrap();
        run_sync(&net, cli.path(), "mods/m", "mp_stock").unwrap();
    }

    #[test]
    fn a_stock_map_the_install_has_is_not_fetched_but_a_custom_map_is_checked() {
        let (srv, cli) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        put(srv.path(), "usermaps/mp_x/mp_x.ff", &bytes(7, 5_000));
        let offer = Arc::new(server::fastdl::Offer::build(srv.path(), "", Some("mp_x")));
        put(cli.path(), "zone/english/mp_x.ff", b"stock");
        let net = MemNet::new();
        let o = offer.clone();
        let host = Host::start(
            &net,
            SERVER,
            Some((offer.files.clone(), String::new())),
            move |n| o.path(n).map(Path::to_owned),
        );
        run_sync(&net, cli.path(), "", "mp_x").unwrap();
        assert!(host.asked().is_empty());
    }

    #[test]
    fn what_the_install_lacks_is_the_mod_folder_and_the_maps_zone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "zone/english/mp_stock.ff",
            "usermaps/mp_custom/mp_custom.ff",
            "mods/m/mp_inmod.ff",
        ] {
            put(root, f, b"");
        }
        assert_eq!(lacks(root, "", "mp_stock"), None);
        assert_eq!(lacks(root, "", "mp_custom"), None);
        assert!(
            lacks(root, "", "mp_inmod").is_some(),
            "the mod is not running"
        );
        assert_eq!(lacks(root, "mods/m", "mp_inmod"), None);
        assert!(lacks(root, "", "mp_gone").is_some());
        assert!(
            lacks(root, "mods/none", "mp_stock")
                .unwrap()
                .contains("mods/none")
        );
        assert!(
            !wanted(root, "", "mp_stock"),
            "a stock map on a stock server"
        );
        assert!(
            wanted(root, "", "mp_custom"),
            "a custom map is checked against the server's"
        );
        assert!(wanted(root, "mods/m", "mp_stock"), "a mod");
        assert!(wanted(root, "", "mp_gone"));
    }

    #[test]
    fn a_web_server_cannot_be_a_way_into_the_players_network() {
        use std::net::IpAddr;
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let game = ip("203.0.113.9");
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!allowed(ip(bad), game), "{bad}");
        }
        for good in ["8.8.8.8", "93.184.216.34", "2606:4700::1111"] {
            assert!(allowed(ip(good), game), "{good}");
        }
        // The game server's own address is allowed wherever it is: a LAN server may serve from the LAN.
        assert!(allowed(ip("192.168.1.5"), ip("192.168.1.5")));
        assert!(allowed(ip("127.0.0.1"), ip("127.0.0.1")));
        assert!(!allowed(ip("192.168.1.6"), ip("192.168.1.5")));
    }

    #[test]
    fn a_web_server_on_a_local_address_or_that_redirects_is_not_followed() {
        // The game server is somewhere else, the web server is on this machine: refused, and the files still come.
        let far = SocketAddr::from(([203, 0, 113, 9], 1));
        for redirect in [false, true] {
            let (srv, cli, other) = (
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
            );
            let offer = Arc::new(server_install(srv.path()));
            // What a redirect points at, on the game server's own address, which is the one place it is allowed.
            let target = Web::start(srv.path().to_owned(), Mode::Good);
            let web = Web::start(
                srv.path().to_owned(),
                if redirect {
                    Mode::Redirect(target.addr)
                } else {
                    Mode::Good
                },
            );
            let net = MemNet::new();
            let o = offer.clone();
            let server = if redirect { SERVER } else { far };
            let host = Host::start(
                &net,
                server,
                Some((offer.files.clone(), web.base())),
                move |n| o.path(n).map(Path::to_owned),
            );
            run_sync_from(&net, server, cli.path(), "mods/m", "mp_x").unwrap();
            for f in ALL {
                same(srv.path(), cli.path(), f);
            }
            assert!(
                !host.asked().is_empty(),
                "the server's own transfer did the work"
            );
            assert!(target.hits().is_empty(), "no redirect was followed");
            if !redirect {
                assert!(web.hits().is_empty(), "no connection to a local address");
            }
            drop(other);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_way_is_not_written_through() {
        let (srv, cli, away) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let offer = Arc::new(server_install(srv.path()));
        std::fs::create_dir_all(cli.path().join("mods")).unwrap();
        std::os::unix::fs::symlink(away.path(), cli.path().join("mods/m")).unwrap();
        let net = MemNet::new();
        let o = offer.clone();
        let _host = Host::start(
            &net,
            SERVER,
            Some((offer.files.clone(), String::new())),
            move |n| o.path(n).map(Path::to_owned),
        );
        let err = run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap_err();
        assert!(err.contains("link"), "{err}");
        assert_eq!(std::fs::read_dir(away.path()).unwrap().count(), 0);
        // A stale `.part` that is a link is replaced, not followed.
        std::fs::remove_file(cli.path().join("mods/m")).unwrap();
        std::fs::create_dir_all(cli.path().join("mods/m")).unwrap();
        let victim = away.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, cli.path().join("mods/m/a.iwd.part")).unwrap();
        run_sync(&net, cli.path(), "mods/m", "mp_x").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        same(srv.path(), cli.path(), "mods/m/a.iwd");
    }
}
