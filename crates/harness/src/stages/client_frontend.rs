// SPDX-License-Identifier: GPL-3.0-only
//! `client-frontend`: the front end as a player meets it, with the harness's own fully unlocked profile, driven
//! through the control channel with the player's own pointer, keys and console (#376). The main menu lists every
//! stock row, Select Profile opens the profile list, the profile's unlocks make Create a Class open its menu (and a
//! profile without them shows it locked, a stat no menu writes, so it is typed with `statset`), the options menus show
//! every setting, the join menu cycles its server source and game mode (a favorite added with New Favorite shows until
//! a game mode is chosen, and the internet list is empty), and Start New Server's game mode chooser steps through the
//! modes in the original's order. The install's `players/` folder must be exactly as it was after the run. Needs a
//! display and the install; skips without.
use super::client_custom_class::{HOME, localized, until_top};
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{check_menu, steps_problem};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const NAME: &str = "client-frontend";

/// The main menu's rows.
const MAIN: [&str; 9] = [
    "Join Game",
    "Start New Server",
    "Select Profile",
    "Rank & Challenges",
    "Controls",
    "Options",
    "Mods",
    "Single Player",
    "Quit",
];

/// The build number the menus draw.
const BUILD: &str = "1.7.568";

/// The options menu's rows, each a live button.
const OPTIONS: [&str; 7] = [
    "Graphics...",
    "Texture Settings...",
    "Sound...",
    "Voice Chat...",
    "Game Options...",
    "Multiplayer Options...",
    "Optimal System Settings",
];

/// The option submenus a row opens, each with every setting showing a value.
const SUBMENUS: [(&str, &str); 4] = [
    ("Graphics...", "options_graphics"),
    ("Texture Settings...", "options_graphics_texture"),
    ("Sound...", "options_sound"),
    ("Voice Chat...", "options_voice"),
];

/// The owner-draw items the join menu and Start New Server draw themselves: the server source
/// (`UI_NETSOURCE`), the join menu's game mode filter (`UI_JOINGAMETYPE`) and the mode chooser (`UI_NETGAMETYPE`).
const NET_SOURCE: i64 = 220;
const JOIN_GAME_TYPE: i64 = 253;
const NET_GAME_TYPE: i64 = 245;

/// Start New Server's modes in the order its chooser steps through them, from `war`, with the screenshot taken at each.
const MODES: [(&str, Option<&str>); 6] = [
    ("koth", None),
    ("dm", Some("mode_dm")),
    ("dom", None),
    ("sd", None),
    ("sab", None),
    ("war", Some("mode_back")),
];

/// A favorite on no server's port: it shows in the favorites list without an answer.
const FAVORITE: &str = "127.0.0.1:28999";

/// A game server running in this process on one of the standard ports the client's LAN scan asks, until dropped.
struct LanServer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LanServer {
    /// Starts one and waits until it has a map loaded; `None` when no standard port is free or it does not come up.
    fn start(install: &Path) -> Option<Self> {
        let port = (28960..=28963).find(|p| std::net::UdpSocket::bind(("0.0.0.0", *p)).is_ok())?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let (flag, install) = (stop.clone(), install.to_path_buf());
        let thread = std::thread::spawn(move || {
            let args: Vec<String> = ["+set", "net_port", &port.to_string()]
                .map(Into::into)
                .to_vec();
            let Ok(mut server) = server::server::Server::boot(&install, &args, false) else {
                return;
            };
            for line in ["set g_gametype war", "map mp_crash"] {
                if server.exec_line(line).is_err() {
                    return;
                }
            }
            if server.net_addr().is_some_and(|a| a.port() == port) {
                let _ = tx.send(());
            }
            while !flag.load(Ordering::Relaxed) {
                server.run_for(Duration::from_millis(16));
            }
        });
        let up = rx.recv_timeout(Duration::from_secs(180)).is_ok();
        let this = LanServer {
            stop,
            thread: Some(thread),
        };
        up.then_some(this)
    }
}

