// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser has no threads and no sockets to run a listen server on: `listen.rs` keeps the native one. The player
//! joins a server (`--connect`) instead.

use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;

pub struct Listen {
    pub addr: SocketAddr,
}

pub fn start(
    _install: &Path,
    _map: &str,
    _bots: usize,
    _gametype: Option<&str>,
) -> Result<Listen, String> {
    Err("a browser cannot host a game; join a server".into())
}

impl Listen {
    pub fn finish(&mut self) -> Value {
        Value::Null
    }
}
