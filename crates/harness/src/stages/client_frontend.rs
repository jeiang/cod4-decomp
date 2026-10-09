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
    match run_client(
        &client,
        install,
        &ctx.dir,
        &ctx.dir.join("config"),
        &[],
        STEPS,
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
