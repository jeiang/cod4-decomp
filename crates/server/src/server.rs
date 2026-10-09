// SPDX-License-Identifier: GPL-3.0-only
//! The headless server: boot, map load, console commands and the frame loop.
//!
//! Boot follows the original's dedicated start: command-line `set`s first, `default_mp.cfg`
//! and `language.cfg` from the IWDs, the dedicated zone set, then the held command-line
//! commands (`+exec server.cfg`, `+map x`). A map load follows `SV_SpawnServer`: map zone,
//! game init (script load, entity spawn, `initstructs`, gametype `main`, level `main`, the
//! start callback), then three settle frames 100 ms apart. Frames run `G_RunFrame` with the
//! original's drain points.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gsc::{Builtins, CallOutcome, Obj, Options, Value, Vm, VmError, VmErrorKind, compile};

use crate::ban::BanList;
use crate::bot::{BotShared, Brain};
use crate::client::Conn;
use crate::cmd::{Argv, CommandBuffer, split_commands};
use crate::content::{Content, Install};
use crate::cvar::{self, Cvars};
use crate::game::{self, Game};
use crate::netsv::{Inbound, NetSv};
use crate::rcon::{self, Throttle, Verdict};
use crate::script::{Dispatch, ScriptHost};
use net::Transport;

/// Original loop time limit while scripts run (`LOOP_TIMEOUT` of the VM) applies unchanged.
const SETTLE_FRAMES: i32 = 3;
const SETTLE_STEP_MS: i32 = 100;
/// How long a joined person may take to send their profile before they begin without it (milliseconds of level time).
const STATS_WAIT_MS: i32 = 5000;
/// Stat pairs one `statsync` command may carry.
const STATSYNC_MAX_PAIRS: usize = 64;

/// Called after every frame with its cost.
pub type TickHook = Box<dyn FnMut(&TickSample)>;

/// One frame's cost, in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct TickSample {
    pub total_ms: f64,
    /// Script threads: the three drains of `G_RunFrame`.
    pub gsc_ms: f64,
    /// Bot decisions: building each bot's usercmd.
    pub bot_ms: f64,
    /// Client commands: movement, weapons, hits and what they raise.
    pub client_ms: f64,
    /// Building and sending every client's snapshot.
    pub net_ms: f64,
    /// Everything else in the frame (entity think, console, bookkeeping).
    pub game_ms: f64,
}

/// Names of the per-subsystem tick columns, in [`TickSample::subsystems`] order.
pub const TICK_SUBSYSTEMS: [&str; 5] = ["gsc", "bot", "client", "net", "game"];

impl TickSample {
    pub fn subsystems(&self) -> [f64; 5] {
        [
            self.gsc_ms,
            self.bot_ms,
            self.client_ms,
            self.net_ms,
            self.game_ms,
        ]
    }
}

/// The statistics `expect` and `until` read.
const STATS: &str = "kills|deaths|spawns|respawns|shots|hits|rounds|plants|defuses|hardpoints|airstrikes|helicopters|heli_shots|heli_hits|heli_crashes";

/// Commands the console accepts, for `Console::has_command`.
pub const COMMANDS: &[&str] = &[
    "set",
    "seta",
    "sets",
    "setu",
    "exec",
    "echo",
    "quit",
    "map",
    "devmap",
    "map_restart",
    "fast_restart",
    "map_rotate",
    "status",
    "vstr",
    "serverinfo",
    "wait",
    "killserver",
    "bots",
    "expect",
    "until",
    "devtele",
    "clientkick",
    "tempbanclient",
    "devhardpoint",
    "devheli",
];

/// Commands of the client console that mean nothing to a headless server; accepted silently
/// so the stock configs run unchanged.
const CLIENT_ONLY: &[&str] = &[
    "bind",
    "unbind",
    "unbindall",
    "bindlist",
    "seta_noop",
    "exec_noop",
];

struct Running {
    vm: Vm,
    dispatch: Dispatch,
    /// `TestClient` of the engine's bot script: the team and class picks of a test client.
    test_client: u32,
}

/// The bot's menu choices as script (the stock `_dev` script has them behind developer mode).
const TEST_CLIENT_GSC: &str = include_str!("testclient.gsc");

pub struct Server {
    pub game: Game,
    install: Install,
    run: Option<Running>,
    cbuf: CommandBuffer,
    /// Held command-line commands, run once init is complete.
    startup: Vec<String>,
    /// Zone `localized_code_post_gfx_mp` is loaded: `exec` prefers its rawfiles.
    cfg_from_zone: bool,
    /// Server time in milliseconds.
    svs_time: i32,
    frame_ms: i32,
    pub quit: bool,
    /// Script runtime errors seen on the current map, in the original's printed form.
    pub script_errors: Vec<String>,
    /// Every script error since boot, with the map it happened on.
    pub all_script_errors: Vec<(String, String)>,
    /// Console output kept for reports (last 4000 lines).
    pub log: Vec<String>,
    pub echo_stdout: bool,
    pub on_tick: Option<TickHook>,
    /// `devheli view`: the client held behind the first script vehicle, and how far behind.
    heli_view: Option<(u16, f32)>,
    net: Option<NetSv>,
    /// People who joined and have not yet sent their profile (`statsdone`): the slot, and the level time after which
    /// they begin without it. The original holds a client's game state, so its `begin`, until its stats arrived.
    begin_waits: Vec<(u16, i32)>,
    pub map_load_ms: f64,
    /// Levels started so far: a map change or restart, each of which resets the script pool.
    pub level_loads: u32,
    pub boot_ms: f64,
    pub ticks: u64,
    /// Bots wanted on every map (`bots N`); they join again after a map change.
    bot_target: usize,
    bot_serial: u32,
    bot_shared: BotShared,
    /// While set, console output is collected here instead of printed (`rcon` replies).
    redirect: Option<String>,
    rcon_throttle: Throttle,
    /// Lines typed on the server's own terminal (see [`Self::attach_console`]).
    console: Option<std::sync::mpsc::Receiver<String>>,
}

fn register_core_dvars(c: &mut Cvars) {
    use cvar::*;
    for (n, d, f) in [
        ("dedicated", "2", LATCH),
        ("developer", "0", 0),
        ("developer_script", "0", 0),
        ("sv_fps", "30", 0),
        ("sv_maxclients", "32", ARCHIVE | SERVERINFO | LATCH),
        ("ui_maxclients", "32", ARCHIVE | SERVERINFO | LATCH),
        ("g_gametype", "war", SERVERINFO | LATCH),
        ("mapname", "", ROM | SERVERINFO),
        ("sv_mapname", "", ROM | SERVERINFO),
        ("sv_hostname", "CoD4Host", ARCHIVE | SERVERINFO),
        ("net_port", "28960", LATCH),
        // WebTransport for browser clients, off when `net_wt` is empty: `[host:]port`, the
        // certificate and key PEM files (empty: a 13-day self-signed one), and the JSON file the
        // page reads for the URL and certificate hash. Command line: `--webtransport`, ...
        ("net_wt", "", LATCH),
        ("net_wt_cert", "", LATCH),
        ("net_wt_key", "", LATCH),
        ("net_wt_info", "", LATCH),
        ("g_password", "", 0),
        ("g_speed", "190", 0),
        // Milliseconds past a mantle that its landing spot stays blocked to other players.
        ("g_mantleBlockTimeBuffer", "500", 0),
        ("g_lagcomp", "1", 0),
        ("bot_idle", "0", 0),
        ("g_allowvote", "1", 0),
        ("g_useholdtime", "0", 0),
        ("g_maxDroppedWeapons", "16", 0),
        ("g_dropForwardSpeed", "10", ARCHIVE),
        ("g_dropUpSpeedBase", "10", ARCHIVE),
        ("g_dropUpSpeedRand", "5", ARCHIVE),
        ("g_dropHorzSpeedRand", "100", ARCHIVE),
        ("pickupPrints", "0", CHEAT),
        ("player_throwbackInnerRadius", "90", CHEAT),
        ("player_throwbackOuterRadius", "160", CHEAT),
        ("bg_maxGrenadeIndicatorSpeed", "20", CHEAT),
        ("perk_grenadeDeath", "frag_grenade_short_mp", 0),
        ("g_useholdspawndelay", "500", 0),
        ("g_gravity", "800", 0),
        ("g_knockback", "1000", 0),
        ("g_minGrenadeDamageSpeed", "400", CHEAT),
        ("g_inactivity", "0", 0),
        ("g_synchronousClients", "0", SYSTEMINFO),
        ("sv_cheats", "0", 0),
        // Remote console: empty switches `rcon` off.
        ("rcon_password", "", 0),
        // Seconds `kick` and `tempBanClient` keep a player out (0-3600).
        ("sv_kickBanTime", "300", ARCHIVE),
        // Where `banUser` / `banClient` keep the permanent bans.
        ("sv_banFile", "ban.txt", 0),
        ("sv_mapRotation", "", 0),
        ("sv_mapRotationCurrent", "", 0),
        ("nextmap", "map_restart", 0),
        ("gamename", "Call of Duty 4", SERVERINFO | ROM),
        ("g_log", "games_mp.log", ARCHIVE),
        ("loc_language", "0", ARCHIVE),
    ] {
        c.register(n, d, f);
    }
    for (n, d) in crate::missile::JAVELIN_CVARS {
        c.register(n, d, CHEAT);
    }
}

