// SPDX-License-Identifier: GPL-3.0-only
//! Joining a server from a page. Natively `app::run` asks the server by UDP which map and mod it runs, fetches
//! what the install lacks, and only then starts the window; a browser cannot block for that (the page's WebTransport
//! delivers datagrams between frames), so the same steps run as an async task over the page's one session before the
//! client starts. What is fetched has no file to go to: it is held in memory, mounted in the install the client reads
//! ([`assets::fs::add`]) and handed to the page, which keeps it in the browser for the next visit.

use crate::Cli;
use crate::download;
use crate::input::config;
use crate::profile;
use crate::serverlist::{self, Entry};
use crate::web::{self, Wire};
use assets::vfs::ReadAt;
use net::Transport;
use net::download::{Fetcher, Progress};
use net::manifest::{self, Assembler, File};
use net::oob::Oob;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_time::Instant;

/// How long the session may take to open, and how long the server may take to say what it runs once it has.
const OPEN_WITHIN: Duration = Duration::from_secs(10);
const ANSWER_WITHIN: Duration = Duration::from_secs(5);
/// Most a browser takes from one server: the files are held in memory while the page runs.
const WEB_MAX_BYTES: u64 = 512 << 20;
/// How long the server may take to list its files.
const LIST_WITHIN: Duration = Duration::from_secs(10);

/// Lets the browser run for `ms`: it delivers the session's datagrams between tasks.
pub async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w
                .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Waits for the page's session to open.
async fn opened() -> Result<(), String> {
    let started = Instant::now();
    while !web::wire_open() {
        if let Some(why) = web::wire_failure() {
            return Err(why);
        }
        if started.elapsed() > OPEN_WITHIN {
            return Err("the server did not answer".to_owned());
        }
        sleep(20).await;
    }
    Ok(())
}

/// Asks the server over `wire` for its map and settings.
async fn query(wire: &mut Wire, server: SocketAddr) -> Result<Entry, String> {
    let challenge = ((js_sys::Math::random() * f64::from(u32::MAX)) as u32).max(1);
    let (started, mut next) = (Instant::now(), Instant::now());
    let mut buf = vec![0u8; 2048];
    loop {
        if Instant::now() >= next {
            wire.send_to(server, &Oob::GetInfo(challenge).encode());
            next = Instant::now() + Duration::from_millis(300);
        }
        while let Ok(Some((n, from))) = wire.recv_from(&mut buf, None) {
            if let Some(Oob::InfoResponse(kv)) = Oob::parse(&buf[..n])
                && kv
                    .iter()
                    .any(|(k, v)| k == "challenge" && v.parse() == Ok(challenge))
                && let Some(e) = Entry::from_info(from, &kv, started.elapsed().as_millis() as i32)
            {
                return Ok(e);
            }
        }
        if let Some(why) = web::wire_failure() {
            return Err(why);
        }
        if started.elapsed() > ANSWER_WITHIN {
            return Err(format!("{server} did not answer"));
        }
        sleep(20).await;
    }
}

/// Asks the server over `wire` for its manifest (see [`manifest::fetch_list`], which this is the waiting-free form of).
async fn list(wire: &mut Wire, server: SocketAddr) -> Result<(Vec<File>, String), String> {
    let mut buf = vec![0u8; 2048];
    let mut challenge: Option<u32> = None;
    let mut asm = Assembler::default();
    let (started, mut sent) = (Instant::now(), None::<Instant>);
    loop {
        if started.elapsed() > LIST_WITHIN {
            return Err("The server does not answer".into());
        }
        if sent.is_none_or(|s| s.elapsed() > Duration::from_millis(500)) {
            let ask = match challenge {
                Some(challenge) => Oob::GetManifest { challenge },
                None => Oob::GetChallenge,
            };
            wire.send_to(server, &ask.encode());
            sent = Some(Instant::now());
        }
        while let Ok(Some((n, from))) = wire.recv_from(&mut buf, None) {
            if from != server {
                continue;
            }
            match Oob::parse(&buf[..n]) {
                Some(Oob::Challenge(c)) => {
                    challenge = Some(c);
                    sent = None;
                }
                Some(Oob::Manifest {
                    index,
                    count,
                    file,
                    www,
                }) => {
                    asm.add(index, count, file, www)?;
                    if let Some((files, www)) = asm.done() {
                        return Ok((files, www.to_owned()));
                    }
                }
                Some(Oob::Error(why)) if why == net::download::BAD_CHALLENGE => {
                    challenge = None;
                    sent = None;
                }
                Some(Oob::Error(why)) => return Err(why),
                _ => {}
            }
        }
        if let Some(why) = web::wire_failure() {
            return Err(why);
        }
        sleep(20).await;
    }
}

