// SPDX-License-Identifier: GPL-3.0-only
//! `client-error`: every failed start, load or join ends in the stock error popup with its reason, over the main
//! menu, as `Com_Error(ERR_DROP)` does it. Each run is a real client driven through the control channel (#378) as a
//! player plays it (#376): lines typed at the console opened with its key, the team and class menus clicked with the
//! pointer, the popup dismissed with Escape. Run 1 is the front end: `map` and `devmap` of a map the install lacks
//! (refused before any loading screen, as `SV_Map_f` looks for the zone first), then a `connect` to an address
//! nothing answers. Run 2 connects to a server that answers `getinfo` but never the connect: the connection attempt
//! times out. Run 3 starts a match with `map` typed at the console, spawns through the team and class menus, and
//! plays until the server kicks the player (the kick is the scenario's, typed at the listen server's console). Each
//! must open `error_popmenu` with the reason in `com_errorMessage`, with no match left behind. Needs a display and
//! the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use net::oob::Oob;
use serde_json::json;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const NAME: &str = "client-error";
/// Every run's own flags beside the harness's: the bots the kicked run's match plays with.
const ARGS: &[&str] = &["--bots", "3"];

/// A map no install has: `SV_Map_f` says `Can't find map` and starts nothing.
const BAD_MAP: &str = "nosuchmap";
/// An address nothing listens on: the standard ports are 28960 to 28963.
const NOBODY: &str = "127.0.0.1:28999";
/// The stock error popup, and the menus a listen server's match opens for the player to answer.
const ERROR_MENU: &str = "error_popmenu";
const TEAM_MENU: &str = "team_marinesopfor";
const CLASS_MENU: &str = "changeclass";
/// What the server's kick says (the player is `--name`'s default, `player`).
const KICKED: &str = "Player kicked";

/// What `SV_Map_f` says of a map the install lacks.
fn cant_find(map: &str) -> String {
    format!("Can't find map \"{map}\".")
}

/// Whether `have` says what `want` does, word for word (the variable's own spacing aside); an empty `want` is an
/// empty message.
fn same_words(have: &str, want: &str) -> bool {
    have.split_whitespace().eq(want.split_whitespace())
}

/// `com_errorMessage` says `want`, as the client holds it.
fn message_is(c: &mut Driven, want: &str) -> Result<(), String> {
    let r = c.ask(json!({"cmd": "cvar", "name": "com_errorMessage"}))?;
    let have = r["value"].as_str().unwrap_or_default();
    if same_words(have, want) {
        Ok(())
    } else {
        Err(format!("com_errorMessage is {have:?}, want {want:?}"))
    }
}

/// Whether the error popup is among the open menus.
fn popup_open(c: &mut Driven) -> Result<bool, String> {
    let open = c.lookup("menu.open")?;
    Ok(open
        .as_array()
        .is_some_and(|o| o.iter().any(|m| m == ERROR_MENU)))
}

/// The front end with nothing left of a match: the main menu open, no connection and no world.
fn at_home(c: &mut Driven, limit: Duration) -> Result<(), String> {
    c.until(
        json!({"menu.open": {"contains": "main"}, "net": null, "client.in_world": false}),
        limit,
        "the client is not back at the main menu with no match left",
    )?;
    Ok(())
}

/// The error popup opens with the reason `want`, over the main menu with no match left.
fn popup_says(c: &mut Driven, want: &str, limit: Duration) -> Result<(), String> {
    c.until(
        json!({"menu.open": {"contains": ERROR_MENU}}),
        limit,
        "the error popup did not open",
    )?;
    message_is(c, want)?;
    at_home(c, Duration::from_secs(5))
}

/// Escape, as a player closes the popup: it goes and its message with it, so each check is its own.
fn dismiss(c: &mut Driven) -> Result<(), String> {
    c.key("escape", "tap")?;
    c.pause(Duration::from_secs(1))?;
    if popup_open(c)? {
        return Err("Escape did not close the error popup".into());
    }
    message_is(c, "")
}

/// Run 1: a bare `map` (nothing happens), a typed `map` and `devmap` of a missing map, and a `connect` nobody answers.
fn front_end(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("main menu", |c| at_home(c, secs(90)))?;
    c.step("bare map", |c| {
        // `SV_Map_f` checks the zone only `if (*map)`: no popup, no message, no match.
        c.console("map")?;
        c.pause(secs(2))?;
        message_is(c, "")?;
        if popup_open(c)? {
            return Err("a bare map opened the error popup".into());
        }
        at_home(c, secs(5))
    })?;
    let missing = cant_find(BAD_MAP);
    c.step("map of a missing map", |c| {
        c.console(&format!("map {BAD_MAP}"))?;
        popup_says(c, &missing, secs(30))?;
        c.capture("badmap").map(drop)
    })?;
    c.step("devmap of a missing map", |c| {
        dismiss(c)?;
        c.console(&format!("devmap {BAD_MAP}"))?;
        popup_says(c, &missing, secs(30))
    })?;
    c.step("connect to nobody", |c| {
        dismiss(c)?;
        c.console(&format!("connect {NOBODY}"))?;
        popup_says(c, &format!("{NOBODY} did not answer"), secs(30))?;
        c.capture("refused").map(drop)
    })
}

/// Run 2: a `connect` to `addr`, which answers the query but never the connect; the join loads the map, then the
/// connect is tried for 20 s before it is given up.
fn timed_out(c: &mut Driven, addr: &str) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("main menu", |c| at_home(c, secs(90)))?;
    c.step("connect never answered", |c| {
        c.console(&format!("connect {addr}"))?;
        popup_says(c, net::client::TIMED_OUT, secs(120))?;
        c.capture("timeout").map(drop)
    })
}

