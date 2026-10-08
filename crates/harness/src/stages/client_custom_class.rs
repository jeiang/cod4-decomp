// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-custom-class`: a person with a saved profile plays from a custom class. The profile's stats reach the
//! server before the person begins, so the class loadout holds the weapon they set (a P90 in custom class 5), the
//! perks and rank in the profile survive the join (the server's stand-ins are never taken for news), a class change in
//! the first seconds of a round applies at once, and every other player the server announces is drawn. Needs a display
//! and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-custom-class";

/// The class table's primary-weapon stat of the P90 (`stat + 1` of a custom class slot), and the stats the checks read
/// back: the first perk of custom class 5 (`stat + 5`; the join once replaced it with a stand-in) and the experience.
const STEPS: &str = "set=set customclass5 P90,stat=241 14,stat=242 0,stat=245 176,stat=2301 5000,\
click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,click=auto_assign,\
menu=changeclass:30,wait=1,click=P90,ingame=120,weapon=p90:20,wait=6,shot=custom,\
statis=245 176,statis=2301 5000,\
togglemenu,wait=1,mouse=Choose Class,menu=changeclass:5,mouse=Assault,nomenu=5,weapon=m16:20,shot=changed";

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
    let mut out = StageReport::new(NAME, Status::Passed);
    match run_client(
        &client,
        install,
        &ctx.dir,
        &ctx.dir.join("config"),
        &["--bots", "6"],
        STEPS,
        Duration::from_secs(300),
    ) {
        Ok(report) => {
            out.files.push("ui-script.json".into());
            for f in ["custom", "changed"] {
                out.files.push(format!("{f}.png"));
            }
            match verdict(&report) {
                Some(w) => {
                    out.status = Status::Failed;
                    out.reason = Some(w);
                }
                None => out.notes.push(
                    "spawned with the custom class's P90, kept the profile's perk and experience, changed class in the grace period and saw the other players"
                        .into(),
                ),
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    Ok(out)
}
