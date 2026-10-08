// SPDX-License-Identifier: GPL-3.0-only
//! `client-gfx`: the graphics settings of the options menus reach the window and the renderer. The real client sets
//! the dvars the menu writes, applies them with `vid_restart` (vsync off, a 4:3 screen), starts a match (4x
//! antialiasing, no specular, depth of field, glow or shadows), then turns the match's settings back with a second
//! `vid_restart` in the running match. Every `gfxis=` check reads the renderer's and the surface's own state. Needs a
//! display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-gfx";
const LIMIT: Duration = Duration::from_secs(300);

const STEPS: &str = "set=set r_vsync 0,set=set r_aspectRatio standard,set=set r_aaSamples 4,\
set=set r_specular 0,set=set r_dof_enable 0,set=set r_glow_allowed 0,set=set sm_enable 0,\
vidrestart,wait=1,gfxis=uncapped 1,gfxis=aspect 1.33,\
set=set ui_netGametypeName war,click=Start New Server,menu=createserver:20,click=Start,\
menu=team_marinesopfor:90,click=auto_assign,menu=changeclass:30,wait=1,click=Assault,ingame=120,wait=2,\
gfxis=aa 4,gfxis=specular 0,gfxis=dof 0,gfxis=glow 0,gfxis=shadows 0,shot=gfx-on,\
set=set r_aaSamples 1,set=set r_specular 1,set=set r_dof_enable 1,set=set r_glow_allowed 1,set=set sm_enable 1,\
set=set r_aspectRatio auto,vidrestart,wait=2,\
gfxis=aa 1,gfxis=specular 1,gfxis=dof 1,gfxis=glow 1,gfxis=shadows 1,gfxis=aspect 0.00,shot=gfx-off";

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
        &["--bots", "3"],
        STEPS,
        LIMIT,
    ) {
        Ok(report) => {
            for f in ["ui-script.json", "gfx-on.png", "gfx-off.png"] {
                out.files.push(f.into());
            }
            if let Some(w) = verdict(&report) {
                out.status = Status::Failed;
                out.reason = Some(w);
            } else {
                out.notes.push(
                    "vsync, aspect, antialiasing, specular, depth of field, glow and shadows took the menu's values, in the menu and in a running match".into(),
                );
            }
        }
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
        }
    }
    Ok(out)
}
