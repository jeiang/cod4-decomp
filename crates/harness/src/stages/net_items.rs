// SPDX-License-Identifier: GPL-3.0-only
//! `net-items`: two people (real UDP clients, no window) on mp_crash. One is killed
//! (`devkill`) and the stock scripts drop the weapon it held (`dropitem`). Asserts that the other client is shown the
//! weapon on the floor in their snapshots (`ET_ITEM`), that the other, standing at it and holding
//! +activate, gets the weapon's hint in the player state, and that taking it changes the
//! inventory and removes the item.
use super::net_objective::Human;
use crate::stage::{StageCtx, StageReport, Status};
use net::entity::etype;
use server::client::Session;
use server::server::Server;
use sim::weapon::pickup::WEAPON_HINT_OFFSET;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-items";
/// Waiting for a body and for the item to land depends on the game, not the runner.
const LIMIT: Duration = Duration::from_secs(120);

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let Some(addr) = server.net_addr() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("cannot bind a UDP socket"));
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_crash",
        "bots 4",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    let mut people = [Human::new(addr, 0)?, Human::new(addr, 1)?];
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let deadline = Instant::now() + LIMIT;
    let frame = |server: &mut Server, people: &mut [Human; 2]| {
        server.run_for(Duration::from_millis(16));
        for p in people.iter_mut() {
            p.step();
        }
    };
    // Playing, with the weapons on and the controls free (the countdown before a round freezes them).
    let playing = |server: &Server, people: &[Human; 2]| {
        people.iter().all(|p| {
            p.own.and_then(|n| server.game.client(n)).is_some_and(|c| {
                c.session == Session::Playing
                    && !c.frozen
                    && c.ps.weapon_flags & sim::pm::wf::DISABLED == 0
                    && c.inv.list_primaries(&server.game.weapons).next().is_some()
            })
        })
    };
    while !playing(&server, &people) {
        if Instant::now() > deadline {
            return fail("the two clients never got a body with a primary weapon".into());
        }
        frame(&mut server, &mut people);
    }
    let (a, b) = (
        people[0].own.unwrap_or_default(),
        people[1].own.unwrap_or_default(),
    );
    // A is killed by a bot's rifle (`devkill`), and the stock scripts drop the weapon A held
    // (`dropWeaponForDeath`). B must not own it, so it is taken from B.
    let weapon = server.game.client(a).map_or(0, |c| c.ps.weapon as u16);
    if weapon == 0 {
        return fail("the first player holds no weapon".into());
    }
    {
        let g = &mut server.game;
        let c = &mut g.clients[usize::from(b)];
        c.inv.take(&g.weapons, &mut c.ps, weapon, true);
    }
    let a_team = server.game.client(a).map(|c| c.team);
    let killer = loop {
        if Instant::now() > deadline {
            return fail("no bot of the other team was playing to kill the first player".into());
        }
        frame(&mut server, &mut people);
        let bot = server
            .game
            .clients
            .iter()
            .position(|c| c.bot && c.session == Session::Playing && Some(c.team) != a_team);
        if let Some(bot) = bot {
            break bot;
        }
    };
    if let Err(e) = server.exec_line(&format!("devkill {a} {killer}")) {
        return fail(format!("devkill: {e}"));
    }
    let item = loop {
        if Instant::now() > deadline {
            let st = server.game.client(a).map(|c| (c.session, c.ps.pm_type));
            let health = server.game.ent(a).map(|e| (e.health, e.flags));
            let any: Vec<_> = server
                .game
                .in_use()
                .filter(|(_, e)| e.item.is_some())
                .map(|(n, e)| (n, e.item.as_ref().map(|i| i.weapon)))
                .collect();
            return fail(format!(
                "the death left no weapon on the floor (A {st:?} {health:?}, items {any:?}, errors {:?})",
                server.script_errors
            ));
        }
        frame(&mut server, &mut people);
        if let Some(n) = server
            .game
            .in_use()
            .find(|(_, e)| e.item.is_some())
            .map(|(n, _)| n)
        {
            break n;
        }
    };
    // What was dropped may not be what was in hand; B must not own it either.
    let weapon = server
        .game
        .ent(item)
        .and_then(|e| e.item.as_ref().map(|i| i.weapon))
        .unwrap_or(weapon);
    {
        let g = &mut server.game;
        let c = &mut g.clients[usize::from(b)];
        c.inv.take(&g.weapons, &mut c.ps, weapon, true);
    }
    // Both clients are shown it, once it has landed.
    let shown = |p: &Human| {
        p.c.latest().and_then(|s| {
            s.entities
                .iter()
                .find(|e| e.etype == etype::ITEM && e.number == item)
                .map(|e| e.weapon)
        })
    };
    let mut at = [0.0f32; 3];
    loop {
        if Instant::now() > deadline {
            return fail("the item never showed in the other client's snapshot".into());
        }
        frame(&mut server, &mut people);
        let landed = server
            .game
            .ent(item)
            .is_some_and(|e| e.mv.pos.tr.kind == sim::traj::TrType::Stationary);
        if landed && shown(&people[1]) == Some(weapon) {
            at = server.game.ent(item).map_or(at, |e| e.origin);
            break;
        }
    }
    // B stands at it holding +activate: the hint names the weapon, then it is taken.
    people[1].hold_use = true;
    let hint = u16::from(WEAPON_HINT_OFFSET) + weapon;
    let mut hinted = false;
    while server.game.ent(item).is_some() {
        if Instant::now() > deadline {
            return fail(format!(
                "the item was never taken (hinted: {hinted}, B at {:?}, item at {at:?})",
                server.game.client(b).map(|c| c.ps.origin)
            ));
        }
        server.game.teleport(b, [at[0] - 30.0, at[1], at[2]]);
        for _ in 0..10 {
            frame(&mut server, &mut people);
            hinted |= server.game.client(b).is_some_and(|c| {
                u16::from(c.ps.cursor_hint) == hint && c.ps.cursor_hint_ent_index == item
            });
        }
    }
    people[1].hold_use = false;
    let got = server
        .game
        .client(b)
        .is_some_and(|c| c.inv.has(weapon) && c.inv.clip(&server.game.weapons, weapon) > 0);
    for p in &mut people {
        p.c.disconnect();
    }
    if !hinted {
        return fail("the player state never carried the weapon's hint".into());
    }
    if !got {
        return fail("the weapon did not arrive in the player's inventory".into());
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(format!(
        "weapon {} picked up",
        server.game.weapons.name(weapon)
    ));
    if !server.script_errors.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(format!("{:?}", server.script_errors));
    }
    Ok(report)
}
