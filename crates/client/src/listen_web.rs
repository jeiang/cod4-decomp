// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser has no threads and no sockets to run a listen server on: `listen.rs` keeps the native one. The player
//! joins a server (`--connect`) instead.

use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;

pub struct Listen {
    pub addr: SocketAddr,
}

/// What a listen server would be started with (the menus build it; nothing starts one here).
#[allow(dead_code)] // built by the menus, read only by the native listen server
#[derive(Clone)]
pub struct Config {
    pub map: String,
    pub bots: usize,
    pub gametype: Option<String>,
    pub rotation: Option<String>,
    pub port: u16,
    pub dvars: Vec<(String, String)>,
}

pub fn free_standard_port() -> u16 {
    0
}

pub fn start(_install: &Path, _cfg: Config) -> Result<Listen, String> {
    Err("a browser cannot host a game; join a server".into())
}

impl Listen {
    pub fn finish(&mut self) -> Value {
        Value::Null
    }
}
