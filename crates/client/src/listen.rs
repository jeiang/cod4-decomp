// SPDX-License-Identifier: GPL-3.0-only
//! The in-process listen server for solo play: the headless server running in a thread of the client, a team
//! deathmatch on the chosen map with bots, listening on a loopback-reachable UDP port. The client connects to it
//! like to any other server.

use serde_json::{Value, json};
use server::server::{Server, TickSample};
use std::cell::RefCell;
use std::net::SocketAddr;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Listen {
    /// Where to connect (loopback and the server's real port).
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Value>>,
}

/// What a listen server plays.
#[derive(Clone)]
pub struct Config {
    pub map: String,
    /// `--bots`: the count that overrides `bot_count`.
    pub bots: Option<usize>,
    /// The player's `bot_count` setting: the server keeps it for every map it loads.
    pub bot_count: usize,
    /// `None` is the harness's unlimited team deathmatch; `Some(id)` plays that gametype with its stock time and score
    /// limits (a menu-started match that ends).
    pub gametype: Option<String>,
    /// `sv_mapRotation`; `None` plays `map` again after every match.
    pub rotation: Option<String>,
    /// UDP port; 0 picks a free one. A menu-started server takes a standard port ([`crate::serverlist::PORTS`]) when one
    /// is free so the LAN list finds it.
    pub port: u16,
    /// `fs_game`: the mod the server runs (the client's own), empty for stock; read once at boot.
    pub game: String,
    /// Server dvars the player set on the client (`scr_*`: time and score limits, ...), applied before the map starts.
    pub dvars: Vec<(String, String)>,
    /// The map starts with `devmap` (`sv_cheats` on) instead of `map`.
    pub cheats: bool,
}

/// The first standard port nothing is listening on, else 0 (any).
pub fn free_standard_port() -> u16 {
    crate::serverlist::PORTS
        .into_iter()
        .find(|p| std::net::UdpSocket::bind(("0.0.0.0", *p)).is_ok())
        .unwrap_or(0)
}

/// Console lines for the running listen server (the harness's `server=` steps), run between its frames.
static CONSOLE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The objective counters of the running listen server, as of its last frame batch.
static OBJECTIVES: std::sync::Mutex<(u64, u64, u64, u64, u64)> =
    std::sync::Mutex::new((0, 0, 0, 0, 0));

/// Bombs planted, bombs defused, airstrikes called in, helicopters called in and helicopter shots so far.
pub fn objectives() -> (u64, u64, u64, u64, u64) {
    OBJECTIVES.lock().map_or((0, 0, 0, 0, 0), |o| *o)
}

/// Requests for the running listen server's state and console, keyed by the control channel's request number.
static REQUESTS: std::sync::Mutex<Vec<(u64, serde_json::Map<String, Value>)>> =
    std::sync::Mutex::new(Vec::new());
/// The answers to those requests, keyed the same way; the channel takes one on a later pass.
static REPLIES: std::sync::Mutex<Vec<(u64, Result<Value, String>)>> =
    std::sync::Mutex::new(Vec::new());

/// Asks the listen server one request (`op`: `state`, `exec`, `cvar`). `None` until its answer comes back: the server
/// answers between its frames, and the channel offers the request again on the next one.
pub fn request(seq: u64, req: &serde_json::Map<String, Value>) -> Option<Result<Value, String>> {
    if let Ok(mut replies) = REPLIES.lock()
        && let Some(i) = replies.iter().position(|(s, _)| *s == seq)
    {
        return Some(replies.remove(i).1);
    }
    let queued = REQUESTS.lock().ok().is_some_and(|mut q| {
        let seen = q.iter().any(|(s, _)| *s == seq);
        if !seen {
            q.push((seq, req.clone()));
        }
        true
    });
    if !queued {
        return Some(Err("the listen server's request queue is gone".to_owned()));
    }
    None
}