impl Drop for LanServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Every file under `dir` with its bytes, in path order.
fn snapshot(dir: &Path) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in fs::read_dir(&d)? {
            let p = e?.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push((p.clone(), fs::read(&p)?));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Waits up to two seconds for dvar `name` to read `want`: the same text, or the same number.
pub(super) fn cvar_is(c: &mut Driven, name: &str, want: &str) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let v = c.ask(json!({"cmd": "cvar", "name": name}))?;
        let have = v["value"].as_str().unwrap_or("");
        let same = have == want
            || matches!(
                (have.trim().parse::<f64>(), want.parse::<f64>()),
                (Ok(a), Ok(b)) if (a - b).abs() < 1e-3
            );
        if same {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("{name} is {have:?}, not {want:?}"));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

/// The top menu's name and its items.
fn top_items(c: &mut Driven) -> Result<(String, Vec<Value>), String> {
    let menu = c.state("menu")?;
    let top = menu["open"]
        .as_array()
        .and_then(|o| o.last())
        .and_then(Value::as_str)
        .ok_or_else(|| format!("no menu is open: {}", menu["open"]))?
        .to_owned();
    let items = menu["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|it| it["menu"] == top.as_str())
        .cloned()
        .collect();
    Ok((top, items))
}

fn text_is(it: &Value, text: &str) -> bool {
    it["text"]
        .as_str()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case(text))
}

/// The button the player can press for each of `texts` in the top menu: shown and not only decoration.
fn live(c: &mut Driven, texts: &[&str]) -> Result<(), String> {
    let (top, items) = top_items(c)?;
    let dead: Vec<&&str> = texts
        .iter()
        .filter(|t| {
            !items
                .iter()
                .any(|it| text_is(it, t) && it["visible"] == true && it["decoration"] == false)
        })
        .collect();
    if dead.is_empty() {
        Ok(())
    } else {
        Err(format!("{top} has no live button for {dead:?}"))
    }
}

/// `text` shown in the top menu as only decoration, the locked stand-in, with no live button of that text; a click
/// on it opens no menu.
fn locked(c: &mut Driven, text: &str, opens: &str) -> Result<(), String> {
    let (top, items) = top_items(c)?;
    if items
        .iter()
        .any(|it| text_is(it, text) && it["visible"] == true && it["decoration"] == false)
    {
        return Err(format!("{top} shows {text} live, not locked"));
    }
    let shown = items
        .iter()
        .find(|it| text_is(it, text) && it["visible"] == true && it["decoration"] == true)
        .ok_or_else(|| format!("{top} does not show {text} locked"))?
        .clone();
    click_rect(c, &shown["rect"])?;
    c.pause(Duration::from_secs(1))?;
    let (now, _) = top_items(c)?;
    if now.eq_ignore_ascii_case(opens) {
        return Err(format!("clicking the locked {text} opened {opens}"));
    }
    Ok(())
}

/// Every choice `menu` shows (yes/no, multiple choice, dvar enum: `choice` in the menu state) shows the setting it
/// holds: none whose value is missing or reads blank.
fn values(c: &mut Driven, menu: &str) -> Result<(), String> {
    let (top, items) = top_items(c)?;
    if !top.eq_ignore_ascii_case(menu) {
        return Err(format!("{menu} is not the top menu ({top} is)"));
    }
    let choices: Vec<&Value> = items
        .iter()
        .filter(|it| it["visible"] == true && it["choice"] == true)
        .collect();
    let blank: Vec<String> = choices
        .iter()
        .filter(|it| it["value"].as_str().is_none_or(|v| v.trim().is_empty()))
        .map(|it| format!("{} {}", it["name"], it["text"]))
        .collect();
    if blank.is_empty() {
        Ok(())
    } else {
        Err(format!("{menu} shows choices without a setting: {blank:?}"))
    }
}

/// The pointer moved to the middle of `rect` (`[x, y, w, h]` in window pixels) and the left button pressed there, as
/// [`Driven::click`] does for a named item.
pub(super) fn click_rect(c: &mut Driven, rect: &Value) -> Result<(), String> {
    let r: Vec<f64> = rect
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let [x, y, w, h] = r[..] else {
        return Err(format!("no rectangle to click: {rect}"));
    };
    c.ask(json!({"cmd": "mouse", "x": x + w / 2.0, "y": y + h / 2.0}))?;
    c.frames(1)?;
    c.button("mouse1", "down")?;
    c.frames(1)?;
    c.button("mouse1", "up")
}

/// Clicks the top menu's shown item the engine draws as owner-draw `id`.
fn click_owner(c: &mut Driven, id: i64) -> Result<(), String> {
    let (top, items) = top_items(c)?;
    let it = items
        .iter()
        .find(|it| it["visible"] == true && it["owner_draw"] == id)
        .ok_or_else(|| format!("{top} shows no owner-draw {id} item"))?
        .clone();
    click_rect(c, &it["rect"])
}

/// The join menu's server list shows `want` rows (`at_least`: that many or more) within `secs` seconds.
fn rows(c: &mut Driven, want: u64, at_least: bool, secs: u64) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let have = c.lookup("menu.server_rows")?;
        if have
            .as_u64()
            .is_some_and(|n| n == want || (at_least && n > want))
        {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!(
                "the server list shows {have} rows, not {}{want}",
                if at_least { "at least " } else { "" }
            ));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

fn open_by(c: &mut Driven, item: &str, menu: &str, secs: u64) -> Result<(), String> {
    c.click(item)?;
    until_top(c, menu, Duration::from_secs(secs)).map_err(|e| format!("clicking {item}: {e}"))?;
    c.pause(Duration::from_secs(1))
}

fn back(c: &mut Driven) -> Result<(), String> {
    c.key("escape", "tap")?;
    c.pause(Duration::from_secs(1))
}

/// Escape until the main menu is on top again, as a player backs out of nested menus.
fn to_main(c: &mut Driven) -> Result<(), String> {
    for _ in 0..3 {
        let (top, _) = top_items(c)?;
        if top.eq_ignore_ascii_case(HOME) {
            return Ok(());
        }
        back(c)?;
    }
    until_top(c, HOME, Duration::from_secs(1))
}

fn build(c: &mut Driven) -> Result<(), String> {
    let have = c.lookup("menu.build")?;
    if have == BUILD {
        Ok(())
    } else {
        Err(format!("the menus drew build {have}, not {BUILD}"))
    }
}

/// New Favorite with the address typed into its field, as a player adds one.
fn add_favorite(c: &mut Driven) -> Result<(), String> {
    open_by(c, "createFavorite", "createfavorite_popmenu", 5)?;
    c.click("fav_ip")?;
    c.ask(json!({"cmd": "text", "text": FAVORITE}))?;
    c.key("enter", "tap")?;
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let (top, _) = top_items(c)?;
        if top.eq_ignore_ascii_case("pc_join_unranked") {
            return Ok(());
        }
        // The message popup saying the favorite was added goes as any popup does.
        if top.eq_ignore_ascii_case("fav_message_popmenu") {
            c.key("escape", "tap")?;
        }
        if Instant::now() > end {
            return Err(format!(
                "the join menu did not come back after adding the favorite ({top} is on top)"
            ));
        }
        c.pause(Duration::from_millis(200))?;
    }
}

