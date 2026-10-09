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
use std::time::{Duration, Instant};

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

/// Largest file a client accepts: above any stock zone, below what a hostile server could use to fill a disk or memory.
pub const MAX_TOTAL: u64 = 256 << 20;

/// A map name a client may ask for: lower-case letters, digits and `_`, so it cannot name a path.
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

/// What a packet did to a wait.
enum Step {
    /// Not what was waited for.
    Ignore,
    /// A piece of what was waited for: the silence clock starts over.
    More,
    /// Done waiting.
    Done,
}

/// Reads packets from `server` until `take` says [`Step::Done`] (`true`), `first` passes with no [`Step::More`] or
/// `after` passes since the last one (`false`), or `cancel` is set.
fn wait_for(
    t: &mut dyn Transport,
    server: SocketAddr,
    buf: &mut [u8],
    (first, after): (Duration, Duration),
    cancel: &AtomicBool,
    mut take: impl FnMut(Oob) -> Step,
) -> Result<bool, String> {
    let mut until = Instant::now() + first;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Download cancelled".into());
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        // Wake often enough to notice a cancel.
        match t.recv_from(buf, Some(left.min(Duration::from_millis(50)))) {
            Ok(Some((n, from))) if from == server => {
                if let Some(o) = Oob::parse(&buf[..n]) {
                    match take(o) {
                        Step::Done => return Ok(true),
                        Step::More => until = Instant::now() + after,
                        Step::Ignore => {}
                    }
                }
            }
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn challenge(
    t: &mut dyn Transport,
    server: SocketAddr,
    buf: &mut [u8],
    cancel: &AtomicBool,
) -> Result<u32, String> {
    for _ in 0..8 {
        t.send_to(server, &Oob::GetChallenge.encode());
        let mut got = None;
        let mut refused = None;
        wait_for(
            t,
            server,
            buf,
            (Duration::from_millis(700), Duration::ZERO),
            cancel,
            |o| match o {
                Oob::Challenge(c) => {
                    got = Some(c);
                    Step::Done
                }
                Oob::Error(why) => {
                    refused = Some(why);
                    Step::Done
                }
                _ => Step::Ignore,
            },
        )?;
        if let Some(why) = refused {
            return Err(why);
        }
        if let Some(c) = got {
            return Ok(c);
        }
    }
    Err("The server does not answer".into())
}

/// Downloads the map zone `name` from `server` into `dest` (written as `dest.part` and renamed when whole). Blocks
/// until it is done, fails or `cancel` is set; `progress` is updated as pieces arrive.
pub fn fetch(
    t: &mut dyn Transport,
    server: SocketAddr,
    name: &str,
    dest: &Path,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let part = dest.with_extension("ff.part");
    let mut out = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let result = fetch_into(t, server, name, &mut out, progress, cancel);
    drop(out);
    match result {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn fetch_into(
    t: &mut dyn Transport,
    server: SocketAddr,
    name: &str,
    out: &mut File,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut buf = vec![0u8; 2048];
    let mut ch = challenge(t, server, &mut buf, cancel)?;
    let mut ch_at = Instant::now();
    // Which pieces are in, once the size is known.
    let mut have: Vec<bool> = Vec::new();
    let mut total: Option<u64> = None;
    let mut offset = 0u64;
    let mut idle = 0;
    loop {
        if ch_at.elapsed() > CHALLENGE_REFRESH {
            ch = challenge(t, server, &mut buf, cancel)?;
            ch_at = Instant::now();
        }
        t.send_to(
            server,
            &Oob::GetFile {
                challenge: ch,
                name: name.to_owned(),
                offset,
            }
            .encode(),
        );
        let (mut failed, mut stale) = (None, false);
        let mut new = 0usize;
        wait_for(
            t,
            server,
            &mut buf,
            (REQUEST_SILENCE, WINDOW_SILENCE),
            cancel,
            |o| match o {
                Oob::FileChunk {
                    total: tot,
                    offset: at,
                    data,
                } => {
                    if total.is_some_and(|t| t != tot) {
                        failed = Some("The file changed during the download".to_owned());
                        return Step::Done;
                    }
                    if total.is_none() {
                        if tot > MAX_TOTAL {
                            failed = Some("The file is too large".to_owned());
                            return Step::Done;
                        }
                        total = Some(tot);
                        have = vec![false; (tot as usize).div_ceil(CHUNK)];
                        progress.total.store(tot, Ordering::Relaxed);
                        if let Err(e) = out.set_len(tot) {
                            failed = Some(e.to_string());
                            return Step::Done;
                        }
                    }
                    let (idx, window_end) = (
                        (at / CHUNK as u64) as usize,
                        tot.min(offset + (WINDOW * CHUNK) as u64),
                    );
                    let expected = (tot - at.min(tot)).min(CHUNK as u64) as usize;
                    if tot == 0 {
                        return Step::Done;
                    }
                    if at % CHUNK as u64 != 0
                        || at < offset
                        || at >= window_end
                        || data.len() != expected
                    {
                        return Step::Ignore;
                    }
                    if !have[idx] {
                        let wrote = out
                            .seek(SeekFrom::Start(at))
                            .and_then(|_| out.write_all(&data));
                        if let Err(e) = wrote {
                            failed = Some(e.to_string());
                            return Step::Done;
                        }
                        have[idx] = true;
                        new += 1;
                        progress
                            .done
                            .fetch_add(data.len() as u64, Ordering::Relaxed);
                    }
                    let first = (offset / CHUNK as u64) as usize;
                    let last = (window_end as usize).div_ceil(CHUNK);
                    if have[first..last].iter().all(|h| *h) {
                        Step::Done
                    } else {
                        Step::More
                    }
                }
                Oob::Error(why) if why == BAD_CHALLENGE => {
                    stale = true;
                    Step::Done
                }
                Oob::Error(why) => {
                    failed = Some(why);
                    Step::Done
                }
                _ => Step::Ignore,
            },
        )?;
        if let Some(e) = failed {
            return Err(e);
        }
        if stale {
            ch_at = Instant::now() - CHALLENGE_REFRESH - Duration::from_millis(1);
        }
        if total.is_some() {
            match have.iter().position(|h| !*h) {
                None => return out.flush().map_err(|e| e.to_string()),
                Some(i) => offset = (i * CHUNK) as u64,
            }
        }
        idle = if new == 0 { idle + 1 } else { 0 };
        if idle >= MAX_RETRIES {
            return Err("The download timed out".into());
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