/// Answers one request against the running server.
fn answer(s: &mut Server, req: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let op = req.get("op").and_then(Value::as_str).unwrap_or("state");
    match op {
        "state" => {
            let what = req.get("what").and_then(Value::as_str);
            Ok(s.control_state(what))
        }
        "exec" => {
            let line = req
                .get("line")
                .and_then(Value::as_str)
                .ok_or("exec needs the line")?;
            s.exec_line(line)?;
            Ok(json!({"ok": true, "line": line}))
        }
        "cvar" => {
            let name = req
                .get("name")
                .and_then(Value::as_str)
                .ok_or("cvar needs a name")?;
            match req.get("value").and_then(Value::as_str) {
                None => Ok(json!({"name": name, "value": s.control_cvar(name)})),
                Some(v) => {
                    s.exec_line(&format!("set {name} \"{}\"", v.replace('"', "'")))?;
                    Ok(json!({"name": name, "value": s.control_cvar(name)}))
                }
            }
        }
        other => Err(format!("no such server request: {other}")),
    }
}

/// Queues `line` for the listen server's console.
pub fn send(line: &str) {
    if let Ok(mut q) = CONSOLE.lock() {
        q.push(line.to_owned());
    }
}

/// A listen server that is still booting (map zone, scripts, navigation); see [`begin`].
pub struct Booting {
    rx: mpsc::Receiver<Result<SocketAddr, String>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Value>>,
}

/// Starts the server and returns once it listens on a map with its bots.
pub fn start(install: &Path, cfg: Config) -> Result<Listen, String> {
    begin(install, cfg)?.wait()
}

/// Starts the server's thread and returns at once; [`Booting::wait`] joins the boot.
pub fn begin(install: &Path, cfg: Config) -> Result<Booting, String> {
    let install = install.to_owned();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Result<SocketAddr, String>>();
    let stop2 = stop.clone();
    let thread = std::thread::Builder::new()
        .name("listen-server".into())
        .spawn(move || run(&install, &cfg, &stop2, &tx))
        .map_err(|e| {
            tracing::error!(error = %e, "cannot start the listen server's thread");
            e.to_string()
        })?;
    Ok(Booting {
        rx,
        stop,
        thread: Some(thread),
    })
}

impl Booting {
    /// Blocks until the server listens.
    pub fn wait(mut self) -> Result<Listen, String> {
        let addr = self.rx.recv().map_err(|_| {
            tracing::error!("the listen server died while starting");
            "the listen server died while starting".to_string()
        })??;
        Ok(Listen {
            addr,
            stop: self.stop.clone(),
            thread: self.thread.take(),
        })
    }
}