fn io_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Server {
    /// Boots a dedicated server on the install at `root`. `cmdline` is the program's
    /// arguments after the executable name (`+set a b +exec server.cfg +map mp_crash`).
    pub fn boot(root: &Path, cmdline: &[String], echo: bool) -> Result<Self, String> {
        let t0 = Instant::now();
        let install =
            Install::open(root).map_err(|e| format!("{}: {}", root.display(), io_err(e)))?;
        let mut cvars = Cvars::new();
        // Com_StartupVariable: set lines run before the cvars are registered, and again with the other held
        // commands (Com_AddStartupCommands) so they win over the stock configs, whose `set` lines would
        // otherwise replace the command line's value (`+set scr_war_timelimit 1`).
        let held = parse_command_line(cmdline);
        let mut startup = Vec::new();
        for line in held {
            let argv = crate::cmd::tokenize(&line);
            if matches!(
                argv.first().map(|s| s.to_ascii_lowercase()).as_deref(),
                Some("set" | "seta" | "sets" | "setu")
            ) && argv.len() >= 3
            {
                cvars.set(&argv[1], &argv[2..].join(" "));
            }
            startup.push(line);
        }
        register_core_dvars(&mut cvars);
        let game = Game::new(cvars, Content::default());
        let mut s = Server {
            game,
            install,
            run: None,
            cbuf: CommandBuffer::new(),
            startup,
            cfg_from_zone: false,
            svs_time: 0,
            frame_ms: 33,
            quit: false,
            script_errors: Vec::new(),
            all_script_errors: Vec::new(),
            log: Vec::new(),
            echo_stdout: echo,
            on_tick: None,
            heli_view: None,
            net: None,
            begin_waits: Vec::new(),
            map_load_ms: 0.0,
            level_loads: 0,
            boot_ms: 0.0,
            ticks: 0,
            bot_target: 0,
            bot_serial: 0,
            bot_shared: BotShared::default(),
            redirect: None,
            rcon_throttle: Throttle::default(),
            console: None,
        };
        s.say("CoD4 MP headless server (cod4e)\n");
        s.say(&format!(
            "install: {} (language {})\n",
            root.display(),
            s.install.language
        ));
        s.game.known_maps = known_maps(&s.install);
        // Boot-time execs read the IWD copies: no zone is loaded yet.
        s.cbuf.add_text("exec default_mp.cfg; exec language.cfg");
        s.exec_buffer();
        s.game
            .content
            .load_boot(&s.install)
            .map_err(|e| format!("ERROR: {e}"))?;
        s.cfg_from_zone = true;
        for (z, d) in s.game.content.timings.clone() {
            s.say(&format!("zone {z}: {:.0} ms\n", d.as_secs_f64() * 1000.0));
        }
        s.game.field_types = s
            .game
            .content
            .rawfile("radiant/keys.txt")
            .map(|t| game::parse_field_types(&String::from_utf8_lossy(t)))
            .unwrap_or_default();
        s.bind_socket();
        s.boot_ms = t0.elapsed().as_secs_f64() * 1000.0;
        s.say(&format!(
            "--- Common Initialization Complete ({:.0} ms) ---\n",
            s.boot_ms
        ));
        let startup = std::mem::take(&mut s.startup);
        for l in startup {
            s.cbuf.add_text(&l);
        }
        s.exec_buffer();
        Ok(s)
    }

    fn bind_socket(&mut self) {
        let port = self.game.cvars.int("net_port");
        let addr = SocketAddr::from(([0, 0, 0, 0], u16::try_from(port).unwrap_or(28960)));
        let udp = match net::UdpTransport::bind(addr) {
            Ok(t) => t,
            Err(e) => return self.say(&format!("WARNING: cannot bind udp port {port}: {e}\n")),
        };
        let max = usize::try_from(self.game.cvars.int("sv_maxclients")).unwrap_or(32);
        let wt = self.game.cvars.string("net_wt").trim().to_owned();
        #[cfg(not(target_arch = "wasm32"))]
        let t: Box<dyn net::Transport + Send> = if wt.is_empty() {
            Box::new(udp)
        } else {
            match self.start_webtransport(udp, &wt) {
                Ok(j) => Box::new(j),
                Err(e) => return self.say(&format!("WARNING: webtransport {wt}: {e}\n")),
            }
        };
        // The WebTransport endpoint needs a runtime the browser build does not have (a browser never serves).
        #[cfg(target_arch = "wasm32")]
        let t: Box<dyn net::Transport + Send> = {
            let _ = wt;
            Box::new(udp)
        };
        let mut n = NetSv::new(t, max.clamp(1, 64));
        n.bans = BanList::load(PathBuf::from(self.game.cvars.string("sv_banFile")));
        self.say(&format!(
            "listening on udp port {}\n",
            n.local_addr().port()
        ));
        self.net = Some(n);
    }

    /// Adds the WebTransport endpoint for `bind` (`[host:]port`) to the UDP socket and publishes
    /// the certificate hash (log line, and the `net_wt_info` JSON file).
    #[cfg(not(target_arch = "wasm32"))]
    fn start_webtransport(
        &mut self,
        udp: net::UdpTransport,
        bind: &str,
    ) -> Result<net::transport::Joined, String> {
        let addr = bind
            .parse::<SocketAddr>()
            .or_else(|_| {
                bind.parse::<u16>()
                    .map(|p| SocketAddr::from(([0, 0, 0, 0], p)))
            })
            .map_err(|_| "expected [host:]port".to_owned())?;
        let c = &self.game.cvars;
        let (cert, key, info) = (
            c.string("net_wt_cert").to_owned(),
            c.string("net_wt_key").to_owned(),
            c.string("net_wt_info").to_owned(),
        );
        let tls = match (cert.is_empty(), key.is_empty()) {
            (true, true) => None,
            (false, false) => Some((cert.into(), key.into())),
            _ => return Err("net_wt_cert and net_wt_key go together".to_owned()),
        };
        let mut joined = net::transport::Joined::new(udp).map_err(io_err)?;
        let (wt, up) =
            net::wt::WtTransport::start(net::wt::WtConfig { bind: addr, tls }, joined.inbox())
                .map_err(io_err)?;
        joined.add(Box::new(wt));
        let host = if addr.ip().is_unspecified() {
            "localhost".to_owned()
        } else if addr.is_ipv6() {
            format!("[{}]", addr.ip())
        } else {
            addr.ip().to_string()
        };
        let url = format!("https://{host}:{}/", up.addr.port());
        let mut json = format!("{{\"url\":\"{url}\"");
        self.say(&format!("listening on webtransport {url}\n"));
        if let Some(h) = up.cert_hash {
            let b64 = net::wt::base64(&h);
            let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
            json.push_str(&format!(",\"certHashSha256Base64\":\"{b64}\""));
            self.say(&format!(
                "webtransport certificate (valid {} days) sha256 base64={b64} hex={hex}\n",
                net::wt::SELF_SIGNED_DAYS
            ));
        }
        json.push('}');
        if !info.is_empty() {
            std::fs::write(&info, json).map_err(|e| format!("{info}: {e}"))?;
        }
        Ok(joined)
    }

    /// Where clients connect (the port is the real one when `net_port` was 0).
    pub fn net_addr(&self) -> Option<SocketAddr> {
        self.net.as_ref().map(NetSv::local_addr)
    }

    /// Network traffic counters since boot.
    pub fn net_stats(&self) -> Option<crate::netsv::NetStats> {
        self.net.as_ref().map(|n| n.stats)
    }

    /// Clients connected over the network.
    pub fn net_clients(&self) -> usize {
        self.net.as_ref().map_or(0, NetSv::peer_count)
    }

    /// Reads the network for up to `wait`: joins, leaves, client console commands.
    fn net_service(&mut self, wait: Duration) {
        let Some(mut net) = self.net.take() else {
            std::thread::sleep(wait);
            return;
        };
        let info = self.server_info();
        let mut remote = Vec::new();
        for i in net.poll(wait, &|| info.clone()) {
            match i {
                Inbound::Rcon {
                    from,
                    password,
                    command,
                } => remote.push((from, password, command)),
                Inbound::Connect(req) => self.net_accept(&mut net, &req),
                Inbound::Left(addr) => {
                    if let Some(slot) = net.slot_of(addr) {
                        self.net_drop(&mut net, slot, DropReason::Left);
                    }
                }
            }
        }
        for (slot, line) in std::mem::take(&mut net.inbox) {
            self.net_client_command(&mut net, slot, &line);
        }
        for slot in net.timed_out() {
            self.net_drop(&mut net, slot, DropReason::TimedOut);
        }
        for slot in net.overflowed() {
            self.net_drop(&mut net, slot, DropReason::Overflow);
        }
        let now = self.game.level.time;
        let late: Vec<u16> = self
            .begin_waits
            .iter()
            .filter(|(_, until)| now >= *until)
            .map(|(n, _)| *n)
            .collect();
        for slot in late {
            self.begin_client(slot);
        }
        self.net = Some(net);
        // Commands may kick, ban or change the map, which need the network back in place.
        for (from, password, command) in remote {
            self.remote_command(from, &password, &command);
        }
    }

    /// `rcon <password> <command>` from `from`: the password is checked, the attempt logged, the
    /// commands run now with their console output sent back as print packets.
    fn remote_command(&mut self, from: SocketAddr, password: &str, command: &str) {
        if !self.rcon_throttle.allow(Instant::now()) {
            return;
        }
        let verdict = rcon::check(self.game.cvars.string("rcon_password"), password);
        let bad = if verdict == Verdict::Granted {
            ""
        } else {
            "Bad "
        };
        self.say(&format!("{bad}Rcon from {from}:\n{command}\n"));
        self.redirect = Some(String::new());
        match verdict {
            Verdict::Disabled => {
                self.say("The server must set 'rcon_password' for clients to use 'rcon'.\n");
            }
            Verdict::Missing => self.say("You must give the password: rcon <password> <command>\n"),
            Verdict::Wrong => self.say("Invalid password.\n"),
            Verdict::Granted => {
                for line in split_commands(command) {
                    if let Err(e) = self.exec_command(&crate::cmd::tokenize(&line)) {
                        self.say(&format!("{e}\n"));
                    }
                    self.flush_game_output();
                }
            }
        }
        let out = self.redirect.take().unwrap_or_default();
        if let Some(net) = self.net.as_mut() {
            for p in net::Oob::print_chunks(&out) {
                net.t.send_to(from, &p.encode());
            }
        }
    }

    /// Runs lines arriving on `lines` as console commands: the server's terminal.
    pub fn attach_console(&mut self, lines: std::sync::mpsc::Receiver<String>) {
        self.console = Some(lines);
    }

    fn server_info(&self) -> Vec<(String, String)> {
        let c = &self.game.cvars;
        vec![
            ("hostname".into(), c.string("sv_hostname").to_owned()),
            ("mapname".into(), self.map_name().unwrap_or("").to_owned()),
            ("gametype".into(), c.string("g_gametype").to_owned()),
            (
                "clients".into(),
                self.game.connected_clients().count().to_string(),
            ),
            ("sv_maxclients".into(), c.string("sv_maxclients").to_owned()),
            ("protocol".into(), net::oob::PROTOCOL.to_string()),
        ]
    }

    /// A `connect` that passed the challenge: gives the sender a slot like a bot gets one, and
    /// tells the scripts.
    fn net_accept(&mut self, net: &mut NetSv, req: &net::connect::ConnectRequest) {
        let refuse = |net: &mut NetSv, why: &str| {
            net.t
                .send_to(req.from, &net::Oob::Error(why.into()).encode());
        };
        if net.slot_of(req.from).is_some() {
            // The response was lost: say it again.
            net.t.send_to(req.from, &net::Oob::ConnectResponse.encode());
            return;
        }
        if net.bans.is_banned(req.from.ip(), Instant::now()) {
            return refuse(net, "You are banned from this server.");
        }
        let pw = self.game.cvars.string("g_password");
        if !pw.is_empty() && pw != req.password {
            return refuse(net, "Invalid password.");
        }
        let name: String = req
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(31)
            .collect();
        let slot = match self.join_human(&name, None) {
            Ok(n) => n,
            Err(why) => return refuse(net, why),
        };
        net.add_peer(slot, req, &name);
        let map = self.map_name().unwrap_or("").to_owned();
        for line in NetSv::world_commands(&mut self.game, &map) {
            net.command(slot, &line);
        }
        if let Some(a) = &self.game.ambient {
            net.command(slot, a);
        }
        self.say(&format!(
            "{name} connected as client {slot} from {}\n",
            req.from
        ));
    }

    /// Gives a person a client slot like a bot gets one: the scripts see a connect. With `stats` (a person kept across
    /// a map change) they begin at once; a new person begins when their profile has arrived ([`Self::begin_client`]).
    fn join_human(
        &mut self,
        name: &str,
        stats: Option<std::collections::HashMap<i32, i32>>,
    ) -> Result<u16, &'static str> {
        let Some(run) = self.run.as_mut() else {
            return Err("No map is loaded.");
        };
        let mut host = ScriptHost {
            game: &mut self.game,
            dispatch: &run.dispatch,
        };
        let Some(slot) = host.game.connect_client(&mut run.vm, false, name) else {
            return Err("Server is full.");
        };
        let known = stats.is_some();
        if let Some(c) = host.game.client_mut(slot) {
            // The stock scripts kick a client whose profile stats are zero (the original's stand-in for a checksum);
            // a person has no profile until it uploads one (`statsync`), so it starts with the nonzero defaults.
            // They are the server's own: announcing them would make the client take them for news and write them
            // into its saved profile.
            for i in 0..5 {
                c.stats.insert(205 + i * 10, 1);
            }
            c.stats.extend(stats.unwrap_or_default());
        }
        host.run_calls(&mut run.vm);
        if known {
            host.game.client_begin(&mut run.vm, slot);
        } else {
            let until = self.game.level.time + STATS_WAIT_MS;
            self.begin_waits.push((slot, until));
        }
        // No team is chosen for a person: the scripts open the team menu themselves and the
        // client answers with `menuresponse`, as at the original's menus.
        Ok(slot)
    }

    /// `ClientBegin` for a person whose profile has arrived (or did not in time).
    fn begin_client(&mut self, slot: u16) {
        self.begin_waits.retain(|(n, _)| *n != slot);
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let mut host = ScriptHost {
            game: &mut self.game,
            dispatch: &run.dispatch,
        };
        host.game.client_begin(&mut run.vm, slot);
        host.run_calls(&mut run.vm);
    }

    /// Takes a client out of the match: the others and the console hear why, the client is told (`SV_DropClient`).
    fn net_drop(&mut self, net: &mut NetSv, slot: u16, why: DropReason) {
        self.begin_waits.retain(|(n, _)| *n != slot);
        if let Some(c) = self.game.client(slot).filter(|c| c.connected()) {
            let text = format!("{}^7 {}", c.name, why.text());
            net.print_to_others(slot, &text);
            self.say(&format!("{slot}:{}\n", crate::vote::clean_name(&text)));
        }
        if let Some(run) = self.run.as_mut() {
            let mut host = ScriptHost {
                game: &mut self.game,
                dispatch: &run.dispatch,
            };
            host.game.disconnect_client(&mut run.vm, slot);
            host.run_calls(&mut run.vm);
        }
        net.remove_peer(slot, why.notice());
    }

    /// What a connected client may ask of the server.
    fn net_client_command(&mut self, net: &mut NetSv, slot: u16, line: &str) {
        let argv = crate::cmd::tokenize(line);
        match argv.first().map(String::as_str) {
            Some("disconnect") => self.net_drop(net, slot, DropReason::Left),
            Some(net::ui::SCORES_REQUEST) => net.send_scoreboard(slot, &self.game),
            Some("callvote") => self.game.call_vote(slot, &argv[1..]),
            Some("vote") => self
                .game
                .cast_vote(slot, argv.get(1).map_or("", String::as_str)),
            Some("menuresponse") if argv.len() >= 3 => {
                if let Some(run) = self.run.as_mut() {
                    run.vm.notify_entity(
                        slot,
                        "menuresponse",
                        &[Value::str(&argv[1]), Value::str(&argv[2])],
                    );
                }
            }
            // The client's profile stats (`statsync <index> <value> ...`): the person's own, as the
            // original's stats file is kept on the client.
            Some("statsync") => {
                if let Some(c) = self.game.client_mut(slot) {
                    for kv in argv[1..].as_chunks::<2>().0.iter().take(STATSYNC_MAX_PAIRS) {
                        let (i, v) = (cvar::parse_int(&kv[0]), cvar::parse_int(&kv[1]));
                        if (0..4000).contains(&i) {
                            c.stats.insert(i, v);
                        }
                    }
                }
            }
            // The profile is complete: the person may begin.
            Some("statsdone") if self.begin_waits.iter().any(|(n, _)| *n == slot) => {
                self.begin_client(slot);
            }
            _ => {}
        }
    }

    /// Prints a console line: to the log and, when enabled, stdout.
    pub fn say(&mut self, s: &str) {
        if let Some(r) = self.redirect.as_mut() {
            r.push_str(s);
            return;
        }
        if self.echo_stdout {
            print!("{s}");
        }
        for l in s.lines() {
            if self.log.len() >= 4000 {
                self.log.drain(..1000);
            }
            self.log.push(l.to_owned());
        }
    }

    fn flush_game_output(&mut self) {
        for s in std::mem::take(&mut self.game.printed) {
            self.say(&s);
        }
    }

    pub fn map_name(&self) -> Option<&str> {
        self.game.content.map_name.as_deref()
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn level_time(&self) -> i32 {
        self.game.level.time
    }

    /// Queues console text, as the console and `+` arguments do.
    pub fn add_command_text(&mut self, text: &str) {
        self.cbuf.add_text(text);
    }

    /// Runs queued commands.
    pub fn exec_buffer(&mut self) {
        while let Some(argv) = self.cbuf.pop() {
            if let Err(e) = self.exec_command(&argv) {
                self.say(&format!("{e}\n"));
            }
            self.flush_game_output();
            if self.quit {
                break;
            }
        }
    }

    /// Runs one console line now (`harness` scenarios), returning its error.
    pub fn exec_line(&mut self, line: &str) -> Result<(), String> {
        for l in split_commands(line) {
            self.exec_command(&crate::cmd::tokenize(&l))?;
            self.flush_game_output();
        }
        Ok(())
    }

    fn exec_command(&mut self, argv: &Argv) -> Result<(), String> {
        let Some(name) = argv.first() else {
            return Ok(());
        };
        let lname = name.to_ascii_lowercase();
        let arg = |i: usize| argv.get(i).map(String::as_str);
        match lname.as_str() {
            "set" | "seta" | "sets" | "setu" => {
                let (Some(n), true) = (arg(1), argv.len() >= 3) else {
                    return Err(format!("usage: {lname} <variable> <value>"));
                };
                let v = argv[2..].join(" ");
                self.game.cvars.set(n, &v);
                let flag = match lname.as_str() {
                    "seta" => cvar::ARCHIVE,
                    "sets" => cvar::SERVERINFO,
                    "setu" => cvar::USERINFO,
                    _ => 0,
                };
                self.game.cvars.add_flags(n, flag);
            }
            "echo" => self.say(&format!("{}\n", argv[1..].join(" "))),
            "wait" => self.cbuf.wait(),
            "quit" | "killserver" => self.quit = true,
            "exec" => {
                let f = arg(1).ok_or("exec <filename> : execute a script file")?;
                self.exec_cfg(f)?;
            }
            "vstr" => {
                let v = arg(1).ok_or("vstr <variablename> : execute a variable command")?;
                let text = self.game.cvars.string(v).to_owned();
                self.cbuf.insert_text(&text);
            }
            "map" | "devmap" => {
                let m = arg(1).ok_or("usage: map <mapname>")?.to_ascii_lowercase();
                if !self.install.map_exists(&m) {
                    return Err(format!("Can't find map \"{m}\"."));
                }
                self.game
                    .cvars
                    .set("sv_cheats", if lname == "devmap" { "1" } else { "0" });
                self.spawn_server(&m)?;
            }
            "map_restart" | "fast_restart" => {
                let m = self.map_name().ok_or("Server is not running.")?.to_owned();
                self.map_restart(&m)?;
            }
            "map_rotate" => self.map_rotate()?,
            "status" => self.status(),
            "expect" => {
                let (Some(what), Some(min)) = (arg(1), arg(2)) else {
                    return Err(format!("usage: expect <{STATS}> <minimum>"));
                };
                let have = self.stat(what)?;
                let min = u64::try_from(cvar::parse_int(min)).unwrap_or(0);
                if have < min {
                    return Err(format!("expect {what} {min}: only {have}"));
                }
            }
            // `until <stat> <minimum> <seconds>`: runs the server, unpaced, until the statistic
            // reaches the minimum; an error when it has not within the simulated time.
            "until" => {
                let (Some(what), Some(min), Some(secs)) = (arg(1), arg(2), arg(3)) else {
                    return Err(format!("usage: until <{STATS}> <minimum> <seconds>"));
                };
                let min = u64::try_from(cvar::parse_int(min)).unwrap_or(0);
                let limit =
                    cvar::parse_int(secs).max(0) as u32 * 1000 / self.frame_ms.max(1) as u32;
                let mut have = self.stat(what)?;
                for _ in 0..limit / 15 {
                    if have >= min {
                        return Ok(());
                    }
                    self.run_frames(15);
                    have = self.stat(what)?;
                }
                if have < min {
                    return Err(format!("until {what} {min}: only {have} after {secs} s"));
                }
            }
            // Test hook: `devtele <client|human> <x> <y> <z>` puts the player there, standing still;
            // `devtele <client|human> <name>` puts it on the floor in the first live trigger whose
            // targetname starts with `name` and that is on for the player's team. `human` is the first
            // client that is not a bot.
            "devtele" => {
                let (Some(who), Some(first)) = (arg(1), arg(2)) else {
                    return Err("usage: devtele <client|human> <x y z | name>".into());
                };
                let n = if who == "human" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| n)
                        .ok_or("devtele: no human is connected")?
                } else {
                    cvar::parse_int(who) as u16
                };
                match (arg(3), arg(4)) {
                    (Some(y), Some(z)) => {
                        self.game.teleport(n, [first, y, z].map(cvar::parse_float));
                    }
                    _ => self.game.teleport_to_named(n, first)?,
                }
            }
            // Test hook: `devhardpoint <client|human|bot> <weapon>` gives the player a killstreak reward as the
            // scripts would (`radar_mp`, `airstrike_mp`, `helicopter_mp`).
            "devhardpoint" => {
                let (Some(who), Some(weapon)) = (arg(1), arg(2)) else {
                    return Err("usage: devhardpoint <client|human|bot> <weapon>".into());
                };
                let n = if who == "human" || who == "bot" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| c.bot == (who == "bot"))
                        .map(|(n, _)| n)
                        .ok_or("devhardpoint: no such client is connected")?
                } else {
                    cvar::parse_int(who) as u16
                };
                let func = self
                    .game
                    .callbacks
                    .give_hardpoint
                    .ok_or("devhardpoint: no hardpoint script")?;
                let Some(run) = self.run.as_mut() else {
                    return Err("Server is not running.".into());
                };
                let mut host = ScriptHost {
                    game: &mut self.game,
                    dispatch: &run.dispatch,
                };
                host.game.calls.push(crate::game::ScriptCall {
                    func,
                    this: Some(n),
                    args: vec![Value::str(weapon)],
                });
                host.run_calls(&mut run.vm);
            }
            // Test hook: `devheli view <client|human> [distance]` makes the player a chase camera: every frame it
            // floats `distance` (700) units behind and to the side of the first script vehicle, a little below it, looking at it.
            "devheli" if arg(1) == Some("view") => {
                let who = arg(2).ok_or("usage: devheli view <client|human> [distance]")?;
                let distance = arg(3).map_or(700.0, cvar::parse_float);
                let n = if who == "human" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| n)
                        .ok_or("devheli: no human is connected")?
                } else {
                    cvar::parse_int(who) as u16
                };
                self.heli_view = Some((n, distance));
                self.place_heli_view();
            }
            // Test hook: `devheli` prints every script vehicle: where it is, how fast, how it leans and its damage stage.
            "devheli" => {
                let lines: Vec<String> = self
                    .game
                    .vehicles()
                    .map(|(n, e, v)| {
                        format!(
                            "heli {n}: origin {:.0} {:.0} {:.0}, {:.1} mph, angles {:.0} {:.0} {:.0}, stage {}, {:?}\n",
                            e.origin[0],
                            e.origin[1],
                            e.origin[2],
                            v.speed / crate::vehicle::MPH,
                            e.angles[0],
                            e.angles[1],
                            e.angles[2],
                            v.stage,
                            v.state
                        )
                    })
                    .collect();
                for l in lines {
                    self.say(&l);
                }
            }
            // Kicking: `clientkick` / `tempBanClient` / `banClient` take a client number (what a passed vote runs),
            // `onlykick` / `kick` (a brief ban, `sv_kickBanTime` seconds) / `tempBanUser` / `banUser` (for good) a name.
            "clientkick" | "tempbanclient" | "banclient" => {
                let n = arg(1)
                    .map(cvar::parse_int)
                    .ok_or_else(|| format!("usage: {lname} <client number>"))?;
                let slot = u16::try_from(n)
                    .ok()
                    .filter(|s| {
                        self.game
                            .clients
                            .get(usize::from(*s))
                            .is_some_and(|c| c.conn != Conn::Free)
                    })
                    .ok_or_else(|| format!("Client {n} is not on the server."))?;
                self.kick(slot, Penalty::of(&lname))?;
            }
            "onlykick" | "kick" | "tempbanuser" | "banuser" => {
                let who = arg(1).ok_or_else(|| {
                    format!("usage: {lname} <player name>\n{lname} all = kick everyone")
                })?;
                if self.run.is_none() {
                    return Err("Server is not running.".into());
                }
                match self.find_player(who) {
                    Some(slot) => self.kick(slot, Penalty::of(&lname))?,
                    None if lname != "banuser" && who.eq_ignore_ascii_case("all") => {
                        let all: Vec<u16> = (0..self.game.clients.len() as u16)
                            .filter(|n| self.game.clients[usize::from(*n)].conn != Conn::Free)
                            .collect();
                        for slot in all {
                            self.kick(slot, Penalty::None)?;
                        }
                    }
                    None => return Err(format!("Player {who} is not on the server")),
                }
            }
            "unbanuser" => {
                let ip: std::net::IpAddr = arg(1)
                    .ok_or("usage: unbanUser <ip address>")?
                    .parse()
                    .map_err(
                    |_| "unbanUser takes an IP address, as banUser and banClient record it",
                )?;
                let net = self.net.as_mut().ok_or("There is no network.")?;
                let was = net.bans.unban(ip)?;
                self.say(&if was {
                    format!("{ip} is no longer banned\n")
                } else {
                    format!("{ip} is not banned\n")
                });
            }
            "say" | "tell" => {
                let (to, from_arg) = if lname == "tell" {
                    let n = arg(1)
                        .and_then(|n| n.parse::<u16>().ok())
                        .ok_or("usage: tell <client number> <message>")?;
                    (crate::ui::Dest::Client(n), 2)
                } else {
                    (crate::ui::Dest::All, 1)
                };
                if argv.len() <= from_arg {
                    return Err(format!("usage: {lname} <message>"));
                }
                let text = format!("console: {}", argv[from_arg..].join(" "));
                self.say(&format!("{text}\n"));
                self.game.send(
                    to,
                    net::ui::ServerCmd::Print {
                        kind: net::ui::PrintKind::Normal,
                        text,
                    },
                );
            }
            "systeminfo" => {
                let s = self.game.cvars.info_string(cvar::SYSTEMINFO);
                self.say(&format!("System info settings:\n{s}\n"));
            }
            "dumpuser" => {
                let who = arg(1).ok_or("usage: dumpuser <player name>")?;
                let slot = self
                    .find_player(who)
                    .ok_or_else(|| format!("Player {who} is not on the server"))?;
                self.dump_user(slot);
            }
            // This server announces itself to no master server, so there is nothing to send.
            "heartbeat" | "gamecompletestatus" => {
                self.say("This server has no master server to tell.\n");
            }
            "toggle" => {
                let n = arg(1).ok_or("usage: toggle <variable> [value1 value2 ...]")?;
                if !self.game.cvars.exists(n) {
                    return Err(format!("toggle: {n} is not a variable"));
                }
                let cur = self.game.cvars.string(n).to_owned();
                let next = if argv.len() > 2 {
                    let at = argv[2..].iter().position(|v| *v == cur);
                    argv[2 + at.map_or(0, |i| (i + 1) % (argv.len() - 2))].clone()
                } else {
                    (if cvar::parse_int(&cur) == 0 { "1" } else { "0" }).to_owned()
                };
                self.game.cvars.set(n, &next);
            }
            "reset" => {
                let n = arg(1).ok_or("usage: reset <variable>")?;
                let default = self
                    .game
                    .cvars
                    .get(n)
                    .map(|c| c.default.clone())
                    .ok_or_else(|| format!("reset: {n} is not a variable"))?;
                self.game.cvars.set(n, &default);
            }
            "setfromdvar" => {
                let (Some(dst), Some(src)) = (arg(1), arg(2)) else {
                    return Err("usage: setfromdvar <variable> <source variable>".into());
                };
                let v = self
                    .game
                    .cvars
                    .get(src)
                    .map(|c| c.value.clone())
                    .ok_or_else(|| format!("setfromdvar: {src} is not a variable"))?;
                self.game.cvars.set(dst, &v);
            }
            "cvarlist" => {
                let filter = arg(1).map(str::to_ascii_lowercase);
                let mut out = String::new();
                let mut n = 0;
                for c in self.game.cvars.iter() {
                    if filter
                        .as_ref()
                        .is_some_and(|f| !c.name.to_ascii_lowercase().contains(f))
                    {
                        continue;
                    }
                    n += 1;
                    let f = |bit, ch| if c.flags & bit != 0 { ch } else { ' ' };
                    out.push_str(&format!(
                        "{}{}{}{}{}{}{} {} \"{}\"\n",
                        f(cvar::ARCHIVE, 'A'),
                        f(cvar::USERINFO, 'U'),
                        f(cvar::SERVERINFO, 'S'),
                        f(cvar::SYSTEMINFO, 'Y'),
                        f(cvar::LATCH, 'L'),
                        f(cvar::ROM, 'R'),
                        f(cvar::CHEAT, 'C'),
                        c.name,
                        if c.name.eq_ignore_ascii_case("rcon_password") {
                            "*"
                        } else {
                            &c.value
                        }
                    ));
                }
                self.say(&format!("{out}\n{n} cvar indexes\n"));
            }
            "bots" => {
                let n = arg(1)
                    .map(cvar::parse_int)
                    .ok_or("usage: bots <count>")?
                    .clamp(0, 64) as usize;
                self.bot_target = self.bot_target.max(n);
                self.add_bots(n)?;
            }
            // Test hook: `devkill <victim> <attacker>` has the attacker's rifle kill the victim.
            "devkill" => {
                let (Some(v), Some(a)) = (arg(1), arg(2)) else {
                    return Err("usage: devkill <victim> <attacker>".into());
                };
                let (v, a) = (cvar::parse_int(v) as u16, cvar::parse_int(a) as u16);
                let Some(run) = self.run.as_mut() else {
                    return Err("Server is not running.".into());
                };
                let mut host = ScriptHost {
                    game: &mut self.game,
                    dispatch: &run.dispatch,
                };
                let mut d = crate::combat::Damage::new(1000, crate::combat::MOD_RIFLE_BULLET);
                d.attacker = Some(a);
                d.inflictor = Some(a);
                d.weapon = host.game.weapon_index("ak47_mp");
                host.game.g_damage(&mut run.vm, v, d);
                host.run_calls(&mut run.vm);
            }
            "serverinfo" => {
                let s = self.game.cvars.info_string(cvar::SERVERINFO);
                self.say(&format!("Server info settings:\n{s}\n"));
            }
            _ if CLIENT_ONLY.contains(&lname.as_str()) => {}
            _ => {
                if self.game.cvars.exists(name) {
                    match arg(1) {
                        Some(_) => {
                            let v = argv[1..].join(" ");
                            self.game.cvars.set(name, &v);
                        }
                        None => {
                            let c = self.game.cvars.get(name).cloned();
                            if let Some(c) = c {
                                self.say(&format!(
                                    "\"{}\" is: \"{}\" default: \"{}\"\n",
                                    c.name, c.value, c.default
                                ));
                            }
                        }
                    }
                } else {
                    self.say(&format!("Unknown command \"{name}\"\n"));
                }
            }
        }
        Ok(())
    }

    /// `Cmd_Exec`: zone rawfile first once `localized_code_post_gfx_mp` is loaded, then the
    /// search path, then the working directory.
    fn exec_cfg(&mut self, name: &str) -> Result<(), String> {
        let mut file = name.replace('\\', "/");
        if !file.to_ascii_lowercase().ends_with(".cfg") {
            file.push_str(".cfg");
        }
        let zone = self
            .cfg_from_zone
            .then(|| self.game.content.rawfile(&file))
            .flatten()
            .map(|b| ("fastfile", b.to_vec()));
        let found = zone
            .or_else(|| {
                self.install
                    .vfs
                    .read(&file)
                    .ok()
                    .flatten()
                    .map(|b| ("disk", b))
            })
            .or_else(|| std::fs::read(&file).ok().map(|b| ("disk", b)));
        let Some((src, bytes)) = found else {
            return Err(format!("couldn't exec {file}"));
        };
        self.say(&format!("execing {file} from {src}\n"));
        self.cbuf.insert_text(&String::from_utf8_lossy(&bytes));
        Ok(())
    }

    fn status(&mut self) {
        let map = self.map_name().unwrap_or("(none)").to_owned();
        let host = self.game.cvars.string("sv_hostname").to_owned();
        let mut s = format!(
            "hostname: {host}\nmap: {map}\nnum score ping guid name            lastmsg address               qport rate\n\
             --- ----- ---- ---- --------------- ------- --------------------- ----- ----\n"
        );
        for (n, c) in self.game.clients.iter().enumerate() {
            if c.conn == Conn::Free {
                continue;
            }
            let n = n as u16;
            let net = self.net.as_ref();
            let peer = net.and_then(|net| net.peer_line(n));
            let ping = ping_field(
                c.bot,
                self.game.pending_free.contains(&n),
                c.conn == Conn::Connecting,
                net.and_then(|net| net.ping_of(n)),
            );
            // The guid column is the original's key hash, which this server has no use for; the rate
            // column its per-client rate setting, which this protocol does not have.
            let (last, addr, qport) = match peer {
                Some((a, q, ms)) => (ms.to_string(), a.to_string(), q),
                None => ("0".into(), (if c.bot { "bot" } else { "-" }).into(), 0),
            };
            s.push_str(&format!(
                "{n:3} {:5} {ping:>4} {:>4} {:<15} {last:>7} {addr:<21} {qport:5} {:>4}\n",
                c.score, 0, c.name, "-"
            ));
        }
        s.push_str(&format!(
            "tick: {}  level time: {} ms  entities: {}  script threads: {}\n",
            self.ticks,
            self.game.level.time,
            self.game.in_use().count(),
            self.run.as_ref().map_or(0, |r| r.vm.thread_count())
        ));
        let st = self.game.stats;
        s.push_str(&format!(
            "match: {} kills, {} deaths, {} spawns ({} respawns), {} shots, {} hits, {} rounds ended\n",
            st.kills, st.deaths, st.spawns, st.respawns, st.shots, st.hits, st.matches_ended
        ));
        if let Some(r) = rss_line() {
            s.push_str(&r);
        }
        self.say(&s);
    }

    /// The slot of the player called `who`, with or without colour codes, in any case.
    fn find_player(&self, who: &str) -> Option<u16> {
        let clean = crate::vote::clean_name(who);
        self.game
            .clients
            .iter()
            .enumerate()
            .find(|(_, c)| {
                c.conn != Conn::Free
                    && (c.name.eq_ignore_ascii_case(who)
                        || crate::vote::clean_name(&c.name).eq_ignore_ascii_case(&clean))
            })
            .map(|(n, _)| n as u16)
    }

    /// Drops a client, and bans its address as `penalty` says.
    fn kick(&mut self, slot: u16, penalty: Penalty) -> Result<(), String> {
        let Some(mut net) = self.net.take() else {
            return Err("There is no network.".into());
        };
        if let Some((addr, ..)) = net.peer_line(slot) {
            let ip = addr.ip();
            match penalty {
                Penalty::None => {}
                Penalty::Brief => {
                    let secs = self.game.cvars.int("sv_kickBanTime").clamp(0, 3600);
                    net.bans.ban_briefly(ip, Instant::now(), secs as u64);
                }
                Penalty::Permanent => {
                    if let Err(e) = net.bans.ban(ip) {
                        self.say(&format!(
                            "WARNING: the ban will not outlast this run: {e}\n"
                        ));
                    }
                }
            }
        }
        self.net_drop(&mut net, slot, DropReason::Kicked);
        self.net = Some(net);
        Ok(())
    }

    /// `dumpuser`: what the server knows of one client.
    fn dump_user(&mut self, slot: u16) {
        let c = &self.game.clients[usize::from(slot)];
        let mut s = format!(
            "userinfo\n--------\nname   {}\nclient {slot}\nbot    {}\n",
            c.name,
            u8::from(c.bot)
        );
        if let Some((addr, qport, _)) = self.net.as_ref().and_then(|n| n.peer_line(slot)) {
            s.push_str(&format!("ip     {}\nqport  {qport}\n", addr.ip()));
        }
        self.say(&s);
    }

    /// `SV_MapRotate_f`: consume `gametype X` / `map Y` tokens from the rotation.
    fn map_rotate(&mut self) -> Result<(), String> {
        let mut rot = self
            .game
            .cvars
            .string("sv_mapRotationCurrent")
            .trim()
            .to_owned();
        if rot.is_empty() {
            rot = self.game.cvars.string("sv_mapRotation").trim().to_owned();
        }
        if rot.is_empty() {
            let m = self.map_name().ok_or("Server is not running.")?.to_owned();
            self.say("map rotation is empty; restarting the map\n");
            return self.spawn_server(&m);
        }
        let toks: Vec<&str> = rot.split_whitespace().collect();
        let mut i = 0;
        let mut next_map = None;
        while i + 1 < toks.len() && next_map.is_none() {
            match toks[i].to_ascii_lowercase().as_str() {
                "gametype" => {
                    self.game.cvars.set("g_gametype", toks[i + 1]);
                }
                "map" => next_map = Some(toks[i + 1].to_ascii_lowercase()),
                t => return Err(format!("map_rotate: unknown token {t:?} in sv_mapRotation")),
            };
            i += 2;
        }
        let rest = toks[i..].join(" ");
        self.game.cvars.set("sv_mapRotationCurrent", &rest);
        match next_map {
            Some(m) if self.install.map_exists(&m) => self.spawn_server(&m),
            Some(m) => Err(format!("Can't find map \"{m}\".")),
            None => Err("map_rotate: the rotation names no map".into()),
        }
    }

    fn map_restart(&mut self, map: &str) -> Result<(), String> {
        let game_var = self.run.as_ref().map(|r| r.vm.game().clone());
        self.start_game(map, game_var)
    }

    /// `SV_SpawnServer`.
    pub fn spawn_server(&mut self, map: &str) -> Result<(), String> {
        self.game.cvars.apply_latched();
        self.start_game(map, None)
    }

    fn start_game(&mut self, map: &str, game_var: Option<Value>) -> Result<(), String> {
        let t0 = Instant::now();
        // People stay connected across a map change; the level they were in is gone.
        self.begin_waits.clear();
        let humans: Vec<_> = self
            .net
            .as_mut()
            .map_or_else(Vec::new, NetSv::take_peers)
            .into_iter()
            .map(|(slot, peer, name)| {
                let stats = self.game.client(slot).map(|c| c.stats.clone());
                (peer, name, stats.unwrap_or_default())
            })
            .collect();
        self.run = None;
        self.level_loads += 1;
        self.script_errors.clear();
        self.say("------ Server Initialization ------\n");
        self.say(&format!("Server: {map}\n"));
        let restart = game_var.is_some();
        if !restart || self.game.content.map_name.as_deref() != Some(map) {
            self.game
                .content
                .load_map(&self.install, map)
                .map_err(|e| format!("ERROR: {e}"))?;
        }
        self.game.cvars.force("mapname", map);
        self.game.cvars.force("sv_mapname", map);
        let fps = self.game.cvars.int("sv_fps").clamp(10, 1000);
        self.frame_ms = 1000 / fps;
        self.svs_time = 0;
        self.game
            .reset_level(self.game.cvars.int("sv_maxclients").clamp(1, 64) as usize);
        self.init_game(map, game_var)?;
        for _ in 0..SETTLE_FRAMES {
            self.svs_time += SETTLE_STEP_MS;
            self.run_frame();
        }
        self.flush_game_output();
        for (mut peer, name, stats) in humans {
            let j = self.join_human(&name, Some(stats));
            if let Ok(slot) = j {
                for line in NetSv::world_commands(&mut self.game, map) {
                    peer.queue(line);
                }
                if let Some(net) = self.net.as_mut() {
                    net.put_peer(slot, peer);
                }
            } else if let (Err(why), Some(net)) = (j, self.net.as_mut()) {
                net.notify(peer.link.addr, why);
            }
        }
        if self.bot_target > 0 {
            self.add_bots(self.bot_target)?;
        }
        self.map_load_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let errs = self.script_errors.len();
        self.say(&format!(
            "map {map} loaded in {:.0} ms, {errs} script errors\n",
            self.map_load_ms
        ));
        Ok(())
    }

    /// `G_InitGame`: scripts, entities, gametype and level init.
    fn init_game(&mut self, map: &str, game_var: Option<Value>) -> Result<(), String> {
        let gametype = self.game.cvars.string("g_gametype").to_ascii_lowercase();
        let prog = {
            let sources: Vec<(String, String)> = self
                .game
                .content
                .rawfiles()
                .filter(|(n, _)| n.ends_with(".gsc"))
                .map(|(n, b)| (n.to_owned(), String::from_utf8_lossy(b).into_owned()))
                .collect();
            let mut refs: Vec<(&str, &str)> = sources
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            refs.push(("cod4e/testclient.gsc", TEST_CLIENT_GSC));
            compile(&refs, &Builtins::stock_mp(), Options::default()).map_err(|errs| {
                let mut s = String::from("script compile errors:\n");
                for e in errs.iter().take(20) {
                    s.push_str(&format!("  {e}\n"));
                }
                s
            })?
        };
        let find = |file: &str, f: &str| prog.find(file, f);
        let gt_file = format!("maps/mp/gametypes/{gametype}");
        let need = |id: Option<u32>, what: &str| id.ok_or_else(|| format!("Could not find {what}"));
        let initstructs = need(
            find("codescripts/struct", "initstructs"),
            "label initstructs in script codescripts/struct",
        )?;
        let createstruct = need(
            find("codescripts/struct", "createstruct"),
            "label createstruct in script codescripts/struct",
        )?;
        let gt_main = need(
            find(&gt_file, "main"),
            &format!("label main in script {gt_file}"),
        )?;
        let cbs = "maps/mp/gametypes/_callbacksetup";
        let callbacks = game::Callbacks {
            start_game_type: Some(need(
                find(cbs, "CodeCallback_StartGameType"),
                "CodeCallback_StartGameType",
            )?),
            give_hardpoint: find("maps/mp/gametypes/_hardpoints", "giveHardpointItem"),
            player_connect: find(cbs, "CodeCallback_PlayerConnect"),
            player_disconnect: find(cbs, "CodeCallback_PlayerDisconnect"),
            player_damage: find(cbs, "CodeCallback_PlayerDamage"),
            player_killed: find(cbs, "CodeCallback_PlayerKilled"),
            player_last_stand: find(cbs, "CodeCallback_PlayerLastStand"),
        };
        self.game.callbacks = callbacks;
        self.game.weapons = sim::weapon::WeaponTable::new(&self.game.content.weapons())
            .map_err(|e| format!("weapon table: {e:?}"))?;
        self.game.pm_params.mantle_anims = self.game.content.mantle_anims();
        let test_client = need(
            find("cod4e/testclient", "TestClient"),
            "TestClient in the bot script",
        )?;
        let level_main = find(&format!("maps/mp/{map}"), "main");
        let dispatch = Dispatch::new(&prog);
        let mut vm = Vm::new(prog).map_err(|e| format!("script load: {e}"))?;
        vm.set_loading(true);
        if let Some(g) = game_var {
            vm.set_game(g);
        }
        let spawn_vars = {
            let clip = self
                .game
                .content
                .clipmap()
                .ok_or("map has no collision data")?;
            let ents = clip.map_ents.as_ref().ok_or("map has no entity string")?;
            game::parse_spawn_vars(&ents.entity_string)?
        };
        self.game.spawn_map_entities(&mut vm, &spawn_vars)?;
        self.game.level.initializing = true;
        let mut errors = Vec::new();
        {
            let mut host = ScriptHost {
                game: &mut self.game,
                dispatch: &dispatch,
            };
            // G_LoadStructs
            call(&mut vm, &mut host, initstructs, None, &mut errors);
            for vars in &spawn_vars {
                if game::spawn_var(vars, "classname") != Some("script_struct") {
                    continue;
                }
                if let Some(Value::Object(o)) =
                    call(&mut vm, &mut host, createstruct, None, &mut errors)
                {
                    set_struct_fields(&host.game.field_types, &o, vars);
                }
            }
            call(&mut vm, &mut host, gt_main, None, &mut errors);
            if let Some(l) = level_main {
                call(&mut vm, &mut host, l, None, &mut errors);
            }
            if let Some(cb) = callbacks.start_game_type {
                call(&mut vm, &mut host, cb, None, &mut errors);
            }
            host.run_calls(&mut vm);
        }
        self.game.level.initializing = false;
        self.record_errors(errors);
        self.run = Some(Running {
            vm,
            dispatch,
            test_client,
        });
        self.flush_game_output();
        Ok(())
    }

    fn record_errors(&mut self, mut errors: Vec<VmError>) {
        errors.append(&mut self.game.nested_errors);
        for e in errors {
            let kind = match e.kind {
                VmErrorKind::Script => "script runtime error",
                VmErrorKind::Fault => "script VM fault",
            };
            let text = format!("******* {kind} *******\n{e}\n");
            self.say(&text);
            let map = self.map_name().unwrap_or("").to_owned();
            self.all_script_errors.push((map, text.clone()));
            self.script_errors.push(text);
        }
    }

    /// One `G_RunFrame` at the current `svs_time`.
    fn run_frame(&mut self) -> TickSample {
        let t0 = Instant::now();
        let (mut gsc, mut bot, mut client) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        self.game.level.frame += 1;
        self.game.level.frametime = self.frame_ms;
        self.game.level.time = self.svs_time;
        for line in self.game.vote_frame() {
            self.cbuf.add_text(&format!("{line}\n"));
        }
        let mut errors = Vec::new();
        // A killcam cannot start before the history does; the script sees the trimmed
        // `archivetime` and gives up when too little is left.
        if let Some(net) = &self.net {
            let span = net.archive_span(self.svs_time) as f32 * 0.001;
            for c in &mut self.game.clients {
                c.archive_time = c.archive_time.min(span);
            }
        }
        if let Some(run) = self.run.as_mut() {
            let mut host = ScriptHost {
                game: &mut self.game,
                dispatch: &run.dispatch,
            };
            // `SV_BotFrame` then each client's `ClientThink`: usercmds reach the game before
            // the frame's script threads run.
            for n in 0..host.game.clients.len() as u16 {
                if !host.game.clients[usize::from(n)].connected() {
                    continue;
                }
                let t = Instant::now();
                let Some(mut b) = host.game.clients[usize::from(n)].bot_brain.take() else {
                    // A person: the usercmds that arrived since the last frame.
                    let t = Instant::now();
                    for cmd in self.net.as_mut().map_or_else(Vec::new, |s| s.take_cmds(n)) {
                        host.game.client_think(&mut run.vm, n, cmd);
                        host.run_calls(&mut run.vm);
                    }
                    client += t.elapsed();
                    continue;
                };
                let cmd = b.usercmd(host.game, &mut self.bot_shared, n, self.svs_time);
                host.game.clients[usize::from(n)].bot_brain = Some(b);
                bot += t.elapsed();
                let t = Instant::now();
                host.game.client_think(&mut run.vm, n, cmd);
                host.run_calls(&mut run.vm);
                client += t.elapsed();
            }
            // Trigger pass: the first drain of the tick's bucket.
            let t = Instant::now();
            errors.extend(run.vm.run_current_threads(&mut host));
            gsc += t.elapsed();
            // G_XAnimUpdateEnt: each entity's animations advance one notetrack at a time and
            // the scripts run between notetracks.
            let t = Instant::now();
            let dt = host.game.level.frametime as f32 * 0.001;
            for n in 0..host.game.ents.len() {
                let n = n as u16;
                let mut left = dt;
                while let Some(note) = host.game.step_anim(n, left) {
                    left -= note.elapsed;
                    run.vm
                        .notify_entity(n, &note.flag, &[Value::str(&note.note)]);
                    errors.extend(run.vm.run_current_threads(&mut host));
                }
            }
            errors.extend(run.vm.inc_time(&mut host));
            gsc += t.elapsed();
            // G_RunFrameForEntity: script movers, then every client's end of frame.
            for n in 0..host.game.ents.len() {
                host.game.run_entity(&mut run.vm, n as u16);
            }
            host.run_calls(&mut run.vm);
            for n in 0..host.game.clients.len() as u16 {
                host.game.client_end_frame(&mut run.vm, n);
            }
            host.game.lag_record();
            host.game.finish_disconnects(&mut run.vm);
        }
        let t = Instant::now();
        let mut gone_lines = Vec::new();
        if let Some(net) = self.net.as_mut() {
            // A script dropped these clients (`kick`): they never sent a leave.
            let mut gone = Vec::new();
            for (n, c) in self.game.clients.iter().enumerate() {
                if c.conn == Conn::Free
                    && let Some(p) = net.peers.get(n).and_then(Option::as_ref)
                {
                    gone.push((
                        n as u16,
                        format!("{}^7 {}", p.name, DropReason::Kicked.text()),
                    ));
                }
            }
            for (n, text) in &gone {
                net.remove_peer(*n, DropReason::Kicked.notice());
                net.print_to_others(*n, text);
                gone_lines.push(format!("{n}:{}\n", crate::vote::clean_name(text)));
            }
            net.flush_ui(&mut self.game);
            net.send_snapshots(&self.game, self.svs_time);
            net.send_sounds(&mut self.game);
        } else {
            self.game.ui.out.clear();
            self.game.ui.dirty_cs.clear();
            self.game.sound_out.clear();
        }
        let net_t = t.elapsed();
        for line in gone_lines {
            self.say(&line);
        }
        self.record_errors(errors);
        if self.game.level.exit_requested {
            self.game.level.exit_requested = false;
            self.game.stats.matches_ended += 1;
            self.cbuf.add_text("map_rotate");
        }
        if let Some(m) = self.game.level.map_requested.take() {
            self.cbuf.add_text(&format!("map {m}"));
        }
        if self.game.level.map_restart_requested {
            self.game.level.map_restart_requested = false;
            self.cbuf.add_text("map_restart");
        }
        let total = t0.elapsed();
        let s = TickSample {
            total_ms: total.as_secs_f64() * 1000.0,
            gsc_ms: gsc.as_secs_f64() * 1000.0,
            bot_ms: bot.as_secs_f64() * 1000.0,
            client_ms: client.as_secs_f64() * 1000.0,
            net_ms: net_t.as_secs_f64() * 1000.0,
            game_ms: (total - gsc - bot - client - net_t).as_secs_f64() * 1000.0,
        };
        self.ticks += 1;
        s
    }

    /// `bots N`: joins `n` test clients. Each connects, begins, and a script thread picks
    /// the team and class like a player at the menus.
    fn add_bots(&mut self, n: usize) -> Result<(), String> {
        let Some(run) = self.run.as_mut() else {
            return Err("Server is not running.".into());
        };
        let mut errors = Vec::new();
        let mut host = ScriptHost {
            game: &mut self.game,
            dispatch: &run.dispatch,
        };
        if host.game.nav.is_none() {
            self.bot_shared.scratch = None;
        }
        host.game.ensure_nav();
        for _ in 0..n {
            let name = format!("bot{}", self.bot_serial);
            let Some(num) = host.game.connect_client(&mut run.vm, true, &name) else {
                self.say("bots: no free client slot\n");
                break;
            };
            self.bot_serial += 1;
            host.run_calls(&mut run.vm);
            host.game.client_begin(&mut run.vm, num);
            host.game.clients[usize::from(num)].bot_brain = Some(Box::new(Brain::new(num)));
            let obj = run.vm.entity(num, gsc::EntClass::Entity);
            match run.vm.call(
                &mut host,
                run.test_client,
                Some(obj),
                &[Value::str("autoassign")],
            ) {
                Ok(_) => {}
                Err(e) => errors.push(e),
            }
            host.run_calls(&mut run.vm);
        }
        self.record_errors(errors);
        self.flush_game_output();
        Ok(())
    }

    /// Runs `n` frames back to back without waiting for the clock (tests, soak runs).
    /// The value of the match statistic `what`, as `expect` names it.
    fn stat(&self, what: &str) -> Result<u64, String> {
        let st = self.game.stats;
        Ok(match what {
            "kills" => st.kills,
            "deaths" => st.deaths,
            "spawns" => st.spawns,
            "respawns" => st.respawns,
            "shots" => st.shots,
            "hits" => st.hits,
            "rounds" => st.matches_ended,
            "plants" => st.plants,
            "defuses" => st.defuses,
            "hardpoints" => st.hardpoints,
            "airstrikes" => st.airstrikes,
            "helicopters" => st.helicopters,
            "heli_shots" => st.heli_shots,
            "heli_hits" => st.heli_hits,
            "heli_crashes" => st.heli_crashes,
            w => return Err(format!("unknown statistic {w:?}")),
        })
    }

    pub fn run_frames(&mut self, n: u32) {
        for _ in 0..n {
            self.svs_time += self.frame_ms;
            let sample = self.run_frame();
            self.place_heli_view();
            if let Some(h) = self.on_tick.as_mut() {
                h(&sample);
            }
            if let Some(r) = self.run.as_mut() {
                r.vm.set_loading(false);
            }
            self.exec_buffer();
            self.cbuf.end_frame();
        }
        self.flush_game_output();
    }

    /// `devheli view`: floats the client behind the first script vehicle (opposite its heading, turned aside, so the view shows its
    /// tail and flank as it flies away), a little below it, looking at it. Does nothing without a vehicle.
    fn place_heli_view(&mut self) {
        let Some((n, distance)) = self.heli_view else {
            return;
        };
        let Some((target, yaw)) = self
            .game
            .vehicles()
            .next()
            .map(|(_, e, _)| (e.origin, e.angles[1]))
        else {
            return;
        };
        // A third of a turn round to the side, so the tilt shows in profile instead of end on.
        let (s, c) = (yaw - 50.0).to_radians().sin_cos();
        let from = [
            target[0] - c * distance,
            target[1] - s * distance,
            target[2] - 120.0,
        ];
        // The eye is 60 units above the feet.
        let (dx, dy, dz) = (
            target[0] - from[0],
            target[1] - from[1],
            target[2] - (from[2] + 60.0),
        );
        // Quake pitch: negative looks up.
        let pitch = -dz.atan2(dx.hypot(dy)).to_degrees();
        self.game.teleport(n, from);
        // Floating there: a player in the air would fall out of the picture.
        if let Some(c) = self.game.client_mut(n) {
            c.noclip = true;
        }
        self.game
            .set_client_view_angle(n, [pitch, dy.atan2(dx).to_degrees(), 0.0]);
    }

    /// Runs the server for `d` of wall-clock time (frames at `sv_fps`, console between them).
    pub fn run_for(&mut self, d: Duration) {
        let end = Instant::now() + d;
        self.run_until(Some(end));
    }

    /// Runs until `quit`.
    pub fn run_forever(&mut self) {
        self.run_until(None);
    }

    fn run_until(&mut self, end: Option<Instant>) {
        let frame = Duration::from_millis(self.frame_ms.max(1) as u64);
        let mut next = Instant::now();
        while !self.quit {
            let now = Instant::now();
            if end.is_some_and(|e| now >= e) {
                break;
            }
            while let Some(line) = self.console.as_ref().and_then(|c| c.try_recv().ok()) {
                self.cbuf.add_text(&line);
            }
            self.exec_buffer();
            self.cbuf.end_frame();
            if self.quit {
                break;
            }
            let now = Instant::now();
            if self.run.is_some() && now >= next {
                // Catch up after a stall, but never spiral: at most 5 frames at once.
                let mut n = 0;
                while Instant::now() >= next && n < 5 {
                    self.svs_time += self.frame_ms;
                    let s = self.run_frame();
                    if let Some(h) = self.on_tick.as_mut() {
                        h(&s);
                    }
                    if let Some(r) = self.run.as_mut() {
                        r.vm.set_loading(false);
                    }
                    next += frame;
                    n += 1;
                }
                if Instant::now() >= next + frame * 5 {
                    next = Instant::now();
                }
                self.flush_game_output();
                continue;
            }
            // Block until the next tick (or console poll) deadline.
            let mut until = if self.run.is_some() {
                next
            } else {
                now + Duration::from_millis(50)
            };
            if let Some(e) = end {
                until = until.min(e);
            }
            let wait = until.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                continue;
            }
            self.net_service(wait);
        }
        self.flush_game_output();
    }

    /// Script callback for a connecting client slot (used once bots and clients exist).
    pub fn callbacks_ready(&self) -> bool {
        self.run.is_some()
            && self.game.callbacks.player_connect.is_some()
            && self.game.callbacks.player_disconnect.is_some()
    }

    /// Live script objects and values, counted against the original's pool limits.
    pub fn script_pool() -> (u32, u32) {
        gsc::value::pool_usage()
    }

    pub fn thread_count(&self) -> usize {
        self.run.as_ref().map_or(0, |r| r.vm.thread_count())
    }

    /// Builtins bound by the loaded scripts that run as logged no-ops, with call counts.
    pub fn stub_calls(&self) -> Vec<(String, u64)> {
        let mut v: Vec<_> = self
            .game
            .stub_calls
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        v.sort();
        v
    }

    pub fn missing_builtins(&self) -> Vec<String> {
        self.run
            .as_ref()
            .map(|r| {
                r.dispatch
                    .missing()
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Why a client left the match.
#[derive(Clone, Copy)]
enum DropReason {
    /// It said so.
    Left,
    Kicked,
    TimedOut,
    /// It fell too far behind on reliable commands.
    Overflow,
}

impl DropReason {
    /// What the others read after the player's name.
    fn text(self) -> &'static str {
        match self {
            Self::Left => "left the game",
            Self::Kicked => "was kicked",
            Self::TimedOut => "timed out",
            Self::Overflow => "overflowed its reliable commands",
        }
    }

    /// What the client itself is told, when it did not choose to go.
    fn notice(self) -> Option<&'static str> {
        match self {
            Self::Left => None,
            Self::Kicked => Some("Player kicked"),
            Self::TimedOut => Some("Server timed out your connection"),
            Self::Overflow => Some("Server command overflow"),
        }
    }
}

/// What a kick costs the player.
enum Penalty {
    None,
    /// `sv_kickBanTime` seconds.
    Brief,
    Permanent,
}

impl Penalty {
    /// The penalty of a kick command by its lower-case name.
    fn of(command: &str) -> Self {
        match command {
            "kick" | "tempbanuser" | "tempbanclient" => Self::Brief,
            "banuser" | "banclient" => Self::Permanent,
            _ => Self::None,
        }
    }
}

/// The `ping` column of `status`: the measured round trip (at most 9999), or the state the client is in.
fn ping_field(bot: bool, zombie: bool, connecting: bool, ping: Option<i32>) -> String {
    if bot {
        "BOT".into()
    } else if zombie {
        "ZMBI".into()
    } else if connecting {
        "CNCT".into()
    } else {
        ping.unwrap_or(0).clamp(0, 9999).to_string()
    }
}

/// Splits `+a b +c d` into command lines.
fn parse_command_line(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in args {
        if let Some(rest) = a.strip_prefix('+') {
            out.push(quote_word(rest));
        } else if let Some(last) = out.last_mut() {
            last.push(' ');
            last.push_str(&quote_word(a));
        }
    }
    out
}

fn quote_word(w: &str) -> String {
    if w.is_empty() || w.contains(char::is_whitespace) {
        format!("\"{w}\"")
    } else {
        w.to_owned()
    }
}

fn known_maps(install: &Install) -> Vec<String> {
    let mut dir = install.root.clone();
    for part in ["zone", install.language] {
        match std::fs::read_dir(&dir).ok().and_then(|mut d| {
            d.find_map(|e| {
                e.ok()
                    .filter(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))
            })
        }) {
            Some(e) => dir = e.path(),
            None => return Vec::new(),
        }
    }
    let mut v: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p: PathBuf = e.path();
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let is_ff = p.extension()?.eq_ignore_ascii_case("ff");
            (is_ff && stem.starts_with("mp_") && !stem.ends_with("_load")).then_some(stem)
        })
        .collect();
    v.sort();
    v
}