fn drive(c: &mut Driven, enable_console: &str) -> Result<(), String> {
    c.step("main menu", |c| {
        until_top(c, HOME, Duration::from_secs(60))?;
        c.pause(Duration::from_secs(2))?;
        check_menu(c, &MAIN, &[])?;
        c.capture("main").map(|_| ())
    })?;
    c.step("Select Profile", |c| {
        open_by(c, "Select Profile", "player_profile", 5)?;
        c.capture("profile")?;
        back(c)
    })?;
    c.step("Create a Class with the unlocked profile", |c| {
        open_by(c, "Create a Class", "pc_cac_popup", 5)?;
        c.capture("cac")?;
        back(c)
    })?;
    c.step("Create a Class locked without the unlock", |c| {
        build(c)?;
        live(c, &["Create a Class"])?;
        // The unlock is a profile stat no menu writes: the stage types it, as the scenario's setup.
        c.console("statset 260 0")?;
        c.pause(Duration::from_secs(1))?;
        locked(c, "Create a Class", "pc_cac_popup")?;
        c.capture("cac_locked")?;
        c.console("statset 260 1")?;
        c.pause(Duration::from_secs(1))?;
        live(c, &["Create a Class"])
    })?;
    c.step("Options", |c| {
        open_by(c, "Options", "main_options", 5)?;
        build(c)?;
        c.capture("options")?;
        live(c, &OPTIONS)?;
        check_menu(c, &[], &["Cheats..."])
    })?;
    for (row, menu) in SUBMENUS {
        c.step(&format!("{row} shows every setting"), |c| {
            until_top(c, "main_options", Duration::from_secs(5))
                .or_else(|_| open_by(c, "Options", "main_options", 5))?;
            open_by(c, row, menu, 5)?;
            values(c, menu)?;
            back(c)
        })?;
    }
    c.step("Game Options: Enable Console", |c| {
        until_top(c, "main_options", Duration::from_secs(5))
            .or_else(|_| open_by(c, "Options", "main_options", 5))?;
        open_by(c, "Game Options...", "options_game", 5)?;
        values(c, "options_game")?;
        c.click(enable_console)?;
        c.pause(Duration::from_secs(1))?;
        cvar_is(c, "monkeytoy", "0")?;
        back(c)
    })?;
    c.step("Multiplayer Options", |c| {
        until_top(c, "main_options", Duration::from_secs(5))
            .or_else(|_| open_by(c, "Options", "main_options", 5))?;
        open_by(c, "Multiplayer Options...", "options_multi", 5)?;
        values(c, "options_multi")?;
        c.capture("options_multi")?;
        back(c)
    })?;
    c.step("Join Game: favorites, LAN and internet", |c| {
        to_main(c)?;
        open_by(c, "Join Game", "pc_join_unranked", 5)?;
        c.capture("join")?;
        c.console("set ui_netSource 2")?;
        cvar_is(c, "ui_netSource", "2")?;
        add_favorite(c)?;
        rows(c, 1, false, 5)?;
        click_owner(c, NET_SOURCE)?;
        cvar_is(c, "ui_netSource", "0")?;
        // The stage's own server is not pure and says `mod 1`; the stock filter lists only pure, unmodified
        // servers, so a player would untick the one and set the other to either to see it, and so does the stage.
        c.console("set ui_browserShowPure 0")?;
        c.console("set ui_browserMod -1")?;
        rows(c, 1, true, 15)?;
        click_owner(c, JOIN_GAME_TYPE)?;
        cvar_is(c, "ui_joinGameType", "1")?;
        c.console("set ui_joinGameType 0")?;
        click_owner(c, NET_SOURCE)?;
        cvar_is(c, "ui_netSource", "1")?;
        rows(c, 0, false, 5)?;
        click_owner(c, NET_SOURCE)?;
        cvar_is(c, "ui_netSource", "2")?;
        rows(c, 1, false, 5)?;
        // A chosen game mode hides the favorite: no answer says what it plays.
        c.console("set ui_joinGameType 1")?;
        rows(c, 0, false, 5)?;
        c.console("set ui_joinGameType 0")?;
        rows(c, 1, false, 5)?;
        back(c)
    })?;
    c.step("Start New Server: the game mode order", |c| {
        to_main(c)?;
        c.console("set ui_netGametypeName war")?;
        cvar_is(c, "ui_netGametypeName", "war")?;
        open_by(c, "Start New Server", "createserver", 10)?;
        c.capture("mode_war")?;
        for (mode, shot) in MODES {
            click_owner(c, NET_GAME_TYPE)?;
            cvar_is(c, "ui_netGametypeName", mode)?;
            if let Some(shot) = shot {
                c.capture(shot)?;
            }
        }
        Ok(())
    })
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let players = install.join("players");
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let before = match snapshot(&players) {
        Ok(rows) => rows,
        Err(e) => {
            // Without the before-picture the untouched-install claim cannot be made, and the LAN scan cannot be
            // asked for: the run would prove less than the stage says it does.
            return Ok(StageReport::new(NAME, Status::Failed)
                .with_reason(format!("cannot list {}: {e}", players.display())));
        }
    };
    let enable_console = match localized(install, &["MENU_ENABLE_CONSOLE"]) {
        Ok(t) => t["MENU_ENABLE_CONSOLE"].clone(),
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let mut out = StageReport::new(NAME, Status::Passed);
    let Some(lan) = LanServer::start(install) else {
        let mut r = StageReport::new(NAME, Status::Failed);
        r.reason = Some(
            "the stage's LAN server did not start, so the join menu's LAN list was never scanned"
                .into(),
        );
        return Ok(r);
    };
    let problem = match Driven::start(&client, install, &ctx.dir, &[]) {
        Ok(mut c) => {
            let played = drive(&mut c, &enable_console);
            let report = c.report("control-run.json", &["menu"]);
            let status = c.finish(Duration::from_secs(30));
            out.files.push("control-run.json".into());
            for f in [
                "main",
                "profile",
                "cac",
                "cac_locked",
                "options",
                "options_multi",
                "join",
                "mode_war",
                "mode_dm",
                "mode_back",
            ] {
                if ctx.dir.join(format!("{f}.png")).is_file() {
                    out.files.push(format!("{f}.png"));
                }
            }
            // The run reads the steps' own verdicts; the player never spawns here.
            match (played, report) {
                (Err(e), _) | (Ok(()), Err(e)) => Some(e),
                (Ok(()), Ok(report)) => steps_problem(&report),
            }
            .or_else(|| {
                (!status.is_some_and(|s| s.success()))
                    .then(|| format!("the client did not exit successfully: {status:?}"))
            })
        }
        Err(e) => Some(e),
    };
    if let Some(w) = problem {
        out.status = Status::Failed;
        out.reason = Some(w);
    }
    drop(lan);
    if snapshot(&players).ok().as_deref() != Some(before.as_slice()) {
        out.status = Status::Failed;
        out.reason = Some("the run changed the install's players folder".into());
    }
    out.notes.push(
        "the main menu, Select Profile, Create a Class unlocked and locked, the options menus, the join menu's sources, favorite and filters, and the game mode order; the install's players folder is untouched"
            .into(),
    );
    Ok(out)
}