/// Run 3: a match started with `map` at the console (the server's `g_gametype` default, war, with no gametype played
/// yet), spawned through the stock team and class menus; the listen server then kicks the player by name.
fn kicked(c: &mut Driven) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("main menu", |c| at_home(c, secs(90)))?;
    c.step("map typed at the console", |c| {
        c.console("map mp_crash")?;
        c.until(
            json!({"menu.open": {"contains": TEAM_MENU}}),
            secs(120),
            "the team menu of the match did not open",
        )?;
        Ok(())
    })?;
    c.step("team and class menus", |c| {
        c.click("auto_assign")?;
        c.until(
            json!({"menu.open": {"contains": CLASS_MENU}}),
            secs(30),
            "the class menu did not open after the team was picked",
        )?;
        c.pause(secs(1))?;
        c.click("Assault")?;
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            secs(120),
            "the player did not spawn",
        )?;
        c.pause(secs(2))
    })?;
    c.step("kicked by the server", |c| {
        c.ask(json!({"cmd": "server", "op": "exec", "line": "kick player"}))?;
        popup_says(c, KICKED, secs(30))?;
        c.capture("kicked").map(drop)
    })
}

/// One run: a fresh client at the main menu, `play` driven through its channel and its steps written to
/// `control-run.json`. The error names the step that failed, or how the client ended.
fn drive(
    client: &Path,
    install: &Path,
    dir: &Path,
    play: impl FnOnce(&mut Driven) -> Result<(), String>,
) -> Result<(), String> {
    let mut c = Driven::start(client, install, dir, ARGS)?;
    let played = play(&mut c);
    let written = c.report("control-run.json", &["menu", "console"]);
    let status = c.finish(Duration::from_secs(30));
    played?;
    written.map_err(|e| format!("the run's report could not be written: {e}"))?;
    match status {
        Some(s) if s.success() => Ok(()),
        Some(s) => Err(format!("the client exited {s}")),
        None => Err("the client did not quit and was killed".into()),
    }
}

/// A server that answers `getinfo` (so the join menu and `connect` take it for a server playing mp_crash) and nothing
/// else: the connect is never answered, as a server that died between the query and the join.
struct MuteServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MuteServer {
    fn start() -> io::Result<Self> {
        let sock = UdpSocket::bind("127.0.0.1:0")?;
        sock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let addr = sock.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while !flag.load(Ordering::Relaxed) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    continue;
                };
                if let Some(Oob::GetInfo(challenge)) = Oob::parse(&buf[..n]) {
                    let info = Oob::InfoResponse(vec![
                        ("challenge".into(), challenge.to_string()),
                        ("protocol".into(), net::oob::PROTOCOL.to_string()),
                        ("hostname".into(), "mute".into()),
                        ("mapname".into(), "mp_crash".into()),
                        ("gametype".into(), "war".into()),
                        ("sv_maxclients".into(), "8".into()),
                    ]);
                    let _ = sock.send_to(&info.encode(), from);
                }
            }
        });
        Ok(MuteServer {
            addr,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for MuteServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    /// One run's play, driven through the control channel.
    type Play<'a> = &'a dyn Fn(&mut Driven) -> Result<(), String>;
    let mute = match MuteServer::start() {
        Ok(m) => m,
        Err(e) => {
            return Ok(StageReport::new(NAME, Status::Failed)
                .with_reason(format!("the stage's mute server did not start: {e}")));
        }
    };
    let mute_addr = mute.addr.to_string();
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut problems = Vec::new();
    let runs: [(&str, Play<'_>, &[&str]); 3] = [
        ("frontend", &front_end, &["badmap", "refused"]),
        (
            "timeout",
            &|c: &mut Driven| timed_out(c, &mute_addr),
            &["timeout"],
        ),
        ("kicked", &kicked, &["kicked"]),
    ];
    for (label, play, shots) in runs {
        let dir = ctx.dir.join(label);
        let result = drive(&client, install, &dir, play);
        for f in std::iter::once("control-run.json".to_owned())
            .chain(shots.iter().map(|s| format!("{s}.png")))
        {
            if dir.join(&f).is_file() {
                out.files.push(format!("{label}/{f}"));
            }
        }
        if let Err(e) = result {
            problems.push(format!("{label}: {e}"));
        }
    }
    drop(mute);
    out.notes.push(format!(
        "map and devmap of {BAD_MAP} and a connect to {NOBODY} typed at the console, a connect that is never answered, and a kick from a match started with map at the console and joined through the team and class menus each opened {ERROR_MENU} with the reason over the main menu, with no match left"
    ));
    if !problems.is_empty() {
        out.status = Status::Failed;
        out.reason = Some(problems.join("; "));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_is_compared_word_for_word() {
        assert!(same_words("Player kicked", KICKED));
        assert!(same_words(" Player  kicked\n", KICKED));
        assert!(!same_words("Player", KICKED));
        assert!(!same_words("Player kicked again", KICKED));
        // A dismissed popup leaves no message; any word left is one too many.
        assert!(same_words("", ""));
        assert!(same_words("  ", ""));
        assert!(!same_words(&cant_find(BAD_MAP), ""));
    }

    #[test]
    fn a_missing_map_is_named_as_sv_map_f_names_it() {
        assert_eq!(cant_find(BAD_MAP), "Can't find map \"nosuchmap\".");
    }
}
