// SPDX-License-Identifier: GPL-3.0-only
//! Fetching a map's zone from the server that is playing it. Wire compatibility with the original's download
//! protocol is not a goal, so this is a small native one over the out-of-band packets ([`Oob::GetFile`],
//! [`Oob::FileChunk`]); it needs no connection, so a client can fetch what it is missing before it joins.
//!
//! The client asks for a [`WINDOW`] of [`CHUNK`]-byte pieces starting at the first one it lacks, keeps every piece of
//! the window that arrives (in any order) and asks again from the first it still lacks when the window goes quiet. The `getfile` carries the server's
//! challenge for the client's address, so a forged source address cannot make the server send a window to a victim.

use crate::oob::Oob;
use crate::transport::Transport;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use web_time::Instant;

/// Bytes of file in one packet.
pub const CHUNK: usize = 1024;
/// Packets a server sends for one request.
pub const WINDOW: usize = 32;
/// What a server says to a `getfile` for a file it will not give.
pub const NOT_AVAILABLE: &str = "That file is not available";
/// What a server says to a `getfile` whose challenge is not (or no longer) its own.
pub const BAD_CHALLENGE: &str = "Bad challenge.";
/// A challenge is good for 10 to 20 s; the client takes a new one this often.
const CHALLENGE_REFRESH: Duration = Duration::from_secs(4);
/// How long a request may go unanswered before it is made again, and how long a window that has begun arriving may
/// go quiet before its missing pieces are asked for.
const REQUEST_SILENCE: Duration = Duration::from_millis(600);
const WINDOW_SILENCE: Duration = Duration::from_millis(200);
/// Windows asked for in a row without a byte arriving before the download is given up.
const MAX_RETRIES: u32 = 25;
/// How long a `getchallenge` may go unanswered before it is sent again, and how many times it is sent.
const CHALLENGE_WAIT: Duration = Duration::from_millis(700);
const CHALLENGE_TRIES: u32 = 8;

/// Largest file a client accepts: above any stock zone, below what a hostile server could use to fill a disk or memory.
pub const MAX_TOTAL: u64 = 256 << 20;

/// A map name a client may ask for (a custom map's IWD is that name and `.iwd`): lower-case letters, digits and `_`, so
/// it cannot name a path.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The packets that answer a `getfile` for `offset` of the file at `path`: up to [`WINDOW`] pieces (none past the
/// end; an empty file is answered by one empty piece so the client learns it is done).
pub fn window(path: &Path, offset: u64) -> io::Result<Vec<Oob>> {
    let mut f = File::open(path)?;
    let total = f.metadata()?.len();
    if offset > total {
        return Ok(Vec::new());
    }
    f.seek(SeekFrom::Start(offset))?;
    let want = ((total - offset) as usize).min(WINDOW * CHUNK);
    let mut bytes = vec![0u8; want];
    f.read_exact(&mut bytes)?;
    if bytes.is_empty() {
        return Ok(vec![Oob::FileChunk {
            total,
            offset,
            data: Vec::new(),
        }]);
    }
    Ok(bytes
        .chunks(CHUNK)
        .enumerate()
        .map(|(i, c)| Oob::FileChunk {
            total,
            offset: offset + (i * CHUNK) as u64,
            data: c.to_vec(),
        })
        .collect())
}

/// How far a download has come; shared with whoever draws the progress screen.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

/// Where a download's bytes go: a file on disk, or memory (the browser has no disk to write to).
pub trait Sink {
    /// The size the file will have, known with the first piece.
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    fn write_at(&mut self, at: u64, data: &[u8]) -> io::Result<()>;
    /// Every piece is in.
    fn finish(&mut self) -> io::Result<()>;
}

impl Sink for File {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        File::set_len(self, len)
    }

    fn write_at(&mut self, at: u64, data: &[u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(at))?;
        self.write_all(data)
    }

    fn finish(&mut self) -> io::Result<()> {
        self.flush()
    }
}

