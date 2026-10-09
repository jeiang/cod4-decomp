// SPDX-License-Identifier: GPL-3.0-only
//! `client-frontend`: the front end as a player meets it, with the install's own player profile. The main menu lists
//! every stock row, Select Profile opens the profile list, the profile's unlocks make Create a Class open its
//! menu, the join menu cycles its server source and game mode (a favorite shows until a game mode is chosen, and the
//! internet list is empty), and Start New Server's game mode chooser steps through the modes in the original's order. The install's
//! `players/` folder must be exactly as it was after the run. Needs a display and an install that has a
//! `players/profiles/` folder with an unlocked profile; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const NAME: &str = "client-frontend";

const STEPS: &str = "wait=2,see=Join Game+Start New Server+Select Profile+Rank & Challenges+Controls+Options+Mods+Single Player+Quit,shot=main,\
mouse=Select Profile,menu=player_profile:5,wait=1,shot=profile,key=escape,wait=1,\
mouse=Create a Class,menu=pc_cac_popup:5,wait=1,shot=cac,key=escape,wait=1,\
mouse=Join Game,menu=pc_join_unranked:5,wait=1,shot=join,\
set=set ui_netSource 2,favorite=127.0.0.1:28999,rows=2 1,\
mouse=#220,cvaris=ui_netSource 0,mouse=#253,cvaris=ui_joinGameType 1,set=set ui_joinGameType 0,\
mouse=#220,cvaris=ui_netSource 1,rows=2 0,mouse=#220,cvaris=ui_netSource 2,rows=2 1,\
set=set ui_joinGameType 1,rows=2 0,set=set ui_joinGameType 0,rows=2 1,\
key=escape,wait=1,\
set=set ui_netGametypeName war,mouse=Start New Server,menu=createserver:10,wait=1,shot=mode_war,\
mouse=#245,cvaris=ui_netGametypeName koth,mouse=#245,cvaris=ui_netGametypeName dm,shot=mode_dm,\
mouse=#245,cvaris=ui_netGametypeName dom,mouse=#245,cvaris=ui_netGametypeName sd,\
mouse=#245,cvaris=ui_netGametypeName sab,mouse=#245,cvaris=ui_netGametypeName war,shot=mode_back";

/// What the join menu's LAN list must show once it has scanned: a server of this stage's own, on a standard port.
const LAN_STEP: &str = "rows=2 +1:15,";

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

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let players = install.join("players");
    if !players.join("profiles").is_dir() {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("the install has no players/profiles folder"));
    }
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let before = snapshot(&players)?;
    let mut out = StageReport::new(NAME, Status::Passed);
    let lan = LanServer::start(install);
    let steps = STEPS.replacen(
        "cvaris=ui_netSource 0,",
        &format!(
            "cvaris=ui_netSource 0,{}",
            if lan.is_some() { LAN_STEP } else { "" }
        ),
        1,
    );
    match run_client(
        &client,
        install,
        &ctx.dir,
        &ctx.dir.join("config"),
        &[],
        &steps,
        Duration::from_secs(120),
    ) {
        Ok(report) => {
            out.files.push("ui-script.json".into());
            for f in ["main", "profile", "cac", "mode_war", "mode_dm", "mode_back"] {
                out.files.push(format!("{f}.png"));
            }
            // The run reads the steps' own verdicts; the player never spawns here.
            let failed: Vec<String> = report["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| s["ok"] != serde_json::Value::Bool(true))
                .map(|s| format!("{} ({})", s["step"], s["note"]))
                .collect();
            if !failed.is_empty() {
                out.status = Status::Failed;
                out.reason = Some(format!("steps failed: {}", failed.join(", ")));
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    drop(lan);
    if snapshot(&players)? != before {
        out.status = Status::Failed;
        out.reason = Some("the run changed the install's players folder".into());
    }
    out.notes.push(
        "the main menu, Select Profile, Create a Class with the install's profile, and the game mode order; the install's players folder is untouched"
            .into(),
    );
    Ok(out)
}
