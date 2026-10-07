// SPDX-License-Identifier: GPL-3.0-or-later
//! The headless server: boot, map load, console commands and the frame loop.
//!
//! Boot follows the original's dedicated start: command-line `set`s first, `default_mp.cfg`
//! and `language.cfg` from the IWDs, the dedicated zone set, then the held command-line
//! commands (`+exec server.cfg`, `+map x`). A map load follows `SV_SpawnServer`: map zone,
//! game init (script load, entity spawn, `initstructs`, gametype `main`, level `main`, the
//! start callback), then three settle frames 100 ms apart. Frames run `G_RunFrame` with the
//! original's drain points.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gsc::{Builtins, CallOutcome, Obj, Options, Value, Vm, VmError, VmErrorKind, compile};

use crate::cmd::{Argv, CommandBuffer, split_commands};
use crate::content::{Content, Install};
use crate::cvar::{self, Cvars};
use crate::game::{self, Game};
use crate::script::{Dispatch, ScriptHost};

/// Original loop time limit while scripts run (`LOOP_TIMEOUT` of the VM) applies unchanged.
const SETTLE_FRAMES: i32 = 3;
const SETTLE_STEP_MS: i32 = 100;

/// Called after every frame with its cost.
pub type TickHook = Box<dyn FnMut(&TickSample)>;

/// One frame's cost, in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct TickSample {
    pub total_ms: f64,
    /// Script threads: the three drains of `G_RunFrame`.
    pub gsc_ms: f64,
    /// Everything else in the frame (entity think, console, bookkeeping).
    pub game_ms: f64,
}

/// Names of the per-subsystem tick columns, in [`TickSample::subsystems`] order.
pub const TICK_SUBSYSTEMS: [&str; 2] = ["gsc", "game"];

impl TickSample {
    pub fn subsystems(&self) -> [f64; 2] {
        [self.gsc_ms, self.game_ms]
    }
}

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
];

/// Commands of the client console that mean nothing to a headless server; accepted silently
/// so the stock configs run unchanged.
const CLIENT_ONLY: &[&str] = &[
    "bind",
    "unbind",
    "unbindall",
    "bindlist",
    "seta_noop",
    "setfromdvar",
    "exec_noop",
];

#[derive(Default)]
struct Callbacks {
    start_game_type: Option<u32>,
    player_connect: Option<u32>,
    player_disconnect: Option<u32>,
}

struct Running {
    vm: Vm,
    dispatch: Dispatch,
    callbacks: Callbacks,
}

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
    socket: Option<UdpSocket>,
    pub map_load_ms: f64,
    pub boot_ms: f64,
    pub ticks: u64,
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
        ("g_password", "", 0),
        ("g_speed", "190", 0),
        ("g_gravity", "800", 0),
        ("g_knockback", "1000", 0),
        ("g_inactivity", "0", 0),
        ("g_synchronousClients", "0", SYSTEMINFO),
        ("sv_cheats", "0", 0),
        ("sv_mapRotation", "", 0),
        ("sv_mapRotationCurrent", "", 0),
        ("nextmap", "map_restart", 0),
        ("gamename", "Call of Duty 4", SERVERINFO | ROM),
        ("g_log", "games_mp.log", ARCHIVE),
        ("loc_language", "0", ARCHIVE),
    ] {
        c.register(n, d, f);
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
        // Com_StartupVariable: only set lines run before the cvars are registered.
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
            } else {
                startup.push(line);
            }
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
            socket: None,
            map_load_ms: 0.0,
            boot_ms: 0.0,
            ticks: 0,
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
        match UdpSocket::bind(("0.0.0.0", u16::try_from(port).unwrap_or(28960))) {
            Ok(s) => {
                self.say(&format!("listening on udp port {port}\n"));
                self.socket = Some(s);
            }
            Err(e) => self.say(&format!("WARNING: cannot bind udp port {port}: {e}\n")),
        }
    }

    /// Prints a console line: to the log and, when enabled, stdout.
    pub fn say(&mut self, s: &str) {
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
        let mut s =
            format!("hostname: {host}\nmap: {map}\nnum score ping name\n--- ----- ---- ----\n");
        s.push_str(&format!(
            "tick: {}  level time: {} ms  entities: {}  script threads: {}\n",
            self.ticks,
            self.game.level.time,
            self.game.in_use().count(),
            self.run.as_ref().map_or(0, |r| r.vm.thread_count())
        ));
        if let Some(r) = rss_line() {
            s.push_str(&r);
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
        self.run = None;
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
            let refs: Vec<(&str, &str)> = sources
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
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
        let callbacks = Callbacks {
            start_game_type: Some(need(
                find(cbs, "CodeCallback_StartGameType"),
                "CodeCallback_StartGameType",
            )?),
            player_connect: find(cbs, "CodeCallback_PlayerConnect"),
            player_disconnect: find(cbs, "CodeCallback_PlayerDisconnect"),
        };
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
        }
        self.game.level.initializing = false;
        self.record_errors(errors);
        self.run = Some(Running {
            vm,
            dispatch,
            callbacks,
        });
        self.flush_game_output();
        Ok(())
    }

    fn record_errors(&mut self, errors: Vec<VmError>) {
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
        let mut gsc = Duration::ZERO;
        self.game.level.frame += 1;
        self.game.level.frametime = self.frame_ms;
        self.game.level.time = self.svs_time;
        let mut errors = Vec::new();
        if let Some(run) = self.run.as_mut() {
            let mut host = ScriptHost {
                game: &mut self.game,
                dispatch: &run.dispatch,
            };
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
            // G_RunFrameForEntity: script movers (client end frames: nothing to run yet).
            for n in 0..host.game.ents.len() {
                host.game.run_mover(&mut run.vm, n as u16);
            }
        }
        self.record_errors(errors);
        if self.game.level.exit_requested {
            self.game.level.exit_requested = false;
            self.cbuf.add_text("map_rotate");
        }
        if self.game.level.map_restart_requested {
            self.game.level.map_restart_requested = false;
            self.cbuf.add_text("map_restart");
        }
        let total = t0.elapsed();
        let s = TickSample {
            total_ms: total.as_secs_f64() * 1000.0,
            gsc_ms: gsc.as_secs_f64() * 1000.0,
            game_ms: (total - gsc).as_secs_f64() * 1000.0,
        };
        self.ticks += 1;
        s
    }

    /// Runs `n` frames back to back without waiting for the clock (tests, soak runs).
    pub fn run_frames(&mut self, n: u32) {
        for _ in 0..n {
            self.svs_time += self.frame_ms;
            self.run_frame();
            if let Some(r) = self.run.as_mut() {
                r.vm.set_loading(false);
            }
            self.exec_buffer();
            self.cbuf.end_frame();
        }
        self.flush_game_output();
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
        let mut buf = [0u8; 2048];
        while !self.quit {
            let now = Instant::now();
            if end.is_some_and(|e| now >= e) {
                break;
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
            match &self.socket {
                Some(s) if s.set_read_timeout(Some(wait)).is_ok() => {
                    // Packets are dropped until the netcode lands.
                    let _ = s.recv_from(&mut buf);
                }
                _ => std::thread::sleep(wait),
            }
        }
        self.flush_game_output();
    }

    /// Script callback for a connecting client slot (used once bots and clients exist).
    pub fn callbacks_ready(&self) -> bool {
        self.run.as_ref().is_some_and(|r| {
            r.callbacks.player_connect.is_some() && r.callbacks.player_disconnect.is_some()
        })
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