/// Fetches `file` into memory, shows how far it is, and checks it is the file the manifest described.
async fn download(
    wire: &mut Wire,
    server: SocketAddr,
    file: &File,
    note: &str,
) -> Result<Vec<u8>, String> {
    let progress = Progress::default();
    progress.total.store(file.size, Ordering::Relaxed);
    let mut bytes: Vec<u8> = Vec::new();
    let mut fetcher = Fetcher::new(wire, server, &file.name).expecting(file.size);
    let mut shown: Option<Instant> = None;
    loop {
        let before = progress.done.load(Ordering::Relaxed);
        if let Some(end) = fetcher.poll(wire, &mut bytes, &progress, Duration::ZERO) {
            end?;
            break;
        }
        let arrived = progress.done.load(Ordering::Relaxed) != before;
        if let Some(why) = web::wire_failure() {
            return Err(why);
        }
        if shown.is_none_or(|t| t.elapsed() >= Duration::from_millis(250)) {
            shown = Some(Instant::now());
            let done = progress.done.load(Ordering::Relaxed);
            web::show_progress(&format!(
                "{note}: {:.1} of {:.1} MB",
                done as f64 / 1e6,
                file.size as f64 / 1e6
            ));
        }
        // Pieces are arriving: give the browser just long enough to deliver the next ones.
        sleep(if arrived { 0 } else { 10 }).await;
    }
    if bytes.len() as u64 != file.size || assets::crc::crc32(&bytes) != file.crc {
        return Err(format!("{} does not match the server's copy", file.name));
    }
    Ok(bytes)
}

/// Fetches from the server everything the install lacks to run `game` and play `map`, as `download::Download` does
/// natively: the files are mounted for the client and kept by the page.
async fn sync(
    wire: &mut Wire,
    server: SocketAddr,
    cli: &Cli,
    game: &str,
    map: &str,
    allowed: bool,
) -> Result<(), String> {
    let lacks = |why: &str| {
        download::lacks(&cli.install, game, map).map(|what| format!("You do not have {what}{why}"))
    };
    if !allowed {
        return lacks(", and downloading is turned off (cl_allowDownload).").map_or(Ok(()), Err);
    }
    web::show_progress("Asking the server for its files");
    let (listing, _www) = match list(wire, server).await {
        Ok(l) => l,
        Err(why) if why == manifest::DOWNLOADS_OFF => {
            return lacks(", and this server does not allow downloads").map_or(Ok(()), Err);
        }
        Err(why) => return Err(why),
    };
    let todo = download::plan(&cli.install, game, map, listing);
    let count = todo.len();
    let need: u64 = todo.iter().map(|(f, _)| f.size).sum();
    if need > manifest::MAX_SUM.min(WEB_MAX_BYTES) {
        return Err("The server's files are too large for this browser".into());
    }
    if let Some(free) = web::storage_free()
        && free < need
    {
        return Err(format!(
            "There is not enough room in this browser's storage for the server's files ({:.0} MB needed, {:.0} MB free)",
            need as f64 / 1e6,
            free as f64 / 1e6
        ));
    }
    for (i, (file, dest)) in todo.into_iter().enumerate() {
        let note = format!("Downloading {} ({} of {count})", file.name, i + 1);
        let bytes = download(wire, server, &file, &note).await?;
        web::save_download(&dest, &bytes);
        assets::fs::add(&dest, Arc::new(bytes) as Arc<dyn ReadAt>);
    }
    web::show_progress("");
    Ok(())
}

/// Finds out what the server runs and makes `cli` play it: its mod and its map, fetched first when the install lacks
/// them and the player allows downloads.
pub async fn prepare(cli: &mut Cli) -> Result<(), String> {
    let Some(target) = cli.connect.clone() else {
        return Ok(());
    };
    let server = serverlist::resolve(&target)?;
    let mut wire = Wire::open(&web::server_target(), server);
    opened().await?;
    let e = query(&mut wire, server).await?;
    if !assets::paths::valid_game(&e.game) {
        return Err(format!("The server runs an invalid mod ({}).", e.game));
    }
    if download::wanted(&cli.install, &e.game, &e.map) {
        // `cl_allowDownload` is the player's, kept in the settings the client has not read yet. A browser has one
        // profile (profiles are folders it has none of), so the active one is the default.
        let kept =
            web::pref_get(&profile::web_config_key(profile::DEFAULT_PROFILE).to_string_lossy());
        let allowed = kept
            .and_then(|text| config::saved_value(&text, "cl_allowDownload"))
            .is_none_or(|v| v.trim() != "0");
        let result = sync(&mut wire, server, cli, &e.game, &e.map, allowed).await;
        web::show_progress("");
        result?;
    }
    // The server's mod is the client's too.
    if e.game != cli.game {
        if !e.game.is_empty() && !assets::paths::game_dir(&cli.install, &e.game) {
            return Err(format!(
                "You do not have the mod {} this server is running.",
                e.game
            ));
        }
        cli.game = e.game;
    }
    cli.map = e.map;
    Ok(())
}
