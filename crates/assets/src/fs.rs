// SPDX-License-Identifier: GPL-3.0-or-later
//! File access for the original install, the one place the code base touches `std::fs` for install content.
//!
//! Native targets are `std::fs`. A browser has no file system: on `wasm32` the page [`mount`]s the files it can read
//! (a picked folder, the OPFS copy of it) as [`ReadAt`] sources under [`WEB_ROOT`], and the same calls (`File::open`,
//! [`read_dir`], [`exists`] and so on) answer from that index. Nothing is copied: a [`File`] reads its source on
//! demand.

use std::io::{self, BufReader};
use std::path::Path;

/// Bytes a [`buffered`] reader asks the file for at a time: a browser file read is a round trip to a worker.
#[cfg(not(target_arch = "wasm32"))]
const READ_CHUNK: usize = 1 << 16;
#[cfg(target_arch = "wasm32")]
const READ_CHUNK: usize = 1 << 20;

/// Opens `path` for sequential reading, the way zones are streamed.
pub fn buffered(path: impl AsRef<Path>) -> io::Result<BufReader<File>> {
    File::open(path).map(|f| BufReader::with_capacity(READ_CHUNK, f))
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::path::Path;

    pub use std::fs::{File, read, read_dir, read_to_string};

    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    pub fn is_dir(path: &Path) -> bool {
        path.is_dir()
    }

    pub fn is_file(path: &Path) -> bool {
        path.is_file()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
pub use web::*;
