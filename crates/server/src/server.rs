// SPDX-License-Identifier: GPL-3.0-only
// The voice audience and session checks (`hears_voice`) are translated from KisakCOD (server_mp/sv_voice_mp.cpp G_BroadcastVoice, game_mp/g_main_mp.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
// Synchronous command selection follows KisakCOD game_mp/g_active_mp.cpp:801-833 (GPL-3.0, copyright the KisakCOD contributors and Activision).
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gsc::{Builtins, CallOutcome, EntClass, Obj, Value, Vm, VmError, VmErrorKind, compile};

use crate::ban::BanList;
use crate::bot::{BotShared, Brain};
use crate::client::{Client, Conn, Session, Team};
use crate::cmd::{Argv, CommandBuffer, split_commands};
use crate::content::{Content, Install};
use crate::cvar::{self, Cvars, Kind};
use crate::game::{self, Game};
use crate::netsv::{Inbound, NetSv, Peer, is_lan};
use crate::rcon::{self, Throttle, Verdict};
use crate::script::{Dispatch, ScriptHost};
use crate::ui::Dest;
use net::Transport;
use net::ui::{PrintKind, ServerCmd};

/// Original loop time limit while scripts run (`LOOP_TIMEOUT` of the VM) applies unchanged.
const SETTLE_FRAMES: i32 = 3;
const SETTLE_STEP_MS: i32 = 100;
/// How long a joined person may take to send their profile before they begin without it (milliseconds of level time).
const STATS_WAIT_MS: i32 = 5000;
/// Stat pairs one `statsync` command may carry.
const STATSYNC_MAX_PAIRS: usize = 64;
/// The least time between one person's client commands while `sv_floodProtect` is on.
const FLOOD_MS: i32 = 800;

/// What a person keeps when the level changes under them (`SV_MapRestart`, `SV_SpawnServer`): their slot
/// (`svs.clients[i]` is reconnected in place), their profile stats, and their `self.pers` script array when the level
/// was left with `savepersist` (`client_t::scriptId`, which `G_ShutdownGame` frees otherwise).
struct Kept {
    slot: u16,
    name: String,
    stats: std::collections::HashMap<i32, i32>,
    pers: Option<Value>,
}

/// `Cmd_MenuResponse_f`: a client's `menuresponse <server id> <menu> <response>` as the script notify's arguments.
/// None when the server id is not this level's (a menu the player answered across a map change: dropped). Anything
/// but three arguments reaches the scripts as the menu `""` and the response `"bad"`.
fn menu_response_args(server_id: i32, argv: &[String]) -> Option<[String; 2]> {
    let [_, id, menu, response] = argv else {
        return Some([String::new(), "bad".into()]);
    };
    (cvar::parse_int(id) == server_id).then(|| [menu.clone(), response.clone()])
}

/// What a person typed as the text of a chat line: no control characters, which steer the localizer.
fn plain_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// `SVC_Status`/`SVC_GameCompleteStatus`: the server's keywords, with the stock `demo ` marker under `fs_restrict`.
fn server_keywords(c: &Cvars) -> String {
    let keywords = c.string("sv_keywords");
    if c.bool("fs_restrict") {
        format!("demo {keywords}")
    } else {
        keywords.to_owned()
    }
}

/// `FS_iwIwd(name, "main")` (KisakCOD `qcommon/files.cpp:57`): one of the stock IWDs of `main`, `main/iw_00` to
/// `main/iw_24` (`name` lowercase, `/`, extension kept).
fn stock_iwd(name: &str) -> bool {
    name.strip_prefix("main/iw_")
        .and_then(|n| n.strip_suffix(".iwd"))
        .is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit()) && n <= "24")
}

/// Whether `sv_referencedIwdNames` would name an IWD that is not a stock one of `main`, which makes `getinfo` and
/// `getstatus` say `mod 1` (KisakCOD `server_mp/sv_main_mp.cpp:609-626`). The original lists the IWDs outside `main`
/// and those of `main` a file was read from (`FS_ReferencedIwdNames`); this counts every non-localized IWD on the
/// search path, so an extra IWD in `main` nothing was read from counts too.
fn foreign_iwds(install: &Install) -> bool {
    install
        .vfs
        .pure_iwds(&install.root)
        .iter()
        .any(|(name, _)| !stock_iwd(name))
}

/// The command line a listen server (the one a player starts from the client) boots with: `dedicated 0`, as the
/// client program registers it, and `sv_pure 1`, which is ours: the original registers `sv_pure` 0 and its menus never
/// set it, so its listen server is impure and says `mod 1` (README, `sv_pure`). `game` is the client's `fs_game`.
pub fn listen_cmdline(port: u16, game: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "+set",
        "dedicated",
        "0",
        "+set",
        "sv_pure",
        "1",
        "+set",
        "net_port",
        &port.to_string(),
    ]
    .map(String::from)
    .to_vec();
    if !game.is_empty() {
        args.extend(["+set", "fs_game", game].map(String::from));
    }
    args
}

/// `Scr_CompileScripts`' `developer_script`: the `/# ... #/` blocks only compile when it is on.
fn script_options(c: &Cvars) -> gsc::Options {
    gsc::Options {
        developer: c.bool("developer_script"),
    }
}

/// `ClientThink` retains the latest received command; `G_RunClient` reuses it every synchronous frame
/// at level time, even when nothing arrived. Normal play still runs the entire received backlog.
fn frame_cmds(
    mut cmds: Vec<sim::pm::UserCmd>,
    retained: sim::pm::UserCmd,
    synchronous: bool,
    time: i32,
) -> impl Iterator<Item = sim::pm::UserCmd> {
    let frame = synchronous.then(|| {
        let mut cmd = cmds.last().copied().unwrap_or(retained);
        cmd.server_time = time;
        cmd
    });
    if synchronous {
        cmds.clear();
    }
    frame.into_iter().chain(cmds)
}

/// The password a `connect` must give (`g_password`); the literal `none` means no password, as at the original.
fn join_password(c: &Cvars) -> &str {
    match c.string("g_password") {
        "none" => "",
        p => p,
    }
}

/// `SV_ExecuteClientCommandString`'s flood check: whether a client command is dropped for coming too soon after
/// the last one. Not for loopback, and not for what the client sends without the player acting (the team and
/// score menus, `mr`, the profile upload, `disconnect`). `next` is when the next command is welcome.
fn flood_drops(verb: &str, loopback: bool, enabled: bool, now: i32, next: &mut i32) -> bool {
    if matches!(
        verb,
        "team" | "score" | "mr" | "statsync" | "statsdone" | "disconnect"
    ) {
        return false;
    }
    let drop = !loopback && enabled && now < *next;
    *next = now + FLOOD_MS;
    drop
}

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
const STATS: &str = "kills|deaths|spawns|respawns|shots|hits|rounds|plants|defuses|hardpoints|airstrikes|helicopters|heli_shots|heli_hits|heli_crashes|playing";

/// Commands the console accepts, for `Console::has_command`: one entry per arm of [`Server::exec_command`], plus the
/// client-only verbs it ignores. The harness asks [`accepts`] before it runs a scenario line, and a line whose verb it
/// does not accept fails the stage, so an arm missing here shows up as a red stage and not as a skipped line.
pub const COMMANDS: &[&str] = &[
    "set",
    "seta",
    "sets",
    "setu",
    "exec",
    "echo",
    "wait",
    "quit",
    "killserver",
    "vstr",
    "map",
    "devmap",
    "map_restart",
    "fast_restart",
    "map_rotate",
    "status",
    "serverinfo",
    "systeminfo",
    "bots",
    "expect",
    "until",
    // Test hooks: `devtele`, `devrun`, `devgrenade`, `devhardpoint`, `devradar`, `devheli`, `devkill`, `devshoot`,
    // `devbotfire`, `devobjective`, `devline`.
    "devtele",
    "devrun",
    "devgrenade",
    "devhardpoint",
    "devradar",
    "devheli",
    "devkill",
    "devshoot",
    "devbotfire",
    "devobjective",
    "devline",
    // Players and administration.
    "clientkick",
    "tempbanclient",
    "devperk",
    "banclient",
    "onlykick",
    "kick",
    "tempbanuser",
    "banuser",
    "unbanuser",
    "dumpuser",
    "say",
    "tell",
    "heartbeat",
    "gamecompletestatus",
    // Variables.
    "toggle",
    "togglep",
    "reset",
    "setfromdvar",
    "dvar_bool",
    "dvar_int",
    "dvar_float",
    "cvarlist",
    "dvarlist",
    "dvardump",
    // Perks, configs and memory (`SV_SetPerk_f`, `Com_WriteConfig_f`, `Com_Meminfo_f`).
    "setperk",
    "writeconfig",
    "writedefaults",
    "meminfo",
    // The command list and the console's print channels (the stock configs run `con_showchannel`).
    "cmdlist",
    "con_showchannel",
    "con_hidechannel",
    "con_channellist",
    "con_visiblechannellist",
];

/// Stock dedicated-server commands this server does not carry out: accepted with a warning that names them and the
/// issue that tracks them, so a config or an admin's line is not `Unknown command`. The 1.8 debug-build commands
/// (`scriptdebugger`, `scriptvarusage`, `scriptprofile`, `scriptbuiltin`, `tempmeminfo`, `dumpraw`) are not in
/// iw3mp.exe 1.7 and are unknown, as there.
const MISSING: &[(&str, u32)] = &[("stringusage", 535), ("scriptusage", 535)];

/// The issue a known-but-unimplemented stock command is tracked by.
fn missing_issue(name: &str) -> Option<u32> {
    MISSING
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, issue)| *issue)
}

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

/// Whether the console has a handler for `name`: a command of [`COMMANDS`], or a client-only verb it ignores.
pub fn accepts(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    COMMANDS.contains(&l.as_str())
        || CLIENT_ONLY.contains(&l.as_str())
        || missing_issue(&l).is_some()
}

struct Running {
    vm: Vm,
    dispatch: Dispatch,
    /// `TestClient` of the engine's bot script: the team and class picks of a test client.
    test_client: u32,
}

/// Level time a test client waits for before it picks a team, on every level start (a `map_restart` loads the level
/// again and the spawn logic's sweep starts over). The stock scripts add their test clients
/// (`_dev::addTestClients`) after `wait 5`, long after the spawn logic's first sweep of the level's spawn points (one
/// point per frame, over a second on a large map) has given every point its player count; a bot picking a team in
/// the first second is placed by `getSpawnpoint_NearTeam` before that.
const BOT_JOIN_MS: i32 = 5000;

/// The bot's menu choices as script (the stock `_dev` script has them behind developer mode).
const TEST_CLIENT_GSC: &str = include_str!("testclient.gsc");

/// The `devline` hook's script: the line's ends, where the string goes and how many frames both stay.
const DEVLINE_GSC: &str = "main(start, end, text_at, frames)\n{\n\tline(start, end, (1, 1, 0), 1, 0, frames);\n\tprint3d(text_at, \"devline\", (1, 1, 1), 1, 0.25, frames);\n}\n";

/// The compiled `devline` script and the VM that runs it.
struct DevLine {
    vm: Vm,
    dispatch: Dispatch,
    main: gsc::bytecode::FuncId,
}

/// What a client is told when its IWDs are not the server's and `sv_pure` is on.
pub const IMPURE: &str = "Unpure client detected: your IWD files differ from this server's (sv_pure is on). Verify your game files, and remove extra .iwd files from main and the mod folder.";

pub struct Server {
    pub game: Game,
    install: Install,
    /// The `devline` hook's script, compiled on first use and kept for the server's lifetime.
    devline: Option<DevLine>,
    run: Option<Running>,
    cbuf: CommandBuffer,
    /// The console's print channels (`con_showchannel`, ...). The output itself is not filtered: the original's
    /// dedicated server writes every line to stdout (`Com_PrintMessage`, KisakCOD common.cpp:174).
    channels: logging::channels::Channels,
    /// Held command-line commands, run once init is complete.
    startup: Vec<String>,
    /// Zone `localized_code_post_gfx_mp` is loaded: `exec` prefers its rawfiles.
    cfg_from_zone: bool,
    /// Server time in milliseconds.
    svs_time: i32,
    frame_ms: i32,
    /// When the next frame is due: kept between [`Server::run_for`] calls, so running in slices keeps the pace (a
    /// slice that began its own schedule ran a frame at its start and the server ran fast).
    next_frame: Option<Instant>,
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
    /// The files a joining client may download for the running mod and map, built when first asked for and kept until
    /// the map changes or a file does.
    offers: crate::fastdl::Offers,
    pub map_load_ms: f64,
    /// Levels started so far: a map change or restart, each of which resets the script pool.
    pub level_loads: u32,
    pub boot_ms: f64,
    pub ticks: u64,
    bot_shared: BotShared,
    /// While set, console output is collected here instead of printed (`rcon` replies).
    redirect: Option<String>,
    rcon_throttle: Throttle,
    /// Lines typed on the server's own terminal (see [`Self::attach_console`]).
    console: Option<std::sync::mpsc::Receiver<String>>,
    /// The opt-in dev control channel, serviced only by this server's thread.
    pub(crate) control: Option<devctrl::Channel>,
    /// When each waiting control request was first offered.
    pub(crate) control_waits: std::collections::HashMap<u64, Instant>,
    /// The game type the running level was started with (`sv.gametype`).
    gametype: String,
    /// The directory `g_log` names a file in.
    log_dir: PathBuf,
    /// Whether this server may tell masters about itself (set by [`Self::announce_to_masters`]).
    masters_allowed: bool,
    heartbeats: crate::heartbeat::Heartbeats,
    /// The search path holds an IWD that is not one of `main`'s stock `iw_00`..`iw_24` (see [`foreign_iwds`]).
    foreign_iwds: bool,
}

/// Set by a signal handler (SIGINT, SIGTERM) so the server leaves its loop and says goodbye to its masters.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Asks every running [`Server`] to stop; safe to call from a signal handler.
pub fn request_stop() {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
}

