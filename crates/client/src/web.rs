// SPDX-License-Identifier: GPL-3.0-only
//! The browser platform layer: how the page and the wasm module talk.
//!
//! The page (`web/`) picks the install, gives it to a reader worker, and instantiates this module. The module reads
//! the install through [`read_into`], a synchronous ranged read that the page implements by waiting for the reader
//! worker (the only way to read a `File` or an OPFS file synchronously from the thread that owns the canvas). The
//! install is mounted in [`assets::fs`], so the rest of the client opens files the way it does natively.
//!
//! Everything the page provides lives in the `cod4` global (`web/cod4.js`).

use assets::fs::{self, WEB_ROOT};
use assets::vfs::ReadAt;
use serde_json::Value;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// Fills `buf` from `offset` of install file `file`; throws when the read fails.
    #[wasm_bindgen(js_namespace = cod4, catch)]
    fn read_into(file: u32, offset: f64, buf: &mut [u8]) -> Result<(), JsValue>;

    /// The start-up configuration: `{"args": [...], "files": [[path, size], ...]}`. A file's index is its id.
    #[wasm_bindgen(js_namespace = cod4)]
    fn config() -> String;

    /// The client cannot run; the page shows `message`.
    #[wasm_bindgen(js_namespace = cod4, js_name = fatal)]
    fn page_fatal(message: &str);

    /// A line for the browser console.
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    pub fn log(message: &str);

    /// Debug overlay data, a JSON object of name to value.
    #[wasm_bindgen(js_namespace = cod4, js_name = overlay)]
    fn page_overlay(json: &str);
}

#[wasm_bindgen]
extern "C" {
    /// Opens the WebTransport session to `target` (`host:port` or an https URL); the page knows any pinned certificate.
    #[wasm_bindgen(js_namespace = cod4)]
    fn net_open(target: &str);
    #[wasm_bindgen(js_namespace = cod4)]
    fn net_send(data: &[u8]);
    /// The next received datagram copied into `buf`, as its length; -1 when none is waiting.
    #[wasm_bindgen(js_namespace = cod4)]
    fn net_recv(buf: &mut [u8]) -> i32;
    /// `idle`, `connecting`, `open` or `closed`.
    #[wasm_bindgen(js_namespace = cod4)]
    fn net_state() -> String;
    #[wasm_bindgen(js_namespace = cod4)]
    fn net_error() -> String;
    #[wasm_bindgen(js_namespace = cod4)]
    fn net_close();
}

/// The session the client plays on: [`net::Transport`] over the page's WebTransport. There is one peer, the server,
/// so the address datagrams are sent to is ignored and everything received comes from `server`. Receiving never waits:
/// the browser delivers datagrams between frames.
pub struct Wire {
    server: SocketAddr,
}

impl Wire {
    /// Opens a session to `target`; `server` is the stand-in address the net layer names the peer by.
    pub fn open(target: &str, server: SocketAddr) -> Self {
        net_open(target);
        Self { server }
    }
}

impl net::Transport for Wire {
    fn local_addr(&self) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 0))
    }

    fn send_to(&mut self, _to: SocketAddr, data: &[u8]) {
        net_send(data);
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        _timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        let n = net_recv(buf);
        Ok((n >= 0).then(|| (n as usize, self.server)))
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        net_close();
    }
}

/// Why the session cannot be used, once it has closed.
pub fn wire_failure() -> Option<String> {
    (net_state() == "closed").then(|| {
        let e = net_error();
        if e.is_empty() {
            "the connection to the server closed".to_owned()
        } else {
            e
        }
    })
}

/// The address text the player gave for the server, kept for the transport (a browser cannot resolve names).
static TARGET: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// The stand-in socket address for the server at `target` (`host:port`): the net layer names its peer by address, and
/// the browser connects by name, so the address only has to be stable.
pub fn server_addr(target: &str) -> SocketAddr {
    *TARGET.lock().unwrap() = target.to_owned();
    let port = target
        .rsplit(':')
        .next()
        .and_then(|p| p.trim_end_matches('/').parse().ok())
        .unwrap_or(443);
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// The target last given to [`server_addr`].
pub fn server_target() -> String {
    TARGET.lock().unwrap().clone()
}

/// One install file, read through the page.
struct WebFile {
    id: u32,
    len: u64,
}

impl ReadAt for WebFile {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.len)
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        debug_assert!(end <= self.len);
        read_into(self.id, offset as f64, buf).map_err(|e| io::Error::other(format!("{e:?}")))
    }
}

/// Tells the page the client has stopped, and why.
pub fn fatal(message: &str) {
    page_fatal(message);
}

/// Hands the debug overlay its values.
pub fn overlay(values: &Value) {
    page_overlay(&values.to_string());
}

/// Whether the page holds the pointer lock on the canvas.
pub fn pointer_locked() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.pointer_lock_element().is_some())
}

/// Whether the player touched the page in the last few seconds (transient user activation): what the browser wants
/// before it grants a pointer lock or fullscreen.
pub fn user_active() -> bool {
    web_sys::window()
        .and_then(|w| js_sys::Reflect::get(&w.navigator(), &"userActivation".into()).ok())
        .and_then(|a| js_sys::Reflect::get(&a, &"isActive".into()).ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Wasm linear memory in bytes. It only grows, so this is the peak.
pub fn memory_bytes() -> u64 {
    (core::arch::wasm32::memory_size::<0>() * 65536) as u64
}

/// Entry point: mounts the install the page prepared and runs the client with the page's arguments.
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        page_fatal(&format!("the client crashed: {info}"));
    }));
    if let Err(e) = boot() {
        page_fatal(&e);
    }
}

fn boot() -> Result<(), String> {
    let cfg: Value =
        serde_json::from_str(&config()).map_err(|e| format!("bad page config: {e}"))?;
    let files = cfg["files"].as_array().ok_or("page config has no files")?;
    fs::mount(files.iter().enumerate().filter_map(|(id, f)| {
        let path = f[0].as_str()?.to_owned();
        let len = f[1].as_u64()?;
        Some((
            path,
            Arc::new(WebFile { id: id as u32, len }) as Arc<dyn ReadAt>,
        ))
    }));
    let args: Vec<String> = cfg["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut cli = crate::parse(&args).map_err(|e| format!("bad arguments: {e}"))?;
    cli.install = PathBuf::from(WEB_ROOT);
    crate::app::run(cli)
}
