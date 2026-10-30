// SPDX-License-Identifier: GPL-3.0-only
//! `client-error`: every failed start, load or join ends in the stock error popup with its reason, over the main
//! menu, as `Com_Error(ERR_DROP)` does it. Run 1 is the front end: `map` and `devmap` of a map the install lacks
//! (refused before any loading screen, as `SV_Map_f` looks for the zone first), then a join of an address nothing
//! answers. Run 2 joins a server that answers `getinfo` but never the connect: the connection attempt times out.
//! Run 3 starts a match with `map` typed at the console, spawns through the team and class menus, and plays until
//! the server kicks the player. Each must open `error_popmenu` with the reason in `com_errorMessage`, with no match
//! left behind. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, steps_problem};
use crate::stage::{StageCtx, StageReport, Status};
use net::oob::Oob;
use serde_json::Value;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const NAME: &str = "client-error";
const LIMIT: Duration = Duration::from_secs(240);

/// A map no install has: `SV_Map_f` says `Can't find map` and starts nothing.
const BAD_MAP: &str = "nosuchmap";
/// An address nothing listens on: the standard ports are 28960 to 28963.
const NOBODY: &str = "127.0.0.1:28999";

/// The front end: a bare `map` (nothing happens), a typed `map` and `devmap` of a missing map, and a join nobody
/// answers. The popup is dismissed as a player does, with Escape, which empties the message, so each check is its own.
const FRONTEND_STEPS: &str = "wait=2,console=map,wait=2,cvaris=com_errorMessage ,home=5,\
console=map nosuchmap,menu=error_popmenu:30,cvaris=com_errorMessage Can't find map \"nosuchmap\".,home=5,shot=badmap,\
key=escape,wait=1,cvaris=com_errorMessage ,console=devmap nosuchmap,menu=error_popmenu:30,cvaris=com_errorMessage Can't find map \"nosuchmap\".,home=5,\
key=escape,wait=1,cvaris=com_errorMessage ,connect=127.0.0.1:28999,menu=error_popmenu:30,cvaris=com_errorMessage 127.0.0.1:28999 did not answer,home=5,shot=refused";

/// A match started with `map` at the console (the server's `g_gametype` default, war, with no gametype played yet),
/// spawned through the stock team and class menus; the server then kicks the player by name (`--name` defaults to
/// `player`).
const KICKED_STEPS: &str = "wait=2,console=map mp_crash,menu=team_marinesopfor:120,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,\
ingame=120,wait=2,server=kick player,menu=error_popmenu:30,cvaris=com_errorMessage Player kicked,home=5,shot=kicked";

/// The steps whose outcome the stage is about: a run that stopped before them proves nothing.
const REQUIRED: &[&[&str]] = &[
    &[
        "cvaris=com_errorMessage Can't find map \"nosuchmap\".",
        "cvaris=com_errorMessage 127.0.0.1:28999 did not answer",
        "shot=refused",
    ],
    &[
        "cvaris=com_errorMessage Server connection timed out",
        "shot=timeout",
    ],
    &["cvaris=com_errorMessage Player kicked", "shot=kicked"],
];

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

/// Why a run failed, or `None`: every step passed and the run reached the ones that matter.
fn verdict(report: &Value, required: &[&str]) -> Option<String> {
    if let Some(w) = steps_problem(report) {
        return Some(w);
    }
    let steps = report["steps"].as_array().map_or(&[][..], Vec::as_slice);
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|want| !steps.iter().any(|s| s["step"] == *want))
        .collect();
    (!missing.is_empty()).then(|| format!("the run never reached {}", missing.join(", ")))
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
    let mute = match MuteServer::start() {
        Ok(m) => m,
        Err(e) => {
            return Ok(StageReport::new(NAME, Status::Failed)
                .with_reason(format!("the stage's mute server did not start: {e}")));
        }
    };
    // The join loads the map, then the connect is tried for 20 s before it is given up.
    let timeout_steps = format!(
        "wait=2,connect={addr},menu=error_popmenu:120,cvaris=com_errorMessage {timed_out},home=5,shot=timeout",
        addr = mute.addr,
        timed_out = net::client::TIMED_OUT,
    );
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut problems = Vec::new();
    // Every run starts at the main menu with no flags; the steps do the rest.
    let runs: [(&str, &str, &[&str], &[&str]); 3] = [
        (
            "frontend",
            FRONTEND_STEPS,
            REQUIRED[0],
            &["badmap", "refused"],
        ),
        ("timeout", timeout_steps.as_str(), REQUIRED[1], &["timeout"]),
        ("kicked", KICKED_STEPS, REQUIRED[2], &["kicked"]),
    ];
    for (label, steps, required, shots) in runs {
        let dir = ctx.dir.join(label);
        match run_client(
            &client,
            install,
            &dir,
            &dir.join("config"),
            &[],
            steps,
            LIMIT,
        ) {
            Ok(report) => {
                out.files.push(format!("{label}/ui-script.json"));
                for s in shots {
                    out.files.push(format!("{label}/{s}.png"));
                }
                if let Some(w) = verdict(&report, required) {
                    problems.push(format!("{label}: {w}"));
                }
            }
            Err(e) => problems.push(format!("{label}: {e}")),
        }
    }
    drop(mute);
    out.notes.push(format!(
        "map and devmap of {BAD_MAP}, a join of {NOBODY}, a join whose connect is never answered, and a kick from a match started with map at the console each opened error_popmenu with the reason over the main menu, with no match left"
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
    use serde_json::json;

    fn steps(ok: &[(&str, bool)]) -> Value {
        json!({
            "steps": ok.iter().map(|(s, ok)| json!({"step": s, "ok": ok, "note": ""})).collect::<Vec<_>>()
        })
    }

    #[test]
    fn a_popup_that_never_opened_fails() {
        let r = steps(&[
            ("console=map nosuchmap", true),
            ("menu=error_popmenu:30", false),
        ]);
        assert!(verdict(&r, REQUIRED[0]).unwrap().contains("error_popmenu"));
    }

    #[test]
    fn a_run_that_stopped_early_fails() {
        let r = steps(&[("console=map nosuchmap", true)]);
        assert!(verdict(&r, REQUIRED[0]).unwrap().contains("never reached"));
    }

    #[test]
    fn every_check_reached_passes() {
        let r = steps(&[
            ("cvaris=com_errorMessage Player kicked", true),
            ("shot=kicked", true),
        ]);
        assert_eq!(verdict(&r, REQUIRED[2]), None);
    }

    #[test]
    fn the_scripts_name_the_required_steps() {
        for (steps, required) in [(FRONTEND_STEPS, REQUIRED[0]), (KICKED_STEPS, REQUIRED[2])] {
            for want in required {
                assert!(
                    steps.split(',').any(|s| s == *want),
                    "{want} not in {steps}"
                );
            }
        }
        assert!(FRONTEND_STEPS.contains(BAD_MAP) && FRONTEND_STEPS.contains(NOBODY));
    }
}
