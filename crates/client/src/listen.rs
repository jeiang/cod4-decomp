// SPDX-License-Identifier: GPL-3.0-or-later
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
    pub bots: usize,
    /// `None` is the harness's unlimited team deathmatch; `Some(id)` plays that gametype with its stock time and score
    /// limits (a menu-started match that ends).
    pub gametype: Option<String>,
    /// `sv_mapRotation`; `None` plays `map` again after every match.
    pub rotation: Option<String>,
    /// UDP port; 0 picks a free one. A menu-started server takes a standard port ([`crate::serverlist::PORTS`]) when one
    /// is free so the LAN list finds it.
    pub port: u16,
    /// Server dvars the player set on the client (`scr_*`: time and score limits, ...), applied before the map starts.
    pub dvars: Vec<(String, String)>,
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
        .map_err(|e| e.to_string())?;
    Ok(Booting {
        rx,
        stop,
        thread: Some(thread),
    })
}

impl Booting {
    /// Blocks until the server listens.
    pub fn wait(mut self) -> Result<Listen, String> {
        let addr = self
            .rx
            .recv()
            .map_err(|_| "the listen server died while starting".to_string())??;
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
            .and_then(|t| t.join().ok())
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
        let args: Vec<String> = ["+set", "net_port", &cfg.port.to_string()]
            .map(String::from)
            .to_vec();
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
        lines.push(format!("map {}", cfg.map));
        for line in lines {
            s.exec_line(&line).map_err(|e| format!("{line}: {e}"))?;
        }
        // The spawn logic scores one spawn point per frame from the first frame on; a player placed before its
        // first sweep reads an unset field and the gametype's spawn script fails (koth does on every map).
        s.run_for(Duration::from_secs(SPAWN_SETTLE_SECS));
        let bots = format!("bots {}", cfg.bots);
        s.exec_line(&bots).map_err(|e| format!("{bots}: {e}"))?;
        Ok((s, SocketAddr::from(([127, 0, 0, 1], addr.port()))))
    };
    let (mut server, addr) = match started() {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(Err(e));
            return Value::Null;
        }
    };
    let ticks: Rc<RefCell<Vec<TickSample>>> = Rc::default();
    {
        let ticks = ticks.clone();
        server.on_tick = Some(Box::new(move |t| ticks.borrow_mut().push(*t)));
    }
    let _ = tx.send(Ok(addr));
    // The first person's counters, kept while they are connected (the slot is freed when they leave).
    let mut person = json!(null);
    while !stop.load(Ordering::Relaxed) {
        let lines = CONSOLE
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for line in lines {
            let _ = server.exec_line(&line);
        }
        server.run_for(Duration::from_millis(100));
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
        "bots": cfg.bots,
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