/// The registrations of the server's dvars (`Dvar_Register*`): name, type and range, default, flags. Each is the
/// stock one (KisakCOD MP 1.8, re-checked against the 1.7 binary's registration calls) unless a comment says why
/// not; the unit test `core_dvars_are_registered_as_stock` holds them to the stock list.
fn core_dvars() -> Vec<(&'static str, Kind, String, u32)> {
    use cvar::*;
    let (int_any, int_pos, f_pos) = (
        Kind::Int(i32::MIN, i32::MAX),
        Kind::Int(0, i32::MAX),
        Kind::Float(0.0, f32::MAX),
    );
    let protocol = net::oob::PROTOCOL as i32;
    let rows: Vec<(&'static str, Kind, &str, u32)> = vec![
        // An enum in stock (listen, LAN, internet), kept as its index.
        ("dedicated", Kind::Int(0, 2), "2", LATCH),
        ("developer", Kind::Int(0, 2), "0", 0),
        ("log_filter", Kind::String, "", 0),
        ("developer_script", Kind::Bool, "0", 0),
        // "Turn off script debugging info" (KisakCOD `game_mp/g_main_mp.cpp:624`): silences `print`/`println`.
        ("g_no_script_spam", Kind::Bool, "0", 0),
        // The stock server runs 20; this one deliberately keeps 30 (README: the game runs at 30 Hz).
        ("sv_fps", Kind::Int(10, 1000), "30", 0),
        (
            "sv_maxclients",
            Kind::Int(1, 64),
            "32",
            ARCHIVE | SERVERINFO | LATCH,
        ),
        (
            "ui_maxclients",
            Kind::Int(1, 64),
            "32",
            ARCHIVE | SERVERINFO | LATCH,
        ),
        ("g_gametype", Kind::String, "war", SERVERINFO | LATCH),
        ("mapname", Kind::String, "", ROM | SERVERINFO),
        ("sv_mapname", Kind::String, "", ROM | SERVERINFO),
        (
            "sv_hostname",
            Kind::String,
            "CoD4Host",
            ARCHIVE | SERVERINFO,
        ),
        ("net_port", Kind::Int(0, 65535), "28960", LATCH),
        // Opt-in: a dedicated server with `sv_public 1` sends heartbeats to the masters `sv_master1`..`sv_master5`
        // (`host[:port]`, all empty by default) so clients can find it on the Internet list.
        ("sv_public", Kind::Bool, "0", 0),
        ("sv_master1", Kind::String, "", 0),
        ("sv_master2", Kind::String, "", 0),
        ("sv_master3", Kind::String, "", 0),
        ("sv_master4", Kind::String, "", 0),
        ("sv_master5", Kind::String, "", 0),
        // WebTransport for browser clients, off when `net_wt` is empty: `[host:]port`, the
        // certificate and key PEM files (empty: a 13-day self-signed one), and the JSON file the
        // page reads for the URL and certificate hash. Command line: `--webtransport`, ...
        ("net_wt", Kind::String, "", LATCH),
        ("net_wt_cert", Kind::String, "", LATCH),
        ("net_wt_key", Kind::String, "", LATCH),
        ("net_wt_info", Kind::String, "", LATCH),
        ("g_password", Kind::String, "", 0),
        ("g_speed", int_any, "190", 0),
        // Milliseconds past a mantle that its landing spot stays blocked to other players.
        (
            "g_mantleBlockTimeBuffer",
            Kind::Int(0, 60_000),
            "500",
            CHEAT,
        ),
        // "Turn on antilag checks for weapon hits" (KisakCOD `game_mp/g_main_mp.cpp:614`): shots and knife swings are
        // judged where the shooter saw the others (`lagcomp`).
        ("g_antilag", Kind::Bool, "1", ARCHIVE | SERVERINFO),
        // "Enable extrapolation between client states" (KisakCOD `game_mp/g_main_mp.cpp:613`; 1.7 registers it at
        // 0x4be6cb): which trajectory type a player's entity state carries (`netsv::player_pos_type`).
        ("g_smoothClients", Kind::Bool, "1", 0),
        ("bot_idle", Kind::Bool, "0", 0),
        // Bots on every map; the listen server's player sets it, and a `bots` command sets it too.
        ("bot_count", Kind::Int(0, 64), "0", 0),
        ("g_allowvote", Kind::Bool, "1", ARCHIVE),
        ("g_deadChat", Kind::Bool, "0", ARCHIVE),
        // KisakCOD registers a Bool 1; the 1.7 binary an Int 4 (0..INT_MAX). Any value but 0 turns it on here.
        ("sv_floodProtect", int_pos, "4", ARCHIVE | SERVERINFO),
        ("g_useholdtime", int_pos, "0", 0),
        ("g_maxDroppedWeapons", Kind::Int(2, 32), "16", 0),
        (
            "g_dropForwardSpeed",
            Kind::Float(0.0, 1000.0),
            "10",
            ARCHIVE,
        ),
        ("g_dropUpSpeedBase", Kind::Float(0.0, 1000.0), "10", ARCHIVE),
        ("g_dropUpSpeedRand", Kind::Float(0.0, 1000.0), "5", ARCHIVE),
        (
            "g_dropHorzSpeedRand",
            Kind::Float(0.0, 1000.0),
            "100",
            ARCHIVE,
        ),
        ("pickupPrints", Kind::Bool, "0", CHEAT),
        ("player_throwbackInnerRadius", f_pos, "90", CHEAT),
        ("player_throwbackOuterRadius", f_pos, "160", CHEAT),
        ("player_MGUseRadius", f_pos, "128", CHEAT),
        (
            "bg_maxGrenadeIndicatorSpeed",
            Kind::Float(0.0, 1000.0),
            "20",
            CHEAT,
        ),
        (
            "perk_grenadeDeath",
            Kind::String,
            "frag_grenade_short_mp",
            CHEAT,
        ),
        (
            "g_useholdspawndelay",
            Kind::Int(0, 1000),
            "500",
            ARCHIVE | CHEAT,
        ),
        ("g_gravity", Kind::Float(1.0, f32::MAX), "800", 0),
        ("g_knockback", Kind::Float(-f32::MAX, f32::MAX), "1000", 0),
        ("g_minGrenadeDamageSpeed", f_pos, "400", CHEAT),
        ("g_inactivity", int_pos, "0", 0),
        ("g_synchronousClients", Kind::Bool, "0", SYSTEMINFO),
        // The original's default is on until the first `map` turns it off (`devmap` on). Stock has no flags; here
        // the map commands own it (init protected) and the clients take it from the systeminfo.
        ("sv_cheats", Kind::Bool, "1", SYSTEMINFO | INIT),
        ("sv_serverid", int_any, "0", SYSTEMINFO | ROM),
        // How much an abstention counts against a vote that runs out of time.
        ("g_voteAbstainWeight", Kind::Float(0.0, 1.0), "0.5", ARCHIVE),
        ("g_logSync", Kind::Bool, "0", ARCHIVE),
        ("g_oldVoting", Kind::Bool, "1", ARCHIVE),
        ("fs_game", Kind::String, "", SERVERINFO | SYSTEMINFO | INIT),
        (
            "g_playerCollisionEjectSpeed",
            Kind::Int(0, 32_000),
            "25",
            ARCHIVE,
        ),
        // Stock registrations a config or script may read (issue #369; `game_mp/g_main_mp.cpp`,
        // `server_mp/sv_init_mp.cpp`): red crosshairs over enemies, the ban list, the friendly fire and name
        // distances, the published IWD lists, the client command log switch, packet padding and the animation dump.
        ("g_redCrosshairs", Kind::Bool, "1", LATCH | ARCHIVE),
        ("g_banIPs", Kind::String, "", ARCHIVE),
        (
            "g_friendlyfireDist",
            Kind::Float(0.0, 15000.0),
            "256",
            CHEAT,
        ),
        (
            "g_friendlyNameDist",
            Kind::Float(0.0, 15000.0),
            "15000",
            CHEAT,
        ),
        ("sv_iwds", Kind::String, "", SYSTEMINFO | ROM),
        ("sv_iwdNames", Kind::String, "", SYSTEMINFO | ROM),
        ("sv_showCommands", Kind::Bool, "0", 0),
        ("sv_padPackets", int_pos, "0", 0),
        ("g_dumpAnims", Kind::Int(-1, 1023), "-1", CHEAT),
        ("sv_keywords", Kind::String, "", SERVERINFO),
        (
            "protocol",
            Kind::Int(protocol, protocol),
            "",
            ROM | SERVERINFO,
        ),
        // `Com_Init`: the game version (`shortversion`, published in the server info) and this build's name.
        (
            "shortversion",
            Kind::String,
            net::SHORT_VERSION,
            ROM | SERVERINFO,
        ),
        ("version", Kind::String, "", ROM),
        ("sv_privateClients", Kind::Int(0, 64), "0", SERVERINFO),
        ("sv_pure", Kind::Bool, "0", SERVERINFO | SYSTEMINFO),
        (
            "sv_voice",
            Kind::Bool,
            "0",
            ARCHIVE | SERVERINFO | SYSTEMINFO,
        ),
        ("voice_global", Kind::Bool, "0", ARCHIVE),
        ("voice_deadChat", Kind::Bool, "0", ARCHIVE),
        ("perk_parabolicRadius", f_pos, "400", CHEAT),
        ("perk_parabolicAngle", Kind::Float(0.0, 180.0), "180", CHEAT),
        ("sv_allowDownload", Kind::Bool, "1", ARCHIVE),
        // Fast download: clients are sent to `sv_wwwBaseURL` for the files `sv_allowDownload` lets them have.
        // Clients here always download before they connect, so `sv_wwwDlDisconnected` (stock: should a client leave
        // the server while it downloads) has nothing to change; it is kept for configs that set it.
        ("sv_wwwDownload", Kind::Bool, "0", ARCHIVE),
        ("sv_wwwBaseURL", Kind::String, "", ARCHIVE),
        ("sv_wwwDlDisconnected", Kind::Bool, "0", ARCHIVE),
        // Stock defaults to 1; PunkBuster is not built here, so the server info does not claim it.
        (
            "sv_punkbuster",
            Kind::Bool,
            "0",
            ARCHIVE | SERVERINFO | INIT,
        ),
        ("sv_allowAnonymous", Kind::Bool, "0", SERVERINFO),
        ("sv_disableClientConsole", Kind::Bool, "0", SERVERINFO),
        ("sv_minPing", Kind::Int(0, 999), "0", ARCHIVE | SERVERINFO),
        ("sv_maxPing", Kind::Int(0, 999), "0", ARCHIVE | SERVERINFO),
        // Seconds of silence after which a client is dropped (`sv_timeout`), and for one that has not sent a packet
        // since it connected (`sv_connectTimeout`).
        ("sv_timeout", Kind::Int(0, 1800), "240", 0),
        ("sv_connectTimeout", Kind::Int(0, 1800), "45", 0),
        // Seconds a connected client must have been in before the same address and qport may connect again
        // (`sv_reconnectlimit`).
        ("sv_reconnectlimit", Kind::Int(0, 1800), "3", ARCHIVE),
        // A `connect` giving this password may take the first `sv_privateClients` slots, and skips `g_password`.
        ("sv_privatePassword", Kind::String, "", 0),
        // Clients one address outside the local network may have at once (0: no limit).
        ("sv_maxClientsPerIP", Kind::Int(0, 64), "0", 0),
        // Most bytes a second one client is sent (0: what the client asks for). The original defaults to 5000, which
        // our snapshots (bigger than its delta-compressed ones) would stretch to a few a second; a limit is opt-in.
        (
            "sv_maxRate",
            Kind::Int(0, 25_000),
            "0",
            ARCHIVE | SERVERINFO,
        ),
        // Remote console: empty switches `rcon` off.
        ("rcon_password", Kind::String, "", 0),
        // Seconds `kick` and `tempBanClient` keep a player out.
        ("sv_kickBanTime", Kind::Float(0.0, 3600.0), "300", 0),
        // Where `banUser` / `banClient` keep the permanent bans.
        ("sv_banFile", Kind::String, "ban.txt", 0),
        ("sv_mapRotation", Kind::String, "", 0),
        ("sv_mapRotationCurrent", Kind::String, "", 0),
        // Stock registers it empty; here `vstr nextmap` restarts the map until a rotation sets it.
        ("nextmap", Kind::String, "map_restart", 0),
        ("gamename", Kind::String, "Call of Duty 4", SERVERINFO | ROM),
        // The log file name (empty: no log; stock `games_mp.log`); `cod4e-server` asks for `games_mp.log`.
        ("g_log", Kind::String, "", ARCHIVE),
        ("loc_language", Kind::Int(0, 14), "0", ARCHIVE | LATCH),
        // The message of the day (`g_motd`, KisakCOD `game_mp/g_main_mp.cpp:454`), which each level spawn puts in
        // `cs::MOTD` (`game_mp/g_spawn_mp.cpp:646`, `Game::spawn_map_entities`).
        ("g_motd", Kind::String, "", 0),
        // Set only by `SV_CheckPaused` (`qcommon/common.cpp:1584`, `server_mp/sv_main_mp.cpp:920-940`), for a listen
        // server whose one client paused; nothing pauses a multiplayer game here, so it stays 0.
        ("sv_paused", Kind::Int(0, 2), "0", ROM),
        // "Allow testclients to press attack button" (`server_mp/sv_init_mp.cpp:784`, read by `SV_BotUserMove` at
        // `server_mp/sv_main_mp.cpp:1051`): off, the bots never fire.
        ("sv_botsPressAttackBtn", Kind::Bool, "1", 0),
    ];
    let mut all: Vec<_> = rows
        .into_iter()
        .map(|(n, k, d, f)| (n, k, d.to_owned(), f))
        .collect();
    for &(n, d, hi) in crate::missile::MISSILE_CVARS {
        all.push((n, Kind::Float(0.0, hi), d.to_owned(), CHEAT));
    }
    // The movement dvars (`jump_height`, `friction`, ...): cheat protected, replicated to the clients. Stock also
    // publishes the fall damage heights in the systeminfo, makes the two move thresholds read only and leaves the
    // sprint stick minimum open to all (`bgame/bg_misc.cpp:488-496,817-864`).
    for (n, d, domain) in sim::pm::Params::dvars() {
        let kind = match domain {
            sim::pm::Domain::Bool => Kind::Bool,
            sim::pm::Domain::Int(lo, hi) => Kind::Int(lo, hi),
            sim::pm::Domain::Float(lo, hi) => Kind::Float(lo, hi),
            sim::pm::Domain::Vec2(lo, hi) => Kind::Vec2(lo, hi),
        };
        let flags = match n {
            "bg_fallDamageMinHeight" | "bg_fallDamageMaxHeight" => CHEAT | SYSTEMINFO,
            "player_moveThreshhold" | "player_footstepsThreshhold" => CHEAT | ROM,
            "player_sprintForwardMinimum" => 0,
            _ => CHEAT,
        };
        all.push((n, kind, d, flags));
    }
    all
}

fn register_core_dvars(c: &mut Cvars) {
    for (n, k, d, f) in core_dvars() {
        c.register_kind(n, k, &d, f);
    }
    c.force("protocol", &net::oob::PROTOCOL.to_string());
    c.force("version", &net::version());
}

/// Most bots a `bots` command or `bot_count` asks for (`MAX_CLIENTS`).
const MAX_BOTS: usize = 64;

/// The count of a `bots` command or of `bot_count`: a whole number from 0 to [`MAX_BOTS`], else why not.
fn bot_count(s: &str) -> Result<usize, String> {
    let n: i64 = s
        .trim()
        .parse()
        .map_err(|_| format!("bots: '{s}' is not a number (usage: bots <0-{MAX_BOTS}>)"))?;
    usize::try_from(n)
        .ok()
        .filter(|n| *n <= MAX_BOTS)
        .ok_or_else(|| format!("bots: {n} is out of range (0-{MAX_BOTS})"))
}

/// The stock dvars registered here whose effect this server cannot give, warned about once when set (each level
/// start reads them): `g_banIPs` is filtered by nothing in the stock multiplayer game either (KisakCOD keeps its
/// `ipFilters` only for `addip`, `removeip` and `listip`, `game/g_svcmds.cpp:17-240`), which this console does not
/// have.
fn warn_unhonoured_dvars(c: &Cvars) {
    if !c.string("g_banIPs").trim().is_empty() {
        logging::dedup!(
            WARN,
            "g_banIPs",
            dvar = "g_banIPs",
            "IP list not enforced, as in the stock game; addip/removeip/listip are not implemented (banUser/banClient ban)"
        );
    }
}

/// What one `map_rotate` does with the rotation.
#[derive(Debug, PartialEq, Eq)]
struct RotationPlan {
    /// The console lines, newline included.
    messages: Vec<String>,
    /// What is left of the rotation for the next time (`sv_mapRotationCurrent`).
    remaining: String,
    /// The `gametype` the rotation named last.
    gametype: Option<String>,
    /// The map to change to; none forces a restart.
    map: Option<String>,
}

/// The next word of a rotation (`Com_Parse`, which takes a quoted word whole); `None` when none is left.
fn next_token(text: &mut String) -> Option<String> {
    let t = text.trim_start();
    if t.is_empty() {
        text.clear();
        return None;
    }
    let (token, rest) = match t.strip_prefix('"') {
        Some(q) => q.split_once('"').unwrap_or((q, "")),
        None => t.split_at(t.find(char::is_whitespace).unwrap_or(t.len())),
    };
    let (token, rest) = (token.to_owned(), rest.to_owned());
    *text = rest;
    Some(token)
}

/// `SV_MapRotate_f`: the rotation is `current` while it lasts and is read again from `rotation` when `current` is
/// empty or runs out. `gametype X` sets the game type, `map Y` ends the search, any other word is skipped; a
/// rotation with nothing to change to (or a keyword without its word) restarts the map.
fn plan_rotation(rotation: &str, current: &str) -> RotationPlan {
    let mut cur = if current.trim().is_empty() {
        rotation.to_owned()
    } else {
        current.to_owned()
    };
    let mut plan = RotationPlan {
        messages: Vec::new(),
        remaining: String::new(),
        gametype: None,
        map: None,
    };
    let mut token = next_token(&mut cur);
    if token.is_none() {
        cur = rotation.to_owned();
        token = next_token(&mut cur);
    }
    loop {
        let Some(word) = token else {
            plan.messages
                .push("No map specified in sv_mapRotation - forcing map_restart.\n".into());
            break;
        };
        if word.eq_ignore_ascii_case("gametype") {
            let Some(g) = next_token(&mut cur) else {
                plan.messages.push(
                    "No gametype specified after 'gametype' keyword in sv_mapRotation - forcing map_restart.\n"
                        .into(),
                );
                break;
            };
            plan.messages.push(format!("Setting g_gametype: {g}.\n"));
            plan.gametype = Some(g);
        } else if word.eq_ignore_ascii_case("map") {
            match next_token(&mut cur) {
                Some(m) => {
                    plan.messages.push(format!("Setting map: {m}.\n"));
                    plan.map = Some(m);
                }
                None => plan.messages.push(
                    "No map specified after 'map' keyword in sv_mapRotation - forcing map_restart.\n"
                        .into(),
                ),
            }
            break;
        } else {
            plan.messages
                .push(format!("Unknown keyword '{word}' in sv_mapRotation.\n"));
        }
        token = next_token(&mut cur);
    }
    plan.remaining = cur;
    plan
}