fn call(
    vm: &mut Vm,
    host: &mut ScriptHost,
    func: u32,
    this: Option<Obj>,
    errors: &mut Vec<VmError>,
) -> Option<Value> {
    match vm.call(host, func, this, &[]) {
        Ok(CallOutcome::Finished(v)) => Some(v),
        Ok(CallOutcome::Pending) => None,
        Err(e) => {
            errors.push(e);
            None
        }
    }
}

fn set_struct_fields(
    types: &std::collections::HashMap<String, game::FieldTy>,
    o: &Obj,
    vars: &game::SpawnVars,
) {
    for (k, v) in vars {
        let k = k.to_ascii_lowercase();
        if let Some(t) = types.get(&k) {
            o.set(&k.as_str().into(), game::typed_value(*t, v));
        }
    }
}

fn rss_line() -> Option<String> {
    let peak = crate::mem::peak_rss()?;
    let cur = crate::mem::rss().map_or(String::new(), |r| format!("rss {} MiB, ", r >> 20));
    Some(format!("memory: {cur}peak {} MiB\n", peak >> 20))
}

#[cfg(test)]
mod tests {
    use super::ping_field;

    #[test]
    fn status_ping_shows_the_state_before_the_round_trip_and_caps_it() {
        assert_eq!(ping_field(true, false, false, Some(40)), "BOT");
        assert_eq!(ping_field(false, true, true, Some(40)), "ZMBI");
        assert_eq!(ping_field(false, false, true, Some(40)), "CNCT");
        assert_eq!(ping_field(false, false, false, Some(40)), "40");
        assert_eq!(ping_field(false, false, false, Some(123_456)), "9999");
        assert_eq!(ping_field(false, false, false, None), "0");
    }
}
