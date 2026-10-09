// SPDX-License-Identifier: GPL-3.0-only
//! `client-ingame-menu`: a player changes class and team in the middle of a round through the stock menus, with the
//! mouse, and gets back to the game each time. Escape closes the in-game menu; picking a class closes the class menu;
//! picking the other team closes the team menu, opens the server's class menu, and after that class pick the player
//! is on the new team and respawns with the new class's weapon. Every step must pass. Needs a display and the install;
//! skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-ingame-menu";

/// (The waits let the server's answer to a pick, which closes the menus again, arrive before the next menu opens.)
/// Join with the Assault class, then: Escape closes the in-game menu, a class pick closes the class menu,
/// and a team change (which kills the player) ends on the other team with a Spec Ops weapon after the respawn.
const STEPS: &str = "set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,wait=1,\
see=OpFor+Marines+Auto-Assign+Controls+Options+Leave Game,nosee=Spectator,shot=team_pre,\
mouse=OpFor,menu=changeclass:30,wait=1,see=Assault+Spec Ops+Heavy Gunner+Demolitions+Sniper,shot=class_pre,\
mouse=Assault,ingame=120,wait=3,team=1:5,weapon=_gl:5,\
togglemenu,wait=1,see=Choose Class+Change Team+Controls+Options+Call Vote+Mute Players+Leave Game,shot=escape_menu,key=escape,nomenu=5,\
togglemenu,wait=1,mouse=Choose Class,menu=changeclass:5,wait=1,shot=class_in,key=escape,nomenu=5,\
togglemenu,wait=1,mouse=Choose Class,menu=changeclass:5,mouse=Spec Ops,nomenu=5,wait=2,\
togglemenu,wait=1,mouse=Change Team,menu=team_marinesopfor:5,wait=1,see=Marines+Auto-Assign+Spectator,nosee=Controls+Options+Leave Game,shot=change_team,\
mouse=Marines,menu=changeclass:10,\
mouse=Spec Ops,nomenu=5,team=2:10,weapon=mp5:90,wait=2,nomenu=5,shot=changed";

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
        &["--bots", "9"],
        STEPS,
        Duration::from_secs(300),
    ) {
        Ok(report) => {
            out.files.push("ui-script.json".into());
            for f in [
                "team_pre",
                "class_pre",
                "escape_menu",
                "class_in",
                "change_team",
                "changed",
            ] {
                out.files.push(format!("{f}.png"));
            }
            match verdict(&report) {
                Some(w) => {
                    out.status = Status::Failed;
                    out.reason = Some(w);
                }
                None => out.notes.push(
                    "Escape and the class and team picks each returned to the game; the team changed and the new class spawned"
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
