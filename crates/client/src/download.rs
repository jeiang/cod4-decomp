// SPDX-License-Identifier: GPL-3.0-only
//! Fetching a map the server is playing and the install lacks (`cl_allowDownload`): a worker thread asks the server
//! for the map's zone (and its loading screen's) over [`net::download`] and the window shows the loading screen with
//! the bytes arrived meanwhile. The browser build has no files to put it in, so this is native only.

use net::UdpTransport;
use net::download::{Progress, fetch};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// A download in progress, and the join it is for.
pub struct Download {
    pub map: String,
    /// The server to join and what it is playing, once the map is here.
    pub join: (SocketAddr, String),
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<(), String>>,
}

/// Whether the install at `root` lacks the zone of `map` (and could take it: it has a zone folder).
pub fn missing(root: &std::path::Path, map: &str) -> bool {
    net::download::valid_name(map)
        && server::content::zone_file(root, map).is_none()
        && server::content::zone_dir(root).is_some()
}

impl Download {
    /// Starts fetching `map` from `server` into the install at `root`.
    pub fn start(
        root: &std::path::Path,
        server: SocketAddr,
        map: &str,
        gametype: &str,
    ) -> Result<Self, String> {
        let dir = server::content::zone_dir(root)
            .ok_or("the install has no zone folder to put the map in")?;
        let progress = Arc::new(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (p, c, name) = (progress.clone(), cancel.clone(), map.to_owned());
        std::thread::Builder::new()
            .name("map-download".into())
            .spawn(move || {
                let _ = tx.send(run(&dir, server, &name, &p, &c));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            map: map.to_owned(),
            join: (server, gametype.to_owned()),
            progress,
            cancel,
            rx,
        })
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
        if total == 0 {
            "Asking the server for the map".to_owned()
        } else {
            format!(
                "Downloading the map from the server: {:.1} of {:.1} MB",
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

fn run(
    dir: &std::path::Path,
    server: SocketAddr,
    map: &str,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut t =
        UdpTransport::bind(SocketAddr::from(([0, 0, 0, 0], 0))).map_err(|e| e.to_string())?;
    let load = format!("{map}_load");
    for (name, optional) in [(map, false), (load.as_str(), true)] {
        let dest: PathBuf = dir.join(format!("{name}.ff"));
        if dest.exists() {
            continue;
        }
        progress.done.store(0, Ordering::Relaxed);
        progress.total.store(0, Ordering::Relaxed);
        match fetch(&mut t, server, name, &dest, progress, cancel) {
            Err(e) if optional && e == net::download::NOT_AVAILABLE => {}
            r => r?,
        }
    }
    Ok(())
}
