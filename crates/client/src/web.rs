// SPDX-License-Identifier: GPL-3.0-or-later
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
use std::path::PathBuf;
use std::sync::Arc;
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