impl Drop for Booting {
    /// A boot nobody waited for (the load was abandoned) stops its server once it is up.
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

impl Listen {
    /// Stops the server and returns what it measured.
    pub fn finish(&mut self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .and_then(|t| {
                t.join()
                    .inspect_err(|_| tracing::error!("the listen server's thread panicked"))
                    .ok()
            })
            .unwrap_or(Value::Null)
    }
}

/// Seconds of server time before anyone joins: the spawn logic needs to have scored every spawn point once.
const SPAWN_SETTLE_SECS: u64 = 3;

fn run(
    install: &Path,
    cfg: &Config,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Result<SocketAddr, String>>,
) -> Value {
    let started = || -> Result<(Server, SocketAddr), String> {
        // The server a player starts runs pure, as the stock one does: its clients' IWDs must be its own.
        let mut args: Vec<String> = [
            "+set",
            "sv_pure",
            "1",
            "+set",
            "net_port",
            &cfg.port.to_string(),
        ]
        .map(String::from)
        .to_vec();
        if !cfg.game.is_empty() {
            args.extend(["+set", "fs_game", &cfg.game].map(String::from));
        }
        let mut s = Server::boot(install, &args, false)?;
        let addr = s.net_addr().ok_or("cannot bind a UDP socket")?;
        let gt = cfg.gametype.as_deref().unwrap_or("war");
        let mut lines = vec![format!("set g_gametype {gt}")];
        if cfg.gametype.is_none() {
            lines.push("set scr_war_timelimit 0".to_owned());
            lines.push("set scr_war_scorelimit 0".to_owned());
        }
        for (k, v) in &cfg.dvars {
            lines.push(format!("set {k} \"{}\"", v.replace('"', "'")));
        }
        let rotation = cfg
            .rotation
            .clone()
            .unwrap_or_else(|| format!("gametype {gt} map {}", cfg.map));
        lines.push(format!("set sv_mapRotation \"{rotation}\""));
        lines.push(format!(
            "{} {}",
            if cfg.cheats { "devmap" } else { "map" },
            cfg.map
        ));
        for line in lines {
            s.exec_line(&line).map_err(|e| format!("{line}: {e}"))?;
        }
        // The spawn logic scores one spawn point per frame from the first frame on; a player placed before its
        // first sweep reads an unset field and the gametype's spawn script fails (koth does on every map).
        s.run_for(Duration::from_secs(SPAWN_SETTLE_SECS));
        // Set after the settle, so the first map's bots join once the spawn points are scored; the server reads it again
        // at every later map start.
        let lines = [
            format!("set bot_count {}", cfg.bot_count),
            format!("bots {}", cfg.bots.unwrap_or(cfg.bot_count)),
        ];
        for line in lines {
            s.exec_line(&line).map_err(|e| format!("{line}: {e}"))?;
        }
        Ok((s, SocketAddr::from(([127, 0, 0, 1], addr.port()))))
    };
    let (mut server, addr) = match started() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(map = %cfg.map, error = %e, "the listen server cannot start");
            if tx.send(Err(e)).is_err() {
                tracing::debug!("nobody waits for the listen server's boot");
            }
            return Value::Null;
        }
    };
    let ticks: Rc<RefCell<Vec<TickSample>>> = Rc::default();
    {
        let ticks = ticks.clone();
        server.on_tick = Some(Box::new(move |t| ticks.borrow_mut().push(*t)));
    }
    tracing::info!(map = %cfg.map, %addr, "the listen server is up");
    if tx.send(Ok(addr)).is_err() {
        tracing::debug!("nobody waits for the listen server's boot");
    }
    // The first person's counters, kept while they are connected (the slot is freed when they leave).
    let mut person = json!(null);
    let mut reported_errors = 0;
    while !stop.load(Ordering::Relaxed) {
        let lines = CONSOLE
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        let asked = REQUESTS
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for (seq, req) in asked {
            let reply = answer(&mut server, &req);
            if let Ok(mut replies) = REPLIES.lock() {
                replies.push((seq, reply));
            }
        }
        for line in lines {
            if let Err(e) = server.exec_line(&line) {
                // The verb only: a console line may carry a password.
                let verb = line.split_whitespace().next().unwrap_or("");
                tracing::warn!(%verb, error = %e, "the listen server's console refused a line");
            }
        }
        server.run_for(Duration::from_millis(100));
        // A script error ends a thread chain silently for the player; the client log is where it shows.
        for (map, text) in server.all_script_errors.iter().skip(reported_errors) {
            tracing::warn!(%map, error = %text, "script error on the listen server");
        }
        reported_errors = server.all_script_errors.len();
        if let Ok(mut o) = OBJECTIVES.lock() {
            *o = (
                server.game.stats.plants,
                server.game.stats.defuses,
                server.game.stats.airstrikes,
                server.game.stats.helicopters,
                server.game.stats.heli_shots,
            );
        }
        if let Some((n, c)) = server.game.connected_clients().find(|(_, c)| !c.bot) {
            person = json!({
                "slot": n, "shots": c.shots, "hits": c.hits,
                "kills": c.kills, "deaths": c.deaths,
            });
        }
    }
    let t = ticks.borrow();
    let pct = |mut v: Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v.get(((v.len() as f64 * p) as usize).min(v.len().saturating_sub(1)))
            .copied()
    };
    let total: Vec<f64> = t.iter().map(|s| s.total_ms).collect();
    let net: Vec<f64> = t.iter().map(|s| s.net_ms).collect();
    let stats = server.net_stats().unwrap_or_default();
    json!({
        "ticks": t.len(),
        "tick_ms_p50": pct(total.clone(), 0.5),
        "tick_ms_p99": pct(total, 0.99),
        "net_ms_p50": pct(net, 0.5),
        "snapshots_out": stats.snapshots_out,
        "bytes_out": stats.bytes_out,
        "bytes_in": stats.bytes_in,
        "bots": cfg.bots.unwrap_or(cfg.bot_count),
        "script_errors": server.all_script_errors.len(),
        "level_time_ms": server.level_time(),
        "stats": {
            "shots": server.game.stats.shots, "hits": server.game.stats.hits,
            "kills": server.game.stats.kills, "deaths": server.game.stats.deaths,
            "plants": server.game.stats.plants, "defuses": server.game.stats.defuses,
        },
        "person": person,
    })
}
