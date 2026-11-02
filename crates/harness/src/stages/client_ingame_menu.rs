// SPDX-License-Identifier: GPL-3.0-only
//! `client-ingame-menu`: a player changes class and team in the middle of a round through the stock menus, with the
//! mouse, and gets back to the game each time. Escape closes the in-game menu; picking a class closes the class menu;
//! picking the other team closes the team menu, opens the server's class menu, and after that class pick the player
//! is on the new team and respawns with the new class's weapon. Every step must pass. Needs a display and the install;
//! skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{check_menu, driven_report, verdict, weapon_prefix};
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::json;
use std::io;
use std::time::Duration;

const NAME: &str = "client-ingame-menu";

fn open_ingame(c: &mut Driven) -> Result<(), String> {
    c.bind("togglemenu", "tap")?;
    c.until(
        json!({"menu.open.0": {"exists": true}}),
        Duration::from_secs(5),
        "the in-game menu did not open on the menu key",
    )?;
    c.pause(Duration::from_secs(1))?;
    Ok(())
}

fn no_menu(c: &mut Driven, what: &str) -> Result<(), String> {
    c.until(
        json!({"menu.open": {"len": 0}}),
        Duration::from_secs(5),
        what,
    )?;
    Ok(())
}

/// Join with the Assault class, then: Escape closes the in-game menu, a class pick closes the class menu,
/// and a team change (which kills the player) ends on the other team with a Spec Ops weapon after the respawn.
fn play(c: &mut Driven) -> Result<(), String> {
    c.step("create server", |c| {
        c.console("set ui_netGametypeName war")?;
        c.click("Start New Server")?;
        c.until(
            json!({"menu.open": {"contains": "createserver"}}),
            Duration::from_secs(20),
            "Start New Server did not open the create-server menu",
        )?;
        c.click("Start")?;
        c.until(
            json!({"menu.open": {"contains": "team_marinesopfor"}}),
            Duration::from_secs(90),
            "the listen server's team menu did not open",
        )?;
        c.pause(Duration::from_secs(1))?;
        check_menu(
            c,
            &[
                "OpFor",
                "Marines",
                "Auto-Assign",
                "Controls",
                "Options",
                "Leave Game",
            ],
            &["Spectator"],
        )?;
        c.capture("team_pre")?;
        Ok(())
    })?;
    c.step("initial team and class", |c| {
        c.click("OpFor")?;
        c.until(
            json!({"menu.open": {"contains": "changeclass"}}),
            Duration::from_secs(30),
            "the class menu did not open after OpFor was picked",
        )?;
        c.pause(Duration::from_secs(1))?;
        check_menu(
            c,
            &[
                "Assault",
                "Spec Ops",
                "Heavy Gunner",
                "Demolitions",
                "Sniper",
            ],
            &[],
        )?;
        c.capture("class_pre")?;
        c.click("Assault")?;
        c.until(
            json!({"net.spawned": true, "player.alive": true}),
            Duration::from_secs(120),
            "the player did not spawn",
        )?;
        c.pause(Duration::from_secs(3))?;
        c.until(
            json!({"player.team": 1}),
            Duration::from_secs(5),
            "the player did not join OpFor",
        )?;
        weapon_prefix(c, "m16_gl", Duration::from_secs(5))
    })?;
    c.step("Escape closes the in-game menu", |c| {
        open_ingame(c)?;
        check_menu(
            c,
            &[
                "Choose Class",
                "Change Team",
                "Controls",
                "Options",
                "Call Vote",
                "Mute Players",
                "Leave Game",
            ],
            &[],
        )?;
        c.capture("escape_menu")?;
        c.key("escape", "tap")?;
        no_menu(c, "Escape did not close the in-game menu")
    })?;
    c.step("Escape closes the class menu", |c| {
        open_ingame(c)?;
        c.click("Choose Class")?;
        c.until(
            json!({"menu.open": {"contains": "changeclass"}}),
            Duration::from_secs(5),
            "Choose Class did not open the class menu",
        )?;
        c.pause(Duration::from_secs(1))?;
        c.capture("class_in")?;
        c.key("escape", "tap")?;
        no_menu(c, "Escape did not close the class menu")
    })?;
    c.step("class pick returns to the game", |c| {
        open_ingame(c)?;
        c.click("Choose Class")?;
        c.until(
            json!({"menu.open": {"contains": "changeclass"}}),
            Duration::from_secs(5),
            "Choose Class did not reopen the class menu",
        )?;
        c.click("Spec Ops")?;
        no_menu(c, "the class menu did not close after Spec Ops was picked")?;
        c.pause(Duration::from_secs(2))?;
        Ok(())
    })?;
    c.step("team and class change", |c| {
        open_ingame(c)?;
        c.click("Change Team")?;
        c.until(
            json!({"menu.open": {"contains": "team_marinesopfor"}}),
            Duration::from_secs(5),
            "Change Team did not open the team menu",
        )?;
        c.pause(Duration::from_secs(1))?;
        check_menu(
            c,
            &["Marines", "Auto-Assign", "Spectator"],
            &["Controls", "Options", "Leave Game"],
        )?;
        c.capture("change_team")?;
        c.click("Marines")?;
        c.until(
            json!({"menu.open": {"contains": "changeclass"}}),
            Duration::from_secs(10),
            "the other team's class menu did not open",
        )?;
        c.click("Spec Ops")?;
        no_menu(c, "the class menu did not close after the team change")?;
        c.until(
            json!({"player.team": 2}),
            Duration::from_secs(10),
            "the player did not change to Marines",
        )?;
        weapon_prefix(c, "mp5", Duration::from_secs(90))?;
        c.pause(Duration::from_secs(2))?;
        no_menu(c, "a menu reopened after the team change")?;
        c.capture("changed")?;
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
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let mut out = StageReport::new(NAME, Status::Passed);
    let dir = ctx.dir.join("control");
    let mut c = match Driven::start(&client, install, &dir, &["--bots", "9"]) {
        Ok(c) => c,
        Err(e) => {
            out.status = Status::Failed;
            out.reason = Some(e);
            return Ok(out);
        }
    };
    let played = play(&mut c);
    let report = driven_report(&mut c);
    let status = c.finish(Duration::from_secs(30));
    out.files.push("control/control-run.json".into());
    for f in [
        "team_pre",
        "class_pre",
        "escape_menu",
        "class_in",
        "change_team",
        "changed",
    ] {
        if dir.join(format!("{f}.png")).is_file() {
            out.files.push(format!("control/{f}.png"));
        }
    }
    let why = match (played, report) {
        (Err(e), _) => Some(e),
        (Ok(()), Err(e)) => Some(format!("the run's report could not be read: {e}")),
        (Ok(()), Ok(report)) => verdict(&report),
    };
    if let Some(w) = why {
        out.status = Status::Failed;
        out.reason = Some(w);
    } else if !status.is_some_and(|s| s.success()) {
        out.status = Status::Failed;
        out.reason = Some(format!("the client did not exit successfully: {status:?}"));
    } else {
        out.notes.push(
            "Escape and the class and team picks each returned to the game; the team changed and the new class spawned"
                .into(),
        );
    }
    Ok(out)
}