fn io_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Server {
    /// Boots a dedicated server on the install at `root`. `cmdline` is the program's
    /// arguments after the executable name (`+set a b +exec server.cfg +map mp_crash`); a listen server's starts
    /// with [`listen_cmdline`].
    pub fn boot(root: &Path, cmdline: &[String], echo: bool) -> Result<Self, String> {
        Self::boot_in(root, cmdline, echo, PathBuf::from("."))
    }

    /// [`Self::boot`] with the directory the game log (`g_log`) is written in.
    pub fn boot_in(
        root: &Path,
        cmdline: &[String],
        echo: bool,
        log_dir: PathBuf,
    ) -> Result<Self, String> {
        let t0 = Instant::now();
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
        // `fs_game` is read once, before the search path is built (`FS_Startup`).
        let fs_game = cvars.string("fs_game").to_owned();
        let install = Install::open_game(root, &fs_game)
            .map_err(|e| format!("{}: {}", root.display(), io_err(e)))?;
        let foreign_iwds = foreign_iwds(&install);
        let game = Game::new(cvars, Content::default());
        let mut s = Server {
            game,
            install,
            run: None,
            cbuf: CommandBuffer::new(),
            devline: None,
            channels: logging::channels::Channels::default(),
            startup,
            cfg_from_zone: false,
            svs_time: 0,
            frame_ms: 33,
            next_frame: None,
            quit: false,
            script_errors: Vec::new(),
            all_script_errors: Vec::new(),
            log: Vec::new(),
            echo_stdout: echo,
            on_tick: None,
            heli_view: None,
            net: None,
            begin_waits: Vec::new(),
            offers: crate::fastdl::Offers::default(),
            map_load_ms: 0.0,
            level_loads: 0,
            boot_ms: 0.0,
            ticks: 0,
            bot_shared: BotShared::default(),
            redirect: None,
            rcon_throttle: Throttle::default(),
            console: None,
            control: None,
            control_waits: Default::default(),
            gametype: String::new(),
            log_dir,
            masters_allowed: false,
            heartbeats: Default::default(),
            foreign_iwds,
        };
        s.say("CoD4 MP headless server (cod4e)\n");
        s.say(&format!(
            "install: {} (language {})\n",
            root.display(),
            s.install.language
        ));
        if !fs_game.is_empty() {
            s.say(&format!("fs_game: {fs_game}\n"));
        }
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
        s.game.field_types = match s.game.content.rawfile("radiant/keys.txt") {
            Some(t) => game::parse_field_types(&String::from_utf8_lossy(t)),
            None => {
                tracing::warn!(asset = "radiant/keys.txt", "entity field types missing");
                Default::default()
            }
        };
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
            Err(e) => {
                tracing::error!(port, error = %e, "cannot bind udp port");
                return self.say(&format!("WARNING: cannot bind udp port {port}: {e}\n"));
            }
        };
        let max = usize::try_from(self.game.cvars.int("sv_maxclients")).unwrap_or(32);
        let wt = self.game.cvars.string("net_wt").trim().to_owned();
        #[cfg(not(target_arch = "wasm32"))]
        let t: Box<dyn net::Transport + Send> = if wt.is_empty() {
            Box::new(udp)
        } else {
            match self.start_webtransport(udp, &wt) {
                Ok(j) => Box::new(j),
                Err(e) => {
                    tracing::error!(bind = %wt, error = %e, "webtransport endpoint did not start");
                    return self.say(&format!("WARNING: webtransport {wt}: {e}\n"));
                }
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
        tracing::info!(port = n.local_addr().port(), "listening on udp");
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

    /// Lets this server list itself on masters when `sv_public` is set: only the dedicated server program asks, a
    /// listen server never does.
    pub fn announce_to_masters(&mut self) {
        self.masters_allowed = true;
    }

    /// Sends the heartbeats and flatlines that are due.
    fn send_heartbeats(&mut self, net: &mut NetSv) {
        let c = &self.game.cvars;
        let public = self.masters_allowed && c.int("sv_public") != 0;
        let masters: Vec<String> = (1..=net::master::MASTER_SLOTS)
            .map(|i| c.string(&format!("sv_master{i}")).to_owned())
            .collect();
        for (to, o) in self.heartbeats.step(
            Instant::now(),
            public,
            &masters,
            crate::heartbeat::resolve_name,
        ) {
            net.t.send_to(to, &o.encode());
        }
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
        self.send_heartbeats(&mut net);
        let info = self.server_info();
        let status = self.server_status();
        let mut remote = Vec::new();
        let answer = || crate::netsv::Status {
            info: status.0.clone(),
            players: status.1.clone(),
        };
        let secs = |name: &str| Duration::from_secs(self.game.cvars.int(name).max(1) as u64);
        net.timeout = secs("sv_timeout");
        net.connect_timeout = secs("sv_connectTimeout");
        net.track_ping =
            self.game.cvars.int("sv_minPing") != 0 || self.game.cvars.int("sv_maxPing") != 0;
        for i in net.poll(wait, &|| info.clone(), &answer) {
            match i {
                Inbound::Rcon {
                    from,
                    password,
                    command,
                } => remote.push((from, password, command)),
                Inbound::Connect(req, ping) => self.net_accept(&mut net, &req, ping),
                Inbound::GetFile { from, name, offset } => {
                    self.net_file(&mut net, from, &name, offset);
                }
                Inbound::GetManifest { from } => self.net_manifest(&mut net, from),
            }
        }
        self.relay_voice(&mut net);
        for (slot, line) in std::mem::take(&mut net.inbox) {
            self.net_client_command(&mut net, slot, &line);
        }
        for slot in net.closed() {
            self.net_drop(&mut net, slot, DropReason::Left);
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

    /// Forwards the voice frames clients sent (`sv_voice`; off drops them) to whoever [`hears_voice`] says may hear
    /// the speaker, minus the players who muted the speaker. The speaker counts as talking for a moment.
    fn relay_voice(&mut self, net: &mut NetSv) {
        let frames = net.take_voice();
        if self.game.cvars.int("sv_voice") == 0 {
            return;
        }
        let rules = VoiceRules {
            global: self.game.cvars.bool("voice_global"),
            dead_chat: self.game.cvars.bool("voice_deadChat"),
            parabolic_radius: self.game.cvars.float("perk_parabolicRadius"),
            parabolic_angle: self.game.cvars.float("perk_parabolicAngle"),
        };
        let time = self.game.level.time;
        for (speaker, seq, frame) in frames {
            // Only a person who has joined talks: not a bot, not a slot still connecting.
            if !self.game.client(speaker).is_some_and(may_speak) {
                continue;
            }
            if let Some(c) = self.game.client_mut(speaker) {
                c.talking_until = time + crate::netsv::TALKING_MS;
            }
            let game = &self.game;
            let Some(talker) = game.client(speaker) else {
                continue;
            };
            net.relay_voice(speaker, seq, frame, |listener| {
                game.client(listener)
                    .is_some_and(|l| hears_voice(&rules, talker, l))
            });
        }
    }

    /// The files on offer for the running map, once listed (see [`crate::fastdl::Offers`]); `recheck` lists them
    /// again when one changed.
    fn offer(&mut self, recheck: bool) -> Option<&crate::fastdl::Offer> {
        self.offers.refresh(
            &self.install.root,
            &self.install.game,
            self.game.content.map_name.as_deref(),
            recheck,
        );
        for note in self.offers.take_notes() {
            self.say(&note);
        }
        self.offers.offer()
    }

    /// A `getmanifest`: the files this server gives for its mod and map, and the fast-download base when it has one
    /// (see [`net::manifest`]). Unanswered while the list is being made: the client asks again.
    fn net_manifest(&mut self, net: &mut NetSv, from: SocketAddr) {
        if !self.game.cvars.bool("sv_allowDownload") {
            return net.refuse(from, net::manifest::DOWNLOADS_OFF);
        }
        let www = self.game.cvars.string("sv_wwwBaseURL").trim().to_owned();
        let www = if self.game.cvars.bool("sv_wwwDownload") && net::manifest::valid_www(&www) {
            www
        } else {
            String::new()
        };
        if let Some(offer) = self.offer(true) {
            let files = offer.files.clone();
            net.send_manifest(from, &files, &www);
        } else {
            tracing::debug!(%from, "manifest requested before the file list is ready");
        }
    }

    /// A `getfile`: one of the files of the manifest, from `offset` on (see [`net::download`]).
    fn net_file(&mut self, net: &mut NetSv, from: SocketAddr, name: &str, offset: u64) {
        if !self.game.cvars.bool("sv_allowDownload") {
            return net.refuse(from, net::manifest::DOWNLOADS_OFF);
        }
        let Some(offer) = self.offer(false) else {
            tracing::debug!(%from, file = name, "file requested before the file list is ready");
            return;
        };
        match offer.path(name).map(Path::to_owned) {
            Some(path) => net.send_file(from, &path, offset),
            None => {
                logging::dedup!(WARN, "not-on-offer", %from, file = name, "download of a file not on offer");
                net.refuse(from, net::download::NOT_AVAILABLE);
            }
        }
    }

    /// `rcon <password> <command>` from `from`: the password is checked, the attempt logged, the
    /// commands run now with their console output sent back as print packets.
    fn remote_command(&mut self, from: SocketAddr, password: &str, command: &str) {
        if !self.rcon_throttle.allow(Instant::now()) {
            logging::dedup!(WARN, "rcon", %from, "rcon request dropped by the throttle");
            return;
        }
        let verdict = rcon::check(self.game.cvars.string("rcon_password"), password);
        let bad = if verdict == Verdict::Granted {
            ""
        } else {
            "Bad "
        };
        if verdict != Verdict::Granted {
            tracing::warn!(%from, ?verdict, "rcon with a bad password");
        }
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

    /// The slots from `from` (a private slot's index) up that hold a connected client.
    fn used_slots(&self, range: std::ops::Range<usize>) -> usize {
        range
            .filter(|n| {
                self.game
                    .clients
                    .get(*n)
                    .is_some_and(|c| c.conn != Conn::Free)
            })
            .count()
    }

    /// `SVC_Info`: the answer to `getinfo`, in the original's key order (`challenge`, which the original puts first,
    /// is added by `net::connect::serve`). The reference is the CoD4 MP 1.7 `iw3mp.exe` (`SVC_Info` at
    /// `0x531c..0x532163`); KisakCOD's 1.8 (`server_mp/sv_main_mp.cpp:488-634`) sends the same keys but `hc` and `od`.
    /// A key whose value is empty is left out, as `Info_SetValueForKey` does, and so is one the original only sends
    /// when it is set.
    pub fn server_info(&self) -> Vec<(String, String)> {
        let c = &self.game.cvars;
        let max = usize::try_from(c.int("sv_maxclients")).unwrap_or(0);
        let private = usize::try_from(c.int("sv_privateClients"))
            .unwrap_or(0)
            .min(max);
        let private_used = self.used_slots(0..private);
        let clients = private_used + self.used_slots(private..max);
        let max_clients = max - (private - private_used);
        let mut kv = Vec::new();
        let mut put = |k: &str, v: String| {
            if !v.is_empty() {
                kv.push((k.to_owned(), v));
            }
        };
        put("protocol", net::oob::PROTOCOL.to_string());
        put("hostname", c.string("sv_hostname").to_owned());
        put("mapname", self.map_name().unwrap_or("").to_owned());
        if clients > 0 {
            put("clients", clients.to_string());
        }
        if max_clients > 0 {
            put("sv_maxclients", max_clients.to_string());
        }
        put("gametype", c.string("g_gametype").to_owned());
        // The original also says `pure` when `fs_numServerIwds` is non-zero, but that count is the client side's: the
        // IWD list of the pure server the same process's client is connected to (`FS_PureServerSetLoadedIwds`, from
        // the systeminfo `sv_iwds`, which an `sv_pure 0` server leaves empty). A dedicated server never has one and a
        // listen server's own client is told its `sv_pure`, so it comes to `sv_pure`.
        if c.bool("sv_pure") {
            put("pure", "1".into());
        }
        for (key, cvar) in [("minPing", "sv_minPing"), ("maxPing", "sv_maxPing")] {
            if c.int(cvar) != 0 {
                put(key, c.int(cvar).to_string());
            }
        }
        put("game", c.string("fs_game").to_owned());
        if c.bool("sv_allowAnonymous") {
            put("sv_allowAnonymous", "1".into());
        }
        if c.bool("sv_disableClientConsole") {
            put("con_disabled", "1".into());
        }
        if !join_password(c).is_empty() {
            put("pswrd", "1".into());
        }
        // `hc` and `od` (1.7 only, `0x531f15-0x531fa7`): `scr_hardcore` and `scr_oldschool`, which the scripts set.
        for (key, cvar) in [
            ("ff", "scr_team_fftype"),
            ("kc", "scr_game_allowkillcam"),
            ("hc", "scr_hardcore"),
            ("od", "scr_oldschool"),
        ] {
            if c.int(cvar) != 0 {
                put(key, c.int(cvar).to_string());
            }
        }
        // `com_dedicated`: a dedicated server is hardware 2 (the original's Windows dedicated icon), a listen server
        // 6 (Windows listen).
        put("hw", if c.int("dedicated") != 0 { "2" } else { "6" }.into());
        put("mod", u8::from(self.modded()).to_string());
        put("voice", u8::from(c.bool("sv_voice")).to_string());
        put("pb", u8::from(c.bool("sv_punkbuster")).to_string());
        // Not the original's: our master's way to learn that a dedicated server is no (longer) public (README).
        if self.masters_allowed && c.int("sv_public") == 0 {
            put("public", "0".into());
        }
        kv
    }

    /// `mod` in `getinfo` and `getstatus` (KisakCOD `server_mp/sv_main_mp.cpp:601-627` and `:391-415`): set unless
    /// the server is pure, runs no `fs_game` and its IWDs are all stock ones of `main`.
    fn modded(&self) -> bool {
        let c = &self.game.cvars;
        !c.bool("sv_pure") || !c.string("fs_game").is_empty() || self.foreign_iwds
    }

    /// `SVC_Status`: the answer to `getstatus`: the server info variables, the password and mod flags, and the
    /// players with their score and ping.
    pub fn server_status(&self) -> (Vec<(String, String)>, Vec<net::oob::StatusPlayer>) {
        let c = &self.game.cvars;
        let mut info: Vec<(String, String)> = c
            .info_pairs(cvar::SERVERINFO)
            .filter(|(k, _)| !k.eq_ignore_ascii_case("g_password"))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let mut set =
            |k: &str, v: String| match info.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
                Some(e) => e.1 = v,
                None => info.push((k.to_owned(), v)),
            };
        set("pswrd", u8::from(!join_password(c).is_empty()).to_string());
        set("mod", u8::from(self.modded()).to_string());
        if c.bool("fs_restrict") {
            let keywords = server_keywords(c);
            set("sv_keywords", keywords);
        }
        let players = self
            .game
            .connected_clients()
            .map(|(n, p)| net::oob::StatusPlayer {
                score: p.score,
                ping: if p.bot {
                    0
                } else {
                    self.net
                        .as_ref()
                        .and_then(|net| net.ping_of(n))
                        .unwrap_or(0)
                },
                name: p.name.clone(),
            })
            .collect();
        (info, players)
    }

    /// A `connect` that passed the challenge: gives the sender a slot like a bot gets one, and
    /// tells the scripts. `challenge_ping` is the time since the sender asked for its challenge, when known.
    fn net_accept(
        &mut self,
        net: &mut NetSv,
        req: &net::connect::ConnectRequest,
        challenge_ping: Option<u32>,
    ) {
        // A refused connect is an expected outcome; the reason is the dedup key so a flood costs a few lines.
        let refuse = |net: &mut NetSv, why: &str| {
            logging::dedup!(INFO, why, from = %req.from, why, "connect refused");
            net.t
                .send_to(req.from, &net::Oob::Error(why.into()).encode());
        };
        let ip = req.from.ip();
        let local = is_lan(ip);
        // A client already here with this address and qport: either its `connect` repeated because the response was
        // lost, or it is back (after a crash or a new port), and the old session is a ghost to free first. The
        // original's slot for it is reused; here it is freed and the lowest free slot taken.
        if let Some(slot) = net.session_of(ip, req.qport)
            && let Some(p) = net.peer(slot)
        {
            if p.link.addr == req.from && !p.heard() {
                net.t.send_to(req.from, &net::Oob::ConnectResponse.encode());
                return;
            }
            let limit = Duration::from_secs(self.game.cvars.int("sv_reconnectlimit").max(0) as u64);
            if p.age() < limit {
                return refuse(net, "Reconnecting too soon.");
            }
            self.net_drop(net, slot, DropReason::Left);
        }
        if net.bans.is_banned(ip, Instant::now()) {
            return refuse(net, "You are banned from this server.");
        }
        if self.game.cvars.bool("sv_pure") && req.pure != self.pure_digest() {
            return refuse(net, IMPURE);
        }
        let given = req.password.as_str();
        let private = self.game.cvars.string("sv_privatePassword");
        let privileged = !private.is_empty() && private == given;
        let pw = join_password(&self.game.cvars);
        if !privileged && !pw.is_empty() && pw != given {
            return refuse(net, "Invalid password.");
        }
        // The ping the sender's challenge round trip showed; the local network is exempt.
        if !local && let Some(ping) = challenge_ping {
            let (min, max) = (
                self.game.cvars.int("sv_minPing"),
                self.game.cvars.int("sv_maxPing"),
            );
            let ping = i64::from(ping);
            if min > 0 && ping < i64::from(min) {
                return refuse(net, "Server is for high pings only.");
            }
            if max > 0 && ping > i64::from(max) {
                return refuse(net, "Server is for low pings only.");
            }
        }
        let per_ip = usize::try_from(self.game.cvars.int("sv_maxClientsPerIP")).unwrap_or(0);
        if per_ip > 0 && !local && net.clients_from(ip) >= per_ip {
            return refuse(net, "Too many connections from your address.");
        }
        let name = crate::client::clean_client_name(&req.name);
        let slot = match self.join_human(&name, None, privileged) {
            Ok(n) => n,
            Err(why) => return refuse(net, why),
        };
        if let Some(c) = self.game.client_mut(slot) {
            c.local = ip.is_loopback();
        }
        net.add_peer(slot, req, &name);
        if let Some(p) = net.peers[usize::from(slot)].as_mut() {
            p.privileged = privileged;
        }
        let map = self.map_name().unwrap_or("").to_owned();
        for line in NetSv::world_commands(&mut self.game, &map) {
            net.command(slot, &line);
        }
        if let Some(a) = &self.game.ambient {
            net.command(slot, a);
        }
        tracing::info!(client = slot, name = %name, from = %req.from, "client connected");
        self.say(&format!(
            "{name} connected as client {slot} from {}\n",
            req.from
        ));
    }

    /// What a client's IWDs must add up to on an `sv_pure` server: this server's own search path.
    fn pure_digest(&self) -> u32 {
        assets::vfs::pure_digest(&self.install.vfs.pure_iwds(&self.install.root))
    }

    /// Gives a person a client slot like a bot gets one: the scripts see a connect. With `kept` (a person kept across
    /// a level change) they take their old slot back and begin at once; a new person begins when their profile has
    /// arrived ([`Self::begin_client`]).
    fn join_human(
        &mut self,
        name: &str,
        kept: Option<Kept>,
        privileged: bool,
    ) -> Result<u16, &'static str> {
        let known = kept.is_some();
        // A kept person's slot is free again (the level reset every slot) and is the first one tried from it, as the
        // original reconnects `svs.clients[i]` in place. A person who gave the private password may take a private
        // slot, which the others may not.
        let (first, stats, pers) = match kept {
            Some(k) => (usize::from(k.slot), k.stats, k.pers),
            None => (
                if privileged {
                    0
                } else {
                    usize::try_from(self.game.cvars.int("sv_privateClients")).unwrap_or(0)
                },
                Default::default(),
                None,
            ),
        };
        let Some(run) = self.run.as_mut() else {
            return Err("No map is loaded.");
        };
        let mut host = ScriptHost {
            game: &mut self.game,
            dispatch: &run.dispatch,
        };
        let Some(slot) = host
            .game
            .connect_client_from(&mut run.vm, false, name, first, pers)
        else {
            return Err("Server is full.");
        };
        if let Some(c) = host.game.client_mut(slot) {
            // The stock scripts kick a client whose profile stats are zero (the original's stand-in for a checksum);
            // a person has no profile until it uploads one (`statsync`), so it starts with the nonzero defaults.
            // They are the server's own: announcing them would make the client take them for news and write them
            // into its saved profile.
            for i in 0..5 {
                c.stats.insert(205 + i * 10, 1);
            }
            c.stats.extend(stats);
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
        tracing::info!(client = slot, reason = why.text(), "client dropped");
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
        let verb = argv.first().map(|v| v.to_ascii_lowercase());
        let loopback = net
            .peer(slot)
            .is_some_and(|p| p.link.addr.ip().is_loopback());
        let flood = self.game.cvars.bool("sv_floodProtect");
        let now = self.game.level.time;
        if let (Some(v), Some(c)) = (verb.as_deref(), self.game.client_mut(slot))
            && flood_drops(v, loopback, flood, now, &mut c.next_cmd_at)
        {
            logging::dedup!(
                WARN,
                v,
                client = slot,
                cmd = v,
                "client command dropped by flood protection"
            );
            return;
        }
        match verb.as_deref() {
            Some("disconnect") => self.net_drop(net, slot, DropReason::Left),
            Some(v @ ("mute" | "unmute")) => {
                match argv.get(1).and_then(|n| n.parse::<u16>().ok()) {
                    Some(who) => net.set_muted(slot, who, v == "mute"),
                    None => {
                        logging::dedup!(
                            WARN,
                            v,
                            client = slot,
                            cmd = v,
                            "mute without a client number"
                        );
                    }
                }
            }
            Some(net::ui::SCORES_REQUEST) => net.send_scoreboard(slot, &self.game),
            Some("callvote") => {
                if let Some([what, p1, p2]) = self.game.call_vote(slot, &argv[1..])
                    && let Some(run) = self.run.as_mut()
                {
                    let args = [what, p1, p2].map(|a| Value::str(&a));
                    run.vm.notify_entity(slot, "call_vote", &args);
                }
            }
            Some("vote") => {
                let answer = argv.get(1).map_or("", String::as_str);
                if let Some(option) = self.game.cast_vote(slot, answer)
                    && let Some(run) = self.run.as_mut()
                {
                    run.vm.notify_entity(slot, "vote", &[Value::str(option)]);
                }
            }
            Some("userinfo") => {
                self.userinfo_changed(net, slot, argv.get(1).map_or("", String::as_str))
            }
            Some(cmd @ ("say" | "say_team")) if argv.len() >= 2 => {
                // What a person types is plain text: no control characters, which steer the localizer.
                // `\x15` marks it as an argument, which the client shows as it is, never as a string key.
                let text = format!("\x15{}", plain_text(&argv[1..].join(" ")));
                self.game.chat(slot, &text, cmd == "say_team");
            }
            Some("menuresponse" | "mr") => {
                if let Some(args) = menu_response_args(self.game.server_id, &argv)
                    && let Some(run) = self.run.as_mut()
                {
                    run.vm.notify_entity(
                        slot,
                        "menuresponse",
                        &[Value::str(&args[0]), Value::str(&args[1])],
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
            // `Cmd_Kill_f` needs a playing client and `CheatsOk`: without `sv_cheats` it says so.
            Some("kill") => {
                if let Some(run) = self.run.as_mut() {
                    let mut host = ScriptHost {
                        game: &mut self.game,
                        dispatch: &run.dispatch,
                    };
                    host.game.cmd_kill(&mut run.vm, slot);
                    host.run_calls(&mut run.vm);
                }
            }
            Some(v @ ("follownext" | "followprev")) => {
                if self.game.client(slot).is_some_and(|c| {
                    c.session == crate::client::Session::Spectator && c.archive_time <= 0.0
                }) {
                    self.game
                        .spectate_cycle(slot, if v == "follownext" { 1 } else { -1 });
                }
            }
            Some("where") => self.game.where_am_i(slot),
            // The cheat and developer commands (`crates/server/src/cheats.rs`), each behind `CheatsOk`.
            Some("give") => self.game.cmd_give(slot, &argv),
            Some("take") => self.game.cmd_take(slot, &argv),
            Some("god") => self.game.cmd_toggle_flag(
                slot,
                crate::cheats::FL_GODMODE,
                "GAME_GODMODE_ON",
                "GAME_GODMODE_OFF",
            ),
            Some("demigod") => self.game.cmd_toggle_flag(
                slot,
                crate::cheats::FL_DEMI_GODMODE,
                "GAME_DEMI_GODMODE_ON",
                "GAME_DEMI_GODMODE_OFF",
            ),
            Some("notarget") => self.game.cmd_toggle_flag(
                slot,
                crate::cheats::FL_NOTARGET,
                "GAME_NOTARGETON",
                "GAME_NOTARGETOFF",
            ),
            Some("noclip") => self.game.cmd_noclip(slot, false),
            Some("ufo") => self.game.cmd_noclip(slot, true),
            Some("setviewpos") => self.game.cmd_set_viewpos(slot, &argv),
            Some("entitycount") => self.game.cmd_entity_count(),
            Some("printentities") => self.game.cmd_print_entities(),
            Some("visionsetnaked") => self.game.cmd_vision_set(&argv, false),
            Some("visionsetnight") => self.game.cmd_vision_set(&argv, true),
            // A `statsdone` from a client that is not waiting to begin: nothing to do.
            Some("statsdone") => {}
            // What the original says to a client for a verb it does not know.
            Some(cmd) => {
                logging::dedup!(
                    WARN,
                    "unknown-client-command",
                    client = slot,
                    cmd,
                    "unknown client command"
                );
                self.game.send(
                    Dest::Client(slot),
                    ServerCmd::Print {
                        kind: PrintKind::Console,
                        text: format!("GAME_UNKNOWNCLIENTCOMMAND\x15{cmd}\n"),
                    },
                );
            }
            None => {}
        }
    }

    /// `SV_UpdateUserinfo_f`: a person's userinfo string (`\\name\\X\\rate\\R\\snaps\\S`) arrived. The name goes through
    /// `ClientCleanName` and a new one is published to the others (they see "renamed to"); the rate and snapshot rate
    /// set how often snapshots are sent. In `setclientnamemode("manual_change")` a connected person's new name only
    /// waits for `updateclientnames` (`ClientUserinfoChanged`, KisakCOD `game_mp/g_client_mp.cpp:170-181`); the
    /// server's own copy follows the userinfo whatever the mode (`SV_UserinfoChanged`).
    fn userinfo_changed(&mut self, net: &mut NetSv, slot: u16, info: &str) {
        net.set_userinfo(slot, info);
        let manual = self.game.level.manual_name_change;
        let Some(c) = self.game.client_mut(slot) else {
            return;
        };
        let name = crate::netsv::info_value(info, "name");
        if name.is_empty() {
            return;
        }
        let name = crate::client::clean_client_name(name);
        net.rename(slot, &name);
        if !(manual && c.connected()) {
            c.name.clone_from(&name);
        }
        c.new_name = name;
    }

    /// Prints a console line: to the log and, when enabled, stdout.
    // The dedicated server's console output (command results, chat) is its stdout, not a diagnostic.
    #[allow(clippy::print_stdout)]
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

    /// One section of the state document the dev control channel reads (see `docs/control-channel.md`): the game's
    /// real state rather than what a client drew. `None` for the whole document.
    pub fn control_state(&mut self, what: Option<&str>) -> serde_json::Value {
        use serde_json::{Value, json};
        let clients = || -> Value {
            let mut out = Vec::new();
            for (n, c) in self.game.connected_clients() {
                let flags = self.game.ent(n).map_or(0, |e| e.flags);
                let weapons: Vec<&str> = (1..=self.game.weapons.len() as u16)
                    .filter(|&w| c.inv.has(w))
                    .map(|w| self.game.weapons.name(w))
                    .collect();
                let command = |cmd: sim::pm::UserCmd| {
                    json!({
                        "server_time": cmd.server_time,
                        "forwardmove": cmd.forwardmove,
                        "rightmove": cmd.rightmove,
                        "buttons": cmd.buttons,
                        "angles": cmd.angles,
                        "weapon": cmd.weapon,
                    })
                };
                let f = &c.command_frame;
                out.push(json!({
                    "slot": n,
                    "session": c.session.name(),
                    "name": c.name,
                    "bot": c.bot,
                    "team": c.team as u8,
                    "score": c.score,
                    "kills": c.kills,
                    "deaths": c.deaths,
                    "ping": 0,
                    "health": c.ps.health,
                    "origin": c.ps.origin,
                    "viewangles": c.ps.viewangles,
                    "weapon": c.ps.weapon,
                    "weapon_name": self.game.weapons.name(c.ps.weapon as u16),
                    "weapons": weapons,
                    // The weapon state machine, so a stage can say where an offhand throw stopped.
                    "weapon_state": c.ps.weapon_state,
                    "weapon_flags": c.ps.weapon_flags,
                    "offhand_index": c.ps.offhand_index,
                    "grenade_time_left": c.ps.grenade_time_left,
                    "pm_flags": c.ps.pm_flags,
                    "buttons": c.buttons,
                    // Shots through `FireWeapon`; compass pings are derived client-side from snapshot events.
                    "shots": c.shots,
                    "alive": c.ps.pm_type < sim::pm::PmType::Dead,
                    "spawn_count": c.spawn_count,
                    "command_frame": {
                        "synchronous": f.synchronous,
                        "received": f.received,
                        "latest_received": f.latest_received.map(command),
                        "retained": command(f.retained),
                        "selected": f.selected.map(command),
                        "frame_time": f.frame_time,
                        "previous_time": f.previous_time,
                        "simulated_time": f.simulated_time,
                    },
                    // The cheat commands' state (`noclip`, `ufo`, `god`, `demigod`, `notarget`; #370).
                    "noclip": c.noclip,
                    "ufo": c.ufo,
                    "godmode": flags & crate::cheats::FL_GODMODE != 0,
                    "demigod": flags & crate::cheats::FL_DEMI_GODMODE != 0,
                    "notarget": flags & crate::cheats::FL_NOTARGET != 0,
                }));
            }
            Value::Array(out)
        };
        let entities = || -> Value {
            let mut out = Vec::new();
            for (n, e) in self.game.ents.iter().enumerate() {
                let Some(e) = e else { continue };
                out.push(json!({
                    "number": n,
                    "classname": &*e.classname,
                    "origin": e.origin,
                    "health": e.health,
                    "hidden": e.hidden,
                }));
            }
            Value::Array(out)
        };
        let cvars = || -> Value {
            Value::Object(
                self.game
                    .cvars
                    .iter()
                    .map(|c| (c.name.clone(), Value::String(c.value.clone())))
                    .collect(),
            )
        };
        let section = |name: &str| -> Value {
            match name {
                "map" => json!({
                    "map": self.map_name().unwrap_or(""),
                    "gametype": self.gametype,
                    "time_ms": self.level_time(),
                    "level_loads": self.level_loads,
                    "running": self.is_running(),
                }),
                "clients" | "scoreboard" => json!({
                    "clients": clients(),
                    "axis_score": self.game.team_score[1],
                    "allies_score": self.game.team_score[2],
                }),
                "entities" => entities(),
                "cvars" => cvars(),
                "console" => {
                    json!({"lines": self.log.iter().rev().take(256).rev().collect::<Vec<_>>()})
                }
                "script" => json!({"errors": self.all_script_errors}),
                "net" => {
                    let s = self.net_stats();
                    json!({
                        "addr": self.net_addr().map(|a| a.to_string()),
                        "clients": self.net_clients(),
                        "snapshots_out": s.map(|s| s.snapshots_out),
                        "bytes_in": s.map(|s| s.bytes_in),
                        "bytes_out": s.map(|s| s.bytes_out),
                    })
                }
                _ => Value::Null,
            }
        };
        match what {
            Some(name) => section(name),
            None => {
                let mut m = serde_json::Map::new();
                for name in ["map", "clients", "entities", "net", "console", "script"] {
                    m.insert(name.to_owned(), section(name));
                }
                Value::Object(m)
            }
        }
    }

    /// A cvar's value as the server holds it, for the control channel.
    pub fn control_cvar(&self, name: &str) -> Option<String> {
        self.game.cvars.get(name).map(|c| c.value.clone())
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

    /// A line the host of a listen server typed at the client's console. The original has one command table and
    /// one dvar store for the client and the server it runs (`Cmd_ExecuteSingleCommand`, then `Dvar_Command`, then
    /// `CL_ForwardCommandToServer`), so the host's `status`, `kick`, `map_restart` and `set` of a server variable
    /// reach the server. Here the line runs on the server when it is a command or variable of the server's, and what
    /// it printed (or its error) is the answer for the host's console. `None` when the server knows neither the
    /// command nor the variable: the client forwards the line to the server as a client command, as it would to any
    /// server. A `set` of a variable the server does not register is the client's own and prints nothing (#366).
    pub fn host_line(&mut self, line: &str) -> Option<String> {
        let argv = crate::cmd::tokenize(line);
        let name = argv.first()?;
        let lname = name.to_ascii_lowercase();
        if matches!(lname.as_str(), "set" | "seta" | "sets" | "setu") {
            if !argv.get(1).is_some_and(|n| self.game.cvars.exists(n)) {
                return Some(String::new());
            }
        } else if !self.has_command(name) || CLIENT_ONLY.contains(&lname.as_str()) {
            return None;
        }
        self.redirect = Some(String::new());
        if let Err(e) = self.exec_line(line) {
            self.say(&format!("{e}\n"));
        }
        Some(self.redirect.take().unwrap_or_default())
    }

    /// Whether this console would run `name`: a command of [`COMMANDS`], a client-only verb it ignores, or a
    /// variable ([`Server::exec_command`]'s last arm reads and writes one). The harness asks this before each
    /// scenario line, so a verb nothing handles fails the stage instead of being skipped.
    pub fn has_command(&self, name: &str) -> bool {
        accepts(name) || self.game.cvars.exists(name)
    }

    fn exec_command(&mut self, argv: &Argv) -> Result<(), String> {
        let Some(name) = argv.first() else {
            return Ok(());
        };
        let lname = name.to_ascii_lowercase();
        // The name only: the arguments may hold a password.
        let _span = tracing::debug_span!("command", cmd = lname.as_str()).entered();
        let arg = |i: usize| argv.get(i).map(String::as_str);
        match lname.as_str() {
            "set" | "seta" | "sets" | "setu" => {
                let (Some(n), true) = (arg(1), argv.len() >= 3) else {
                    // `Dvar_Set_f` and the others (KisakCOD dvar_cmds.cpp:244-301).
                    return Err(format!("USAGE: {lname} <variable> <value>"));
                };
                if lname == "set" && !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err(format!("invalid variable name: {n}"));
                }
                let v = argv[2..].join(" ");
                self.console_set(n, &v)?;
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
                    .force("sv_cheats", if lname == "devmap" { "1" } else { "0" });
                self.spawn_server(&m)?;
            }
            "map_restart" => self.restart(false)?,
            "fast_restart" => self.restart(true)?,
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
            // Test hook: `devrun <seconds>` runs the server that long, so a script can let what it set up happen
            // (a teleported bot picks the bomb up) before its next step.
            "devrun" => {
                let secs = arg(1).ok_or("usage: devrun <seconds>")?;
                self.run_frames(
                    cvar::parse_int(secs).max(0) as u32 * 1000 / self.frame_ms.max(1) as u32,
                );
            }
            // Test hook: `devtele <client|human|axis|allies> <x> <y> <z>` puts the player there, standing still;
            // `devtele <client|human|axis|allies> <name>` puts it on the floor in the first live trigger whose
            // targetname starts with `name` and that is on for the player's team; `devtele <…> near <client> <dist>`
            // puts it about `dist` units from that client where the two stand on floor and see each other eye to eye
            // (`Game::teleport_near`). `human` is the first client that is not a bot, `axis` and `allies` the first bot
            // on that team (a bot's team is a random pick).
            "devtele" => {
                let (Some(who), Some(first)) = (arg(1), arg(2)) else {
                    return Err(
                        "usage: devtele <client|human|axis|allies> <x y z | name | near client dist>".into(),
                    );
                };
                let team_bot = |team| {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| c.bot && c.team == team)
                        .map(|(n, _)| n)
                        .ok_or(format!("devtele: no bot on {who}"))
                };
                let n = match who.to_ascii_lowercase().as_str() {
                    "human" => self
                        .game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| n)
                        .ok_or("devtele: no human is connected")?,
                    "axis" => team_bot(Team::Axis)?,
                    "allies" => team_bot(Team::Allies)?,
                    _ => cvar::parse_int(who) as u16,
                };
                match (arg(3), arg(4)) {
                    (Some(target), Some(dist)) if first.eq_ignore_ascii_case("near") => {
                        self.game.teleport_near(
                            n,
                            cvar::parse_int(target) as u16,
                            cvar::parse_float(dist),
                        )?;
                    }
                    (Some(y), Some(z)) => {
                        self.game.teleport(n, [first, y, z].map(cvar::parse_float));
                    }
                    _ => self.game.teleport_to_named(n, first)?,
                }
            }
            // Test hook: `devgrenade <client|human> <weapon> [speed] [fuse_ms]` throws a grenade (or any missile weapon
            // launched like one) from the player's eye along where they look, as a thrown frag would be.
            "devgrenade" => {
                let (Some(who), Some(weapon)) = (arg(1), arg(2)) else {
                    return Err(
                        "usage: devgrenade <client|human> <weapon> [speed] [fuse_ms]".into(),
                    );
                };
                let n = if who == "human" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| n)
                        .ok_or("devgrenade: no human is connected")?
                } else {
                    cvar::parse_int(who) as u16
                };
                let index = self.game.weapons.index(weapon);
                let ps = &self.game.client(n).ok_or("devgrenade: no such client")?.ps;
                let start = crate::fire::view_origin(ps, &self.game.pm_params.bob);
                let (fwd, _, _) = sim::pm::math::angle_vectors(&ps.viewangles);
                let speed = arg(3).map_or(500.0, cvar::parse_float);
                let toss = fwd.map(|v| v * speed);
                let fuse = arg(4).map_or(3000, cvar::parse_int);
                let Some(run) = self.run.as_mut() else {
                    return Err("Server is not running.".into());
                };
                self.game.launch_grenade(
                    &mut run.vm,
                    n,
                    index,
                    start,
                    toss,
                    [0.0; 3],
                    false,
                    fuse,
                )?;
            }
            // Test hook: `devline <client|human> [frames]` runs a script compiled for developer that calls the stock
            // `line` and `print3d` builtins: a yellow line across the player's view 100 units ahead and the string
            // `devline` over it, kept `frames` server frames (default 400), so a listen server's own player draws them
            // (`RB_DrawDebugLines`/`RB_DrawDebugStrings`, KisakCOD `gfx_d3d/rb_debug.cpp:158-226`). Not a stock command;
            // needs `developer 1`. The script's VM lives as long as the server: dropping a VM releases every object
            // on the thread that it does not reach (`gsc::value::release_objects`), the level's included.
            "devline" => {
                if !self.game.cvars.bool("developer") {
                    return Err("devline needs developer 1".into());
                }
                let who = arg(1).ok_or("usage: devline <client|human> [frames]")?;
                let n = if who == "human" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| n)
                        .ok_or("devline: no human is connected")?
                } else {
                    cvar::parse_int(who) as u16
                };
                let frames = arg(2).map_or(400, cvar::parse_int);
                let ps = &self.game.client(n).ok_or("devline: no such client")?.ps;
                let eye = crate::fire::view_origin(ps, &self.game.pm_params.bob);
                let (fwd, right, up) = sim::pm::math::angle_vectors(&ps.viewangles);
                let at = |f: f32, r: f32, u: f32| {
                    Value::Vector(std::array::from_fn(|i| {
                        eye[i] + fwd[i] * f + right[i] * r + up[i] * u
                    }))
                };
                let args = [
                    at(100.0, -30.0, 0.0),
                    at(100.0, 30.0, 0.0),
                    at(100.0, 0.0, 8.0),
                    Value::Int(frames),
                ];
                if self.devline.is_none() {
                    let prog = compile(
                        &[("devline.gsc", DEVLINE_GSC)],
                        &Builtins::stock_mp(),
                        gsc::Options { developer: true },
                    )
                    .map_err(|e| format!("devline: {e:?}"))?;
                    let dispatch = Dispatch::new(&prog);
                    let main = prog
                        .find("devline", "main")
                        .ok_or("devline: the script has no main")?;
                    let vm = Vm::new(prog).map_err(|e| format!("devline: {e:?}"))?;
                    self.devline = Some(DevLine { vm, dispatch, main });
                }
                let d = self.devline.as_mut().expect("compiled above");
                let mut host = ScriptHost {
                    game: &mut self.game,
                    dispatch: &d.dispatch,
                };
                match d.vm.call(&mut host, d.main, None, &args) {
                    Ok(CallOutcome::Finished(_)) => {}
                    Ok(CallOutcome::Pending) => return Err("devline: the script waited".into()),
                    Err(e) => return Err(format!("devline: {}", e.message)),
                }
            }
            // Test hook: `devbotfire <client|axis|allies> <weapon|frag> <ms>` makes a bot stand and, through its
            // usercmds and the shared weapon state machine, hold attack with `weapon` (given with its ammunition and
            // switched to) for `ms` from when it is raised, or hold the frag key (after a `frag_grenade_mp` is given)
            // for `ms`.
            "devbotfire" => {
                self.say("WARNING: devbotfire development-only bot input override requested\n");
                if !self.game.cvars.bool("developer") {
                    return Err("devbotfire needs developer 1".into());
                }
                let (Some(who), Some(weapon), Some(ms)) = (arg(1), arg(2), arg(3)) else {
                    return Err("usage: devbotfire <client|axis|allies> <weapon|frag> <ms>".into());
                };
                let team_bot = |team| {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| c.bot && c.team == team)
                        .map(|(n, _)| n)
                        .ok_or(format!("devbotfire: no bot on {who}"))
                };
                let n = match who.to_ascii_lowercase().as_str() {
                    "axis" => team_bot(Team::Axis)?,
                    "allies" => team_bot(Team::Allies)?,
                    _ => cvar::parse_int(who) as u16,
                };
                let ms = cvar::parse_int(ms);
                let g = &mut self.game;
                let c = g
                    .clients
                    .get_mut(usize::from(n))
                    .filter(|c| c.bot)
                    .ok_or("devbotfire: no such bot")?;
                let (buttons, weapon) = if weapon.eq_ignore_ascii_case("frag") {
                    let index = g.weapons.index("frag_grenade_mp");
                    if index != 0 {
                        // A bot that already threw its frags in the fighting has the weapon but none left.
                        c.inv.give(&g.weapons, &mut c.ps, index, 0);
                        c.inv.give_max_ammo(&g.weapons, &mut c.ps, index);
                    }
                    (sim::pm::button::FRAG, 0)
                } else {
                    let index = g.weapons.index(weapon);
                    if index == 0 {
                        return Err(format!("devbotfire: unknown weapon {weapon:?}"));
                    }
                    c.inv.give(&g.weapons, &mut c.ps, index, 0);
                    c.inv.select(index);
                    (sim::pm::button::ATTACK, index)
                };
                let brain = c.bot_brain.as_mut().ok_or("devbotfire: no such bot")?;
                brain.dev_fire = Some(crate::bot::DevFire::new(buttons, weapon, ms));
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
            // Test hook: `devradar <client|human> <0|1>` gives the player (or takes away) the radar of a UAV, as
            // `hasradar` does.
            "devradar" => {
                let (Some(who), Some(on)) = (arg(1), arg(2)) else {
                    return Err("usage: devradar <client|human> <0|1>".into());
                };
                let n = if who == "human" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| n)
                        .ok_or("devradar: no human is connected")?
                } else {
                    cvar::parse_int(who) as u16
                };
                self.game
                    .client_mut(n)
                    .ok_or("devradar: no such client")?
                    .has_radar = cvar::parse_int(on) != 0;
            }
            // Test hook: `devperk <client|human|bot> <0|1>` sets or clears the GPS jammer perk
            // (`specialty_gpsjammer`, bit 0 of `ps.perks`), which keeps a player off the radar.
            "devperk" => {
                let (Some(who), Some(on)) = (arg(1), arg(2)) else {
                    return Err("usage: devperk <client|human|bot> <0|1>".into());
                };
                let set = cvar::parse_int(on) != 0;
                let targets: Vec<u16> = if who == "bot" {
                    self.game
                        .connected_clients()
                        .filter(|(_, c)| c.bot)
                        .map(|(n, _)| n)
                        .collect()
                } else if who == "human" {
                    self.game
                        .connected_clients()
                        .find(|(_, c)| !c.bot)
                        .map(|(n, _)| vec![n])
                        .ok_or("devperk: no human is connected")?
                } else {
                    vec![cvar::parse_int(who) as u16]
                };
                if targets.is_empty() {
                    return Err("devperk: no such client".into());
                }
                for n in targets {
                    let c = self.game.client_mut(n).ok_or("devperk: no such client")?;
                    if set {
                        c.ps.perks |= 1;
                    } else {
                        c.ps.perks &= !1;
                    }
                }
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
                // `SV_KickNum_f`, `SV_BanNum_f` (KisakCOD sv_ccmds_mp.cpp:784, 885).
                let n = arg(1)
                    .map(cvar::parse_int)
                    .ok_or_else(|| match lname.as_str() {
                        "banclient" => "Usage: banClient <client number>".to_owned(),
                        _ => format!("Usage: {name} <client number>"),
                    })?;
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
                // `SV_Kick_f`, `SV_Ban_f` (KisakCOD sv_ccmds_mp.cpp:675, 761).
                let who = arg(1).ok_or_else(|| match lname.as_str() {
                    "banuser" => "Usage: banUser <player name>".to_owned(),
                    _ => format!("Usage: {name} <player name>\n{name} all = kick everyone"),
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
            "heartbeat" => self.heartbeats.send_now(),
            "gamecompletestatus" => {}
            "toggle" | "togglep" => {
                let n = arg(1).ok_or_else(|| {
                    format!("USAGE: {lname} <variable> <optional value sequence>")
                })?;
                if !self.game.cvars.exists(n) {
                    return Err(format!("toggle failed: dvar '{n}' not found."));
                }
                let cur = self.game.cvars.string(n).to_owned();
                let next = if argv.len() > 2 {
                    let at = argv[2..].iter().position(|v| *v == cur);
                    argv[2 + at.map_or(0, |i| (i + 1) % (argv.len() - 2))].clone()
                } else {
                    (if cvar::parse_int(&cur) == 0 { "1" } else { "0" }).to_owned()
                };
                self.console_set(n, &next)?;
                if lname == "togglep" {
                    let v = self.game.cvars.string(n).to_owned();
                    self.say(&format!("{n} toggled to {v}\n"));
                }
            }
            "reset" => {
                let (Some(n), 2) = (arg(1), argv.len()) else {
                    return Err("USAGE: reset <variable>".into());
                };
                // `Dvar_Reset_f`: a name that is no dvar does nothing.
                if let Some(default) = self.game.cvars.get(n).map(|c| c.default.clone()) {
                    self.console_set(n, &default)?;
                }
            }
            "setfromdvar" => {
                let (Some(dst), Some(src), 3) = (arg(1), arg(2), argv.len()) else {
                    return Err("USAGE: setfromdvar <dest_dvar> <source_dvar>".into());
                };
                let v = self
                    .game
                    .cvars
                    .get(src)
                    .map(|c| c.value.clone())
                    .ok_or_else(|| format!("dvar '{src}' doesn't exist"))?;
                self.console_set(dst, &v)?;
            }
            "cvarlist" | "dvarlist" | "dvardump" => {
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
            "cmdlist" => {
                let lines = logging::channels::cmd_list(COMMANDS.iter().copied(), arg(1));
                self.say(&format!("{}\n", lines.join("\n")));
            }
            "con_showchannel"
            | "con_hidechannel"
            | "con_channellist"
            | "con_visiblechannellist" => {
                let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
                let lines = self.channels.command(&lname, &args).unwrap_or_default();
                self.say(&format!("{}\n", lines.join("\n")));
            }
            // `SV_SetPerk_f` (KisakCOD sv_ccmds_mp.cpp:1065): the named player gets the named perk's bit.
            "setperk" => {
                let Some(who) = arg(1) else {
                    self.say("No player specified.\n");
                    return Ok(());
                };
                let Some(n) = self.find_player(who) else {
                    self.say(&format!("Player {who} is not on the server\n"));
                    return Ok(());
                };
                let perk = arg(2).unwrap_or("");
                match crate::script::PERK_NAMES
                    .iter()
                    .position(|p| p.eq_ignore_ascii_case(perk))
                {
                    Some(i) => {
                        if let Some(c) = self.game.client_mut(n) {
                            c.ps.perks |= 1u32 << i;
                        }
                    }
                    // Stock prints this with `Com_DPrintf`, so only a developer run sees it.
                    None => self.say(&format!("Unknown perk: {perk}\n")),
                }
            }
            // `Dvar_RegisterBool_f`, `Dvar_RegisterInt_f` and `Dvar_RegisterFloat_f`
            // (KisakCOD dvar_cmds.cpp:540-660): a dvar that does not exist, or a string one a command made, is
            // registered as a command-made (`DVAR_EXTERNAL`) dvar of the type; an engine dvar of another type is
            // refused with a message, and one of the same type (an enum counts as an integer) is left alone.
            "dvar_bool" | "dvar_int" | "dvar_float" => {
                let (Some(name), Some(default)) = (arg(1), arg(2)) else {
                    let range = if lname == "dvar_bool" {
                        ""
                    } else {
                        " <min> <max>"
                    };
                    return Err(format!("USAGE: {lname} <name> <default>{range}"));
                };
                let name = name.to_owned();
                let default = default.to_owned();
                let (kind, what) = match lname.as_str() {
                    "dvar_bool" => (cvar::Kind::Bool, "a boolean"),
                    _ => {
                        let (Some(min), Some(max)) = (arg(3), arg(4)) else {
                            return Err(format!("USAGE: {lname} <name> <default> <min> <max>"));
                        };
                        if lname == "dvar_int" {
                            let (min, max) = (cvar::parse_int(min), cvar::parse_int(max));
                            if min > max {
                                self.say(&format!(
                                    "dvar {name}: min {min} should not be greater than max {max}\n"
                                ));
                                return Ok(());
                            }
                            (cvar::Kind::Int(min, max), "an integer")
                        } else {
                            let (min, max) = (cvar::parse_float(min), cvar::parse_float(max));
                            if min > max {
                                self.say(&format!(
                                    "dvar {name}: min {min} should not be greater than max {max}\n"
                                ));
                                return Ok(());
                            }
                            // Stock's float refusal says "integer" too (dvar_cmds.cpp:651).
                            (cvar::Kind::Float(min, max), "an integer")
                        }
                    }
                };
                let existing = self.game.cvars.get(&name).map(|c| {
                    let command_made = !c.registered
                        || (c.flags & cvar::EXTERNAL != 0 && matches!(c.kind, cvar::Kind::String));
                    (
                        command_made,
                        std::mem::discriminant(&c.kind),
                        c.name.clone(),
                    )
                });
                match existing {
                    None | Some((true, ..)) => {
                        self.game
                            .cvars
                            .register_kind(&name, kind, &default, cvar::EXTERNAL);
                    }
                    Some((false, d, _)) if d == std::mem::discriminant(&kind) => {}
                    Some((false, _, stored)) => {
                        self.say(&format!("dvar '{stored}' is not {what} dvar\n"));
                    }
                }
            }
            // `Com_WriteConfig_f` and `Com_WriteDefaults_f` (KisakCOD common.cpp:1628): the archived dvars'
            // values, or every engine dvar's reset value. The name is cut to 63 bytes (`I_strncpyz` into 64) and
            // lands under the log folder (`fs_homepath`'s game directory here), as `FS_BuildOSPath` always puts it
            // under the game directory; `FS_CreatePath` refuses a name with `..` or `::` (com_files.cpp:459), and a
            // drive (`:`) is refused with it.
            "writeconfig" | "writedefaults" => {
                let Some(name) = arg(1) else {
                    return Err(format!("Usage: {lname} <filename>"));
                };
                let mut end = name.len().min(63);
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                let mut file = name[..end].to_owned();
                if !file.rsplit(['/', '\\']).next().unwrap_or("").contains('.') {
                    file.push_str(&".cfg"[..4.min(63 - file.len())]);
                }
                self.say(&format!("Writing {file}.\n"));
                if file.contains("..") || file.contains(':') {
                    self.say(&format!(
                        "WARNING: refusing to create relative path \"{file}\"\n"
                    ));
                    return Ok(());
                }
                let path = self.log_dir.join(file.trim_start_matches(['/', '\\']));
                let mut text = String::from("// generated by Call of Duty, do not modify\n");
                for c in self.game.cvars.iter() {
                    if lname == "writeconfig" {
                        if c.flags & cvar::ARCHIVE != 0 {
                            text += &format!("seta {} \"{}\"\n", c.name, c.value);
                        }
                    } else if c.registered
                        && c.flags & (cvar::ROM | cvar::CHEAT | cvar::EXTERNAL) == 0
                    {
                        text += &format!("set {} \"{}\"\n", c.name, c.default);
                    }
                }
                if let Some(dir) = path.parent()
                    && let Err(e) = std::fs::create_dir_all(dir)
                {
                    tracing::warn!(error = %e, file = file.as_str(), "cannot create the config's folder");
                }
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::warn!(error = %e, file = file.as_str(), "cannot write the config");
                    self.say(&format!("Couldn't write {file}.\n"));
                }
            }
            // `Com_Meminfo_f`: the process's memory (there is no zone allocator to break it down).
            "meminfo" => {
                if let Some(line) = rss_line() {
                    self.say(&line);
                }
            }
            _ if missing_issue(&lname).is_some() => {
                logging::dedup!(
                    WARN,
                    lname.as_str(),
                    command = lname.as_str(),
                    issue = missing_issue(&lname).unwrap_or(535),
                    "stock command not implemented"
                );
            }
            "bots" => {
                let n = bot_count(arg(1).ok_or("usage: bots <count>")?)?;
                // The variable is the one count: the map starts that follow read it.
                self.game.cvars.set("bot_count", &n.to_string());
                self.set_bots(n)?;
            }
            // Test hook: `devobjective <index> <state> [x y z [icon]]` sets an objective as the scripts'
            // `objective_add` does, shown to every team.
            "devobjective" => {
                let (Some(i), Some(state)) = (arg(1), arg(2)) else {
                    return Err("usage: devobjective <index> <state> [x y z [icon]]".into());
                };
                let i = usize::try_from(cvar::parse_int(i)).map_err(|e| e.to_string())?;
                let origin = match (arg(3), arg(4), arg(5)) {
                    (Some(x), Some(y), Some(z)) => Some([x, y, z].map(cvar::parse_float)),
                    _ => None,
                };
                crate::script::uicmd::add_objective(&mut self.game, i, state, origin, arg(6))?;
            }
            // Test hook: `devkill <victim> <attacker> [damage] [mod] [weapon]` has the attacker's weapon hit the victim
            // for that much (a 1000 damage ak47 rifle bullet by default), through `G_Damage` and the scripts' damage
            // callback like a real hit, so the perks the stock scripts apply there act.
            "devkill" => {
                let (Some(v), Some(a)) = (arg(1), arg(2)) else {
                    return Err("usage: devkill <victim> <attacker> [damage] [mod] [weapon]".into());
                };
                let (v, a) = (cvar::parse_int(v) as u16, cvar::parse_int(a) as u16);
                let damage = arg(3).map_or(1000, cvar::parse_int);
                let mean = match arg(4) {
                    Some(m) => crate::combat::mod_from_name(m)
                        .ok_or_else(|| format!("devkill: unknown means of death {m:?}"))?,
                    None => crate::combat::MOD_RIFLE_BULLET,
                };
                let weapon = arg(5).unwrap_or("ak47_mp").to_owned();
                let Some(run) = self.run.as_mut() else {
                    return Err("Server is not running.".into());
                };
                let mut host = ScriptHost {
                    game: &mut self.game,
                    dispatch: &run.dispatch,
                };
                let mut d = crate::combat::Damage::new(damage, mean);
                d.attacker = Some(a);
                d.inflictor = Some(a);
                d.weapon = host.game.weapon_index(&weapon);
                host.game.g_damage(&mut run.vm, v, d);
                host.run_calls(&mut run.vm);
            }
            // Test hook: `devshoot <attacker> <x0 y0 z0> <x1 y1 z1>` fires a 100 damage rifle bullet along the
            // segment through any `trigger_damage` volume on it.
            "devshoot" => {
                if !self.game.cvars.bool("sv_cheats") {
                    return Err("devshoot needs sv_cheats 1".into());
                }
                let nums: Vec<f32> = argv[1..].iter().map(|a| cvar::parse_float(a)).collect();
                if nums.len() != 7 {
                    return Err("usage: devshoot <attacker> <x0 y0 z0> <x1 y1 z1>".into());
                }
                let Some(run) = self.run.as_mut() else {
                    return Err("Server is not running.".into());
                };
                let mut host = ScriptHost {
                    game: &mut self.game,
                    dispatch: &run.dispatch,
                };
                host.game.check_hit_trigger_damage(
                    &mut run.vm,
                    nums[0] as u16,
                    [nums[1], nums[2], nums[3]],
                    [nums[4], nums[5], nums[6]],
                    100,
                    crate::combat::MOD_RIFLE_BULLET,
                );
                host.run_calls(&mut run.vm);
            }
            "serverinfo" => {
                let s = self.game.cvars.info_string(cvar::SERVERINFO);
                self.say(&format!("Server info settings:\n{s}\n"));
            }
            _ if CLIENT_ONLY.contains(&lname.as_str()) => {
                tracing::debug!(cmd = lname.as_str(), "client-only command ignored");
            }
            _ => {
                if self.game.cvars.exists(name) {
                    match arg(1) {
                        Some(_) => {
                            let v = argv[1..].join(" ");
                            self.console_set(name, &v)?;
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
                    logging::dedup!(WARN, "unknown", cmd = lname.as_str(), "unknown command");
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
            .or_else(|| match self.install.vfs.read(&file) {
                Ok(found) => found.map(|b| ("disk", b)),
                Err(e) => {
                    tracing::warn!(file = file.as_str(), error = %e, "config unreadable in the search path");
                    None
                }
            })
            .or_else(|| std::fs::read(&file).ok().map(|b| ("disk", b)));
        let Some((src, bytes)) = found else {
            tracing::warn!(file = file.as_str(), "config not found");
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
                        tracing::error!(%ip, error = %e, "ban not saved; it will not outlast this run");
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

    /// A variable set from the console, a cfg, an rcon or a vote: refusals (read only, cheat protected, out of range)
    /// are told, and a latched value says when it takes effect.
    fn console_set(&mut self, name: &str, value: &str) -> Result<(), String> {
        self.game
            .cvars
            .set_external(name, value)
            .map_err(|e| e.to_string())?;
        if self.game.cvars.pending(name).is_some() {
            self.say(&format!("{name} will be changed upon restarting.\n"));
        }
        Ok(())
    }

    /// `SV_MapRestart`: `map_restart` always loads the level afresh, with the latched variables (`g_gametype`,
    /// `sv_maxclients`) applied; `fast_restart` only restarts the running level, unless one of those has a new value
    /// waiting. A script's `map_restart(true)` keeps the `game` array across it.
    fn restart(&mut self, fast: bool) -> Result<(), String> {
        let map = self.map_name().ok_or("Server is not running.")?.to_owned();
        let cvars = &self.game.cvars;
        let gametype_changes = cvars
            .get("g_gametype")
            .and_then(|c| c.latched.as_deref())
            .is_some_and(|g| !g.eq_ignore_ascii_case(&self.gametype));
        let clients_change = cvars.pending("sv_maxclients").is_some();
        if !fast || gametype_changes || clients_change {
            self.game.level.save_persist = false;
            return self.spawn_server(&map);
        }
        let persist = std::mem::take(&mut self.game.level.save_persist);
        self.start_game(&map, self.persisted_game(persist))
    }

    /// The `game` array to carry into the next level, when the script asked to keep it (`savepersist`).
    fn persisted_game(&self, persist: bool) -> Option<Value> {
        self.run
            .as_ref()
            .filter(|_| persist)
            .map(|r| r.vm.game().clone())
    }

    /// `SV_MapRotate_f`: the next `gametype X` / `map Y` of `sv_mapRotationCurrent`, refilled from `sv_mapRotation`
    /// when it runs out.
    fn map_rotate(&mut self) -> Result<(), String> {
        let c = &self.game.cvars;
        let (rotation, current) = (
            c.string("sv_mapRotation").to_owned(),
            c.string("sv_mapRotationCurrent").to_owned(),
        );
        self.say(&format!(
            "map_rotate...\n\n\"sv_mapRotation\" is:\"{rotation}\"\n\n\"sv_mapRotationCurrent\" is:\"{current}\"\n\n"
        ));
        let running = self.game.cvars.string("g_gametype").to_owned();
        let plan = plan_rotation(&rotation, &current);
        self.say(&plan.messages.concat());
        self.game
            .cvars
            .set("sv_mapRotationCurrent", &plan.remaining);
        if let Some(g) = &plan.gametype {
            if self.run.is_some() && !running.eq_ignore_ascii_case(g) {
                self.game.level.save_persist = false;
            }
            self.game.cvars.set("g_gametype", g);
        }
        match plan.map {
            Some(m) => self.exec_command(&vec!["map".to_owned(), m]),
            None => self.restart(true),
        }
    }

    /// `SV_SpawnServer`: loads the level with the latched variables applied. The `game` array is kept when a script
    /// asked for it (`savepersist`).
    pub fn spawn_server(&mut self, map: &str) -> Result<(), String> {
        let persist = std::mem::take(&mut self.game.level.save_persist);
        let game_var = self.persisted_game(persist);
        self.game.cvars.apply_latched();
        self.start_game(map, game_var)
    }

    /// Loads a level; the `map_load` span and the lifecycle lines wrap [`Self::load_level`].
    fn start_game(&mut self, map: &str, game_var: Option<Value>) -> Result<(), String> {
        let _span = tracing::info_span!("map_load", map).entered();
        tracing::info!("map load started");
        let r = self.load_level(map, game_var);
        match &r {
            Ok(()) => tracing::info!(ms = self.map_load_ms, "map loaded"),
            Err(e) => tracing::error!(error = %e, "map load failed"),
        }
        r
    }

    fn load_level(&mut self, map: &str, game_var: Option<Value>) -> Result<(), String> {
        let t0 = Instant::now();
        // People stay connected across a map change; the level they were in is gone. Each keeps their slot and
        // profile stats, and their `self.pers` array when the script asked for `savepersist` (a round's
        // `map_restart(true)`): `G_ShutdownGame` frees the pers arrays otherwise, so a new map starts everyone at the
        // team menu while a round restart spawns them back on their team with their class.
        self.begin_waits.clear();
        let keep_pers = game_var.is_some();
        let humans: Vec<(Peer, Kept)> = self
            .net
            .as_mut()
            .map_or_else(Vec::new, NetSv::take_peers)
            .into_iter()
            .map(|(slot, peer, name)| {
                let stats = self.game.client(slot).map(|c| c.stats.clone());
                let pers = self.run.as_mut().filter(|_| keep_pers).and_then(|r| {
                    let pers = r.vm.entity(slot, EntClass::Entity).get("pers")?;
                    r.vm.persist(pers.clone());
                    Some(pers)
                });
                let kept = Kept {
                    slot,
                    name,
                    stats: stats.unwrap_or_default(),
                    pers,
                };
                (peer, kept)
            })
            .collect();
        self.shutdown_game_log();
        self.run = None;
        self.level_loads += 1;
        self.game.server_id = (self.level_loads & 0xff) as i32;
        self.game
            .cvars
            .force("sv_serverid", &self.game.server_id.to_string());
        self.script_errors.clear();
        self.say("------ Server Initialization ------\n");
        self.say(&format!("Server: {map}\n"));
        let restart = game_var.is_some();
        if !restart || self.game.content.map_name.as_deref() != Some(map) {
            self.install
                .mount_usermap(map)
                .map_err(|e| format!("ERROR: {e}"))?;
            self.game
                .content
                .load_map(&self.install, map)
                .map_err(|e| format!("ERROR: {e}"))?;
        }
        // `SV_SpawnServer` publishes `sv_referencedIwdNames` for the level: a custom map's IWDs are on it.
        self.foreign_iwds = foreign_iwds(&self.install);
        self.game.cvars.force("mapname", map);
        self.game.cvars.force("sv_mapname", map);
        let fps = self.game.cvars.int("sv_fps").clamp(10, 1000);
        self.frame_ms = 1000 / fps;
        self.svs_time = 0;
        let max = self.game.cvars.int("sv_maxclients").clamp(1, 64) as usize;
        self.game.reset_level(max);
        // The peer table follows `sv_maxclients`, which a map change applies (`SV_ChangeMaxClients`).
        if let Some(net) = self.net.as_mut() {
            net.resize(max);
        }
        self.game.publish_info(true);
        self.init_game(map, game_var)?;
        for _ in 0..SETTLE_FRAMES {
            self.svs_time += SETTLE_STEP_MS;
            self.run_frame();
        }
        self.flush_game_output();
        for (mut peer, kept) in humans {
            let name = kept.name.clone();
            let j = self.join_human(&name, Some(kept), peer.privileged);
            if let Ok(slot) = j {
                if let Some(c) = self.game.client_mut(slot) {
                    c.local = peer.link.addr.ip().is_loopback();
                }
                for line in NetSv::world_commands(&mut self.game, map) {
                    peer.queue(line);
                }
                // The old level's sounds end, and the new level's ambience (started while nobody was attached) begins.
                peer.queue("stopsounds");
                if let Some(a) = &self.game.ambient {
                    peer.queue(a.clone());
                }
                if let Some(net) = self.net.as_mut() {
                    net.put_peer(slot, peer);
                }
            } else if let (Err(why), Some(net)) = (j, self.net.as_mut()) {
                tracing::warn!(name = %name, why, "client could not rejoin after the map change");
                net.notify(peer.link.addr, why);
            }
        }
        warn_unhonoured_dvars(&self.game.cvars);
        let wanted = match bot_count(self.game.cvars.string("bot_count")) {
            Ok(n) => n,
            Err(why) => {
                tracing::warn!(dvar = "bot_count", why, "no bots added");
                0
            }
        };
        if wanted > 0 {
            self.set_bots(wanted)?;
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
        self.gametype.clone_from(&gametype);
        self.open_game_log();
        let prog = {
            let mut sources: Vec<(String, String)> = self
                .game
                .content
                .rawfiles()
                .filter(|(n, _)| n.ends_with(".gsc"))
                .map(|(n, b)| (n.to_owned(), String::from_utf8_lossy(b).into_owned()))
                .collect();
            // Under a mod a script is read from the search path first, and from the fastfiles when no file has
            // it (`Scr_ReadFile`): the mod's loose scripts and IWDs, and `main_shared`, override and add to them.
            if !self.install.game.is_empty() {
                let vfs = &self.install.vfs;
                for name in vfs.list("", ".gsc") {
                    let bytes = match vfs.read(&name) {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!(script = name.as_str(), error = %e, "mod script unreadable");
                            continue;
                        }
                    };
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    match sources.iter_mut().find(|(n, _)| *n == name) {
                        Some(s) => s.1 = text,
                        None => sources.push((name, text)),
                    }
                }
            }
            let mut refs: Vec<(&str, &str)> = sources
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            refs.push(("cod4e/testclient.gsc", TEST_CLIENT_GSC));
            compile(
                &refs,
                &Builtins::stock_mp(),
                script_options(&self.game.cvars),
            )
            .map_err(|errs| {
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
        vm.set_tick_rate(self.game.cvars.int("sv_fps").clamp(10, 1000) as u32);
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

    /// `G_InitGame`'s log lines: the file named by `g_log` is opened for appending and told what started.
    fn open_game_log(&mut self) {
        let c = &self.game.cvars;
        let (path, sync) = (c.string("g_log").to_owned(), c.bool("g_logSync"));
        if path.is_empty() {
            self.say("Not logging to disk.\n");
            return;
        }
        // A file name in the working directory, never a path out of it.
        if path.contains(['/', '\\', ':']) || path.contains("..") {
            tracing::error!(
                file = path.as_str(),
                "game log not opened: not a plain file name"
            );
            self.say(&format!(
                "WARNING: Couldn't open logfile: {path}: not a plain file name\n"
            ));
            return;
        }
        let file = self.log_dir.join(&path);
        match crate::gamelog::GameLog::open(&file.to_string_lossy(), sync) {
            Ok(log) => {
                self.game.log = log;
                let info = self.game.cvars.info_string(cvar::SERVERINFO);
                self.game
                    .log_print("------------------------------------------------------------\n");
                self.game.log_print(&format!("InitGame: {info}\n"));
            }
            Err(e) => {
                tracing::error!(file = path.as_str(), error = %e, "game log not opened");
                self.say(&format!("WARNING: Couldn't open logfile: {path}: {e}\n"));
            }
        }
    }

    /// `G_ShutdownGame`'s log lines.
    fn shutdown_game_log(&mut self) {
        if self.game.log.is_open() {
            self.game.log_print("ShutdownGame:\n");
            self.game
                .log_print("------------------------------------------------------------\n");
            self.game.log.close();
        }
    }

    fn record_errors(&mut self, mut errors: Vec<VmError>) {
        errors.append(&mut self.game.nested_errors);
        for e in errors {
            let kind = match e.kind {
                VmErrorKind::Script => "script runtime error",
                VmErrorKind::Fault => "script VM fault",
            };
            let text = format!("******* {kind} *******\n{e}\n");
            let at = e.trace.first().map_or("", String::as_str);
            // The same error at the same place every frame is one line, then a count.
            match e.kind {
                VmErrorKind::Script => {
                    logging::dedup!(WARN, at, at, "script runtime error: {}", e.message);
                }
                VmErrorKind::Fault => {
                    logging::dedup!(ERROR, at, at, "script VM fault: {}", e.message);
                }
            }
            self.say(&text);
            let map = self.map_name().unwrap_or("").to_owned();
            self.all_script_errors.push((map, text.clone()));
            self.script_errors.push(text);
        }
    }

    /// One `G_RunFrame` at the current `svs_time`.
    fn run_frame(&mut self) -> TickSample {
        let t0 = Instant::now();
        let _span = tracing::debug_span!("server_frame", time = self.svs_time).entered();
        let (mut gsc, mut bot, mut client) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        self.game.level.frame += 1;
        self.game.level.frametime = self.frame_ms;
        self.game.level.time = self.svs_time;
        self.game.publish_info(false);
        // `FlushDebugLines`/`FlushDebugStrings`: last frame's `line`/`print3d` drawings age before this frame adds more.
        self.game.debug_draw.flush();
        // `G_SetClientSound` sets a player's loop afresh every frame: a script's `playloopsound` on one does not stay.
        for e in self.game.ents.iter_mut().flatten() {
            if e.kind == crate::game::EntKind::Client {
                e.loop_sound = 0;
            }
        }
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
            // `ClientThink` receives inputs; `G_RunClient` runs the latest at level time when synchronous.
            let synchronous = host.game.cvars.bool("g_synchronousClients");
            let bots_attack = host.game.cvars.bool("sv_botsPressAttackBtn");
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
                    let cmds = self.net.as_mut().map_or_else(Vec::new, |s| s.take_cmds(n));
                    let c = &mut host.game.clients[usize::from(n)];
                    let mut observed = crate::client::CommandFrame {
                        synchronous,
                        received: cmds.len(),
                        latest_received: cmds.last().copied(),
                        retained: c.received_cmd.unwrap_or(c.cmd),
                        frame_time: self.svs_time,
                        previous_time: c.ps.command_time,
                        ..Default::default()
                    };
                    if !cmds.is_empty() {
                        c.last_cmd_time = self.svs_time;
                    }
                    for cmd in frame_cmds(cmds, observed.retained, synchronous, self.svs_time) {
                        observed.selected = Some(cmd);
                        host.game.clients[usize::from(n)].received_cmd = Some(cmd);
                        // `SV_ClientThink`: `G_SetLastServerTime`, then `ClientThink`.
                        host.game.set_last_server_time(n, cmd.server_time);
                        host.game.client_think(&mut run.vm, n, cmd);
                        host.run_calls(&mut run.vm);
                    }
                    let c = &mut host.game.clients[usize::from(n)];
                    observed.simulated_time = c.ps.command_time;
                    c.command_frame = observed;
                    client += t.elapsed();
                    continue;
                };
                let mut cmd = b.usercmd(host.game, &mut self.bot_shared, n, self.svs_time);
                if !bots_attack {
                    cmd.buttons &= !sim::pm::button::ATTACK;
                }
                host.game.clients[usize::from(n)].bot_brain = Some(b);
                bot += t.elapsed();
                let t = Instant::now();
                host.game.set_last_server_time(n, cmd.server_time);
                host.game.client_think(&mut run.vm, n, cmd);
                host.run_calls(&mut run.vm);
                client += t.elapsed();
            }
            // `G_TouchTriggers` for every player, then the trigger pass: each triggered entity is
            // notified once per round, the scripts run, and touches held back get another round.
            let t = Instant::now();
            host.game.touch_all_triggers(&mut run.vm);
            host.run_calls(&mut run.vm);
            let mut pending = std::mem::take(&mut host.game.level.pending_triggers);
            loop {
                let more = host.game.deliver_triggers(&mut run.vm, &mut pending);
                errors.extend(run.vm.run_current_threads(&mut host));
                if !more {
                    break;
                }
            }
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
            let inactive = std::mem::take(&mut self.game.inactive);
            let reason_of = |n: u16| {
                if inactive.contains(&n) {
                    DropReason::Inactive
                } else {
                    DropReason::Kicked
                }
            };
            for (n, c) in self.game.clients.iter().enumerate() {
                if c.conn == Conn::Free
                    && let Some(p) = net.peers.get(n).and_then(Option::as_ref)
                {
                    gone.push((
                        n as u16,
                        format!("{}^7 {}", p.name, reason_of(n as u16).text()),
                        reason_of(n as u16),
                    ));
                }
            }
            for (n, text, why) in &gone {
                tracing::info!(
                    client = *n,
                    reason = why.text(),
                    "client dropped by the game"
                );
                net.remove_peer(*n, why.notice());
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
            self.cbuf.add_text("fast_restart");
        }
        // `CL_UpdateDebugServerData`: what a listen server's own client draws until the next frame.
        self.game.debug_draw.publish();
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

    /// `bots N`: brings the bots on the server to `n`, kicking the last ones that joined or adding more.
    fn set_bots(&mut self, n: usize) -> Result<(), String> {
        let bots: Vec<u16> = self
            .game
            .connected_clients()
            .filter(|(s, c)| c.bot && !self.game.pending_free.contains(s))
            .map(|(s, _)| s)
            .collect();
        if bots.len() < n {
            return self.add_bots(n - bots.len());
        }
        let Some(run) = self.run.as_mut() else {
            return Err("Server is not running.".into());
        };
        let mut host = ScriptHost {
            game: &mut self.game,
            dispatch: &run.dispatch,
        };
        for slot in bots.into_iter().skip(n) {
            host.game.disconnect_client(&mut run.vm, slot);
            host.run_calls(&mut run.vm);
        }
        self.flush_game_output();
        Ok(())
    }

    /// Joins `n` test clients. Each connects, begins, and a script thread picks
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
        let mut full = false;
        for _ in 0..n {
            let name = format!("bot{}", host.game.bot_serial);
            let Some(num) = host.game.connect_client(&mut run.vm, true, &name) else {
                tracing::warn!(bot = name.as_str(), "no free client slot for a bot");
                full = true;
                break;
            };
            host.game.bot_serial += 1;
            host.run_calls(&mut run.vm);
            host.game.client_begin(&mut run.vm, num);
            host.game.clients[usize::from(num)].bot_brain = Some(Box::new(Brain::new(num)));
            let obj = run.vm.entity(num, gsc::EntClass::Entity);
            match run.vm.call(
                &mut host,
                run.test_client,
                Some(obj),
                &[Value::str("autoassign"), Value::Int(BOT_JOIN_MS)],
            ) {
                Ok(_) => {}
                Err(e) => errors.push(e),
            }
            host.run_calls(&mut run.vm);
        }
        self.record_errors(errors);
        if full {
            self.say("bots: no free client slot\n");
        }
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
            // Players in the game with the controls free (after the prematch countdown), as of now rather than a total.
            "playing" => self
                .game
                .connected_clients()
                .filter(|(_, c)| c.session == Session::Playing && !c.frozen)
                .count() as u64,
            w => return Err(format!("unknown statistic {w:?}")),
        })
    }

    /// Dev-control scheduling fixture: collect actual UDP traffic without ticking, then observe exactly one
    /// normal simulation tick. No commands are injected or rewritten here; `frame_cmds` still selects them.
    pub fn control_command_frame(&mut self, collect: Duration) -> serde_json::Value {
        let end = Instant::now() + collect;
        while Instant::now() < end {
            self.net_service(end.saturating_duration_since(Instant::now()));
        }
        self.run_frames(1);
        self.control_state(Some("clients"))
    }

    pub fn run_frames(&mut self, n: u32) {
        for _ in 0..n {
            self.service_control();
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
        let mut next = self.next_frame.unwrap_or_else(Instant::now);
        while !self.quit {
            let now = Instant::now();
            if STOP_REQUESTED.load(Ordering::Relaxed) {
                break;
            }
            if end.is_some_and(|e| now >= e) {
                break;
            }
            while let Some(line) = self.console.as_ref().and_then(|c| c.try_recv().ok()) {
                self.cbuf.add_text(&line);
            }
            self.service_control();
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
        self.next_frame = Some(next);
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

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(n) = self.net.as_mut() {
            for (to, o) in self.heartbeats.finish() {
                n.t.send_to(to, &o.encode());
            }
        }
        self.shutdown_game_log();
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
    /// It did nothing for `g_inactivity` seconds.
    Inactive,
}

impl DropReason {
    /// What the others read after the player's name.
    fn text(self) -> &'static str {
        match self {
            Self::Left => "left the game",
            Self::Kicked => "was kicked",
            Self::TimedOut => "timed out",
            Self::Overflow => "overflowed its reliable commands",
            Self::Inactive => "was dropped for inactivity",
        }
    }

    /// What the client itself is told, when it did not choose to go.
    fn notice(self) -> Option<&'static str> {
        match self {
            Self::Left => None,
            Self::Kicked => Some("Player kicked"),
            Self::TimedOut => Some("Server timed out your connection"),
            Self::Overflow => Some("Server command overflow"),
            Self::Inactive => Some("You were dropped for inactivity"),
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

/// The `ping` column of `status`: the measured round trip (at most 999), or the state the client is in.
fn ping_field(bot: bool, zombie: bool, connecting: bool, ping: Option<i32>) -> String {
    if bot {
        "BOT".into()
    } else if zombie {
        "ZMBI".into()
    } else if connecting {
        "CNCT".into()
    } else {
        ping.unwrap_or(0).clamp(0, 999).to_string()
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
    assets::paths::maps(&install.root, &install.game)
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

/// The voice dvars `G_BroadcastVoice` reads.
struct VoiceRules {
    /// `voice_global`: everybody hears everybody.
    global: bool,
    /// `voice_deadChat`: the dead and the living hear each other.
    dead_chat: bool,
    /// `perk_parabolicRadius` and `perk_parabolicAngle`: how far and how wide Eavesdrop listens.
    parabolic_radius: f32,
    parabolic_angle: f32,
}

/// Only a person who has joined talks: not a bot, not a slot still connecting.
fn may_speak(c: &Client) -> bool {
    c.conn == Conn::Connected && !c.bot
}

/// Perk bit of `specialty_parabolic` (Eavesdrop).
const PERK_PARABOLIC: u32 = 1 << 9;

// The audience and session checks of `G_BroadcastVoice` (KisakCOD `server_mp/sv_voice_mp.cpp`): a listener is in the
// audience when it is in intermission, voice is global, it shares the speaker's team, the speaker is in a free-for-all
// or it has Eavesdrop and the speaker is within its view cone and radius. An audience member then hears only a
// speaker in the same session state, unless `voice_deadChat` lets the dead and the living talk.
fn hears_voice(rules: &VoiceRules, talker: &Client, listener: &Client) -> bool {
    let same_team = talker.team != Team::Free && talker.team == listener.team;
    let eavesdrops = || {
        if listener.ps.perks & PERK_PARABOLIC == 0 {
            return false;
        }
        let (forward, _, _) = sim::pm::math::angle_vectors(&listener.ps.viewangles);
        let delta = [
            talker.ps.origin[0] - listener.ps.origin[0],
            talker.ps.origin[1] - listener.ps.origin[1],
            talker.ps.origin[2] - listener.ps.origin[2],
        ];
        let (dir, dist) = sim::pm::math::normalize_to(&delta);
        let dot = dir[0] * forward[0] + dir[1] * forward[1] + dir[2] * forward[2];
        rules.parabolic_angle.to_radians().cos() <= dot && rules.parabolic_radius >= dist
    };
    let in_audience = listener.session == Session::Intermission
        || rules.global
        || same_team
        || talker.team == Team::Free
        || eavesdrops();
    in_audience
        && (listener.session == talker.session
            || (listener.session == Session::Dead || talker.session == Session::Dead)
                && rules.dead_chat)
}

#[cfg(test)]
#[path = "stock_dvars.rs"]
mod stock_dvars;

#[cfg(test)]
mod tests {
    use super::may_speak;
    use super::{
        PERK_PARABOLIC, RotationPlan, VoiceRules, flood_drops, frame_cmds, hears_voice,
        menu_response_args, ping_field, plain_text, plan_rotation, register_core_dvars,
        script_options, server_keywords, stock_iwd,
    };
    use crate::client::{Client, Conn, Session, Team};
    use crate::cvar::Cvars;

    #[test]
    fn keywords_reach_the_server_info_and_demo_servers_are_marked() {
        let mut c = Cvars::new();
        register_core_dvars(&mut c);
        assert_eq!(server_keywords(&c), "");
        c.set("sv_keywords", "hardcore harbor");
        assert_eq!(server_keywords(&c), "hardcore harbor");
        c.set("fs_restrict", "1");
        assert_eq!(server_keywords(&c), "demo hardcore harbor");
    }

    #[test]
    fn only_mains_iw_00_to_iw_24_are_stock_iwds() {
        for stock in ["main/iw_00.iwd", "main/iw_13.iwd", "main/iw_24.iwd"] {
            assert!(stock_iwd(stock), "{stock}");
        }
        for other in [
            "main/iw_25.iwd",
            "main/iw_1.iwd",
            "main/iw_custom.iwd",
            "main/xbase_00.iwd",
            "mods/m/iw_00.iwd",
            "main/iw_00.zip",
        ] {
            assert!(!stock_iwd(other), "{other}");
        }
    }

    #[test]
    fn developer_script_gates_the_slash_comment_blocks() {
        let mut c = Cvars::new();
        register_core_dvars(&mut c);
        assert!(!script_options(&c).developer);
        c.set("developer_script", "1");
        assert!(script_options(&c).developer);
    }

    #[test]
    fn synchronous_clients_run_latest_input_at_frame_time_and_retain_it() {
        use sim::pm::UserCmd;
        let backlog: Vec<_> = [100, 108, 116]
            .into_iter()
            .enumerate()
            .map(|(i, server_time)| UserCmd {
                server_time,
                forwardmove: (i as i8 + 1) * 20,
                buttons: 1 << i,
                ..Default::default()
            })
            .collect();
        let selected: Vec<_> = frame_cmds(backlog.clone(), UserCmd::default(), true, 150).collect();
        assert_eq!(selected.len(), 1);
        let mut want = backlog[2];
        want.server_time = 150;
        assert_eq!(selected[0], want);
        let next: Vec<_> = frame_cmds(Vec::new(), selected[0], true, 200).collect();
        want.server_time = 200;
        assert_eq!(
            next,
            [want],
            "an empty frame still thinks with retained input"
        );
        assert_eq!(
            frame_cmds(backlog.clone(), want, false, 250).collect::<Vec<_>>(),
            backlog,
            "switching off runs all arrivals at their original times"
        );
        assert_eq!(frame_cmds(Vec::new(), want, false, 300).count(), 0);
        let again: Vec<_> = frame_cmds(Vec::new(), want, true, 350).collect();
        want.server_time = 350;
        assert_eq!(again, [want], "switching on retains the session command");
    }

    #[test]
    fn a_menu_response_from_an_earlier_level_is_dropped() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let answer = menu_response_args(
            7,
            &v(&["menuresponse", "7", "team_marinesopfor", "autoassign"]),
        );
        assert_eq!(
            answer,
            Some(["team_marinesopfor".into(), "autoassign".into()])
        );
        assert_eq!(
            menu_response_args(8, &v(&["menuresponse", "7", "m", "r"])),
            None
        );
        let bad = Some([String::new(), "bad".to_string()]);
        assert_eq!(menu_response_args(7, &v(&["menuresponse", "m", "r"])), bad);
        assert_eq!(menu_response_args(7, &v(&["menuresponse"])), bad);
    }

    fn player(num: u16, team: Team, session: Session) -> Client {
        let mut c = Client::new(num, false, format!("p{num}"));
        c.team = team;
        c.session = session;
        c
    }

    const RULES: VoiceRules = VoiceRules {
        global: false,
        dead_chat: false,
        parabolic_radius: 400.0,
        parabolic_angle: 180.0,
    };

    #[test]
    fn voice_reaches_the_team_in_the_same_state_and_not_the_dead_unless_dead_chat_is_on() {
        let alive = player(0, Team::Allies, Session::Playing);
        let mate = player(1, Team::Allies, Session::Playing);
        let dead_mate = player(2, Team::Allies, Session::Dead);
        let enemy = player(3, Team::Axis, Session::Playing);
        assert!(hears_voice(&RULES, &alive, &mate));
        assert!(!hears_voice(&RULES, &alive, &enemy));
        assert!(!hears_voice(&RULES, &dead_mate, &mate), "dead to living");
        assert!(!hears_voice(&RULES, &alive, &dead_mate), "living to dead");
        let chat = VoiceRules {
            dead_chat: true,
            ..RULES
        };
        assert!(hears_voice(&chat, &dead_mate, &mate));
        assert!(hears_voice(&chat, &alive, &dead_mate));
        assert!(!hears_voice(&chat, &dead_mate, &enemy), "still the team");
    }

    #[test]
    fn spectators_hear_spectators_and_not_the_players() {
        let spec = player(0, Team::Spectator, Session::Spectator);
        let other = player(1, Team::Spectator, Session::Spectator);
        let ally = player(2, Team::Allies, Session::Playing);
        assert!(hears_voice(&RULES, &spec, &other));
        assert!(!hears_voice(&RULES, &spec, &ally));
    }

    #[test]
    fn a_bot_or_a_connecting_slot_does_not_speak() {
        let mut c = player(0, Team::Allies, Session::Playing);
        c.conn = Conn::Connected;
        assert!(may_speak(&c));
        c.bot = true;
        assert!(!may_speak(&c));
        c.bot = false;
        c.conn = Conn::Connecting;
        assert!(!may_speak(&c));
    }

    #[test]
    fn global_voice_crosses_teams_and_a_free_for_all_speaker_is_heard_by_all() {
        let ally = player(0, Team::Allies, Session::Playing);
        let enemy = player(1, Team::Axis, Session::Playing);
        let global = VoiceRules {
            global: true,
            ..RULES
        };
        assert!(hears_voice(&global, &ally, &enemy));
        let ffa = player(2, Team::Free, Session::Playing);
        assert!(hears_voice(&RULES, &ffa, &enemy));
    }

    #[test]
    fn eavesdrop_hears_enemies_in_front_and_in_range_only() {
        let mut enemy = player(0, Team::Axis, Session::Playing);
        enemy.ps.origin = [200.0, 0.0, 0.0];
        let mut spy = player(1, Team::Allies, Session::Playing);
        assert!(!hears_voice(&RULES, &enemy, &spy), "no perk");
        spy.ps.perks |= PERK_PARABOLIC;
        assert!(hears_voice(&RULES, &enemy, &spy));
        enemy.ps.origin = [500.0, 0.0, 0.0];
        assert!(!hears_voice(&RULES, &enemy, &spy), "out of range");
        enemy.ps.origin = [-200.0, 0.0, 0.0];
        let cone = VoiceRules {
            parabolic_angle: 90.0,
            ..RULES
        };
        assert!(!hears_voice(&cone, &enemy, &spy), "behind");
    }

    #[test]
    fn the_rotation_skips_a_word_it_does_not_know_and_goes_on() {
        let p = plan_rotation(
            "",
            "bogus gametype sd map mp_crash gametype war map mp_backlot",
        );
        assert_eq!(p.map.as_deref(), Some("mp_crash"));
        assert_eq!(p.gametype.as_deref(), Some("sd"));
        assert_eq!(p.remaining, " gametype war map mp_backlot");
        assert_eq!(
            p.messages,
            [
                "Unknown keyword 'bogus' in sv_mapRotation.\n",
                "Setting g_gametype: sd.\n",
                "Setting map: mp_crash.\n"
            ]
        );
    }

    #[test]
    fn an_empty_or_used_up_rotation_is_read_again_from_the_start() {
        let full = "map mp_crash gametype dm map mp_backlot";
        assert_eq!(plan_rotation(full, "").map.as_deref(), Some("mp_crash"));
        // The current rotation ran out after its last map: the next call wraps.
        let wrapped = plan_rotation(full, "   ");
        assert_eq!(wrapped.map.as_deref(), Some("mp_crash"));
        let last = plan_rotation(full, " gametype dm map mp_backlot");
        assert_eq!(
            (last.map.as_deref(), last.gametype.as_deref()),
            (Some("mp_backlot"), Some("dm"))
        );
        assert_eq!(last.remaining, "");
        // Used up by a trailing keyword: the rotation is read again.
        let again = plan_rotation(full, "gametype");
        assert_eq!(
            again.map, None,
            "a gametype without a name forces a restart"
        );
    }

    #[test]
    fn a_rotation_with_nothing_to_change_to_restarts_the_map() {
        let restart = |p: RotationPlan| (p.map, p.messages);
        let (map, said) = restart(plan_rotation("", ""));
        assert_eq!(map, None);
        assert_eq!(
            said,
            ["No map specified in sv_mapRotation - forcing map_restart.\n"]
        );
        let (map, said) = restart(plan_rotation("gametype sd", ""));
        assert_eq!(map, None);
        assert!(said[1].starts_with("No map specified in sv_mapRotation"));
        let (map, said) = restart(plan_rotation("map", ""));
        assert_eq!(map, None);
        assert!(said[0].starts_with("No map specified after 'map' keyword"));
        let (_, said) = restart(plan_rotation("gametype", ""));
        assert!(said[0].starts_with("No gametype specified after 'gametype' keyword"));
    }

    #[test]
    fn a_quoted_word_is_one_word() {
        let p = plan_rotation("", "map \"mp_crash\" gametype dm");
        assert_eq!(p.map.as_deref(), Some("mp_crash"));
    }

    #[test]
    fn a_persons_chat_text_loses_the_marks_a_script_may_use() {
        assert_eq!(plain_text("a\x14b\x15c\nd"), "abcd");
    }

    #[test]
    fn a_second_command_inside_800_ms_is_dropped_except_for_loopback_and_the_exempt_ones() {
        let mut next = 0;
        assert!(!flood_drops("say", false, true, 1000, &mut next));
        assert!(
            flood_drops("callvote", false, true, 1500, &mut next),
            "any verb"
        );
        assert!(
            !flood_drops("say", false, true, 2300, &mut next),
            "800 ms after the last, which a dropped one also restarts"
        );
        for exempt in ["team", "score", "mr", "statsync"] {
            assert!(
                !flood_drops(exempt, false, true, 2301, &mut next),
                "{exempt}"
            );
        }
        assert!(!flood_drops("say", true, true, 2302, &mut next), "loopback");
        assert!(
            !flood_drops("say", false, false, 2303, &mut next),
            "protection off"
        );
    }

    #[test]
    fn status_ping_shows_the_state_before_the_round_trip_and_caps_it() {
        assert_eq!(ping_field(true, false, false, Some(40)), "BOT");
        assert_eq!(ping_field(false, true, true, Some(40)), "ZMBI");
        assert_eq!(ping_field(false, false, true, Some(40)), "CNCT");
        assert_eq!(ping_field(false, false, false, Some(40)), "40");
        assert_eq!(ping_field(false, false, false, Some(123_456)), "999");
        assert_eq!(ping_field(false, false, false, None), "0");
    }

    /// Needs a game install (`COD4_PATH`); skips without one.
    #[test]
    fn an_unknown_console_command_warns_with_its_name_but_not_its_arguments() {
        let Some(root) = std::env::var_os("COD4_PATH").map(std::path::PathBuf::from) else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let args = ["+set", "net_port", "0"].map(String::from);
        let mut s = super::Server::boot(&root, &args, false).expect("boot");
        let (_, events) = logging::capture(|| s.exec_line("logtest_nosuchcmd hunter2"));
        let w = events.iter().find(|e| e.level == tracing::Level::WARN);
        assert_eq!(w.and_then(|e| e.field("cmd")), Some("logtest_nosuchcmd"));
        assert!(events.iter().all(|e| !e.message.contains("hunter2")));
    }

    /// Needs a game install (`COD4_PATH`); skips without one.
    #[test]
    fn script_errors_are_logged_by_kind() {
        let Some(root) = std::env::var_os("COD4_PATH").map(std::path::PathBuf::from) else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let args = ["+set", "net_port", "0"].map(String::from);
        let mut s = super::Server::boot(&root, &args, false).expect("boot");
        let err = |kind| super::VmError {
            kind,
            message: "boom".into(),
            trace: vec!["maps/mp/x::y line 3".into()],
        };
        let (_, events) = logging::capture(|| {
            s.record_errors(vec![
                err(super::VmErrorKind::Script),
                err(super::VmErrorKind::Fault),
            ]);
        });
        let levels: Vec<_> = events.iter().map(|e| e.level).collect();
        assert_eq!(levels, [tracing::Level::WARN, tracing::Level::ERROR]);
        assert_eq!(events[0].field("at"), Some("maps/mp/x::y line 3"));
    }

    #[test]
    fn a_bot_count_that_is_not_a_number_or_out_of_range_says_why() {
        assert_eq!(super::bot_count("3"), Ok(3));
        assert_eq!(super::bot_count("0"), Ok(0));
        assert_eq!(super::bot_count("64"), Ok(64));
        let e = super::bot_count("abc").unwrap_err();
        assert!(e.contains("'abc' is not a number"), "{e}");
        let e = super::bot_count("65").unwrap_err();
        assert!(e.contains("65 is out of range"), "{e}");
        let e = super::bot_count("-1").unwrap_err();
        assert!(e.contains("-1 is out of range"), "{e}");
    }

    #[test]
    fn the_s11_dvars_have_their_stock_defaults_and_only_a_set_ban_list_warns() {
        let mut c = crate::cvar::Cvars::new();
        register_core_dvars(&mut c);
        assert_eq!(c.string("g_motd"), "");
        assert!(c.bool("g_smoothClients") && c.bool("sv_botsPressAttackBtn"));
        assert_eq!(c.int("sv_paused"), 0);
        // Nothing warns at the stock defaults.
        let (_, events) = logging::capture(|| super::warn_unhonoured_dvars(&c));
        let dvars: Vec<_> = events.iter().filter_map(|e| e.field("dvar")).collect();
        assert!(dvars.is_empty(), "{dvars:?}");
        c.set("g_motd", "logtest motd");
        c.set("g_banIPs", "10.0.0.1 ");
        let (_, events) = logging::capture(|| super::warn_unhonoured_dvars(&c));
        let dvars: Vec<_> = events
            .iter()
            .filter(|e| e.level == tracing::Level::WARN)
            .filter_map(|e| e.field("dvar"))
            .collect();
        assert_eq!(dvars, ["g_banIPs"]);
    }
}
