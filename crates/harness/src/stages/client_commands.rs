// SPDX-License-Identifier: GPL-3.0-only
//! `client-commands`: the commands a bind, a menu button or a console line runs in a match, each driven as the
//! player does it and checked by its effect (issue #364). The Pause key's stock `toggle cl_paused` is refused as
//! read only (multiplayer has no pause); looking down then a bound `centerview` levels the view; `record` says
//! where it writes and `stoprecord` leaves a demo file; Escape > Call Vote > Restart Map calls a vote the server
//! announces; the B key opens the quick message menu and two keys say a team line the server relays;
//! `setFromLocString` localizes into a cvar; `cmd` forwards its words to the server. Needs a display and the
//! install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-commands";
const DEMO: &str = "stagedemo";

const STEPS: &str = "set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,wait=1,\
mouse=Marines,menu=changeclass:30,wait=1,mouse=Assault,ingame=120,wait=3,team=2:5,\
holdkey=pause,releasekey=pause,saw=cl_paused is read only.:5,cvaris=cl_paused 0,\
set=bind f9 centerview,holdkey=downarrow,wait=1,releasekey=downarrow,wait=0.5,pitchis=-90 -20,shot=lookdown,\
holdkey=f9,releasekey=f9,wait=0.5,pitchis=-3 3,shot=centered,\
console=record stagedemo,saw=recording to:5,wait=2,console=stoprecord,saw=Stopped demo.:5,console=stoprecord,saw=Not recording a demo.:5,\
console=cmd say line_through_cmd,saw=line_through_cmd:6,\
console=setFromLocString customclass3 @CLASS_CUSTOM3,wait=0.5,cvarloc=customclass3 CLASS_CUSTOM3,\
togglemenu,wait=1,see=Call Vote,mouse=Call Vote,menu=callvote:5,wait=1,see=Restart Map+Next Map,shot=callvote,\
mouse=Restart Map,saw=called a vote:6,nomenu=5,wait=1,\
holdkey=b,releasekey=b,menu=quickmessage:5,wait=0.5,shot=quickmessage,uikey=1,menu=quickcommands:5,wait=0.5,shot=quickcommands,uikey=1,nomenu=5,\
saw=(Team):8";

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
    let config = ctx.dir.join("config");
    let mut out = StageReport::new(NAME, Status::Passed);
    match run_client(
        &client,
        install,
        &ctx.dir,
        &config,
        &["--bots", "9"],
        STEPS,
        Duration::from_secs(300),
    ) {
        Ok(report) => {
            out.files.push("ui-script.json".into());
            for f in [
                "lookdown",
                "centered",
                "callvote",
                "quickmessage",
                "quickcommands",
            ] {
                out.files.push(format!("{f}.png"));
            }
            let demo = config.join("demos").join(format!("{DEMO}.c4edemo"));
            let demo_len = std::fs::metadata(&demo).map(|m| m.len()).unwrap_or(0);
            match verdict(&report) {
                Some(w) => {
                    out.status = Status::Failed;
                    out.reason = Some(w);
                }
                None if demo_len == 0 => {
                    out.status = Status::Failed;
                    out.reason = Some(format!(
                        "record left no demo at {} (the console said it was recording)",
                        demo.display()
                    ));
                }
                None => out.notes.push(format!(
                    "Pause refused, centerview levelled the view, record wrote {demo_len} bytes, the vote and the quick message reached the server, setFromLocString and cmd ran"
                )),
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    Ok(out)
}
