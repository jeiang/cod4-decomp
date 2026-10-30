// SPDX-License-Identifier: GPL-3.0-only
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
    pub bots: Option<usize>,
    pub bot_count: usize,
    pub gametype: Option<String>,
    pub rotation: Option<String>,
    pub port: u16,
    pub game: String,
    pub dvars: Vec<(String, String)>,
    pub cheats: bool,
}

/// No listen server, so no objective counters: bombs planted, defused, airstrikes, helicopters, helicopter shots.
pub fn objectives() -> (u64, u64, u64, u64, u64) {
    (0, 0, 0, 0, 0)
}

/// No listen server console to queue `_line` for.
pub fn send(_line: &str) {}

/// What the stock Start New Server path tells the player in the browser (the error popup's text): there is no
/// listen server to start, so the menu points at Join Game instead of failing silently.
pub const NO_HOSTING: &str = "Starting a server is not available in the browser. Use Join Game.";
/// What a listen server would make of a line the host typed; none runs here.
#[allow(dead_code)] // read by the native client, which drains a real server's replies
pub struct HostReply {
    pub line: String,
    pub output: String,
    pub forward: bool,
}

/// No listen server: the line is the client's alone.
pub fn host_line(_line: &str) -> bool {
    false
}

pub fn host_replies() -> Vec<HostReply> {
    Vec::new()
}

pub fn free_standard_port() -> u16 {
    0
}

pub fn start(_install: &Path, _cfg: Config) -> Result<Listen, String> {
    Err(NO_HOSTING.to_owned())
}

/// What [`begin`] returns: nothing ever boots here.
pub struct Booting;

pub fn begin(_install: &Path, _cfg: Config) -> Result<Booting, String> {
    Err(NO_HOSTING.to_owned())
}

impl Booting {
    pub fn wait(self) -> Result<Listen, String> {
        Err(NO_HOSTING.to_owned())
    }
}

impl Listen {
    pub fn finish(&mut self) -> Value {
        Value::Null
    }
}
