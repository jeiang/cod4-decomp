// SPDX-License-Identifier: GPL-3.0-only
//! `client-commands`: the commands a bind, a menu button or a console line runs in a match, each driven as the
//! player does it and checked by its effect (issue #364). The Pause key's stock `toggle cl_paused` is refused as
//! read only (multiplayer has no pause); looking down then a bound `centerview` levels the view; `record` says
//! where it writes and `stoprecord` leaves a demo file; Escape > Call Vote > Restart Map calls a vote the server
//! announces; the B key opens the quick message menu and two keys say a team line the server relays;
//! `setFromLocString` localizes into a cvar; `cmd` forwards its words to the server; a word nothing knows is
//! `Unknown command` at the front end and, in a match, forwarded to the server, whose answer names it.
//! `writeconfig` exports archived settings and binds to the named file, not the active profile; missing or extra
//! arguments only print usage. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-commands";
const DEMO: &str = "stagedemo";

const STEPS: &str = "console=zz_unknown_at_front,saw=Unknown command \"zz_unknown_at_front\":5,\
set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,wait=1,\
mouse=Marines,menu=changeclass:30,wait=1,mouse=Assault,ingame=120,wait=3,team=2:5,\
holdkey=pause,releasekey=pause,saw=cl_paused is read only.:5,cvaris=cl_paused 0,\
set=bind f9 centerview,holdkey=downarrow,wait=1,releasekey=downarrow,wait=0.5,pitchis=-90 -20,shot=lookdown,\
holdkey=f9,releasekey=f9,wait=0.5,pitchis=-3 3,shot=centered,\
console=record stagedemo,saw=recording to:5,wait=2,console=stoprecord,saw=Stopped demo.:5,console=stoprecord,saw=Not recording a demo.:5,\
console=cmd say line_through_cmd,saw=line_through_cmd:6,\
console=zz_unknown_in_match,sawreply=zz_unknown_in_match:6,\
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
    let active = config.join("players/profiles/harness/config_mp.cfg");
    std::fs::create_dir_all(active.parent().unwrap())?;
    std::fs::write(&active, "seta writeconfig_marker baseline\n")?;
    let exports = ["commands-export.cfg", "commands-extension.cfg"];
    for file in exports.into_iter().chain(["commands-invalid.cfg"]) {
        match std::fs::remove_file(config.join("players").join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // Read the active file back before native shutdown saves it: if writeconfig wrote the profile instead,
    // this exec reads "exported", not "baseline". No internal hook substitutes for the console command.
    let steps = format!(
        "console=seta writeconfig_marker exported,console=bind f10 \"say named_config_export\",\
console=writeconfig commands-export,console=writeconfig commands-extension.cfg,\
console=writeconfig,saw=Usage: writeconfig <filename>:5,\
console=writeconfig commands-invalid extra,saw=Usage: writeconfig <filename>:5,\
console=exec \"{}\",cvaris=writeconfig_marker baseline,{STEPS}",
        active.display()
    );
    let mut out = StageReport::new(NAME, Status::Passed);
    match run_client(
        &client,
        install,
        &ctx.dir,
        &config,
        &["--bots", "9"],
        &steps,
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
            let export_problem = exports.iter().find_map(|file| {
                let path = config.join("players").join(file);
                match std::fs::read_to_string(&path) {
                    Ok(text)
                        if text.starts_with("// generated by cod4e\n")
                            && text.lines().any(|l| l == "seta writeconfig_marker \"exported\"")
                            && text.lines().any(|l| l == "unbindall")
                            && text.lines().any(|l| l == "bind \"f10\" \"say named_config_export\"") =>
                    {
                        out.files.push(format!("config/players/{file}"));
                        None
                    }
                    Ok(_) => Some(format!("{} lacks the exported archived cvar or bind", path.display())),
                    Err(e) => Some(format!("named writeconfig export {}: {e}", path.display())),
                }
            }).or_else(|| {
                ["commands-extension.cfg.cfg", "commands-invalid.cfg"]
                    .iter()
                    .find(|file| config.join("players").join(file).exists())
                    .map(|file| format!("writeconfig unexpectedly created {file}"))
            });
            match verdict(&report) {
                Some(w) => {
                    out.status = Status::Failed;
                    out.reason = Some(w);
                }
                None if export_problem.is_some() => {
                    out.status = Status::Failed;
                    out.reason = export_problem;
                }
                None if demo_len == 0 => {
                    out.status = Status::Failed;
                    out.reason = Some(format!(
                        "record left no demo at {} (the console said it was recording)",
                        demo.display()
                    ));
                }
                None => out.notes.push(format!(
                    "Pause refused, centerview levelled the view, record wrote {demo_len} bytes, the vote and the quick message reached the server, setFromLocString and cmd ran; writeconfig exported both named configs with the archived cvar and bind, left the active profile unchanged until its normal save, and rejected invalid arity"
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