impl Sink for Vec<u8> {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.resize(len as usize, 0);
        Ok(())
    }

    fn write_at(&mut self, at: u64, data: &[u8]) -> io::Result<()> {
        let at = at as usize;
        self.get_mut(at..at + data.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?
            .copy_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Phase {
    /// Waiting for the server's challenge: `getchallenge` was sent `tries` times, the last answer is due by `until`.
    Challenge { tries: u32, until: Instant },
    /// A `getfile` is out: the first piece is due by `until`, and once pieces arrive each is due within
    /// [`WINDOW_SILENCE`] of the last. `new` pieces have come in this window.
    Window { until: Instant, new: usize },
}

/// One file being fetched, driven by [`Fetcher::poll`]: it never blocks beyond the wait it is given, so a browser can
/// drive it a frame at a time over its one WebTransport session and a thread can loop on it ([`fetch`]).
pub struct Fetcher {
    server: SocketAddr,
    name: String,
    buf: Vec<u8>,
    phase: Phase,
    challenge: u32,
    challenge_at: Instant,
    /// The server said the challenge is not (or no longer) its own: the next window takes a new one.
    stale: bool,
    /// Which pieces are in, once the size is known.
    have: Vec<bool>,
    total: Option<u64>,
    offset: u64,
    /// The size the file is known to have (a manifest said): a first piece that claims another is refused.
    expect: Option<u64>,
    /// Windows asked for in a row without a byte arriving.
    idle: u32,
}

impl Fetcher {
    /// Starts fetching `name` from `server`: the first `getchallenge` goes out now.
    pub fn new(t: &mut dyn Transport, server: SocketAddr, name: &str) -> Self {
        t.send_to(server, &Oob::GetChallenge.encode());
        Self {
            server,
            name: name.to_owned(),
            buf: vec![0u8; 2048],
            phase: Phase::Challenge {
                tries: 1,
                until: Instant::now() + CHALLENGE_WAIT,
            },
            challenge: 0,
            challenge_at: Instant::now(),
            stale: false,
            have: Vec::new(),
            total: None,
            offset: 0,
            expect: None,
            idle: 0,
        }
    }

    /// The file is known to be `size` bytes: nothing is allocated for a server that claims another.
    pub fn expecting(mut self, size: u64) -> Self {
        self.expect = Some(size);
        self
    }

    /// Takes in what has arrived (waiting up to `wait` for the first packet, where the transport can wait) and sends
    /// what is due. `None` while the download goes on, the outcome once it ends. The file is whole in `sink`, and
    /// `finish` has been called, on `Some(Ok)`.
    pub fn poll(
        &mut self,
        t: &mut dyn Transport,
        sink: &mut dyn Sink,
        progress: &Progress,
        wait: Duration,
    ) -> Option<Result<(), String>> {
        let mut wait = wait;
        loop {
            let got = t.recv_from(&mut self.buf, Some(wait));
            wait = Duration::ZERO;
            match got {
                Ok(Some((n, from))) if from == self.server => {
                    if let Some(o) = Oob::parse(&self.buf[..n])
                        && let Some(end) = self.take(t, sink, progress, o)
                    {
                        return Some(end);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => return Some(Err(e.to_string())),
            }
        }
        self.tick(t, sink, Instant::now())
    }

    /// A packet from the server.
    fn take(
        &mut self,
        t: &mut dyn Transport,
        sink: &mut dyn Sink,
        progress: &Progress,
        o: Oob,
    ) -> Option<Result<(), String>> {
        match (&self.phase, o) {
            (Phase::Challenge { .. }, Oob::Challenge(c)) => {
                self.challenge = c;
                self.challenge_at = Instant::now();
                self.ask(t);
                None
            }
            (Phase::Challenge { .. }, Oob::Error(why)) => Some(Err(why)),
            (
                Phase::Window { new, .. },
                Oob::FileChunk {
                    total: tot,
                    offset: at,
                    data,
                },
            ) => {
                let mut new = *new;
                if self.total.is_some_and(|t| t != tot) {
                    return Some(Err("The file changed during the download".to_owned()));
                }
                if self.total.is_none() {
                    if self.expect.is_some_and(|e| e != tot) {
                        return Some(Err("The file is not the one the server listed".to_owned()));
                    }
                    if tot > MAX_TOTAL {
                        return Some(Err("The file is too large".to_owned()));
                    }
                    self.total = Some(tot);
                    self.have = vec![false; (tot as usize).div_ceil(CHUNK)];
                    progress.total.store(tot, Ordering::Relaxed);
                    if let Err(e) = sink.set_len(tot) {
                        return Some(Err(e.to_string()));
                    }
                }
                if tot == 0 {
                    return self.end_window(t, sink, new);
                }
                let (idx, window_end) = (
                    (at / CHUNK as u64) as usize,
                    tot.min(self.offset + (WINDOW * CHUNK) as u64),
                );
                let expected = (tot - at.min(tot)).min(CHUNK as u64) as usize;
                if at % CHUNK as u64 != 0
                    || at < self.offset
                    || at >= window_end
                    || data.len() != expected
                {
                    logging::dedup!(
                        WARN,
                        "chunk",
                        at,
                        len = data.len(),
                        expected,
                        "unexpected download chunk dropped"
                    );
                    return None;
                }
                if !self.have[idx] {
                    if let Err(e) = sink.write_at(at, &data) {
                        return Some(Err(e.to_string()));
                    }
                    self.have[idx] = true;
                    new += 1;
                    progress
                        .done
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                let first = (self.offset / CHUNK as u64) as usize;
                let last = (window_end as usize).div_ceil(CHUNK);
                if self.have[first..last].iter().all(|h| *h) {
                    return self.end_window(t, sink, new);
                }
                self.phase = Phase::Window {
                    until: Instant::now() + WINDOW_SILENCE,
                    new,
                };
                None
            }
            (Phase::Window { new, .. }, Oob::Error(why)) if why == BAD_CHALLENGE => {
                // A stale challenge: take a new one and ask again; this window brought what it brought.
                let new = *new;
                self.stale = true;
                self.end_window(t, sink, new)
            }
            (Phase::Window { .. }, Oob::Error(why)) => Some(Err(why)),
            _ => None,
        }
    }

    /// The clock: a request unanswered for too long is made again.
    fn tick(
        &mut self,
        t: &mut dyn Transport,
        sink: &mut dyn Sink,
        now: Instant,
    ) -> Option<Result<(), String>> {
        match self.phase {
            Phase::Challenge { tries, until } if now >= until => {
                if tries >= CHALLENGE_TRIES {
                    return Some(Err("The server does not answer".into()));
                }
                t.send_to(self.server, &Oob::GetChallenge.encode());
                self.phase = Phase::Challenge {
                    tries: tries + 1,
                    until: now + CHALLENGE_WAIT,
                };
                None
            }
            Phase::Window { until, new } if now >= until => self.end_window(t, sink, new),
            _ => None,
        }
    }

    /// Sends the `getfile` for the window starting at `offset`.
    fn ask(&mut self, t: &mut dyn Transport) {
        t.send_to(
            self.server,
            &Oob::GetFile {
                challenge: self.challenge,
                name: self.name.clone(),
                offset: self.offset,
            }
            .encode(),
        );
        self.phase = Phase::Window {
            until: Instant::now() + REQUEST_SILENCE,
            new: 0,
        };
    }

    /// The window is over (complete, or gone quiet): the download is done, or the next window starts.
    fn end_window(
        &mut self,
        t: &mut dyn Transport,
        sink: &mut dyn Sink,
        new: usize,
    ) -> Option<Result<(), String>> {
        if self.total.is_some() {
            match self.have.iter().position(|h| !*h) {
                None => return Some(sink.finish().map_err(|e| e.to_string())),
                Some(i) => self.offset = (i * CHUNK) as u64,
            }
        }
        self.idle = if new == 0 { self.idle + 1 } else { 0 };
        if self.idle >= MAX_RETRIES {
            return Some(Err("The download timed out".into()));
        }
        if std::mem::take(&mut self.stale) || self.challenge_at.elapsed() > CHALLENGE_REFRESH {
            t.send_to(self.server, &Oob::GetChallenge.encode());
            self.phase = Phase::Challenge {
                tries: 1,
                until: Instant::now() + CHALLENGE_WAIT,
            };
        } else {
            self.ask(t);
        }
        None
    }
}

/// Downloads the file `name` (a map zone, or a custom map's `<map>.iwd`) from `server` into `dest` (written as `dest.part` and renamed when whole). Blocks
/// until it is done, fails or `cancel` is set; `progress` is updated as pieces arrive.
pub fn fetch(
    t: &mut dyn Transport,
    server: SocketAddr,
    name: &str,
    dest: &Path,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let mut out = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut f = Fetcher::new(t, server, name);
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("Download cancelled".to_owned());
        }
        // Wake often enough to notice a cancel.
        if let Some(r) = f.poll(t, &mut out, progress, Duration::from_millis(50)) {
            break r;
        }
    };
    drop(out);
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

    #[test]
    fn a_file_arrives_whole_despite_a_lost_piece_and_a_refusal_ends_it() {
        use crate::connect::{Gate, serve};
        use crate::oob::Challenger;
        use crate::transport::MemNet;
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!("cod4e-dl-fetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("mp_x.ff");
        let bytes: Vec<u8> = (0..(WINDOW * CHUNK * 2 + 777))
            .map(|i| (i * 7) as u8)
            .collect();
        std::fs::write(&src, &bytes).unwrap();

        let net = MemNet::new();
        let server = SocketAddr::from(([127, 0, 0, 1], 1));
        let mut st = net.endpoint(server);
        let mut ct = net.endpoint(SocketAddr::from(([127, 0, 0, 1], 2)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let served = src.clone();
        let handle = std::thread::spawn(move || {
            let ch = Challenger::new();
            let mut buf = [0u8; 2048];
            let mut dropped = false;
            while !thread_stop.load(Ordering::Relaxed) {
                let Ok(Some((n, from))) = st.recv_from(&mut buf, Some(Duration::from_millis(10)))
                else {
                    continue;
                };
                match serve(&ch, 0, from, Oob::parse(&buf[..n]).unwrap(), Vec::new) {
                    Gate::Reply(r) => st.send_to(from, &r.encode()),
                    Gate::File { name, offset, .. } if name == "mp_x" => {
                        for c in window(&served, offset).unwrap() {
                            if let Oob::FileChunk { offset: at, .. } = &c
                                && *at == 5 * CHUNK as u64
                                && !dropped
                            {
                                dropped = true;
                                continue;
                            }
                            st.send_to(from, &c.encode());
                        }
                    }
                    Gate::File { from, .. } => {
                        st.send_to(from, &Oob::Error(NOT_AVAILABLE.into()).encode())
                    }
                    _ => {}
                }
            }
        });

        let cancel = AtomicBool::new(false);
        let progress = Progress::default();
        let dest = dir.join("got").join("mp_x.ff");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fetch(&mut ct, server, "mp_x", &dest, &progress, &cancel).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert_eq!(progress.done.load(Ordering::Relaxed), bytes.len() as u64);
        assert!(!dest.with_extension("ff.part").exists());

        let nope = dir.join("got").join("mp_y.ff");
        let err = fetch(&mut ct, server, "mp_y", &nope, &progress, &cancel).unwrap_err();
        assert_eq!(err, NOT_AVAILABLE);
        assert!(!nope.exists() && !nope.with_extension("ff.part").exists());
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_download_into_memory_goes_a_poll_at_a_time_on_one_thread() {
        use crate::connect::{Gate, serve};
        use crate::oob::Challenger;
        use crate::transport::MemNet;

        // What the browser does: one thread, no waiting inside `poll`, the server answered between polls.
        let dir = std::env::temp_dir().join(format!("cod4e-dl-poll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("mp_x.ff");
        let bytes: Vec<u8> = (0..(WINDOW * CHUNK + 300))
            .map(|i| (i * 13) as u8)
            .collect();
        std::fs::write(&src, &bytes).unwrap();
        let net = MemNet::new();
        let server = SocketAddr::from(([127, 0, 0, 1], 1));
        let mut st = net.endpoint(server);
        let mut ct = net.endpoint(SocketAddr::from(([127, 0, 0, 1], 2)));
        let ch = Challenger::new();
        let progress = Progress::default();
        let mut buf = [0u8; 2048];
        let mut drive = |ct: &mut dyn Transport, f: &mut Fetcher, got: &mut Vec<u8>| {
            (0..1000).find_map(|_| {
                while let Ok(Some((n, from))) = st.recv_from(&mut buf, Some(Duration::ZERO)) {
                    match serve(&ch, 0, from, Oob::parse(&buf[..n]).unwrap(), Vec::new) {
                        Gate::Reply(r) => st.send_to(from, &r.encode()),
                        Gate::File { offset, from, .. } => {
                            for c in window(&src, offset).unwrap() {
                                st.send_to(from, &c.encode());
                            }
                        }
                        _ => {}
                    }
                }
                f.poll(ct, got, &progress, Duration::ZERO)
            })
        };
        let mut got: Vec<u8> = Vec::new();
        let mut f = Fetcher::new(&mut ct, server, "mp_x").expecting(bytes.len() as u64);
        assert_eq!(drive(&mut ct, &mut f, &mut got), Some(Ok(())));
        assert_eq!(got, bytes);
        // A server whose file is not the size the manifest listed is refused before anything is allocated.
        let mut wrong = Fetcher::new(&mut ct, server, "mp_x").expecting(bytes.len() as u64 + 1);
        let mut none: Vec<u8> = Vec::new();
        assert!(matches!(
            drive(&mut ct, &mut wrong, &mut none),
            Some(Err(_))
        ));
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_server_claiming_an_absurd_size_is_refused_without_allocating() {
        use crate::connect::{Gate, serve};
        use crate::oob::Challenger;
        use crate::transport::MemNet;
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!("cod4e-dl-hostile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let net = MemNet::new();
        let server = SocketAddr::from(([127, 0, 0, 1], 1));
        let mut st = net.endpoint(server);
        let mut ct = net.endpoint(SocketAddr::from(([127, 0, 0, 1], 2)));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let ch = Challenger::new();
            let mut buf = [0u8; 2048];
            while !flag.load(Ordering::Relaxed) {
                let Ok(Some((n, from))) = st.recv_from(&mut buf, Some(Duration::from_millis(10)))
                else {
                    continue;
                };
                match serve(&ch, 0, from, Oob::parse(&buf[..n]).unwrap(), Vec::new) {
                    Gate::Reply(r) => st.send_to(from, &r.encode()),
                    Gate::File { from, .. } => st.send_to(
                        from,
                        &Oob::FileChunk {
                            total: 1 << 56,
                            offset: 0,
                            data: vec![0; CHUNK],
                        }
                        .encode(),
                    ),
                    _ => {}
                }
            }
        });
        let dest = dir.join("mp_big.ff");
        let err = fetch(
            &mut ct,
            server,
            "mp_big",
            &dest,
            &Progress::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(err, "The file is too large");
        assert!(!dest.exists());
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_plain_map_names_pass() {
        assert!(valid_name("mp_crash_snow"));
        for bad in [
            "",
            "../x",
            "mp/crash",
            "MP_X",
            "a b",
            "x.ff",
            &"a".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_window_covers_the_file_in_order_and_stops_at_its_end() {
        let dir = std::env::temp_dir().join(format!("cod4e-dl-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.bin");
        let bytes: Vec<u8> = (0..(WINDOW * CHUNK + 500)).map(|i| i as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let first = window(&path, 0).unwrap();
        assert_eq!(first.len(), WINDOW);
        let tail = window(&path, (WINDOW * CHUNK) as u64).unwrap();
        assert_eq!(tail.len(), 1);
        match &tail[0] {
            Oob::FileChunk {
                total,
                offset,
                data,
            } => {
                assert_eq!(
                    (*total, *offset),
                    (bytes.len() as u64, (WINDOW * CHUNK) as u64)
                );
                assert_eq!(data[..], bytes[WINDOW * CHUNK..]);
            }
            _ => unreachable!(),
        }
        assert!(window(&path, bytes.len() as u64 + 1).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
